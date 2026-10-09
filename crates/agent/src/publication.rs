//! Privileged publication: scripts never receive the publisher policy or ticket.
use crate::{Api, Entry};
use anyhow::{Context, Result, ensure};
use p4forge_perforce::{Connection, Perforce, Root};
use p4forge_protocol::{Assignment, PublicationRequest};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publisher {
    pub connection: Connection,
    pub server_id: String,
    pub stream: String,
    pub paths: Vec<String>,
    pub tickets_file: PathBuf,
}
impl Publisher {
    pub fn validate(&self, a: &Assignment) -> Result<()> {
        let p = a
            .publish_generated
            .as_ref()
            .context("publication not configured")?;
        p.validate()?;
        self.connection.validate()?;
        ensure!(
            self.connection.port == a.source.connection.port
                && self.server_id == a.source.server_id
                && p.user == self.connection.user,
            "publisher identity differs from approved host policy"
        );
        ensure!(
            matches!(&a.source.root, Root::Stream {stream} if stream == &self.stream),
            "publication requires the approved stream"
        );
        ensure!(
            p.paths.iter().all(|path| self.paths.contains(path)),
            "publication path is not approved on this agent"
        );
        ensure!(cfg!(unix), "publication is available on Unix agents only");
        Ok(())
    }
    fn cli(&self, p4: &Perforce) -> Perforce {
        Perforce {
            tickets_file: Some(self.tickets_file.clone()),
            ..p4.clone()
        }
    }
    pub async fn cleanup(&self, p4: &Perforce, a: &Assignment) -> Result<()> {
        if a.publish_generated.is_none() {
            return Ok(());
        }
        self.validate(a)?;
        let cli = self.cli(p4);
        let name = client(a);
        let existing = cli
            .tagged(&self.connection, &["clients", "-e", &name])
            .await?;
        if existing.get("client") != Some(&name) {
            return Ok(());
        }
        // A single numbered changelist is ever created on this attempt's client.
        let pending = cli
            .tagged(&self.connection, &["changes", "-s", "pending", "-c", &name])
            .await?;
        if let Some(number) = pending.get("change") {
            number.parse::<i64>()?;
            cli.tagged(
                &self.connection,
                &["-c", &name, "revert", "-k", "-c", number, "//..."],
            )
            .await?;
            cli.tagged(&self.connection, &["-c", &name, "change", "-d", number])
                .await?;
        }
        cli.tagged(&self.connection, &["client", "-d", &name])
            .await?;
        Ok(())
    }
    pub(crate) async fn publish(
        &self,
        api: &Api,
        p4: &Perforce,
        a: &Assignment,
        directory: &Path,
        logs: &tokio::sync::mpsc::Sender<Entry>,
    ) -> Result<()> {
        self.validate(a)?;
        let publication = a.publish_generated.as_ref().unwrap();
        let cli = self.cli(p4);
        let name = client(a);
        let stage = directory.join("publication");
        std::fs::create_dir(&stage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700))?;
        }
        // Capture only regular, single-link files after all build processes died.
        let mut total = 0;
        let mut count = 0;
        for path in &publication.paths {
            let source = directory.join("source");
            validate_components(&source, path)?;
            capture(
                &source.join(path),
                &stage.join(path),
                &mut count,
                &mut total,
            )?;
        }
        let info = cli.info(&self.connection).await?;
        ensure!(
            info.get("ServerID").or_else(|| info.get("serverID")) == Some(&self.server_id),
            "publisher server identity changed"
        );
        let form = format!(
            "Client: {name}\nOwner: {}\nHost:\nRoot: {}\nStream: {}\nOptions: noallwrite noclobber nocompress unlocked nomodtime rmdir\nLineEnd: unix\n",
            self.connection.user,
            stage.display(),
            self.stream
        );
        cli.tagged_input(&self.connection, &["client", "-i"], Some(&form))
            .await?;
        let actual = cli
            .tagged(&self.connection, &["client", "-o", &name])
            .await?;
        let old_name = a
            .source
            .client_spec
            .get("Client")
            .context("missing snapshot client")?;
        let expected = views(&a.source.client_spec, old_name, &name);
        ensure!(
            !expected.is_empty() && expected == views(&actual, &name, &name),
            "stream view changed since checkout"
        );
        // Establish have revisions at the immutable input CL without overwriting captured outputs.
        for path in &publication.paths {
            let revision = format!("//{name}/{path}/...@{}", a.source.changelist);
            cli.tagged(
                &self.connection,
                &["-c", &name, "sync", "-k", "-q", &revision],
            )
            .await?;
        }
        let description = marker(a);
        let change = cli
            .tagged_input(
                &self.connection,
                &["-c", &name, "change", "-i"],
                Some(&format!(
                    "Change: new\nClient: {name}\nUser: {}\nDescription:\n\t{description}\n",
                    self.connection.user
                )),
            )
            .await?;
        let number = change
            .get("change")
            .and_then(|s| s.parse::<i64>().ok())
            .or_else(|| {
                change
                    .get("data")
                    .and_then(|s| s.split_whitespace().nth(1))
                    .and_then(|s| s.parse().ok())
            })
            .context("missing numbered changelist")?;
        let number_text = number.to_string();
        for path in &publication.paths {
            let files = format!("//{name}/{path}/...");
            // -I prevents a generated .p4ignore from changing the publication manifest.
            cli.tagged(
                &self.connection,
                &[
                    "-c",
                    &name,
                    "reconcile",
                    "-I",
                    "-a",
                    "-e",
                    "-d",
                    "-c",
                    &number_text,
                    &files,
                ],
            )
            .await?;
        }
        let opened = cli
            .tagged(
                &self.connection,
                &["-c", &name, "opened", "-c", &number_text],
            )
            .await?;
        if !opened.contains_key("depotFile") {
            cli.tagged(
                &self.connection,
                &["-c", &name, "change", "-d", &number_text],
            )
            .await?;
            receipt(api, a, "unchanged", a.source.changelist).await?;
            logs.send(Entry {
                stream: "system",
                content: format!(
                    "Publication unchanged; downstream input CL {}\n",
                    a.source.changelist
                ),
            })
            .await?;
        } else {
            // Persist intent before submit. A lost response never creates a new CL or auto-retries submit.
            receipt(api, a, "prepared", number).await?;
            cli.tagged(
                &self.connection,
                &["-c", &name, "submit", "-c", &number_text],
            )
            .await
            .context("publication submit outcome may be unknown; inspect reserved CL")?;
            let submitted = receipt(api, a, "submitted", number).await?;
            logs.send(Entry {
                stream: "system",
                content: format!(
                    "Published generated files as CL {submitted}; verified by server\n"
                ),
            })
            .await?;
        }
        self.cleanup(p4, a).await?;
        Ok(())
    }
}
fn client(a: &Assignment) -> String {
    format!("p4forge_publish_{}", a.attempt_id.simple())
}
fn marker(a: &Assignment) -> String {
    format!(
        "P4Forge run={} attempt={} base={}",
        a.run_id, a.attempt_id, a.source.changelist
    )
}
fn views(fields: &BTreeMap<String, String>, old: &str, new: &str) -> BTreeMap<String, String> {
    fields
        .iter()
        .filter(|(k, _)| {
            k.strip_prefix("View")
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(|(k, v)| {
            (
                k.clone(),
                v.replace(&format!("//{old}/"), &format!("//{new}/")),
            )
        })
        .collect()
}
async fn receipt(api: &Api, a: &Assignment, status: &str, changelist: i64) -> Result<i64> {
    let input = PublicationRequest {
        fence: a.fence,
        status: status.into(),
        changelist,
    };
    let path = format!("agent/attempts/{}/publication", a.attempt_id);
    let mut result: Result<serde_json::Value> = api.post(&path, &input).await;
    if result.is_err() && !crate::terminal_error(result.as_ref().unwrap_err()) {
        result = api.post(&path, &input).await;
    }
    result?["changelist"]
        .as_i64()
        .context("publication receipt has no verified CL")
}
fn validate_components(root: &Path, relative: &str) -> Result<()> {
    let mut current = root.to_owned();
    ensure!(
        std::fs::symlink_metadata(&current)?.is_dir(),
        "source is not a directory"
    );
    for part in relative.split('/') {
        current.push(part);
        ensure!(
            std::fs::symlink_metadata(&current)?.is_dir(),
            "publication directory is missing or a link"
        );
    }
    Ok(())
}
fn capture(source: &Path, target: &Path, count: &mut u64, total: &mut u64) -> Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        std::fs::create_dir_all(target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let name = entry.file_name();
            let text = name.to_str().context("publication name is not UTF-8")?;
            ensure!(
                !text.is_empty()
                    && text
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')),
                "unsupported generated filename"
            );
            capture(&entry.path(), &target.join(name), count, total)?;
        }
    } else {
        ensure!(
            metadata.is_file(),
            "publication rejects links and special files"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(metadata.nlink() == 1, "publication rejects hard links");
        }
        *count += 1;
        *total += metadata.len();
        ensure!(
            *count <= 10000 && *total <= 256 * 1024 * 1024,
            "generated outputs exceed publication limits"
        );
        std::fs::copy(source, target)?;
    }
    Ok(())
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn capture_rejects_symlinks_hardlinks_and_missing_roots() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("source")).unwrap();
        std::fs::write(root.join("outside"), "secret").unwrap();
        symlink(root.join("outside"), root.join("source/link")).unwrap();
        assert!(capture(&root.join("source"), &root.join("out"), &mut 0, &mut 0).is_err());
        std::fs::remove_file(root.join("source/link")).unwrap();
        std::fs::hard_link(root.join("outside"), root.join("source/hard")).unwrap();
        assert!(capture(&root.join("source"), &root.join("out2"), &mut 0, &mut 0).is_err());
        symlink(root.join("source"), root.join("alias")).unwrap();
        assert!(validate_components(root, "alias").is_err());
        assert!(validate_components(root, "missing").is_err());
    }
}
