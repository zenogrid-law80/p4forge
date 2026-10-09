use anyhow::{Context, Result, ensure};
use clap::Parser;
use p4forge_agent::{AgentConfig, run};
use p4forge_perforce::Perforce;
use std::path::PathBuf;
use url::Url;

#[derive(Parser)]
#[command(version, about = "P4Forge self-hosted build agent")]
struct Cli {
    #[arg(long, env = "P4FORGE_SERVER_URL")]
    server: Url,
    #[arg(long, env = "P4FORGE_AGENT_TOKEN_FILE")]
    token_file: PathBuf,
    #[arg(long, env = "P4FORGE_P4_TICKETS_FILE")]
    tickets_file: PathBuf,
    #[arg(long, env = "P4FORGE_P4_TRUST_FILE")]
    trust_file: Option<PathBuf>,
    #[arg(long, default_value = "p4")]
    p4_bin: PathBuf,
    #[arg(long, default_value = "/work")]
    work_dir: PathBuf,
    #[arg(long, default_value = "/state")]
    state_dir: PathBuf,
    #[arg(long)]
    allow_insecure_internal_http: bool,
    #[arg(long)]
    once: bool,
    #[arg(long)]
    build_account_file: Option<PathBuf>,
    #[arg(long)]
    log_file: Option<PathBuf>,
    #[arg(long, env = "P4FORGE_PUBLISHER_POLICY")]
    publisher_policy: Option<PathBuf>,
}
fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--self-patch-info") {
        println!("{}", p4forge_agent::self_patch::info());
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("--patch-worker") {
        return tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(p4forge_agent::self_patch::worker());
    }
    #[cfg(target_os = "macos")]
    if p4forge_agent::macos::helper_entry()? {
        return Ok(());
    }
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(agent_main());
    if let Err(error) = &result {
        // Scheduled tasks have no console; keep startup/terminal failures in --log-file.
        tracing::error!(error = %format!("{error:#}"), "agent stopped");
    }
    result
}
async fn agent_main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(path) = &cli.log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        tracing_subscriber::fmt()
            .with_env_filter("p4forge_agent=info")
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter("p4forge_agent=info")
            .init();
    }
    ensure!(
        cfg!(windows)
            || cfg!(target_os = "macos")
            || (cfg!(target_os = "linux") && PathBuf::from("/.dockerenv").exists()),
        "use a dedicated Linux container or the native macOS/Windows installation"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            unsafe { libc::geteuid() } == 0,
            "agent supervisor must run as root; build scripts run as UID 10003"
        );
        for file in [&cli.token_file, &cli.tickets_file] {
            ensure!(
                std::fs::metadata(file)?.permissions().mode() & 0o077 == 0,
                "credential files must have permissions 600 or 400"
            );
        }
    }
    let token = tokio::fs::read_to_string(&cli.token_file)
        .await
        .context("read agent credential file")?;
    let mut credential_files = vec![cli.token_file.clone(), cli.tickets_file.clone()];
    if let Some(trust) = &cli.trust_file {
        credential_files.push(trust.clone());
    }
    let publisher: Option<p4forge_agent::publication::Publisher> = cli
        .publisher_policy
        .as_ref()
        .map(|path| -> Result<_> {
            credential_files.push(path.clone());
            let value: p4forge_agent::publication::Publisher =
                serde_json::from_slice(&std::fs::read(path)?)?;
            credential_files.push(value.tickets_file.clone());
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                for file in [path, &value.tickets_file] {
                    let m = std::fs::symlink_metadata(file)?;
                    ensure!(
                        m.is_file() && m.uid() == 0 && m.mode() & 0o077 == 0,
                        "publisher files must be root-owned private regular files"
                    );
                }
            }
            Ok(value)
        })
        .transpose()?;
    let mut perforce = Perforce::new(cli.p4_bin);
    perforce.tickets_file = Some(cli.tickets_file);
    perforce.trust_file = cli.trust_file;
    run(AgentConfig {
        publisher,
        server: cli.server,
        token: token.trim().to_owned(),
        work_dir: cli.work_dir,
        state_dir: cli.state_dir,
        perforce,
        allow_http: cli.allow_insecure_internal_http,
        once: cli.once,
        build_account_file: cli.build_account_file,
        credential_files,
    })
    .await
}
