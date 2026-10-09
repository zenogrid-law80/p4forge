#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use tokio::process::Command;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
mod windows;
use anyhow::{Context, Result, ensure};
use p4forge_domain::{OperatingSystem, PipelineDefinition, Shell, Workspace};
use p4forge_perforce::Perforce;
use p4forge_protocol::*;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::{mpsc, watch},
    time::Instant,
};
use url::Url;
use uuid::Uuid;

mod artifact_inputs;
mod artifacts;
mod credentials;
mod docker;
pub mod publication;
pub mod self_patch;
pub struct AgentConfig {
    pub publisher: Option<publication::Publisher>,
    pub server: Url,
    pub token: String,
    pub work_dir: PathBuf,
    pub state_dir: PathBuf,
    pub perforce: Perforce,
    pub allow_http: bool,
    pub once: bool,
    pub build_account_file: Option<PathBuf>,
    pub credential_files: Vec<PathBuf>,
}
#[derive(Clone)]
struct Api {
    client: reqwest::Client,
    server: Url,
    token: String,
}
impl Api {
    async fn post<I: Serialize, O: DeserializeOwned>(&self, path: &str, input: &I) -> Result<O> {
        Ok(self
            .client
            .post(self.server.join(path)?)
            .bearer_auth(&self.token)
            .json(input)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
    async fn log(&self, a: &Assignment, sequence: &mut i64, entry: Entry) -> Result<()> {
        let input = LogChunk {
            fence: a.fence,
            sequence: *sequence + 1,
            stream: entry.stream.into(),
            content: entry.content,
        };
        let path = format!("agent/attempts/{}/logs", a.attempt_id);
        let mut result: Result<serde_json::Value> = self.post(&path, &input).await;
        if result.is_err() && !terminal_error(result.as_ref().unwrap_err()) {
            result = self.post(&path, &input).await;
        }
        result?;
        *sequence += 1;
        Ok(())
    }
}
fn terminal_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<reqwest::Error>()
        .and_then(|error| error.status())
        .is_some_and(|status| status.is_client_error())
}
struct Entry {
    stream: &'static str,
    content: String,
}
#[derive(Clone, Debug)]
struct Outcome {
    status: String,
    exit_code: Option<i32>,
    reason: String,
}
impl Outcome {
    fn failed(reason: impl Into<String>) -> Self {
        Self {
            status: "failed".into(),
            exit_code: None,
            reason: reason.into(),
        }
    }
}

pub async fn run(mut config: AgentConfig) -> Result<()> {
    ensure!(
        config.server.scheme() == "https"
            || (config.allow_http && config.server.scheme() == "http"),
        "HTTPS required; internal development HTTP requires explicit opt-in"
    );
    ensure!(
        config.server.path() == "/"
            && config.server.query().is_none()
            && config.server.fragment().is_none()
            && config.server.username().is_empty()
            && config.server.password().is_none(),
        "server URL must be an origin"
    );
    #[cfg(windows)]
    {
        let account_file = config
            .build_account_file
            .as_ref()
            .context("--build-account-file required on Windows")?;
        let account = windows::Account::load(account_file)?;
        let mut credentials = config.credential_files.clone();
        credentials.push(account_file.clone());
        windows::validate_supervisor(&account, credentials)?;
    }
    ensure!(config.token.len() == 43, "invalid agent credential file");
    ensure!(
        config.work_dir.is_absolute()
            && config.state_dir.is_absolute()
            && config.work_dir != config.state_dir,
        "distinct absolute work and state directories required"
    );
    tokio::fs::create_dir_all(&config.work_dir).await?;
    tokio::fs::create_dir_all(&config.state_dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&config.state_dir, std::fs::Permissions::from_mode(0o700))
            .await?;
        tokio::fs::set_permissions(&config.work_dir, std::fs::Permissions::from_mode(0o755))
            .await?;
    }
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config.state_dir.join("agent.lock"))?;
    lock.try_lock()
        .context("another agent is using this state directory")?;
    #[cfg(target_os = "macos")]
    let _watchdog = {
        macos::validate_supervisor(&config)?;
        // The watchdog shares the lock until all build descendants are gone.
        let guard = macos::Watchdog::start(&lock)?;
        macos::kill_build_processes()?;
        guard
    };
    credentials::prepare(&mut config)?;
    let api = Api {
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()?,
        server: config.server.clone(),
        token: config.token.clone(),
    };
    let _credentials = credentials::start(&api, &config).await;
    let marker = config.state_dir.join("inflight.json");
    if marker.exists() {
        // Native watchdogs / Docker terminate the previous process tree first.
        let previous: Assignment = serde_json::from_slice(&tokio::fs::read(&marker).await?)?;
        cleanup(
            &config.perforce,
            &previous,
            &config.work_dir,
            config.publisher.as_ref(),
        )
        .await
        .context("recover previous container workspace before accepting work")?;
        tokio::fs::remove_file(&marker).await?;
    }
    let (shutdown_tx, mut shutdown) = watch::channel(false);
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("install SIGTERM handler");
            tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=term.recv()=>{} }
        }
        #[cfg(not(unix))]
        {
            if tokio::signal::ctrl_c().await.is_err() {
                // Scheduled tasks can have no console. Their termination still
                // closes the Job Object; absence of a console is not shutdown.
                std::future::pending::<()>().await;
            }
        }
        let _ = shutdown_tx.send(true);
    });
    let mut request_id = Uuid::new_v4();
    let updater = self_patch::Reporter::new(&config.state_dir)?;
    let mut docker = docker::Reporter::new();
    loop {
        if *shutdown.borrow() {
            break;
        }
        let updating = match updater.poll(&api).await {
            Ok(pending) => pending,
            Err(_) => updater.pending(),
        };
        if updating {
            tokio::select! { _=tokio::time::sleep(Duration::from_secs(2))=>{}, _=shutdown.changed()=>break }
            continue;
        }
        // Probe only while idle: native process cleanup owns the entire build account.
        docker.report_if_due(&api, &config).await;
        let claim = ClaimRequest {
            request_id,
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            protocol_version: 8,
        };
        let response = tokio::select! { response=api.post::<_,ClaimResponse>("agent/claim",&claim)=>response, _=shutdown.changed()=>break };
        match response {
            Ok(ClaimResponse {
                assignment: Some(assignment),
            }) => {
                tracing::info!(attempt=%assignment.attempt_id,job=%assignment.job.name,"job claimed");
                // Persist only non-secret inputs. Never accept a second job before cleanup finishes.
                tokio::fs::write(&marker, serde_json::to_vec(&assignment)?).await?;
                perform(&api, &config, &assignment, shutdown.clone()).await?;
                tokio::fs::remove_file(&marker).await?;
                request_id = Uuid::new_v4();
                if config.once {
                    break;
                }
            }
            Ok(_) => {
                request_id = Uuid::new_v4();
                if config.once {
                    break;
                }
            }
            Err(error) => {
                if terminal_error(&error) {
                    return Err(error.context("agent authentication or claim rejected"));
                }
                tracing::warn!("claim unavailable; retrying the same request ID");
            }
        }
        tokio::select! { _=tokio::time::sleep(Duration::from_secs(2))=>{}, _=shutdown.changed()=>break }
    }
    drop(lock);
    Ok(())
}

async fn perform(
    api: &Api,
    config: &AgentConfig,
    a: &Assignment,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let (cancel_tx, cancel) = watch::channel(false);
    let (log_tx, mut log_rx) = mpsc::channel::<Entry>(32);
    let p4 = config.perforce.clone();
    let assignment = a.clone();
    let work = config.work_dir.clone();
    let worker_api = api.clone();
    let build_account = config.build_account_file.clone();
    let publisher = config.publisher.clone();
    let mut worker = tokio::spawn(async move {
        let mut cancel = cancel;
        let outcome = tokio::select! {
            result=execute(&worker_api,&p4,&assignment,&work,build_account.as_deref(),publisher.as_ref(),log_tx)=>match result { Ok(value)=>value,Err(error)=>Outcome::failed(format!("{error:#}")) },
            _=cancel.changed()=>Outcome::failed("execution interrupted"),
        };
        cleanup(&p4, &assignment, &work, publisher.as_ref()).await?;
        Ok::<_, anyhow::Error>(outcome)
    });
    let started = Instant::now();
    let mut lease_deadline = started + Duration::from_secs(a.lease_seconds.saturating_sub(5));
    let seconds = if a.workspace_jobs.is_empty() {
        a.job.timeout_seconds
    } else {
        a.workspace_jobs
            .iter()
            .map(|j| j.job.timeout_seconds)
            .sum::<u64>()
            + 300
    };
    let job_deadline = started
        + Duration::from_secs(
            seconds
                + if a.artifacts.is_empty() { 0 } else { 600 }
                + if a.publish_generated.is_some() {
                    300
                } else {
                    0
                },
        );
    let (lease_tx, lease_rx) = watch::channel(lease_deadline);
    let (stop_tx, mut stop_rx) = watch::channel(None);
    let watchdog = tokio::spawn(watch_deadlines(
        lease_rx,
        job_deadline,
        shutdown,
        cancel_tx.clone(),
        stop_tx,
    ));
    let mut stop_observed = false;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECONDS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut sequence = -1;
    let mut outcome = None;
    let mut forced = None;
    let mut logs_open = true;
    let mut worker_done = false;
    let mut lease_valid = true;
    let mut cleanup_error = None;
    while !worker_done || logs_open {
        tokio::select! {
            result=&mut worker, if !worker_done => {
                worker_done=true;
                match result {
                    Ok(Ok(value))=>outcome=Some(value),
                    other=>{cleanup_error=Some(format!("workspace cleanup failed: {other:?}"));outcome=Some(Outcome::failed("workspace cleanup failed; agent stopped"));}
                }
            }
            entry=log_rx.recv(), if logs_open => {
                match entry {
                    Some(entry) if lease_valid=>if let Err(error)=api.log(a,&mut sequence,entry).await {
                        forced.get_or_insert_with(||Outcome::failed("log delivery failed; execution stopped"));
                        let _=cancel_tx.send(true);
                        if terminal_error(&error) { lease_valid=false; }
                    },
                    Some(_)=>{},
                    None=>logs_open=false,
                }
            }
            _=heartbeat.tick(), if lease_valid => {
                let sent_at=Instant::now();
                match api.post::<_,HeartbeatResponse>(&format!("agent/attempts/{}/heartbeat",a.attempt_id),&AttemptRequest { fence:a.fence }).await {
                    Ok(response)=> {
                        lease_deadline=sent_at+Duration::from_secs(response.lease_seconds.saturating_sub(5));
                        let _=lease_tx.send(lease_deadline);
                        if response.cancel_requested { forced=Some(Outcome { status:"canceled".into(),exit_code:None,reason:"run canceled".into() });let _=cancel_tx.send(true); }
                    }
                    Err(error)=>if terminal_error(&error) { lease_valid=false;forced=Some(Outcome::failed("attempt rejected or revoked"));let _=cancel_tx.send(true); },
                }
            }
            _=stop_rx.changed(), if !stop_observed=> { stop_observed=true; }
        }
    }
    watchdog.abort();
    if let Some(reason) = *stop_rx.borrow() {
        forced.get_or_insert_with(|| Outcome::failed(reason));
        if reason.starts_with("lease") {
            lease_valid = false;
        }
    }
    let outcome = forced
        .or(outcome)
        .context("worker ended without an outcome")?;
    let reason: String = outcome.reason.chars().take(1000).collect();
    if lease_valid {
        let input = CompleteRequest {
            fence: a.fence,
            status: outcome.status.clone(),
            exit_code: outcome.exit_code,
            reason,
            last_log_sequence: sequence,
        };
        let path = format!("agent/attempts/{}/complete", a.attempt_id);
        loop {
            match api.post::<_, serde_json::Value>(&path, &input).await {
                Ok(_) => break,
                Err(error)
                    if terminal_error(&error)
                        || Instant::now() + Duration::from_secs(5) >= lease_deadline =>
                {
                    tracing::warn!(attempt=%a.attempt_id,"completion not accepted; server will reconcile lease");
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
            }
        }
    }
    tracing::info!(attempt=%a.attempt_id,status=%outcome.status,"job stopped and workspace cleanup finished");
    if let Some(error) = cleanup_error {
        anyhow::bail!(error);
    }
    Ok(())
}

async fn watch_deadlines(
    mut lease: watch::Receiver<Instant>,
    job_deadline: Instant,
    mut shutdown: watch::Receiver<bool>,
    cancel: watch::Sender<bool>,
    reason: watch::Sender<Option<&'static str>>,
) {
    let cause = loop {
        if *shutdown.borrow() {
            break "agent shutting down";
        }
        let deadline = *lease.borrow_and_update();
        tokio::select! {
            _=tokio::time::sleep_until(deadline)=>break "lease renewal failed; execution stopped",
            _=tokio::time::sleep_until(job_deadline)=>break "job timeout",
            changed=lease.changed()=>if changed.is_err() { return; },
            _=shutdown.changed()=>break "agent shutting down",
        }
    };
    let _ = reason.send(Some(cause));
    let _ = cancel.send(true);
}

async fn cleanup(
    p4: &Perforce,
    a: &Assignment,
    root: &Path,
    publisher: Option<&publication::Publisher>,
) -> Result<()> {
    #[cfg(target_os = "macos")]
    macos::kill_build_processes()?;
    #[cfg(target_os = "linux")]
    tokio::time::timeout(Duration::from_secs(3), async {
        while kill_build_processes()? {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("build processes did not terminate")??;
    if let Some(publisher) = publisher {
        publisher.cleanup(p4, a).await?;
    }
    p4.delete_client(&a.source.connection, a.attempt_id).await?;
    let directory = root.join(a.attempt_id.to_string());
    if tokio::fs::try_exists(&directory).await? {
        #[cfg(windows)]
        windows::remove_readonly(&directory)?;
        tokio::fs::remove_dir_all(directory).await?;
    }
    Ok(())
}
async fn report_step(
    api: &Api,
    a: &Assignment,
    job_id: Uuid,
    status: &str,
    exit_code: Option<i32>,
    reason: &str,
) -> Result<()> {
    let request = StepRequest {
        fence: a.fence,
        job_id,
        status: status.into(),
        exit_code,
        reason: reason.into(),
    };
    let path = format!("agent/attempts/{}/step", a.attempt_id);
    let mut response: Result<serde_json::Value> = api.post(&path, &request).await;
    if response.is_err() && !terminal_error(response.as_ref().unwrap_err()) {
        response = api.post(&path, &request).await;
    }
    response?;
    Ok(())
}

async fn execute(
    api: &Api,
    p4: &Perforce,
    a: &Assignment,
    root: &Path,
    build_account_file: Option<&Path>,
    publisher: Option<&publication::Publisher>,
    logs: mpsc::Sender<Entry>,
) -> Result<Outcome> {
    #[cfg(windows)]
    let build_account =
        windows::Account::load(build_account_file.context("build account missing")?)?;
    #[cfg(not(windows))]
    let _ = build_account_file;
    if a.publish_generated.is_some() {
        publisher
            .context("publisher policy is not installed")?
            .validate(a)?;
    }
    let jobs = if a.workspace_jobs.is_empty() {
        vec![AssignedJob {
            id: a.job_id,
            job: a.job.clone(),
        }]
    } else {
        a.workspace_jobs.clone()
    };
    for assigned in &jobs {
        PipelineDefinition {
            schema_version: 1,
            source_root_id: Uuid::new_v4(),
            max_parallel_jobs: 1,
            workspace: Workspace::Job,
            stages: vec![],
            publish_generated: None,
            on_success: vec![],
            artifacts: vec![],
            artifact_inputs: vec![],
            deployment: None,
            jobs: vec![p4forge_domain::Job {
                needs: vec![],
                ..assigned.job.clone()
            }],
        }
        .validate()?;
        ensure!(
            (cfg!(target_os = "linux")
                && assigned.job.runner.os == OperatingSystem::Linux
                && assigned.job.shell == Shell::Sh)
                || (cfg!(target_os = "macos")
                    && assigned.job.runner.os == OperatingSystem::Macos
                    && assigned.job.shell == Shell::Sh)
                || (cfg!(windows)
                    && assigned.job.runner.os == OperatingSystem::Windows
                    && assigned.job.shell == Shell::PowerShell),
            "job does not match this agent platform"
        );
    }
    let directory = root.join(a.attempt_id.to_string());
    tokio::fs::create_dir(&directory)
        .await
        .context("create fresh attempt workspace")?;
    let source = directory.join("source");
    let home = directory.join("home");
    let temp = directory.join("tmp");
    for path in [&source, &home, &temp] {
        tokio::fs::create_dir(path).await?;
    }
    logs.send(Entry {
        stream: "system",
        content: format!(
            "Checkout submitted CL {} with saved Perforce view\n",
            a.source.changelist
        ),
    })
    .await?;
    p4.checkout(&a.source, a.attempt_id, &source).await?;
    if !a.artifact_inputs.is_empty() {
        artifact_inputs::download(api, a, &directory, &logs).await?;
        #[cfg(windows)]
        windows::grant_inputs(&directory.join("inputs"), &build_account).await?;
    }
    // The build may modify source/home/tmp, but cannot replace supervisor scripts.
    for path in [&source, &home, &temp] {
        #[cfg(unix)]
        chown_tree(path, 10003)?;
        #[cfg(windows)]
        windows::grant_workspace(path, &build_account).await?;
    }
    let canonical_source = tokio::fs::canonicalize(&source).await?;
    let budget = Arc::new(AtomicUsize::new(0));
    for assigned in jobs {
        let job = &assigned.job;
        if !a.workspace_jobs.is_empty() {
            report_step(api, a, assigned.id, "running", None, "").await?;
        }
        logs.send(Entry {
            stream: "system",
            content: format!("\n[{} / {}] started\n", job.stage, job.name),
        })
        .await?;
        let result: Result<Outcome> = async {
            let cwd = tokio::fs::canonicalize(source.join(&job.working_directory)).await?;
            ensure!(
                cwd.starts_with(&canonical_source) && cwd.is_dir(),
                "working directory escapes the source workspace"
            );
            let mut environment = job.environment.clone();
            if !a.artifact_inputs.is_empty() {
                environment.insert("P4FORGE_INPUTS".into(), directory.join("inputs").to_string_lossy().into());
                environment.insert("P4FORGE_SOURCE_RUN_ID".into(), a.artifact_inputs[0].source_run_id.to_string());
            }
            environment.insert("P4FORGE_RUN_ID".into(), a.run_id.to_string());
            environment.insert("P4FORGE_ATTEMPT_ID".into(), a.attempt_id.to_string());
            environment.insert("P4FORGE_CHANGELIST".into(), a.source.changelist.to_string());
            environment.insert("HOME".into(), home.to_string_lossy().into());
            let process = async {
                #[cfg(windows)]
                {
                    let script = directory.join(format!("{}.ps1", assigned.id));
                    let commands = job.script.iter().map(|command| format!("$global:LASTEXITCODE = 0\r\n{command}\r\nif ($LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}\r\n")).collect::<String>();
                    tokio::fs::write(&script, format!("\u{feff}$ErrorActionPreference = 'Stop'\r\n[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)\r\n{commands}\r\nexit 0\r\n")).await?;
                    environment.insert("PATH".into(), std::env::var("PATH")?);
                    environment.insert("SystemRoot".into(), std::env::var("SystemRoot")?);
                    environment.insert("TEMP".into(), temp.to_string_lossy().into());
                    environment.insert("TMP".into(), temp.to_string_lossy().into());
                    environment.insert("USERPROFILE".into(), home.to_string_lossy().into());
                    environment.insert("APPDATA".into(), home.join("AppData/Roaming").to_string_lossy().into());
                    environment.insert("LOCALAPPDATA".into(), home.join("AppData/Local").to_string_lossy().into());
                    windows::run(&build_account, &script, &cwd, environment, logs.clone(), budget.clone()).await
                }
                #[cfg(not(windows))]
                {
                    let script = directory.join(format!("{}.sh", assigned.id));
                    tokio::fs::write(&script, format!("#!/bin/sh\nset -eu\n{}\n", job.script.join("\n"))).await?;
                    let mut command = Command::new("/bin/sh");
                    command.arg(&script).current_dir(&cwd).env_clear().envs(environment)
                        .env("PATH", "/usr/local/bin:/usr/bin:/bin").env("TMPDIR", &temp).env("LANG", "C.UTF-8");
                    #[cfg(target_os = "macos")]
                    command.env("PATH", "/Library/P4Forge/toolchain/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin")
                        .env("LANG", "en_US.UTF-8");
                    run_process(command, logs.clone(), Some(10003), budget.clone()).await
                }
            };
            let result = tokio::time::timeout(Duration::from_secs(job.timeout_seconds), process).await;
            match result {
                Ok(result) => {
                    let status = result?;
                    Ok(Outcome {
                        status: if status.success() {
                            "succeeded"
                        } else {
                            "failed"
                        }
                        .into(),
                        exit_code: status.code(),
                        reason: if status.success() {
                            String::new()
                        } else {
                            "build process failed".into()
                        },
                    })
                }
                Err(_) => Ok(Outcome::failed("job timeout")),
            }
        }
        .await;
        let outcome = result.unwrap_or_else(|error| {
            Outcome::failed(format!("{error:#}").chars().take(250).collect::<String>())
        });
        if !a.workspace_jobs.is_empty() {
            report_step(
                api,
                a,
                assigned.id,
                &outcome.status,
                outcome.exit_code,
                &outcome.reason,
            )
            .await?;
        }
        logs.send(Entry {
            stream: "system",
            content: format!("[{} / {}] {}\n", job.stage, job.name, outcome.status),
        })
        .await?;
        if outcome.status != "succeeded" {
            return Ok(outcome);
        }
    }
    if a.publish_generated.is_some() || !a.artifacts.is_empty() {
        #[cfg(target_os = "macos")]
        macos::kill_build_processes()?;
        #[cfg(target_os = "linux")]
        ensure!(
            !kill_build_processes()?,
            "build descendants still present before publication"
        );
    }
    if !a.artifacts.is_empty() {
        artifacts::upload(api, a, &directory, &logs).await?;
    }
    if a.publish_generated.is_some() {
        publisher
            .context("publisher policy missing")?
            .publish(api, p4, a, &directory, &logs)
            .await?;
    }
    Ok(Outcome {
        status: "succeeded".into(),
        exit_code: Some(0),
        reason: String::new(),
    })
}

#[cfg(unix)]
fn chown_tree(path: &Path, uid: u32) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let metadata = std::fs::symlink_metadata(path)?;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // lchown never follows a Perforce symlink into credentials or host paths.
    ensure!(
        unsafe { libc::lchown(name.as_ptr(), uid, uid) } == 0,
        "set build workspace ownership: {}",
        std::io::Error::last_os_error()
    );
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            chown_tree(&entry?.path(), uid)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
#[derive(Debug)]
struct ProcessGroup {
    pid: i32,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    build_uid: Option<u32>,
}

// The dedicated container has one build UID and one slot. This also catches
// descendants that deliberately start a new session and leave the process group.
#[cfg(target_os = "linux")]
fn kill_build_processes() -> Result<bool> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let mut found = false;
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        // Pin the process identity before inspecting UID, so PID reuse cannot
        // make cleanup signal an unrelated supervisor process.
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
        if raw < 0 {
            let error = std::io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::ESRCH | libc::ENOENT)) {
                continue;
            }
            return Err(error.into());
        }
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        let Ok(status) = std::fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        let build = status
            .lines()
            .find_map(|line| line.strip_prefix("Uid:"))
            .and_then(|line| line.split_whitespace().next())
            == Some("10003");
        let zombie = status
            .lines()
            .any(|line| line.starts_with("State:") && line.contains("Z (zombie)"));
        if build && !zombie {
            found = true;
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            if result < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                return Err(std::io::Error::last_os_error().into());
            }
        }
    }
    Ok(found)
}
#[cfg(unix)]
impl ProcessGroup {
    fn kill(&mut self) {
        #[cfg(unix)]
        unsafe {
            if self.pid > 0 {
                #[cfg(target_os = "macos")]
                let signal_group = self.build_uid.is_none();
                #[cfg(not(target_os = "macos"))]
                let signal_group = true;
                if signal_group {
                    libc::kill(-self.pid, libc::SIGKILL);
                }
                self.pid = 0;
            }
        }
        #[cfg(target_os = "linux")]
        if self.build_uid == Some(10003) {
            let _ = kill_build_processes();
        }
        #[cfg(target_os = "macos")]
        if self.build_uid == Some(10003) {
            let _ = macos::kill_build_processes();
        }
    }
}
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(unix)]
async fn run_process(
    mut command: Command,
    logs: mpsc::Sender<Entry>,
    uid: Option<u32>,
    budget: Arc<AtomicUsize>,
) -> Result<std::process::ExitStatus> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        command.process_group(0);
        if let Some(uid) = uid {
            // These credential syscalls are async-signal-safe and run before exec.
            unsafe {
                command.pre_exec(move || {
                    if libc::setgroups(0, std::ptr::null()) != 0
                        || libc::setgid(uid) != 0
                        || libc::setuid(uid) != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
    }
    let mut child = command.spawn().context("start build process")?;
    let mut group = ProcessGroup {
        pid: child.id().context("child has no PID")? as i32,
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        build_uid: uid,
    };
    let stdout = tokio::spawn(capture(
        child.stdout.take().context("stdout missing")?,
        "stdout",
        logs.clone(),
        budget.clone(),
    ));
    let stderr = tokio::spawn(capture(
        child.stderr.take().context("stderr missing")?,
        "stderr",
        logs,
        budget,
    ));
    let status = child.wait().await?;
    // Also terminate background descendants after a successful shell exit.
    group.kill();
    stdout.await??;
    stderr.await??;
    Ok(status)
}
async fn capture<R: AsyncRead + Unpin>(
    mut source: R,
    stream: &'static str,
    logs: mpsc::Sender<Entry>,
    budget: Arc<AtomicUsize>,
) -> Result<()> {
    let mut buffer = [0u8; 4096];
    loop {
        let count = source.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let previous = budget.fetch_add(count, Ordering::Relaxed);
        if previous < 1024 * 1024 {
            let allowed = count.min(1024 * 1024 - previous);
            let content = String::from_utf8_lossy(&buffer[..allowed]).replace('\0', "�");
            if logs.send(Entry { stream, content }).await.is_err() {
                break;
            }
        }
        if previous < 1024 * 1024 && previous + count >= 1024 * 1024 {
            let _=logs.send(Entry { stream:"system",content:"\n[Output truncated after 1 MiB; process output continues to be drained.]\n".into() }).await;
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancels_descendants_and_kills_background_processes_after_success() {
        for script in ["sleep 30 & echo $!; wait", "sleep 30 & echo $!"] {
            let (tx, mut rx) = mpsc::channel(8);
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            let task = tokio::spawn(run_process(
                command,
                tx,
                None,
                Arc::new(AtomicUsize::new(0)),
            ));
            let output = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            let pid: i32 = output.content.trim().parse().unwrap();
            if script.ends_with("wait") {
                task.abort();
                let _ = task.await;
            } else {
                assert!(task.await.unwrap().unwrap().success());
            }
            // A killed descendant may briefly remain a zombie until init reaps it.
            #[cfg(target_os = "linux")]
            {
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                            .unwrap_or_default();
                        if stat.is_empty() || stat.split_whitespace().nth(2) == Some("Z") {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("descendant still running");
            }
            #[cfg(target_os = "macos")]
            {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while unsafe { libc::kill(pid, 0) } == 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("descendant still running on macOS");
            }
        }
    }
}
