//! Read-only capability check under the same identity as build scripts.
//! Called between claims because build-process cleanup owns the entire build UID.
use super::*;

pub(super) struct Reporter {
    next_check: Instant,
}

impl Reporter {
    pub(super) fn new() -> Self {
        Self {
            next_check: Instant::now(),
        }
    }

    pub(super) async fn report_if_due(&mut self, api: &Api, config: &AgentConfig) {
        if Instant::now() < self.next_check {
            return;
        }
        let status = probe(config).await.unwrap_or_else(|_| capability(None));
        // Old servers may not expose this optional endpoint during rolling updates.
        if api
            .post::<_, serde_json::Value>("agent/docker-status", &status)
            .await
            .is_err()
        {
            tracing::debug!("Docker status report unavailable");
        }
        self.next_check = Instant::now() + Duration::from_secs(60);
    }
}

fn capability(code: Option<i32>) -> DockerCapability {
    let (state, engine_os) = match code {
        Some(0) => (DockerState::Available, Some(DockerEngineOs::Linux)),
        Some(2) => (DockerState::Available, Some(DockerEngineOs::Windows)),
        Some(20) => (DockerState::CliMissing, None),
        Some(21) => (DockerState::EngineUnavailable, None),
        _ => (DockerState::CheckFailed, None),
    };
    DockerCapability { state, engine_os }
}

#[cfg(unix)]
const SCRIPT: &str = r#"exec 2>/dev/null
command -v docker >/dev/null 2>&1 || exit 20
engine=$(docker info --format '{{.OSType}}') || exit 21
case "$engine" in linux) exit 0;; windows) exit 2;; *) exit 22;; esac
"#;

#[cfg(windows)]
const SCRIPT: &str = "\u{feff}$ErrorActionPreference = 'SilentlyContinue'\r\n\
$docker = Get-Command docker.exe -CommandType Application\r\n\
if (!$docker) { exit 20 }\r\n\
$engine = & $docker.Source info --format '{{.OSType}}' 2>$null\r\n\
if ($LASTEXITCODE -ne 0) { exit 21 }\r\n\
if ($engine -ceq 'linux') { exit 0 }\r\n\
if ($engine -ceq 'windows') { exit 2 }\r\n\
exit 22\r\n";

async fn probe(config: &AgentConfig) -> Result<DockerCapability> {
    let directory = config
        .work_dir
        .join(format!(".docker-probe-{}", Uuid::new_v4()));
    tokio::fs::create_dir(&directory).await?;
    let result = probe_in(config, &directory).await;
    let cleanup = tokio::fs::remove_dir_all(&directory).await;
    cleanup?;
    result
}

async fn probe_in(_config: &AgentConfig, directory: &Path) -> Result<DockerCapability> {
    let home = directory.join("home");
    let temp = directory.join("tmp");
    tokio::fs::create_dir(&home).await?;
    tokio::fs::create_dir(&temp).await?;
    // Never forward probe output (including Docker endpoint errors) to the server.
    let (logs, receiver) = mpsc::channel(1);
    drop(receiver);
    let budget = Arc::new(AtomicUsize::new(0));
    #[cfg(unix)]
    let process = {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755)).await?;
        chown_tree(&home, 10003)?;
        chown_tree(&temp, 10003)?;
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", SCRIPT])
            .current_dir(&home)
            .env_clear()
            .env("HOME", &home)
            .env("TMPDIR", &temp)
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("LANG", "C.UTF-8");
        #[cfg(target_os = "macos")]
        command.env("PATH", "/Library/P4Forge/toolchain/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin")
            .env("LANG", "en_US.UTF-8");
        run_process(command, logs, Some(10003), budget)
    };
    #[cfg(windows)]
    let process = {
        use std::collections::BTreeMap;
        let account = windows::Account::load(
            _config
                .build_account_file
                .as_ref()
                .context("build account missing")?,
        )?;
        windows::grant_workspace(directory, &account).await?;
        let script = directory.join("probe.ps1");
        tokio::fs::write(&script, SCRIPT).await?;
        let cwd = tokio::fs::canonicalize(&home).await?;
        let mut environment = BTreeMap::from([
            ("PATH".into(), std::env::var("PATH")?),
            ("SystemRoot".into(), std::env::var("SystemRoot")?),
            ("HOME".into(), home.to_string_lossy().into_owned()),
            ("USERPROFILE".into(), home.to_string_lossy().into_owned()),
            ("TEMP".into(), temp.to_string_lossy().into_owned()),
            ("TMP".into(), temp.to_string_lossy().into_owned()),
        ]);
        environment.insert(
            "APPDATA".into(),
            home.join("AppData/Roaming").to_string_lossy().into_owned(),
        );
        environment.insert(
            "LOCALAPPDATA".into(),
            home.join("AppData/Local").to_string_lossy().into_owned(),
        );
        // Own the account and paths for the entire future, including timeout cleanup.
        async move { windows::run(&account, &script, &cwd, environment, logs, budget).await }
    };
    let code = tokio::time::timeout(Duration::from_secs(5), process)
        .await
        .ok()
        .and_then(Result::ok)
        .and_then(|status| status.code());
    Ok(capability(code))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn probe_distinguishes_cli_engine_and_invalid_output_without_reporting_output() {
        let directory = tempfile::tempdir().unwrap();
        for (body, expected, os) in [
            (None, DockerState::CliMissing, None),
            (
                Some("echo linux"),
                DockerState::Available,
                Some(DockerEngineOs::Linux),
            ),
            (
                Some("echo windows"),
                DockerState::Available,
                Some(DockerEngineOs::Windows),
            ),
            (
                Some("echo secret >&2; exit 1"),
                DockerState::EngineUnavailable,
                None,
            ),
            (Some("echo unexpected"), DockerState::CheckFailed, None),
        ] {
            if let Some(body) = body {
                let docker = directory.path().join("docker");
                std::fs::write(&docker, format!("#!/bin/sh\n{body}\n")).unwrap();
                std::fs::set_permissions(docker, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", SCRIPT])
                .env_clear()
                .env("PATH", directory.path());
            let (logs, mut receiver) = mpsc::channel(1);
            let status = run_process(command, logs, None, Arc::new(AtomicUsize::new(0)))
                .await
                .unwrap();
            let result = capability(status.code());
            assert_eq!(result.state, expected);
            assert_eq!(result.engine_os, os);
            assert!(result.valid());
            assert!(receiver.recv().await.is_none());
        }
        assert_eq!(capability(None).state, DockerState::CheckFailed);
    }
}
