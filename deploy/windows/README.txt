P4Forge Windows x64 Agent preview

This package is cross-compiled and statically checked. Installation and authenticated
server connection were verified on DESKTOP-6N6U52L. Complete the smoke/cancel/restart
checks below before using it for project builds.

Requires: dedicated Windows x64 build machine (Windows 10 / Server 2016+),
64-bit Windows PowerShell 5.1, official p4.exe, trusted HTTPS access to
https://p4forge.zenogrid.co.kr:8444/ and TCP p4forge.zenogrid.co.kr:1666.
Install project compilers/SDKs separately. This package contains no credentials.
The configured Perforce server uses Unicode mode. P4Forge connection charset and
the ticket preflight use utf8 so Windows can sync Korean file names correctly.
If installation fails, share installation-result.txt, perforce-install-check.json
and windows-host-report.json; credential file contents are not included in them.

Before installing, extract the ZIP and double-click Diagnose.cmd on the Windows
host. It runs Test-Host.ps1 and creates windows-host-report.json beside the script.
This checks DNS, HTTPS/TCP ports, task state, file presence and tool availability;
it does not install/start the Agent or read credentials or build logs. Administrator
rights improve visibility of protected installed files but are not required for
network checks. Diagnose.cmd uses a process-only PowerShell execution policy;
organization Group Policy still takes precedence.

Download from the same LAN/VPN as the P4Forge server:
https://p4forge.zenogrid.co.kr:8444/downloads/p4forge-windows-x64-preview.zip

1. In P4Forge, select project root, register an Agent with OS windows,
   architecture x86_64, pool default. Save its one-time agent-token file.
2. Obtain a host-independent (-a) P4 login ticket for the read account p4forge.
   Keep agent-token and p4tickets private and transfer them directly to this host.
3. From an elevated 64-bit PowerShell, in the extracted package directory:
   .\Install.ps1 -AgentTokenFile C:\Private\agent-token -P4TicketsFile C:\Private\p4tickets -P4Exe C:\Tools\p4.exe
   Installation does not start the task automatically unless -Start is supplied.
4. C:\P4Forge\Test-Installation.ps1
   Start-ScheduledTask -TaskName P4Forge-Agent
5. Replace SOURCE_ROOT_UUID in windows-smoke.toml with the root-source UUID.
   Create a native pipeline, save/publish this TOML and run submitted CL 6.
   Verify generate and consume succeed on the same Agent and shared workspace.
6. Repeat with a sleep in the first job. Cancel in the UI, then check no child
   process remains. Repeat with Stop-ScheduledTask/Start-ScheduledTask: the old
   run must fail after lease expiry without automatic rerun.

The supervisor task runs as SYSTEM; build commands run as the separate standard
local account p4forge-build. Credential/state directories allow SYSTEM and local
Administrators only. Startup checks that the build account cannot read credential
files and is not an Administrator. The process is born inside a Windows Job Object
with KILL_ON_JOB_CLOSE; only stdin/stdout/stderr handles are inherited. Every build
gets an allowlisted environment, with no Agent token or P4 tickets.

Persistent files: C:\P4Forge\secrets (private), state\agent.log, state\inflight.json.
The installer refuses to replace an existing account, task or installation.
For an existing installation, drain the Agent in P4Forge, wait for builds to finish,
extract the latest public ZIP and run Update-ThisMachine.cmd. It requests elevation,
verifies package hashes, stops the task, backs up/replaces the Agent and diagnostic
scripts, then restores its previous running state. Credentials/account/task settings
are preserved. On failure it attempts to restore the previous executable. Send
update-result.txt and windows-host-report.json before releasing the drain control.
To pause new work, use P4Forge's Agent drain control. Stop-ScheduledTask terminates
the supervisor and its build process tree. No service ports are opened by this
package. To update a P4 ticket, drain/stop the task, replace secrets\p4tickets as an
Administrator while preserving its ACL, then start the task.

Build commands use Windows PowerShell -NoProfile with a process-only execution
policy (Bypass). Machine/user policy is not changed and Group Policy is respected.
See https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_execution_policies
Startup and terminal Agent errors are recorded in state\agent.log.
External command exit codes are
checked after each configured command. Machine PATH is available. Rustup/SDKs
installed only in another person's profile must be configured for the dedicated
build account or installed machine-wide. Logs are limited to 1 MiB per workspace.
The imported root workflows also need publisher credentials, view bindings and
cross-pipeline changelist propagation; this package does not enable those drafts.

Self patch:
The first Update-ThisMachine.cmd installs P4Forge-Updater, a separate SYSTEM task.
After that, a P4Forge global administrator can select a published release under
Build Agents and choose Self patch. Running builds finish before patching. The
worker checks the pinned SHA-256 and size, saves the previous binary, replaces
only p4forge-agent.exe and waits up to 90 seconds for a server health handshake.
If the new Agent cannot connect, the previous binary is restored automatically.
The original drain setting is restored only after the worker result is received.
Credentials, compiler/SDK installation and the updater itself are not patched.
The journal survives task/OS restart; state\self-patch is SYSTEM/Admin-only.
Do not manually replace files during a pending patch. The previous binary remains
at state\self-patch\previous.bin. Updater protocol upgrades use the manual package.

Implementation references:
https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw
https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects
