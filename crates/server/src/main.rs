use anyhow::Result;
use clap::{Parser, Subcommand};
use p4forge_domain::PipelineDefinition;
use p4forge_perforce::{Connection, Perforce, Root};
use p4forge_server::{
    AppState,
    auth::{AuthConfig, AuthService},
};
use sqlx::postgres::PgPoolOptions;
use std::{net::SocketAddr, path::PathBuf};
use url::Url;

#[derive(Parser)]
#[command(name = "p4forge", version, about = "Perforce-only self-hosted CI/CD")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
#[expect(
    clippy::large_enum_variant,
    reason = "CLI arguments are parsed only once at startup"
)]
enum Commands {
    RegisterAgentRelease {
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        database_url: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        version: String,
        #[arg(long)]
        os: String,
        #[arg(long)]
        arch: String,
    },
    ProvisionAgent {
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        database_url: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "default")]
        pool: String,
        #[arg(long, default_value = "aarch64")]
        arch: String,
        #[arg(long, default_value = "linux")]
        os: String,
        #[arg(long)]
        token_file: PathBuf,
    },
    Serve {
        #[arg(long, env = "P4FORGE_CREDENTIAL_POLICY")]
        credential_policy: Option<PathBuf>,
        #[arg(
            long,
            env = "P4FORGE_CREDENTIAL_DIR",
            default_value = ".local/managed-credentials"
        )]
        credential_dir: PathBuf,
        #[arg(long, env = "P4FORGE_ARTIFACT_DIR", default_value = ".local/artifacts")]
        artifact_dir: PathBuf,
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        database_url: String,
        #[arg(long, env = "P4FORGE_BIND", default_value = "127.0.0.1:8088")]
        bind: SocketAddr,
        #[arg(
            long,
            env = "P4FORGE_PUBLIC_URL",
            default_value = "http://127.0.0.1:8088/"
        )]
        public_url: Url,
        #[arg(long, env = "GOOGLE_CLIENT_ID", default_value = "")]
        google_client_id: String,
        #[arg(
            long,
            env = "GOOGLE_CLIENT_SECRET",
            default_value = "",
            hide_env_values = true
        )]
        google_client_secret: String,
        #[arg(long, env = "P4FORGE_BOOTSTRAP_ADMIN", default_value = "")]
        bootstrap_admin: String,
        #[arg(long, env = "P4_BIN", default_value = "p4")]
        p4_bin: PathBuf,
        #[arg(long, env = "P4FORGE_P4_TICKETS_FILE")]
        tickets_file: Option<PathBuf>,
        #[arg(long, env = "P4FORGE_P4_TRUST_FILE")]
        trust_file: Option<PathBuf>,
    },
    Validate {
        file: PathBuf,
    },
    PerforceCheck {
        #[arg(long)]
        port: String,
        #[arg(long, default_value = "p4forge")]
        user: String,
        #[arg(long)]
        stream: String,
        #[arg(long)]
        changelist: i64,
        #[arg(long, default_value = "p4")]
        p4_bin: PathBuf,
        #[arg(long)]
        tickets_file: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "p4forge_server=info,tower_http=info".into()),
        )
        .init();
    match Cli::parse().command {
        Commands::RegisterAgentRelease {
            database_url,
            file,
            version,
            os,
            arch,
        } => {
            anyhow::ensure!(
                tokio::fs::metadata(&file).await?.len()
                    <= p4forge_protocol::MAX_AGENT_BINARY as u64,
                "binary exceeds 64 MiB"
            );
            let db = PgPoolOptions::new()
                .max_connections(2)
                .connect(&database_url)
                .await?;
            p4forge_server::migrate(&db).await?;
            let release = p4forge_server::updates::register(
                &db,
                &version,
                &os,
                &arch,
                tokio::fs::read(file).await?,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&release)?);
        }
        Commands::ProvisionAgent {
            database_url,
            name,
            pool,
            arch,
            os,
            token_file,
        } => {
            use std::io::Write;
            let db = PgPoolOptions::new()
                .max_connections(2)
                .connect(&database_url)
                .await?;
            p4forge_server::migrate(&db).await?;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&token_file)?;
            let result = p4forge_server::agents::provision(
                &db,
                p4forge_server::agents::CreateAgent {
                    name,
                    pool,
                    os,
                    arch,
                },
                None,
            )
            .await;
            match result {
                Ok(created) => {
                    file.write_all(created.token.as_bytes())?;
                    file.sync_all()?;
                    println!(
                        "Agent {} provisioned; credential written to {}",
                        created.agent.id,
                        token_file.display()
                    );
                }
                Err(error) => {
                    drop(file);
                    let _ = std::fs::remove_file(token_file);
                    anyhow::bail!("{}", error.1);
                }
            }
        }
        Commands::Validate { file } => {
            let definition = PipelineDefinition::parse(&tokio::fs::read_to_string(file).await?)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"graph":definition.validate()?,"hash":definition.hash()?,"definition":definition})
                )?
            );
        }
        Commands::PerforceCheck {
            port,
            user,
            stream,
            changelist,
            p4_bin,
            tickets_file,
        } => {
            let mut p4 = Perforce::new(p4_bin);
            p4.tickets_file = tickets_file;
            let snapshot = p4
                .snapshot(
                    &Connection {
                        port,
                        user,
                        charset: None,
                    },
                    &Root::Stream { stream },
                    changelist,
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
        }
        Commands::Serve {
            credential_policy,
            credential_dir,
            artifact_dir,
            database_url,
            bind,
            public_url,
            google_client_id,
            google_client_secret,
            bootstrap_admin,
            p4_bin,
            tickets_file,
            trust_file,
        } => {
            let pool = PgPoolOptions::new()
                .max_connections(10)
                .acquire_timeout(std::time::Duration::from_secs(5))
                .connect(&database_url)
                .await?;
            p4forge_server::migrate(&pool).await?;
            let auth = AuthService::new(
                pool.clone(),
                AuthConfig {
                    client_id: google_client_id,
                    client_secret: google_client_secret,
                    public_url,
                    bootstrap_admin,
                },
            )?;
            if !auth.configured() {
                tracing::warn!("Google OAuth is not configured; protected APIs remain locked");
            }
            let mut perforce = Perforce::new(p4_bin);
            perforce.tickets_file = tickets_file;
            perforce.trust_file = trust_file;
            let credentials = if let Some(path) = credential_policy {
                let broker =
                    p4forge_server::credentials::Broker::load(&pool, &path, &credential_dir)
                        .await?;
                perforce.tickets_file = Some(broker.read_tickets_path());
                Some(broker)
            } else {
                None
            };
            tokio::fs::create_dir_all(&artifact_dir).await?;
            let state = AppState {
                credentials,
                artifact_dir,
                pool,
                auth,
                perforce,
            };
            let scheduler_state = state.clone();
            let credential_state = state.clone();
            let credential_task = tokio::spawn(async move {
                if let Some(broker) = &credential_state.credentials {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        interval.tick().await;
                        if broker
                            .tick(&credential_state.pool, &credential_state.perforce)
                            .await
                            .is_err()
                        {
                            tracing::error!("Perforce credential refresh unavailable");
                        }
                    }
                }
            });
            let scheduler = tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
                loop {
                    interval.tick().await;
                    if let Err(error) = p4forge_server::agents::reap(&scheduler_state.pool).await {
                        tracing::error!(error=%error.1,"agent lease reconciliation failed");
                    }
                }
            });
            // A slow Perforce snapshot for a followup must not delay expired-lease reconciliation.
            let followup_state = state.clone();
            let followups = tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
                loop {
                    interval.tick().await;
                    if let Err(error) = p4forge_server::followups::tick(&followup_state).await {
                        tracing::error!(error=%error.1,"downstream reconciliation failed");
                    }
                }
            });
            let trigger_state = state.clone();
            let triggers = tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    interval.tick().await;
                    if let Err(error) = p4forge_server::triggers::tick(&trigger_state).await {
                        tracing::error!(error=%error.1,"Perforce change reconciliation failed");
                    }
                }
            });
            let retention_state = state.clone();
            let retention = tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    interval.tick().await;
                    if let Err(error) = p4forge_server::retention::tick(&retention_state).await {
                        tracing::error!(error=%error.1,"artifact retention failed");
                    }
                }
            });
            let app = p4forge_server::router(state);
            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!(%bind,"P4Forge server listening");
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
            credential_task.abort();
            scheduler.abort();
            followups.abort();
            retention.abort();
            triggers.abort();
        }
    }
    Ok(())
}
