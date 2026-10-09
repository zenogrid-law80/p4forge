//! Only the supervisor receives and installs brokered tickets. Managed copies
//! live under the private state directory, never in the build workspace.
use crate::{AgentConfig, Api};
use anyhow::{Context, Result, ensure};
use p4forge_perforce::{
    Perforce,
    credentials::{MIN_VALID_SECONDS, atomic_private_file, validate_ticket_file},
};
use p4forge_protocol::{TicketGrant, TicketHealth};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::task::JoinHandle;

pub(crate) struct Guard(JoinHandle<()>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) fn prepare(config: &mut AgentConfig) -> Result<()> {
    let directory = config.state_dir.join("perforce");
    std::fs::create_dir_all(&directory)?;
    ensure!(
        std::fs::symlink_metadata(&directory)?.is_dir(),
        "invalid managed credential directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let read = directory.join("read.tickets");
    seed(&read, config.perforce.tickets_file.as_deref())?;
    config.perforce.tickets_file = Some(read.clone());
    config.credential_files.push(read);
    if let Some(publisher) = &mut config.publisher {
        let publish = directory.join("publish.tickets");
        seed(&publish, Some(&publisher.tickets_file))?;
        publisher.tickets_file = publish.clone();
        config.credential_files.push(publish);
    }
    #[cfg(windows)]
    {
        let account = crate::windows::Account::load(
            config
                .build_account_file
                .as_deref()
                .context("build account missing")?,
        )?;
        crate::windows::validate_supervisor(&account, config.credential_files.clone())?;
    }
    Ok(())
}
fn seed(target: &Path, source: Option<&Path>) -> Result<()> {
    if target.exists() {
        let m = std::fs::symlink_metadata(target)?;
        ensure!(
            m.is_file() && m.len() <= 16384,
            "invalid managed ticket file"
        );
        return Ok(());
    }
    let data = if let Some(path) = source {
        std::fs::read(path)?
    } else {
        Vec::new()
    };
    ensure!(data.len() <= 16384, "ticket file exceeds limit");
    atomic_private_file(target, &data)
}

pub(crate) async fn start(api: &Api, config: &AgentConfig) -> Guard {
    let api = api.clone();
    let p4 = config.perforce.clone();
    let publisher = config.publisher.clone();
    // Perform the first check before restart recovery/claim so a stale bootstrap
    // ticket cannot strand cleanup after the server rotated credentials.
    if !matches!(
        tokio::time::timeout(
            Duration::from_secs(20),
            refresh(&api, &p4, publisher.as_ref())
        )
        .await,
        Ok(Ok(()))
    ) {
        tracing::warn!("Perforce credential check unavailable; managed dispatch remains blocked");
    }
    Guard(tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            interval.tick().await;
            if !matches!(
                tokio::time::timeout(
                    Duration::from_secs(40),
                    refresh(&api, &p4, publisher.as_ref())
                )
                .await,
                Ok(Ok(()))
            ) {
                tracing::warn!(
                    "Perforce credential check unavailable; managed dispatch remains blocked"
                );
            }
        }
    }))
}
async fn refresh(
    api: &Api,
    p4: &Perforce,
    publisher: Option<&crate::publication::Publisher>,
) -> Result<()> {
    // Plain HTTP is only supported by explicit internal test installations.
    let grants: Vec<TicketGrant> = api.post("agent/credentials", &()).await?;
    ensure!(grants.len() <= 2, "too many credential grants");
    let mut health = Vec::new();
    for grant in grants {
        let remaining = apply(p4, publisher, &grant).await.unwrap_or(0);
        health.push(TicketHealth {
            id: grant.id,
            generation: grant.generation,
            ready: remaining > MIN_VALID_SECONDS,
            remaining_seconds: remaining,
        });
    }
    let _: serde_json::Value = api.post("agent/credentials/status", &health).await?;
    Ok(())
}
async fn apply(
    base: &Perforce,
    publisher: Option<&crate::publication::Publisher>,
    grant: &TicketGrant,
) -> Result<u64> {
    grant.connection.validate()?;
    let target: PathBuf = match grant.role.as_str() {
        "read" => base
            .tickets_file
            .clone()
            .context("read ticket path missing")?,
        "publish" => {
            let p = publisher.context("local publisher policy is not installed")?;
            ensure!(
                p.connection == grant.connection && p.server_id == grant.server_id,
                "publisher grant does not match local policy"
            );
            p.tickets_file.clone()
        }
        _ => anyhow::bail!("unsupported credential role"),
    };
    let tickets = grant
        .tickets
        .as_deref()
        .context("server renewal unavailable")?;
    validate_ticket_file(tickets, &grant.connection.user)?;
    let check = target.with_extension("candidate");
    atomic_private_file(&check, tickets.as_bytes())?;
    let p4 = Perforce {
        tickets_file: Some(check.clone()),
        ..base.clone()
    };
    let result = async {
        let info = p4.info(&grant.connection).await?;
        ensure!(
            info.get("ServerID")
                .or_else(|| info.get("serverID"))
                .or_else(|| info.get("serverAddress"))
                == Some(&grant.server_id),
            "Perforce server identity mismatch"
        );
        let lifetime = p4.ticket_lifetime(&grant.connection).await?;
        ensure!(
            lifetime > MIN_VALID_SECONDS,
            "Perforce ticket expires too soon"
        );
        // Candidate authentication succeeded; an invalid delivery never replaces
        // the installed usable ticket. ACL/private mode is inherited on rename.
        atomic_private_file(&target, tickets.as_bytes())?;
        Ok(lifetime)
    }
    .await;
    let _ = std::fs::remove_file(check);
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use p4forge_perforce::Connection;
    use std::os::unix::fs::PermissionsExt;
    #[tokio::test]
    async fn invalid_delivery_preserves_ticket_and_publisher_requires_local_policy() {
        let temp = tempfile::tempdir().unwrap();
        let cli = temp.path().join("p4");
        std::fs::write(&cli,"#!/bin/sh\ncase \"$*\" in *'info -s') printf '... ServerID right\\n';; *'login -s') printf '... TicketExpiration 40000\\n';; *) exit 1;; esac\n").unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tickets = temp.path().join("read.tickets");
        atomic_private_file(&tickets, b"old").unwrap();
        let p4 = Perforce {
            binary: cli,
            tickets_file: Some(tickets.clone()),
            trust_file: None,
        };
        let mut grant = TicketGrant {
            id: uuid::Uuid::new_v4(),
            generation: uuid::Uuid::new_v4(),
            role: "read".into(),
            connection: Connection {
                port: "example.invalid:1666".into(),
                user: "reader".into(),
                charset: None,
            },
            server_id: "wrong".into(),
            tickets: Some(format!("host:1666=reader:{}\n", "a".repeat(32))),
        };
        assert!(apply(&p4, None, &grant).await.is_err());
        assert_eq!(std::fs::read(&tickets).unwrap(), b"old");
        grant.server_id = "right".into();
        assert_eq!(apply(&p4, None, &grant).await.unwrap(), 40000);
        let installed = std::fs::read(&tickets).unwrap();
        grant.role = "publish".into();
        assert!(apply(&p4, None, &grant).await.is_err());
        grant.role = "read".into();
        grant.tickets = Some(format!("host=admin:{}", "b".repeat(32)));
        assert!(apply(&p4, None, &grant).await.is_err());
        assert_eq!(std::fs::read(&tickets).unwrap(), installed);
    }
}
