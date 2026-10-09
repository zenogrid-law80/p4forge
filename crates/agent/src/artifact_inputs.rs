use crate::{Api, Entry};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use p4forge_domain::{ArtifactSpec, MAX_ARTIFACT_FILE, MAX_ARTIFACT_TOTAL};
use p4forge_protocol::{ARTIFACT_CHUNK, ArtifactData, ArtifactInput, ArtifactRead, Assignment};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path, time::Duration};
use tokio::io::AsyncWriteExt;

fn validate(inputs: &[ArtifactInput]) -> Result<()> {
    ensure!(inputs.len() <= 16, "too many artifact inputs");
    let mut names = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut sources = BTreeSet::new();
    let mut total = 0;
    for input in inputs {
        ArtifactSpec {
            name: input.name.clone(),
            path: input.name.clone(),
        }
        .validate()?;
        ensure!(
            names.insert(input.name.to_ascii_lowercase()) && ids.insert(input.artifact_id),
            "duplicate artifact input"
        );
        ensure!(
            !input.artifact_id.is_nil() && !input.source_run_id.is_nil(),
            "invalid artifact identity"
        );
        sources.insert(input.source_run_id);
        ensure!(
            input.size <= MAX_ARTIFACT_FILE,
            "artifact input exceeds 64 MiB"
        );
        total += input.size;
        ensure!(
            input.sha256.len() == 64
                && input
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid artifact digest"
        );
    }
    ensure!(
        total <= MAX_ARTIFACT_TOTAL && sources.len() <= 1,
        "invalid artifact input set"
    );
    Ok(())
}
fn decode(input: &ArtifactInput, offset: u64, response: ArtifactData) -> Result<Vec<u8>> {
    ensure!(
        offset <= input.size && response.offset == offset,
        "artifact chunk offset mismatch"
    );
    ensure!(
        response.data.len() <= ARTIFACT_CHUNK * 2,
        "artifact chunk exceeds limit"
    );
    let bytes = STANDARD.decode(response.data)?;
    ensure!(
        bytes.len() as u64 == (input.size - offset).min(ARTIFACT_CHUNK as u64),
        "artifact chunk size mismatch"
    );
    Ok(bytes)
}
fn verify(input: &ArtifactInput, digest: Sha256) -> Result<()> {
    ensure!(
        format!("{:x}", digest.finalize()) == input.sha256,
        "artifact input SHA-256 mismatch: {}",
        input.name
    );
    Ok(())
}
async fn chunk(api: &Api, a: &Assignment, input: &ArtifactInput, offset: u64) -> Result<Vec<u8>> {
    let path = format!(
        "agent/attempts/{}/inputs/{}",
        a.attempt_id, input.artifact_id
    );
    for retry in 0..3 {
        let request = async {
            let mut response = api
                .client
                .post(api.server.join(&path)?)
                .bearer_auth(&api.token)
                .json(&ArtifactRead {
                    fence: a.fence,
                    offset,
                })
                .send()
                .await?
                .error_for_status()?;
            let mut bytes = Vec::new();
            while let Some(part) = response.chunk().await? {
                ensure!(
                    bytes.len() + part.len() <= ARTIFACT_CHUNK * 2,
                    "artifact response exceeds limit"
                );
                bytes.extend_from_slice(&part);
            }
            decode(input, offset, serde_json::from_slice(&bytes)?)
        };
        let result = tokio::time::timeout(Duration::from_secs(20), request)
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
        match result {
            Ok(bytes) => return Ok(bytes),
            Err(error) if retry == 2 || crate::terminal_error(&error) => {
                return Err(anyhow::anyhow!("artifact input download failed"));
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
    unreachable!()
}
pub(super) async fn download(
    api: &Api,
    a: &Assignment,
    directory: &Path,
    logs: &tokio::sync::mpsc::Sender<Entry>,
) -> Result<()> {
    validate(&a.artifact_inputs)?;
    let stage = directory.join("inputs-stage");
    tokio::fs::create_dir(&stage).await?;
    for input in &a.artifact_inputs {
        let path = stage.join(&input.name);
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await?;
        let mut digest = Sha256::new();
        let mut offset = 0;
        // Even empty files require an authorized read and their empty SHA-256.
        loop {
            let bytes = chunk(api, a, input, offset).await?;
            digest.update(&bytes);
            file.write_all(&bytes).await?;
            offset += bytes.len() as u64;
            if offset == input.size {
                break;
            }
        }
        verify(input, digest)?;
        file.sync_all().await?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).await?;
        }
        logs.send(Entry {
            stream: "system",
            content: format!(
                "Verified input {} · {} bytes · SHA-256 {} · source run {}\n",
                input.name, input.size, input.sha256, input.source_run_id
            ),
        })
        .await?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o555)).await?;
    }
    // The build sees the input directory only after every file passes verification.
    tokio::fs::rename(stage, directory.join("inputs")).await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> ArtifactInput {
        ArtifactInput {
            artifact_id: uuid::Uuid::new_v4(),
            source_run_id: uuid::Uuid::new_v4(),
            name: "data.bin".into(),
            size: 4,
            sha256: p4forge_domain::hash_bytes(b"test"),
        }
    }
    #[test]
    fn inputs_reject_traversal_devices_duplicates_limits_and_corruption() {
        let good = input();
        validate(std::slice::from_ref(&good)).unwrap();
        assert!(
            decode(
                &good,
                0,
                ArtifactData {
                    offset: 0,
                    data: STANDARD.encode(b"test")
                }
            )
            .is_ok()
        );
        assert!(
            decode(
                &good,
                0,
                ArtifactData {
                    offset: 1,
                    data: STANDARD.encode(b"test")
                }
            )
            .is_err()
        );
        assert!(
            decode(
                &good,
                0,
                ArtifactData {
                    offset: 0,
                    data: STANDARD.encode(b"tes")
                }
            )
            .is_err()
        );
        assert!(verify(&good, Sha256::new_with_prefix(b"evil")).is_err());
        assert!(verify(&good, Sha256::new_with_prefix(b"test")).is_ok());
        for name in [
            "../x", "a/b", "a\\b", "CON", "nul.exe", "LPT1.txt", "a:stream",
        ] {
            let mut bad = good.clone();
            bad.name = name.into();
            assert!(validate(&[bad]).is_err());
        }
        let mut huge = good.clone();
        huge.size = MAX_ARTIFACT_FILE + 1;
        assert!(validate(&[huge]).is_err());
        let mut other = good.clone();
        other.name = "DATA.bin".into();
        other.artifact_id = uuid::Uuid::new_v4();
        assert!(validate(&[good, other]).is_err());
    }
}
