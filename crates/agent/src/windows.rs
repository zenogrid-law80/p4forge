//! Native Windows executor: SYSTEM supervisor, separate local build account,
//! an allowlisted environment and a kernel Job Object for the whole process tree.
use super::{Entry, capture};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    mem::{size_of, size_of_val},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    path::Path,
    ptr::{null, null_mut},
    sync::{Arc, atomic::AtomicUsize},
    time::Duration,
};
use tokio::sync::mpsc;
use windows_sys::Win32::{
    Foundation::*,
    Security::*,
    System::{JobObjects::*, Pipes::*, Threading::*},
};

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}
fn checked(value: i32, action: &str) -> Result<()> {
    ensure!(value != 0, "{action}: {}", std::io::Error::last_os_error());
    Ok(())
}
unsafe fn owned(handle: HANDLE) -> OwnedHandle {
    unsafe { OwnedHandle::from_raw_handle(handle) }
}
fn token_info(token: &OwnedHandle, kind: TOKEN_INFORMATION_CLASS) -> Result<Vec<usize>> {
    let mut bytes = 0;
    unsafe {
        GetTokenInformation(token.as_raw_handle(), kind, null_mut(), 0, &mut bytes);
    }
    ensure!(bytes > 0, "read token information size");
    let mut data = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
    checked(
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                kind,
                data.as_mut_ptr().cast(),
                bytes,
                &mut bytes,
            )
        },
        "read token information",
    )?;
    Ok(data)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub username: String,
    password: String,
}
impl Account {
    pub fn load(path: &Path) -> Result<Self> {
        let account: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        ensure!(
            account.username == "p4forge-build" && !account.password.is_empty(),
            "use the dedicated p4forge-build account from the installer"
        );
        Ok(account)
    }
    fn logon(&self) -> Result<OwnedHandle> {
        let mut token = null_mut();
        checked(
            unsafe {
                LogonUserW(
                    wide(&self.username).as_ptr(),
                    wide(".").as_ptr(),
                    wide(&self.password).as_ptr(),
                    LOGON32_LOGON_INTERACTIVE,
                    LOGON32_PROVIDER_DEFAULT,
                    &mut token,
                )
            },
            "log on dedicated build account",
        )?;
        let token = unsafe { owned(token) };
        let data = token_info(&token, TokenGroups)?;
        let groups = unsafe { &*data.as_ptr().cast::<TOKEN_GROUPS>() };
        let groups = unsafe {
            std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize)
        };
        ensure!(
            !groups.iter().any(|group| unsafe {
                IsWellKnownSid(group.Sid, WinBuiltinAdministratorsSid) != 0
            }),
            "build account must not belong to Administrators (including a filtered token)"
        );
        Ok(token)
    }
}

pub fn validate_supervisor(account: &Account, credentials: Vec<std::path::PathBuf>) -> Result<()> {
    let mut raw = null_mut();
    checked(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) },
        "open supervisor token",
    )?;
    let supervisor = unsafe { owned(raw) };
    let data = token_info(&supervisor, TokenUser)?;
    let user = unsafe { &*data.as_ptr().cast::<TOKEN_USER>() };
    ensure!(
        unsafe { IsWellKnownSid(user.User.Sid, WinLocalSystemSid) != 0 },
        "Windows supervisor must run as LocalSystem through the installed scheduled task"
    );
    let token = account.logon()?;
    // A dedicated thread avoids impersonation on an async runtime worker.
    std::thread::spawn(move || -> Result<()> {
        checked(
            unsafe { ImpersonateLoggedOnUser(token.as_raw_handle()) },
            "check build account access",
        )?;
        let accessible = credentials
            .iter()
            .any(|file| std::fs::File::open(file).is_ok());
        checked(unsafe { RevertToSelf() }, "revert credential access check")?;
        ensure!(
            !accessible,
            "build account can read supervisor credentials; repair installer ACLs"
        );
        Ok(())
    })
    .join()
    .map_err(|_| anyhow::anyhow!("credential access check panicked"))??;
    Ok(())
}

pub async fn grant_workspace(path: &Path, account: &Account) -> Result<()> {
    // No /T: inheritable ACLs propagate from this controlled directory. /L keeps
    // an unexpected link from changing ACLs on its destination.
    let system = std::env::var_os("SystemRoot").context("SystemRoot missing")?;
    let status = tokio::process::Command::new(Path::new(&system).join("System32/icacls.exe"))
        .arg(path)
        .args(["/L", "/grant:r", &format!("{}:(OI)(CI)M", account.username)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;
    ensure!(status.success(), "grant build account access to workspace");
    Ok(())
}

pub async fn grant_inputs(path: &Path, account: &Account) -> Result<()> {
    let system = std::env::var_os("SystemRoot").context("SystemRoot missing")?;
    let status = tokio::process::Command::new(Path::new(&system).join("System32/icacls.exe"))
        .arg(path)
        .args([
            "/L",
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-18:(OI)(CI)F",
            "*S-1-5-32-544:(OI)(CI)F",
            &format!("{}:(OI)(CI)RX", account.username),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;
    ensure!(status.success(), "grant read-only artifact input access");
    Ok(())
}

#[allow(clippy::permissions_set_readonly_false)] // Windows-only: clears FILE_ATTRIBUTE_READONLY, not a Unix mode.
pub fn remove_readonly(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    use std::os::windows::fs::MetadataExt;
    // Never traverse a junction, symbolic link or another reparse point.
    if metadata.file_attributes() & 0x400 != 0 {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            remove_readonly(&entry?.path())?;
        }
    } else if metadata.permissions().readonly() {
        let mut permissions = metadata.permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn pipe() -> Result<(OwnedHandle, OwnedHandle)> {
    let mut read = null_mut();
    let mut write = null_mut();
    let attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    checked(
        unsafe { CreatePipe(&mut read, &mut write, &attrs, 0) },
        "create output pipe",
    )?;
    let read = unsafe { owned(read) };
    let write = unsafe { owned(write) };
    checked(
        unsafe { SetHandleInformation(read.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) },
        "protect pipe reader",
    )?;
    Ok((read, write))
}
struct Attributes {
    memory: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    fn new() -> Result<Self> {
        let mut size = 0;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut size);
        }
        ensure!(size > 0, "process attribute size missing");
        let mut value = Self {
            memory: vec![0; size.div_ceil(size_of::<usize>())],
            initialized: false,
        };
        checked(
            unsafe { InitializeProcThreadAttributeList(value.ptr(), 2, 0, &mut size) },
            "initialize process attributes",
        )?;
        value.initialized = true;
        Ok(value)
    }
    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.memory.as_mut_ptr().cast()
    }
    fn handles(&mut self, attribute: u32, values: &[HANDLE]) -> Result<()> {
        checked(
            unsafe {
                UpdateProcThreadAttribute(
                    self.ptr(),
                    0,
                    attribute as usize,
                    values.as_ptr().cast(),
                    std::mem::size_of_val(values),
                    null_mut(),
                    null(),
                )
            },
            "set process handle/job allowlist",
        )
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                DeleteProcThreadAttributeList(self.ptr());
            }
        }
    }
}

struct Child {
    job: OwnedHandle,
    process: OwnedHandle,
    stdout: OwnedHandle,
    stderr: OwnedHandle,
}

// Rust canonical paths use \\?\ syntax. Windows PowerShell 5.1 treats that
// location as a provider-qualified wildcard path and cannot create new files.
// Keep canonical paths for containment checks, but pass the equivalent DOS/UNC
// path to the shell. Verify equivalence before dropping extended-path semantics.
fn shell_directory(cwd: &Path) -> Result<std::path::PathBuf> {
    use std::path::{Component, PathBuf, Prefix};
    let mut components = cwd.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        anyhow::bail!("Windows build directory must be an absolute filesystem path");
    };
    let mut path = match prefix.kind() {
        Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:\\", char::from(drive))),
        Prefix::VerbatimUNC(server, share) => PathBuf::from(r"\\").join(server).join(share),
        Prefix::Disk(_) | Prefix::UNC(_, _) => return Ok(cwd.to_path_buf()),
        _ => anyhow::bail!("unsupported Windows build directory prefix"),
    };
    for component in components {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => path.push(name),
            _ => anyhow::bail!("Windows build directory must be canonical"),
        }
    }
    ensure!(
        std::fs::canonicalize(&path)? == cwd,
        "Windows shell directory differs from the verified workspace"
    );
    Ok(path)
}

fn launch(
    account: &Account,
    script: &Path,
    cwd: &Path,
    environment: BTreeMap<String, String>,
) -> Result<Child> {
    let cwd = shell_directory(cwd)?;
    let token = account.logon()?;
    let job = unsafe { CreateJobObjectW(null(), null()) };
    ensure!(!job.is_null(), "create Windows Job Object");
    let job = unsafe { owned(job) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    checked(
        unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of_val(&limits) as u32,
            )
        },
        "set process tree cleanup",
    )?;
    let (stdout_read, stdout_write) = pipe()?;
    let (stderr_read, stderr_write) = pipe()?;
    let (stdin_read, stdin_write) = pipe()?;
    checked(
        unsafe {
            SetHandleInformation(
                stdin_read.as_raw_handle(),
                HANDLE_FLAG_INHERIT,
                HANDLE_FLAG_INHERIT,
            )
        },
        "inherit empty stdin",
    )?;
    drop(stdin_write);
    let handles = [
        stdin_read.as_raw_handle(),
        stdout_write.as_raw_handle(),
        stderr_write.as_raw_handle(),
    ];
    let jobs = [job.as_raw_handle()];
    let mut attributes = Attributes::new()?;
    attributes.handles(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
    // Atomic job membership at process creation prevents an orphan between
    // CreateProcess and AssignProcessToJobObject if the supervisor is killed.
    attributes.handles(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
    let system = std::env::var_os("SystemRoot").context("SystemRoot missing")?;
    let shell = Path::new(&system).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let app = wide(&shell);
    let mut command = wide(format!(
        "\"{}\" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{}\"",
        shell.display(),
        script.display()
    ));
    let mut environment = environment.into_iter().collect::<Vec<_>>();
    environment.sort_by_key(|(key, _)| key.to_ascii_uppercase());
    let mut env = Vec::new();
    for (key, value) in environment {
        env.extend(wide(format!("{key}={value}")));
    }
    env.push(0);
    let mut startup = STARTUPINFOEXW::default();
    // Use a private station for this logon session; do not inherit SYSTEM's desktop.
    let mut desktop = wide("");
    startup.StartupInfo.lpDesktop = desktop.as_mut_ptr();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = handles[0];
    startup.StartupInfo.hStdOutput = handles[1];
    startup.StartupInfo.hStdError = handles[2];
    startup.lpAttributeList = attributes.ptr();
    let mut process = PROCESS_INFORMATION::default();
    checked(
        unsafe {
            CreateProcessAsUserW(
                token.as_raw_handle(),
                app.as_ptr(),
                command.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
                env.as_ptr().cast(),
                wide(&cwd).as_ptr(),
                &startup.StartupInfo,
                &mut process,
            )
        },
        "start build under its separate Windows account",
    )?;
    let process_handle = unsafe { owned(process.hProcess) };
    let thread = unsafe { owned(process.hThread) };
    drop(thread);
    drop(stdout_write);
    drop(stderr_write);
    drop(stdin_read);
    // Raw pointers in Win32 structs must not be retained across an await.
    drop(attributes);
    drop(token);
    Ok(Child {
        job,
        process: process_handle,
        stdout: stdout_read,
        stderr: stderr_read,
    })
}

pub(super) async fn run(
    account: &Account,
    script: &Path,
    cwd: &Path,
    environment: BTreeMap<String, String>,
    logs: mpsc::Sender<Entry>,
    budget: Arc<AtomicUsize>,
) -> Result<std::process::ExitStatus> {
    let Child {
        job,
        process: process_handle,
        stdout: stdout_read,
        stderr: stderr_read,
    } = launch(account, script, cwd, environment)?;
    let stdout = tokio::spawn(capture(
        tokio::fs::File::from_std(std::fs::File::from(stdout_read)),
        "stdout",
        logs.clone(),
        budget.clone(),
    ));
    let stderr = tokio::spawn(capture(
        tokio::fs::File::from_std(std::fs::File::from(stderr_read)),
        "stderr",
        logs,
        budget,
    ));
    loop {
        let result = unsafe { WaitForSingleObject(process_handle.as_raw_handle(), 0) };
        if result == WAIT_OBJECT_0 {
            break;
        }
        ensure!(result == WAIT_TIMEOUT, "wait for Windows build failed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut exit = 0;
    checked(
        unsafe { GetExitCodeProcess(process_handle.as_raw_handle(), &mut exit) },
        "read Windows exit code",
    )?;
    // Closing the last handle terminates background descendants, also on
    // timeout/cancel when this future is dropped.
    drop(job);
    stdout.await??;
    stderr.await??;
    Ok(std::process::ExitStatus::from_raw(exit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_creates_files_in_a_canonical_workspace() -> Result<()> {
        let directory = std::env::temp_dir()
            .join(format!("p4forge-path-{}", uuid::Uuid::new_v4()))
            .join("workspace with spaces");
        std::fs::create_dir_all(&directory)?;
        let canonical = std::fs::canonicalize(&directory)?;
        let normalized = shell_directory(&canonical)?;
        assert!(
            !normalized
                .as_os_str()
                .to_string_lossy()
                .starts_with(r"\\?\")
        );
        assert_eq!(std::fs::canonicalize(&normalized)?, canonical);
        let shell = Path::new(&std::env::var_os("SystemRoot").context("SystemRoot missing")?)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let output = std::process::Command::new(shell)
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "$ErrorActionPreference='Stop'; Set-Content generated.txt ok; if ((Get-Content generated.txt) -ne 'ok') { throw 'roundtrip failed' }"])
            .current_dir(&normalized)
            .output()?;
        let generated = std::fs::read_to_string(directory.join("generated.txt"));
        std::fs::remove_dir_all(directory.parent().unwrap())?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(generated?.trim(), "ok");
        Ok(())
    }
}
