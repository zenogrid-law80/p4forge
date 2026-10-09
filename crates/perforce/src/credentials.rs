//! Credential operations deliberately discard CLI diagnostics: login can echo secrets.
use crate::{Connection, Perforce};
use anyhow::{Context, Result, ensure};
use std::{io::Write, path::Path};

pub const MIN_VALID_SECONDS: u64 = 300;

impl Perforce {
    pub async fn ticket_lifetime(&self, connection: &Connection) -> Result<u64> {
        let result = self.tagged(connection, &["login", "-s"]).await;
        let fields = result.map_err(|_| anyhow::anyhow!("Perforce authentication check failed"))?;
        fields
            .get("TicketExpiration")
            .and_then(|v| v.parse().ok())
            .context("Perforce did not return a finite ticket lifetime")
    }

    pub async fn renew_ticket(&self, connection: &Connection, password: &str) -> Result<u64> {
        ensure!(
            !password.is_empty()
                && password.len() <= 4096
                && !password.contains(['\n', '\r', '\0']),
            "invalid credential file"
        );
        self.tagged_input(connection, &["login", "-a"], Some(&format!("{password}\n")))
            .await
            .map_err(|_| anyhow::anyhow!("Perforce ticket renewal failed"))?;
        self.ticket_lifetime(connection).await
    }
}

/// Only one account and one ticket may be delivered in a grant. Never accept
/// extra accounts or arbitrary ticket-file lines supplied through the network.
pub fn validate_ticket_file(data: &str, user: &str) -> Result<()> {
    ensure!(
        !data.is_empty() && data.len() <= 16384,
        "invalid ticket file size"
    );
    let mut ticket = None;
    let mut count = 0;
    for line in data.lines() {
        let (server, entry) = line.split_once('=').context("invalid ticket entry")?;
        let (account, value) = entry.split_once(':').context("invalid ticket account")?;
        ensure!(
            !server.is_empty()
                && server.len() <= 512
                && !server.chars().any(|c| c.is_control() || c.is_whitespace()),
            "invalid ticket server"
        );
        ensure!(
            account == user && value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid ticket identity"
        );
        ensure!(
            ticket.is_none_or(|previous| previous == value),
            "conflicting ticket entries"
        );
        ticket = Some(value);
        count += 1;
    }
    ensure!(count > 0 && count <= 16, "invalid ticket entry count");
    Ok(())
}

/// The caller supplies a private, supervisor-owned directory. create_new avoids
/// following a pre-existing temp link; rename leaves concurrent CLI readers a
/// complete old or new file. On Windows the new file inherits that directory's ACL.
pub fn atomic_private_file(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().context("missing credential directory")?;
    ensure!(
        std::fs::symlink_metadata(parent)?.is_dir(),
        "invalid credential directory"
    );
    let temp = parent.join(format!(".credential-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ticket_entries_are_account_scoped() {
        let ticket = "a".repeat(32);
        assert!(
            validate_ticket_file(
                &format!("host:1666=reader:{ticket}\r\nother:1666=reader:{ticket}\n"),
                "reader"
            )
            .is_ok()
        );
        for value in [
            String::new(),
            format!("host=admin:{ticket}"),
            format!("host=reader:{ticket}\nx=reader:{}", "b".repeat(32)),
            "host=reader:secret\n".into(),
        ] {
            assert!(validate_ticket_file(&value, "reader").is_err());
        }
    }
    #[test]
    fn replacement_is_private_and_complete() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tickets");
        atomic_private_file(&file, b"old").unwrap();
        let mut old = std::fs::File::open(&file).unwrap();
        atomic_private_file(&file, b"new").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"new");
        use std::io::Read;
        let mut data = String::new();
        old.read_to_string(&mut data).unwrap();
        assert_eq!(data, "old");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
