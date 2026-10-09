//! Root supervisor with a dedicated, non-login build UID on macOS.
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::MetadataExt, process::CommandExt},
    },
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};

pub const BUILD_UID: u32 = 10003;

// proc_listpids(PROC_UID_ONLY) is an enumeration filter, not the signal
// authorization boundary. Signals are sent only after dropping to BUILD_UID.
fn live_processes(uid: u32) -> Result<Vec<i32>> {
    const PROC_UID_ONLY: u32 = 4;
    let required = unsafe { libc::proc_listpids(PROC_UID_ONLY, uid, std::ptr::null_mut(), 0) };
    ensure!(required >= 0, "enumerate build processes");
    let mut pids = vec![0i32; required as usize / size_of::<i32>() + 256];
    let bytes = unsafe {
        libc::proc_listpids(
            PROC_UID_ONLY,
            uid,
            pids.as_mut_ptr().cast(),
            (pids.len() * size_of::<i32>()) as i32,
        )
    };
    ensure!(bytes >= 0, "read build processes");
    pids.truncate(bytes as usize / size_of::<i32>());
    let own_pid = unsafe { libc::getpid() };
    let mut live = Vec::new();
    for pid in pids.into_iter().filter(|pid| *pid > 1 && *pid != own_pid) {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size_of::<libc::proc_bsdinfo>() as i32,
            )
        };
        if read == size_of::<libc::proc_bsdinfo>() as i32
            && info.pbi_uid == uid
            && info.pbi_status != libc::SZOMB
        {
            live.push(pid);
        } else if read <= 0 {
            let error = std::io::Error::last_os_error();
            ensure!(
                matches!(error.raw_os_error(), Some(libc::ESRCH | libc::EPERM)),
                "inspect build process: {error}"
            );
        }
    }
    Ok(live)
}

fn validate_account() -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "macOS supervisor requires root"
    );
    let account = unsafe { libc::getpwnam(c"_p4forge".as_ptr()) };
    ensure!(
        !account.is_null(),
        "install the dedicated _p4forge account first"
    );
    let account = unsafe { &*account };
    ensure!(
        account.pw_uid == BUILD_UID && account.pw_gid == BUILD_UID,
        "_p4forge must use the dedicated UID/GID 10003"
    );
    Ok(())
}

fn protected(path: &Path, private: bool) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        !metadata.file_type().is_symlink() && metadata.uid() == 0,
        "{} must be owned by root and not a symlink",
        path.display()
    );
    ensure!(
        metadata.mode() & if private { 0o077 } else { 0o022 } == 0,
        "unsafe permissions on {}",
        path.display()
    );
    Ok(())
}

pub fn validate_supervisor(config: &crate::AgentConfig) -> Result<()> {
    validate_account()?;
    ensure!(
        config.work_dir == Path::new("/Library/P4Forge/work")
            && config.state_dir == Path::new("/Library/P4Forge/state"),
        "macOS uses one dedicated slot and fixed installation directories"
    );
    for directory in [
        Path::new("/Library/P4Forge"),
        &config.work_dir,
        &config.state_dir,
    ] {
        protected(directory, directory == config.state_dir)?;
    }
    protected(&std::env::current_exe()?, false)?;
    protected(&config.perforce.binary, false)?;
    for file in &config.credential_files {
        protected(file, true)?;
    }
    Ok(())
}

/// Runs BEFORE creating a Tokio runtime. Helpers never receive server tokens.
pub fn helper_entry() -> Result<bool> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--macos-kill-builds") => {
            validate_account()?;
            // Drop all IDs before signalling. The kernel prevents a reused PID
            // from targeting another user. macOS POSIX kill(-1) includes self,
            // so enumerate individual processes and explicitly exclude self.
            unsafe {
                ensure!(
                    libc::setgroups(0, std::ptr::null()) == 0
                        && libc::setgid(BUILD_UID) == 0
                        && libc::setuid(BUILD_UID) == 0,
                    "drop helper privileges"
                );
                ensure!(
                    libc::getuid() == BUILD_UID && libc::geteuid() == BUILD_UID,
                    "helper UID verification failed"
                );
            }
            for _ in 0..100 {
                let pids = live_processes(BUILD_UID)?;
                if pids.is_empty() {
                    return Ok(true);
                }
                for pid in pids {
                    if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
                        let error = std::io::Error::last_os_error();
                        ensure!(
                            matches!(error.raw_os_error(), Some(libc::ESRCH | libc::EPERM)),
                            "kill build process: {error}"
                        );
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            anyhow::bail!("build processes did not terminate");
        }
        Some("--macos-watchdog") => {
            validate_account()?;
            let fd: i32 = args.get(2).context("lock FD missing")?.parse()?;
            ensure!(fd > 2, "invalid lock FD");
            let _lock = unsafe { File::from_raw_fd(fd) };
            ensure!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
                "protect watchdog lock FD"
            );
            // Leave the supervisor group so its forced termination cannot kill
            // the watchdog before it has reclaimed detached build processes.
            ensure!(unsafe { libc::setsid() } >= 0, "watchdog setsid failed");
            std::io::stdout().write_all(b"R")?;
            std::io::stdout().flush()?;
            let mut buffer = [0u8; 1];
            while std::io::stdin().read(&mut buffer)? != 0 {}
            while let Err(error) = kill_build_processes() {
                eprintln!("build cleanup pending: {error:#}");
                std::thread::sleep(Duration::from_secs(1));
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub fn kill_build_processes() -> Result<()> {
    let status = Command::new(std::env::current_exe()?)
        .arg("--macos-kill-builds")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()?;
    ensure!(status.success(), "macOS build cleanup failed");
    Ok(())
}

pub struct Watchdog(Child);
impl Watchdog {
    pub fn start(lock: &File) -> Result<Self> {
        let fd = lock.as_raw_fd();
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--macos-watchdog", &fd.to_string()])
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let mut ready = [0u8; 1];
        child
            .stdout
            .take()
            .context("watchdog output missing")?
            .read_exact(&mut ready)?;
        ensure!(ready == *b"R", "watchdog did not initialize");
        Ok(Self(child))
    }
}
impl Drop for Watchdog {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enumeration_excludes_self_and_other_users_but_finds_children() {
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let pids = live_processes(unsafe { libc::getuid() }).unwrap();
        let _ = child.kill();
        let _ = child.wait();
        assert!(!pids.contains(&(std::process::id() as i32)));
        assert!(pids.contains(&(child.id() as i32)));
        assert!(
            !live_processes(BUILD_UID)
                .unwrap()
                .contains(&(std::process::id() as i32))
        );
    }
}
