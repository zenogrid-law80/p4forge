//! Native Agent updates. The separately installed worker survives replacement of the Agent.
use anyhow::{Context, Result, ensure};
use p4forge_protocol::{MAX_AGENT_BINARY, UpdateOffer, UpdatePoll, UpdateReceipt, UpdateResponse};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::process::Command;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    offer: UpdateOffer,
    previous_sha256: String,
}
#[derive(Serialize, Deserialize)]
struct Health {
    id: Uuid,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
struct WorkerHealth {
    protocol: u32,
    time: u64,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    pending: Pending,
    phase: String,
    started: u64,
    outcome: Option<UpdateReceipt>,
}
pub fn info() -> serde_json::Value {
    serde_json::json!({"protocol":1,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"version":env!("CARGO_PKG_VERSION")})
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hash_file(path: &Path) -> Result<String> {
    ensure!(
        fs::metadata(path)?.len() <= MAX_AGENT_BINARY as u64,
        "Agent binary exceeds 64 MiB"
    );
    Ok(hash(&fs::read(path)?))
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    ensure!(
        fs::symlink_metadata(path)?.is_file() && fs::metadata(path)?.len() <= 16 * 1024,
        "invalid update state file"
    );
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    atomic_bytes(path, &serde_json::to_vec(value)?, false)
}
fn atomic_bytes(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    use std::io::Write;
    let temp = path.with_extension("next");
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if executable { 0o755 } else { 0o600 });
    }
    #[cfg(not(unix))]
    let _ = executable;
    // Only root/SYSTEM can write the update directory. Remove an interrupted temporary write.
    if temp.exists() {
        fs::remove_file(&temp)?;
    }
    let mut file = options.open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temp, path)?;
    #[cfg(unix)]
    fs::File::open(path.parent().context("missing update directory")?)?.sync_all()?;
    Ok(())
}
fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
fn worker_alive(dir: &Path) -> bool {
    read_json::<WorkerHealth>(&dir.join("worker-health.json"))
        .is_ok_and(|v| v.protocol == 1 && now() >= v.time && now().saturating_sub(v.time) < 20)
}
fn pulse(dir: &Path) -> Result<()> {
    atomic_json(
        &dir.join("worker-health.json"),
        &WorkerHealth {
            protocol: 1,
            time: now(),
        },
    )
}

pub(crate) struct Reporter {
    dir: PathBuf,
    sha256: String,
}
impl Reporter {
    pub(crate) fn new(state: &Path) -> Result<Self> {
        Ok(Self {
            dir: state.join("self-patch"),
            sha256: hash_file(&std::env::current_exe()?)?,
        })
    }
    pub(crate) fn pending(&self) -> bool {
        self.dir.join("pending.json").exists() || self.dir.join("result.json").exists()
    }
    pub(crate) async fn poll(&self, api: &super::Api) -> Result<bool> {
        if !cfg!(any(windows, target_os = "macos")) {
            return Ok(false);
        }
        let pending = if self.dir.join("pending.json").exists() {
            Some(read_json::<Pending>(&self.dir.join("pending.json"))?)
        } else {
            None
        };
        let receipt = if self.dir.join("result.json").exists() {
            Some(read_json::<UpdateReceipt>(&self.dir.join("result.json"))?)
        } else {
            None
        };
        let input = UpdatePoll {
            sha256: self.sha256.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
            ready: worker_alive(&self.dir)
                && (pending.is_some() || !self.dir.join("journal.json").exists()),
            pending_id: pending.as_ref().map(|p| p.offer.id),
            receipt,
        };
        let response: UpdateResponse = api.post("agent/update", &input).await?;
        if response.receipt_accepted {
            remove(&self.dir.join("result.json"))?;
        }
        if let Some(id) = response.healthy_id {
            ensure!(
                pending
                    .as_ref()
                    .is_some_and(|p| p.offer.id == id && p.offer.sha256 == self.sha256),
                "unexpected update health acknowledgement"
            );
            atomic_json(
                &self.dir.join("healthy.json"),
                &Health {
                    id,
                    sha256: self.sha256.clone(),
                },
            )?;
        }
        if let Some(offer) = response.offer {
            let id = offer.id;
            if self.stage(api, offer).await.is_err() {
                // Do not include URLs, response bodies, environment or credentials in update diagnostics.
                atomic_json(
                    &self.dir.join("result.json"),
                    &UpdateReceipt {
                        id,
                        status: "failed".into(),
                        reason: "download, checksum or candidate compatibility check failed".into(),
                    },
                )?;
            }
        }
        Ok(self.pending())
    }
    async fn stage(&self, api: &super::Api, offer: UpdateOffer) -> Result<()> {
        ensure!(api.server.scheme() == "https", "self-patch requires HTTPS");
        ensure!(
            worker_alive(&self.dir) && !self.pending() && !self.dir.join("journal.json").exists(),
            "updater is unavailable or busy"
        );
        ensure!(
            offer.size > 0 && offer.size <= MAX_AGENT_BINARY as i64,
            "invalid update size"
        );
        // The endpoint is derived locally, bound to this Agent, and cannot redirect to another origin.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(90))
            .build()?;
        let mut response = client
            .get(
                api.server
                    .join(&format!("agent/updates/{}/binary", offer.id))?,
            )
            .bearer_auth(&api.token)
            .send()
            .await?
            .error_for_status()?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= offer.size as usize,
                "update response exceeds pinned size"
            );
            bytes.extend_from_slice(&chunk);
        }
        ensure!(
            bytes.len() == offer.size as usize && hash(&bytes) == offer.sha256,
            "update checksum mismatch"
        );
        let candidate = self.dir.join("candidate.bin");
        atomic_bytes(&candidate, &bytes, true)?;
        verify_candidate(&candidate).await?;
        remove(&self.dir.join("healthy.json"))?;
        atomic_json(
            &self.dir.join("pending.json"),
            &Pending {
                offer,
                previous_sha256: self.sha256.clone(),
            },
        )?;
        Ok(())
    }
}
async fn verify_candidate(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    ensure!(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict"])
            .arg(path)
            .status()
            .await?
            .success(),
        "invalid candidate code signature"
    );
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(path)
            .arg("--self-patch-info")
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        output.status.success() && output.stdout.len() < 4096,
        "candidate check failed"
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    ensure!(
        value["protocol"] == 1
            && value["os"] == std::env::consts::OS
            && value["arch"] == std::env::consts::ARCH,
        "incompatible Agent update"
    );
    Ok(())
}

trait Service {
    async fn stop(&self) -> Result<()>;
    async fn start(&self) -> Result<()>;
}
struct NativeService;
impl Service for NativeService {
    async fn stop(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            let status = Command::new("/bin/launchctl")
                .args(["print", "system/kr.co.zenogrid.p4forge.agent"])
                .output()
                .await?;
            if status.status.success() {
                ensure!(
                    Command::new("/bin/launchctl")
                        .args(["bootout", "system/kr.co.zenogrid.p4forge.agent"])
                        .status()
                        .await?
                        .success(),
                    "could not stop Agent"
                );
            }
            for _ in 0..25 {
                if !Command::new("/bin/launchctl")
                    .args(["print", "system/kr.co.zenogrid.p4forge.agent"])
                    .output()
                    .await?
                    .status
                    .success()
                {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            anyhow::bail!("Agent did not stop");
        }
        #[cfg(windows)]
        return powershell("Stop-ScheduledTask -TaskName 'P4Forge-Agent'; $end=[DateTime]::UtcNow.AddSeconds(25); while ((Get-ScheduledTask -TaskName 'P4Forge-Agent').State -eq 'Running') { if ([DateTime]::UtcNow -gt $end) { throw 'Agent did not stop' }; Start-Sleep -Milliseconds 250 }").await;
        #[cfg(not(any(windows, target_os = "macos")))]
        anyhow::bail!("native updater requires Windows or macOS")
    }
    async fn start(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            if !Command::new("/bin/launchctl")
                .args(["print", "system/kr.co.zenogrid.p4forge.agent"])
                .output()
                .await?
                .status
                .success()
            {
                ensure!(
                    Command::new("/bin/launchctl")
                        .args([
                            "bootstrap",
                            "system",
                            "/Library/LaunchDaemons/kr.co.zenogrid.p4forge.agent.plist"
                        ])
                        .status()
                        .await?
                        .success(),
                    "could not start Agent"
                );
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            powershell("Start-ScheduledTask -TaskName 'P4Forge-Agent'").await
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        anyhow::bail!("native updater requires Windows or macOS")
    }
}
#[cfg(windows)]
async fn powershell(script: &str) -> Result<()> {
    let shell = PathBuf::from(std::env::var_os("SystemRoot").context("SystemRoot missing")?)
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let status = tokio::time::timeout(
        Duration::from_secs(35),
        Command::new(shell)
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!("$ErrorActionPreference='Stop'; {script}"))
            .kill_on_drop(true)
            .status(),
    )
    .await??;
    ensure!(status.success(), "Agent task operation failed");
    Ok(())
}
fn valid_pending(p: &Pending) -> bool {
    let valid_hash = |h: &str| {
        h.len() == 64
            && h.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    };
    !p.offer.id.is_nil()
        && valid_hash(&p.offer.sha256)
        && valid_hash(&p.previous_sha256)
        && p.offer.sha256 != p.previous_sha256
        && p.offer.size > 0
        && p.offer.size <= MAX_AGENT_BINARY as i64
}
async fn step(dir: &Path, installed: &Path, service: &impl Service, timeout: u64) -> Result<()> {
    let journal_path = dir.join("journal.json");
    let mut journal = if journal_path.exists() {
        read_json::<Journal>(&journal_path)?
    } else {
        if !dir.join("pending.json").exists() {
            return Ok(());
        }
        let pending: Pending = read_json(&dir.join("pending.json"))?;
        ensure!(valid_pending(&pending), "invalid pending update");
        if dir
            .parent()
            .context("missing state directory")?
            .join("inflight.json")
            .exists()
        {
            return Ok(());
        }
        if !hash_file(installed).is_ok_and(|hash| hash == pending.previous_sha256)
            || !hash_file(&dir.join("candidate.bin")).is_ok_and(|hash| hash == pending.offer.sha256)
        {
            atomic_json(
                &dir.join("result.json"),
                &UpdateReceipt {
                    id: pending.offer.id,
                    status: "failed".into(),
                    reason: "installed or staged binary checksum changed".into(),
                },
            )?;
            remove(&dir.join("pending.json"))?;
            return Ok(());
        }
        atomic_bytes(&dir.join("previous.bin"), &fs::read(installed)?, true)?;
        let journal = Journal {
            pending,
            phase: "prepared".into(),
            started: now(),
            outcome: None,
        };
        atomic_json(&journal_path, &journal)?;
        journal
    };
    ensure!(valid_pending(&journal.pending), "invalid update journal");
    if journal.phase == "prepared" {
        // This is re-entrant after power loss: backup and journal exist before the first replacement.
        ensure!(
            hash_file(&dir.join("previous.bin"))? == journal.pending.previous_sha256,
            "rollback binary checksum mismatch"
        );
        let replaced: Result<()> = async {
            ensure!(
                hash_file(&dir.join("candidate.bin"))? == journal.pending.offer.sha256,
                "staged binary checksum changed"
            );
            service.stop().await?;
            ensure!(
                !dir.parent()
                    .context("missing state directory")?
                    .join("inflight.json")
                    .exists(),
                "build still requires recovery"
            );
            atomic_bytes(installed, &fs::read(dir.join("candidate.bin"))?, true)?;
            Ok(())
        }
        .await;
        journal.phase = if replaced.is_ok() {
            "testing"
        } else {
            "rollback"
        }
        .into();
        journal.started = now();
        atomic_json(&journal_path, &journal)?;
    }
    if journal.phase == "testing" {
        if service.start().await.is_ok() {
            loop {
                pulse(dir)?;
                if read_json::<Health>(&dir.join("healthy.json")).is_ok_and(|h| {
                    h.id == journal.pending.offer.id && h.sha256 == journal.pending.offer.sha256
                }) {
                    journal.phase = "done".into();
                    journal.outcome = Some(UpdateReceipt {
                        id: journal.pending.offer.id,
                        status: "succeeded".into(),
                        reason: String::new(),
                    });
                    atomic_json(&journal_path, &journal)?;
                    break;
                }
                if now() < journal.started || now().saturating_sub(journal.started) >= timeout {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        if journal.phase != "done" {
            journal.phase = "rollback".into();
            atomic_json(&journal_path, &journal)?;
        }
    }
    if journal.phase == "rollback" {
        ensure!(
            hash_file(&dir.join("previous.bin"))? == journal.pending.previous_sha256,
            "rollback binary checksum mismatch"
        );
        service.stop().await?;
        atomic_bytes(installed, &fs::read(dir.join("previous.bin"))?, true)?;
        service.start().await?;
        journal.phase = "done".into();
        journal.outcome = Some(UpdateReceipt {
            id: journal.pending.offer.id,
            status: "rolled_back".into(),
            reason: "new Agent did not complete its server health check; previous binary restored"
                .into(),
        });
        atomic_json(&journal_path, &journal)?;
    }
    if journal.phase == "done" {
        // Keep result until the server acknowledges it; keep previous.bin for local recovery.
        remove(&dir.join("pending.json"))?;
        atomic_json(
            &dir.join("result.json"),
            journal.outcome.as_ref().context("missing update outcome")?,
        )?;
        remove(&dir.join("healthy.json"))?;
        remove(&dir.join("candidate.bin"))?;
        remove(&journal_path)?;
    }
    Ok(())
}
pub async fn worker() -> Result<()> {
    let exe = std::env::current_exe()?;
    #[cfg(target_os = "macos")]
    let (dir, installed) = {
        ensure!(unsafe { libc::geteuid() } == 0, "updater requires root");
        ensure!(
            exe == Path::new("/Library/P4Forge/bin/p4forge-updater"),
            "unexpected updater location"
        );
        (
            PathBuf::from("/Library/P4Forge/state/self-patch"),
            PathBuf::from("/Library/P4Forge/bin/p4forge-agent"),
        )
    };
    #[cfg(windows)]
    let (dir, installed) = {
        let root = exe.parent().context("missing install root")?;
        (
            root.join("state/self-patch"),
            root.join("p4forge-agent.exe"),
        )
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let (dir, installed) = {
        let root = exe.parent().context("missing install root")?;
        (root.join("state/self-patch"), root.join("p4forge-agent"))
    };
    ensure!(
        cfg!(any(windows, target_os = "macos")),
        "updater only supports native Agents"
    );
    ensure!(
        fs::symlink_metadata(&dir)?.is_dir(),
        "install the protected self-patch directory first"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = fs::metadata(&dir)?;
        ensure!(
            m.uid() == 0 && m.mode() & 0o077 == 0,
            "unsafe update directory"
        );
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("worker.lock"))?;
    lock.try_lock()?;
    loop {
        pulse(&dir)?;
        if step(&dir, &installed, &NativeService, 90).await.is_err() {
            // Persist journal for recovery on the next worker iteration or OS reboot.
            eprintln!("Agent update is awaiting local recovery; journal retained");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct FakeService {
        dir: PathBuf,
        healthy: bool,
    }
    impl Service for FakeService {
        async fn stop(&self) -> Result<()> {
            Ok(())
        }
        async fn start(&self) -> Result<()> {
            if self.healthy {
                let p: Pending = read_json(&self.dir.join("pending.json"))?;
                atomic_json(
                    &self.dir.join("healthy.json"),
                    &Health {
                        id: p.offer.id,
                        sha256: p.offer.sha256,
                    },
                )?;
            }
            Ok(())
        }
    }
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Pending) {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("state/self-patch");
        fs::create_dir_all(&dir).unwrap();
        let installed = temp.path().join("agent");
        fs::write(&installed, b"old binary").unwrap();
        fs::write(dir.join("candidate.bin"), b"new binary").unwrap();
        let pending = Pending {
            offer: UpdateOffer {
                id: Uuid::new_v4(),
                release_id: Uuid::new_v4(),
                version: "next".into(),
                sha256: hash(b"new binary"),
                size: 10,
            },
            previous_sha256: hash(b"old binary"),
        };
        atomic_json(&dir.join("pending.json"), &pending).unwrap();
        (temp, dir, installed, pending)
    }
    #[tokio::test]
    async fn successful_patch_keeps_backup_and_reports_only_after_health() {
        let (_t, dir, installed, _) = fixture();
        step(
            &dir,
            &installed,
            &FakeService {
                dir: dir.clone(),
                healthy: true,
            },
            0,
        )
        .await
        .unwrap();
        assert_eq!(fs::read(installed).unwrap(), b"new binary");
        assert_eq!(fs::read(dir.join("previous.bin")).unwrap(), b"old binary");
        assert_eq!(
            read_json::<UpdateReceipt>(&dir.join("result.json"))
                .unwrap()
                .status,
            "succeeded"
        );
        assert!(!dir.join("pending.json").exists());
    }
    #[tokio::test]
    async fn missing_health_rolls_back_and_reboot_resumes_journal() {
        let (_t, dir, installed, pending) = fixture();
        fs::write(dir.join("previous.bin"), b"old binary").unwrap();
        fs::write(&installed, b"new binary").unwrap();
        atomic_json(
            &dir.join("journal.json"),
            &Journal {
                pending,
                phase: "testing".into(),
                started: 0,
                outcome: None,
            },
        )
        .unwrap();
        step(
            &dir,
            &installed,
            &FakeService {
                dir: dir.clone(),
                healthy: false,
            },
            0,
        )
        .await
        .unwrap();
        assert_eq!(fs::read(installed).unwrap(), b"old binary");
        assert_eq!(
            read_json::<UpdateReceipt>(&dir.join("result.json"))
                .unwrap()
                .status,
            "rolled_back"
        );
    }
    #[tokio::test]
    async fn active_build_and_tampered_download_never_replace_binary() {
        let (_t, dir, installed, _) = fixture();
        fs::write(dir.parent().unwrap().join("inflight.json"), b"{}").unwrap();
        let svc = FakeService {
            dir: dir.clone(),
            healthy: false,
        };
        step(&dir, &installed, &svc, 0).await.unwrap();
        assert!(!dir.join("journal.json").exists());
        assert_eq!(fs::read(&installed).unwrap(), b"old binary");
        remove(&dir.parent().unwrap().join("inflight.json")).unwrap();
        fs::write(dir.join("candidate.bin"), b"tampered").unwrap();
        step(&dir, &installed, &svc, 0).await.unwrap();
        assert_eq!(fs::read(installed).unwrap(), b"old binary");
        assert_eq!(
            read_json::<UpdateReceipt>(&dir.join("result.json"))
                .unwrap()
                .status,
            "failed"
        );
    }
}
