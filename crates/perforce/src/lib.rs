use std::{collections::BTreeMap, path::PathBuf, process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use uuid::Uuid;
pub mod credentials;
mod view;

#[derive(Debug, Clone)]
pub struct Perforce {
    pub binary: PathBuf,
    pub tickets_file: Option<PathBuf>,
    pub trust_file: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub port: String,
    pub user: String,
    #[serde(default)]
    pub charset: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Root {
    Stream { stream: String },
    Classic { view: Vec<String> },
    View { view: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSnapshot {
    pub connection: Connection,
    pub root: Root,
    pub changelist: i64,
    pub server_id: String,
    pub server_version: String,
    pub client_spec: BTreeMap<String, String>,
    pub stream_specs: Vec<BTreeMap<String, String>>,
}

impl Connection {
    pub fn validate(&self) -> Result<()> {
        // Reject rsh/command transports. Only host:port and TCP/TLS transports are supported.
        let address = self
            .port
            .strip_prefix("ssl:")
            .or_else(|| self.port.strip_prefix("tcp:"))
            .unwrap_or(&self.port);
        let (host, port) = address
            .rsplit_once(':')
            .context("P4PORT must be host:port or ssl:host:port")?;
        let valid_host = if host.contains(':') {
            host.strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
                .unwrap_or(host)
                .parse::<std::net::Ipv6Addr>()
                .is_ok()
        } else {
            !host.is_empty()
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        };
        ensure!(valid_host, "invalid P4PORT host or transport");
        ensure!(
            port.parse::<u16>().is_ok_and(|number| number > 0),
            "invalid P4PORT port"
        );
        ensure!(
            !self.user.is_empty()
                && self.user.len() <= 128
                && self
                    .user
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'@')),
            "invalid Perforce user"
        );
        ensure!(
            self.charset
                .as_deref()
                .is_none_or(|s| matches!(s, "utf8" | "none" | "utf8-bom")),
            "unsupported charset"
        );
        Ok(())
    }
}

impl Root {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::View { view } => {
                view::client_view(view, "p4forge_validate")?;
            }
            Self::Stream { stream } => {
                ensure!(
                    stream.starts_with("//")
                        && stream[2..].contains('/')
                        && stream.len() <= 512
                        && !stream.contains(['@', '#', '%', '*', ' ', '\\', '\n', '\r'])
                        && !stream.contains("..."),
                    "invalid stream path"
                );
            }
            Self::Classic { view } => {
                ensure!(
                    !view.is_empty() && view.len() <= 64,
                    "classic view needs 1..64 depot paths"
                );
                for path in view {
                    ensure!(
                        path.starts_with("//")
                            && path.ends_with("/...")
                            && !path.contains(['@', '#', '%', '*', ' ', '\\', '\n', '\r'])
                            && path.len() <= 512,
                        "classic view requires //depot/path/... entries"
                    );
                }
                ensure!(
                    view.iter().collect::<std::collections::BTreeSet<_>>().len() == view.len(),
                    "duplicate classic view entry"
                );
            }
        }
        Ok(())
    }
}

impl Perforce {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            tickets_file: None,
            trust_file: None,
        }
    }

    pub async fn tagged(
        &self,
        connection: &Connection,
        arguments: &[&str],
    ) -> Result<BTreeMap<String, String>> {
        self.tagged_input(connection, arguments, None).await
    }

    pub async fn tagged_input(
        &self,
        connection: &Connection,
        arguments: &[&str],
        input: Option<&str>,
    ) -> Result<BTreeMap<String, String>> {
        connection.validate()?;
        let mut command = Command::new(&self.binary);
        command.args(["-ztag", "-p", &connection.port, "-u", &connection.user]);
        if let Some(charset) = &connection.charset {
            command.args(["-C", charset]);
        }
        command
            .args(arguments)
            .env_clear()
            .current_dir(if cfg!(windows) { "C:\\" } else { "/" })
            .env(
                "PATH",
                if cfg!(windows) {
                    std::env::var("PATH").unwrap_or_default()
                } else {
                    "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin".into()
                },
            )
            .env("P4IGNORE", "")
            .env("P4ENVIRO", if cfg!(windows) { "NUL" } else { "/dev/null" });
        if let Some(file) = &self.tickets_file {
            command.env("P4TICKETS", file);
        }
        if let Some(file) = &self.trust_file {
            command.env("P4TRUST", file);
        }
        #[cfg(windows)]
        if let Ok(root) = std::env::var("SystemRoot") {
            command.env("SystemRoot", root);
        }
        command
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().context("start p4 CLI")?;
        let mut stdout = child
            .stdout
            .take()
            .context("capture p4 stdout")?
            .take(4 * 1024 * 1024 + 1);
        let mut stderr = child
            .stderr
            .take()
            .context("capture p4 stderr")?
            .take(16 * 1024 + 1);
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            if let Some(input) = input {
                let mut stdin = child.stdin.take().context("capture p4 stdin")?;
                stdin.write_all(input.as_bytes()).await?;
                stdin.shutdown().await?;
            }
            let mut output = Vec::new();
            let mut diagnostic = Vec::new();
            tokio::try_join!(
                stdout.read_to_end(&mut output),
                stderr.read_to_end(&mut diagnostic)
            )?;
            ensure!(
                output.len() <= 4 * 1024 * 1024 && diagnostic.len() <= 16 * 1024,
                "p4 response exceeds limit"
            );
            let status = child.wait().await?;
            ensure!(
                status.success(),
                "Perforce request failed: {}",
                String::from_utf8_lossy(&diagnostic).trim()
            );
            let text = String::from_utf8(output).context("p4 response is not UTF-8")?;
            let fields = parse_tagged(&text);
            ensure!(
                fields.get("code").is_none_or(|code| code != "error"),
                "Perforce error: {}",
                fields
                    .get("data")
                    .map(String::as_str)
                    .unwrap_or("unknown error")
            );
            Ok::<_, anyhow::Error>(fields)
        })
        .await
        .context("Perforce request timed out")?;
        if result.is_err() {
            let _ = child.kill().await;
        }
        result
    }

    pub fn client_name(attempt: Uuid) -> String {
        format!("p4forge_{}", attempt.simple())
    }

    pub async fn checkout(
        &self,
        snapshot: &SourceSnapshot,
        attempt: Uuid,
        directory: &std::path::Path,
    ) -> Result<()> {
        snapshot.connection.validate()?;
        snapshot.root.validate()?;
        ensure!(snapshot.changelist > 0, "invalid snapshot changelist");
        let info = self.info(&snapshot.connection).await?;
        let identity = info
            .get("ServerID")
            .or_else(|| info.get("serverID"))
            .or_else(|| info.get("serverAddress"));
        ensure!(
            identity == Some(&snapshot.server_id),
            "Perforce server identity changed since run creation"
        );
        let name = Self::client_name(attempt);
        let path = directory.to_str().context("workspace path is not UTF-8")?;
        ensure!(
            directory.is_absolute() && !path.contains(['\n', '\r', '\0']),
            "invalid workspace path"
        );
        let mut form = format!(
            "Client: {name}\nOwner: {}\nHost:\nRoot: {path}\nOptions: noallwrite noclobber nocompress unlocked nomodtime rmdir\nLineEnd: unix\n",
            snapshot.connection.user
        );
        match &snapshot.root {
            Root::Stream { stream } => {
                form.push_str(&format!(
                    "Stream: {stream}\nStreamAtChange: {}\n",
                    snapshot.changelist
                ));
            }
            Root::Classic { view } => {
                form.push_str("View:\n");
                for (index, depot) in view.iter().enumerate() {
                    form.push_str(&format!("\t{depot} //{name}/root{index}/...\n"));
                }
            }
            Root::View { view } => {
                form.push_str("View:\n");
                for mapping in view::client_view(view, &name)? {
                    form.push_str(&format!("\t{mapping}\n"));
                }
            }
        }
        self.tagged_input(&snapshot.connection, &["client", "-i"], Some(&form))
            .await?;
        let actual = self
            .tagged(&snapshot.connection, &["client", "-o", &name])
            .await?;
        let old_name = snapshot
            .client_spec
            .get("Client")
            .context("snapshot has no client name")?;
        let expected: BTreeMap<_, _> = snapshot
            .client_spec
            .iter()
            .filter(|(key, _)| {
                key.strip_prefix("View").is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
                })
            })
            .map(|(key, value)| {
                (
                    key.clone(),
                    value.replace(&format!("//{old_name}/"), &format!("//{name}/")),
                )
            })
            .collect();
        let view: BTreeMap<_, _> = actual
            .iter()
            .filter(|(key, _)| {
                key.strip_prefix("View").is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
                })
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        ensure!(
            !expected.is_empty() && expected == view,
            "Perforce checkout view differs from the saved snapshot"
        );
        let revision = format!("//{name}/...@{}", snapshot.changelist);
        self.tagged(
            &snapshot.connection,
            &["-c", &name, "sync", "-q", &revision],
        )
        .await?;
        Ok(())
    }

    pub async fn delete_client(&self, connection: &Connection, attempt: Uuid) -> Result<()> {
        let name = Self::client_name(attempt);
        let existing = self.tagged(connection, &["clients", "-e", &name]).await?;
        if existing.get("client") == Some(&name) {
            self.tagged(connection, &["client", "-d", &name]).await?;
        }
        Ok(())
    }

    pub async fn info(&self, connection: &Connection) -> Result<BTreeMap<String, String>> {
        // info can succeed with redacted metadata even when a ticket is invalid.
        self.tagged(connection, &["login", "-s"]).await?;
        self.tagged(connection, &["info", "-s"]).await
    }

    pub async fn snapshot(
        &self,
        connection: &Connection,
        root: &Root,
        changelist: i64,
    ) -> Result<SourceSnapshot> {
        root.validate()?;
        ensure!(
            changelist > 0,
            "changelist must be a positive submitted change"
        );
        let number = changelist.to_string();
        let info = self.info(connection).await?;
        let change = self
            .tagged(connection, &["describe", "-s", &number])
            .await?;
        ensure!(
            change.get("change") == Some(&number)
                && change
                    .get("status")
                    .is_some_and(|status| status == "submitted"),
            "changelist is missing, restricted or not submitted"
        );
        let name = format!("p4forge_preview_{}", Uuid::new_v4().simple());
        let mut stream_specs = Vec::new();
        let client_spec = match root {
            Root::Stream { stream } => {
                let mut ancestor = stream.clone();
                loop {
                    ensure!(stream_specs.len() < 32, "stream ancestry exceeds 32 levels");
                    let revision = format!("{ancestor}@{number}");
                    let spec = self
                        .tagged(connection, &["stream", "-o", &revision])
                        .await?;
                    ensure!(
                        spec.get("Stream") == Some(&ancestor),
                        "stream spec not available at the requested changelist"
                    );
                    ensure!(
                        !spec.iter().filter(|(key, _)| key.starts_with("Paths")).any(
                            |(_, value)| value
                                .lines()
                                .any(|line| line.trim_start().starts_with("import"))
                        ),
                        "stream imports are not supported until file revision manifests are implemented"
                    );
                    ensure!(
                        !spec.keys().any(|key| key.starts_with("Components")),
                        "stream components are not supported until file revision manifests are implemented"
                    );
                    let parent = spec
                        .get("Parent")
                        .context("stream response has no Parent")?
                        .clone();
                    let inherits = spec
                        .get("ParentView")
                        .is_none_or(|value| value == "inherit");
                    stream_specs.push(spec);
                    if parent == "none" || !inherits {
                        break;
                    }
                    Root::Stream {
                        stream: parent.clone(),
                    }
                    .validate()?;
                    ancestor = parent;
                }
                let mut spec = self
                    .tagged(
                        connection,
                        &["client", "-o", "-S", stream, "-c", &number, &name],
                    )
                    .await?;
                ensure!(
                    spec.get("Stream").is_some_and(|value| value == stream),
                    "Perforce did not resolve the requested stream"
                );
                // Named imports can move independently of @CL. Do not claim a fully pinned snapshot for them yet.
                ensure!(
                    !spec.keys().any(|key| key.starts_with("ChangeView")),
                    "import ChangeView is not supported yet"
                );
                spec.insert("StreamAtChange".into(), number.clone());
                spec
            }
            Root::Classic { view } => {
                let mut spec = BTreeMap::from([
                    ("Client".into(), name.clone()),
                    ("Owner".into(), connection.user.clone()),
                    ("LineEnd".into(), "local".into()),
                ]);
                for (index, path) in view.iter().enumerate() {
                    spec.insert(
                        format!("View{index}"),
                        format!("{path} //{name}/root{index}/..."),
                    );
                }
                spec
            }
            Root::View { view } => {
                let mut spec = BTreeMap::from([
                    ("Client".into(), name.clone()),
                    ("Owner".into(), connection.user.clone()),
                    ("LineEnd".into(), "unix".into()),
                ]);
                for (index, mapping) in view::client_view(view, &name)?.into_iter().enumerate() {
                    spec.insert(format!("View{index}"), mapping);
                }
                spec
            }
        };
        Ok(SourceSnapshot {
            connection: connection.clone(),
            root: root.clone(),
            changelist,
            server_id: info
                .get("ServerID")
                .or_else(|| info.get("serverID"))
                .or_else(|| info.get("serverAddress"))
                .context("Perforce response has no server identity")?
                .clone(),
            server_version: info
                .get("serverVersion")
                .context("Perforce response has no version")?
                .clone(),
            client_spec,
            stream_specs,
        })
    }
}

pub fn parse_tagged(source: &str) -> BTreeMap<String, String> {
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    let mut last_key: Option<String> = None;
    for line in source.lines() {
        if let Some(tag) = line.strip_prefix("... ") {
            let (key, value) = tag.split_once(' ').unwrap_or((tag, ""));
            fields.insert(key.into(), value.into());
            last_key = Some(key.into());
        } else if !line.is_empty()
            && let Some(key) = &last_key
        {
            let value = fields.get_mut(key).expect("key was inserted");
            value.push('\n');
            value.push_str(line);
        }
    }
    // Some successful commands (notably change -i) emit plain text even with -ztag.
    if fields.is_empty() && !source.trim().is_empty() {
        fields.insert("data".into(), source.trim().into());
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_command_transports_and_revspec_injection() {
        for port in [
            "rsh:evil command",
            "rsh:executable:1666",
            "ssl:host:0",
            "host:abc",
            "host:1666;evil",
        ] {
            assert!(
                Connection {
                    port: port.into(),
                    user: "ci".into(),
                    charset: None
                }
                .validate()
                .is_err()
            );
        }
        for stream in [
            "-S",
            "//streams/main@head",
            "//streams/...",
            "//streams/main\nType: development",
        ] {
            assert!(
                Root::Stream {
                    stream: stream.into()
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn preserves_multiline_tagged_values() {
        assert_eq!(
            parse_tagged("Change 7 created.\n")["data"],
            "Change 7 created."
        );
        let parsed = parse_tagged(
            "... change 12\n... status submitted\n... desc line one\nline two\n\n... depotFile0 //streams/main/file\n",
        );
        assert_eq!(parsed["desc"], "line one\nline two");
        assert_eq!(parsed["depotFile0"], "//streams/main/file");
    }
}
