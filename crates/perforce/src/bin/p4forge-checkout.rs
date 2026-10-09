//! Narrow stdin/stdout bridge for the dedicated Docker executor. No shell commands.
use anyhow::{Result, ensure};
use p4forge_perforce::{Connection, Perforce, SourceSnapshot, credentials::validate_ticket_file};
use serde::Deserialize;
use serde_json::json;
use std::{io::Read, path::PathBuf};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Check {
        connection: Connection,
        server_id: String,
    },
    Checkout {
        snapshot: SourceSnapshot,
        attempt_id: Uuid,
        directory: PathBuf,
    },
    Cleanup {
        connection: Connection,
        attempt_id: Uuid,
    },
}

async fn execute() -> Result<serde_json::Value> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 2,
        "expected p4 binary and private tickets file"
    );
    let mut input = Vec::new();
    std::io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut input)?;
    ensure!(input.len() <= 1024 * 1024, "request too large");
    let request: Request = serde_json::from_slice(&input)?;
    let tickets = PathBuf::from(&args[1]);
    let metadata = std::fs::symlink_metadata(&tickets)?;
    ensure!(
        metadata.is_file() && metadata.len() <= 16384,
        "invalid ticket file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "tickets must be private"
        );
    }
    let connection = match &request {
        Request::Check { connection, .. } | Request::Cleanup { connection, .. } => connection,
        Request::Checkout { snapshot, .. } => &snapshot.connection,
    };
    connection.validate()?;
    validate_ticket_file(&std::fs::read_to_string(&tickets)?, &connection.user)?;
    let p4 = Perforce {
        binary: PathBuf::from(&args[0]),
        tickets_file: Some(tickets),
        trust_file: None,
    };
    match request {
        Request::Check {
            connection,
            server_id,
        } => {
            let info = p4.info(&connection).await?;
            ensure!(
                info.get("ServerID")
                    .or_else(|| info.get("serverID"))
                    .or_else(|| info.get("serverAddress"))
                    == Some(&server_id),
                "server mismatch"
            );
            let remaining = p4.ticket_lifetime(&connection).await?;
            ensure!(remaining > 300, "ticket expires too soon");
            Ok(json!({"remaining_seconds": remaining}))
        }
        Request::Checkout {
            snapshot,
            attempt_id,
            directory,
        } => {
            ensure!(!attempt_id.is_nil(), "missing attempt");
            ensure!(
                directory.is_absolute() && std::fs::symlink_metadata(&directory)?.is_dir(),
                "invalid workspace"
            );
            p4.checkout(&snapshot, attempt_id, &directory).await?;
            Ok(json!({"checked_out": snapshot.changelist}))
        }
        Request::Cleanup {
            connection,
            attempt_id,
        } => {
            ensure!(!attempt_id.is_nil(), "missing attempt");
            p4.delete_client(&connection, attempt_id).await?;
            Ok(json!({"cleaned": true}))
        }
    }
}

#[tokio::main]
async fn main() {
    // Perforce diagnostics can contain credential material. Never forward them.
    match execute().await {
        Ok(value) => println!("{value}"),
        Err(_) => {
            eprintln!(
                "Perforce checkout helper failed; check source binding, ticket health and snapshot"
            );
            std::process::exit(1);
        }
    }
}
