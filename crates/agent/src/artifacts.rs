use crate::{Api, Entry, terminal_error};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use p4forge_domain::{ArtifactSpec, MAX_ARTIFACT_FILE, MAX_ARTIFACT_TOTAL};
use p4forge_protocol::{
    ARTIFACT_CHUNK, ArtifactChunk, ArtifactEntry, ArtifactManifest, ArtifactUpload, Assignment,
};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

struct Captured {
    entry: ArtifactEntry,
    file: PathBuf,
}

fn ordinary(metadata: &std::fs::Metadata) -> Result<()> {
    ensure!(
        !metadata.file_type().is_symlink(),
        "artifact symlinks are forbidden"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        ensure!(
            metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "artifact reparse points are forbidden"
        );
    }
    Ok(())
}
fn open_source(root: &Path, relative: &str) -> Result<File> {
    let mut path = root.to_path_buf();
    ordinary(&std::fs::symlink_metadata(&path)?)?;
    for component in relative.split('/') {
        path.push(component);
        ordinary(&std::fs::symlink_metadata(&path)?)?;
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .share_mode(0);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    ordinary(&metadata)?;
    ensure!(metadata.is_file(), "artifact must be a regular file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(metadata.nlink() == 1, "artifact hardlinks are forbidden");
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, GetFileInformationByHandle,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } != 0,
            "inspect artifact handle: {}",
            std::io::Error::last_os_error()
        );
        ensure!(
            info.nNumberOfLinks == 1 && info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "artifact links are forbidden"
        );
    }
    ensure!(
        metadata.len() <= MAX_ARTIFACT_FILE,
        "artifact exceeds 64 MiB"
    );
    Ok(file)
}

fn capture(directory: &Path, specs: &[ArtifactSpec]) -> Result<Vec<Captured>> {
    ensure!(
        !specs.is_empty() && specs.len() <= 16,
        "invalid artifact count"
    );
    let stage = directory.join("artifact-staging");
    std::fs::create_dir(&stage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut total = 0u64;
    let mut captured = Vec::new();
    for (index, spec) in specs.iter().enumerate() {
        spec.validate()?;
        let mut input = open_source(&directory.join("source"), &spec.path)
            .with_context(|| format!("collect artifact {}", spec.name))?;
        let expected = input.metadata()?.len();
        let path = stage.join(index.to_string());
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            total += n as u64;
            ensure!(
                size <= MAX_ARTIFACT_FILE && total <= MAX_ARTIFACT_TOTAL,
                "artifact size limit exceeded"
            );
            hash.update(&buffer[..n]);
            output.write_all(&buffer[..n])?;
        }
        ensure!(
            size == expected && input.metadata()?.len() == expected,
            "artifact changed during capture"
        );
        output.sync_all()?;
        captured.push(Captured {
            file: path,
            entry: ArtifactEntry {
                name: spec.name.clone(),
                path: spec.path.clone(),
                size,
                sha256: format!("{:x}", hash.finalize()),
            },
        });
    }
    Ok(captured)
}

async fn post<I: Serialize, O: DeserializeOwned>(api: &Api, path: &str, input: &I) -> Result<O> {
    for retry in 0..3 {
        match api.post(path, input).await {
            Ok(result) => return Ok(result),
            Err(error) if retry == 2 || terminal_error(&error) => return Err(error),
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
        }
    }
    unreachable!()
}

pub(super) async fn upload(
    api: &Api,
    a: &Assignment,
    directory: &Path,
    logs: &tokio::sync::mpsc::Sender<Entry>,
) -> Result<()> {
    let owned = directory.to_path_buf();
    let specs = a.artifacts.clone();
    let files = tokio::task::spawn_blocking(move || capture(&owned, &specs)).await??;
    let path = format!("agent/attempts/{}/artifacts", a.attempt_id);
    let manifest = ArtifactManifest {
        fence: a.fence,
        files: files.iter().map(|f| f.entry.clone()).collect(),
    };
    let uploads: Vec<ArtifactUpload> = post(api, &path, &manifest).await?;
    ensure!(
        uploads.len() == files.len(),
        "server artifact manifest mismatch"
    );
    for file in files {
        let upload = uploads
            .iter()
            .find(|u| u.name == file.entry.name)
            .context("server omitted artifact")?;
        let mut offset = u64::try_from(upload.uploaded_size)?;
        ensure!(offset <= file.entry.size, "invalid server artifact offset");
        if upload.status != "verified" {
            let mut source = tokio::fs::File::open(&file.file).await?;
            source.seek(std::io::SeekFrom::Start(offset)).await?;
            loop {
                let length = (file.entry.size - offset).min(ARTIFACT_CHUNK as u64) as usize;
                let mut buffer = vec![0; length];
                source.read_exact(&mut buffer).await?;
                let chunk = ArtifactChunk {
                    fence: a.fence,
                    offset,
                    data: STANDARD.encode(buffer),
                };
                let result: ArtifactUpload =
                    post(api, &format!("{path}/{}", upload.id), &chunk).await?;
                offset += length as u64;
                ensure!(
                    result.id == upload.id && result.uploaded_size == offset as i64,
                    "server artifact progress mismatch"
                );
                if offset == file.entry.size {
                    ensure!(
                        result.status == "verified",
                        "server did not verify artifact hash"
                    );
                    break;
                }
            }
        }
        logs.send(Entry {
            stream: "system",
            content: format!(
                "Artifact verified: {} ({} bytes, SHA-256 {})\n",
                file.entry.name, file.entry.size, file.entry.sha256
            ),
        })
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec(path: &str) -> ArtifactSpec {
        ArtifactSpec {
            name: "output.bin".into(),
            path: path.into(),
        }
    }
    #[test]
    fn capture_is_private_and_independent_of_workspace() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("source")).unwrap();
        std::fs::write(temp.path().join("source/output"), b"payload").unwrap();
        let files = capture(temp.path(), &[spec("output")]).unwrap();
        std::fs::remove_dir_all(temp.path().join("source")).unwrap();
        assert_eq!(std::fs::read(&files[0].file).unwrap(), b"payload");
        assert_eq!(
            files[0].entry.sha256,
            format!("{:x}", Sha256::digest(b"payload"))
        );
    }
    #[test]
    fn rejects_hardlinks_missing_files_and_oversize() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("original"), b"private").unwrap();
        std::fs::hard_link(root.join("original"), root.join("link")).unwrap();
        assert!(open_source(root, "link").is_err());
        assert!(open_source(root, "missing").is_err());
        let file = File::create(root.join("large")).unwrap();
        file.set_len(MAX_ARTIFACT_FILE + 1).unwrap();
        assert!(open_source(root, "large").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_components_and_special_files() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("real")).unwrap();
        std::fs::write(temp.path().join("real/file"), b"secret").unwrap();
        symlink(temp.path().join("real"), temp.path().join("link")).unwrap();
        assert!(open_source(temp.path(), "link/file").is_err());
        assert!(open_source(Path::new("/dev"), "null").is_err());
    }
}
