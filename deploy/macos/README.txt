P4Forge native macOS Agent (preview)

Install target: law80's Mac mini, Apple Silicon. One dedicated slot per Mac.
The supervisor runs as root via launchd; scripts run as hidden non-login
_p4forge UID/GID 10003. An EOF watchdog retains the state lock and kills
remaining processes belonging to that UID after supervisor termination.
The helper enumerates individual live processes and excludes its own PID;
macOS POSIX broadcast kill(-1) includes the caller and is not used.
See Apple's kill(2) contract:
https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/kill.2.html

Fresh install (an existing account/installation is refused):
sudo sh install.sh /absolute/agent-token /absolute/p4tickets \
  /absolute/p4 /absolute/rust-toolchain

The installer copies binaries/toolchain to root-owned /Library/P4Forge,
stores credentials under secrets (0700, files 0600), and starts
/Library/LaunchDaemons/kr.co.zenogrid.p4forge.agent.plist.
No credentials are included in this package. No write-capable P4 ticket is used.
The bundled plist targets https://p4forge.zenogrid.co.kr:8444/.
Register a macos/aarch64/default Agent in the intended project first.

Status: sudo launchctl print system/kr.co.zenogrid.p4forge.agent
Logs: sudo tail -n 30 /Library/P4Forge/logs/agent.log
Stop: sudo launchctl bootout system/kr.co.zenogrid.p4forge.agent
Start: sudo launchctl bootstrap system /Library/LaunchDaemons/kr.co.zenogrid.p4forge.agent.plist

macos-smoke.toml uses root-source //streams/root at a manually chosen CL.
It verifies UID/credentials, Rust tools, source and HOME persistence,
and termination of a detached child between stages. Also validate timeout,
cancel, and SIGKILL/restart before relying on production builds.

macos-lifecycle.toml is an intentionally long-running validation pipeline.
For timeout verification, lower the hold job timeout to 3 seconds.
For cancellation, wait for lifecycle-hold-started before using Cancel.
For restart verification, SIGKILL only this Agent launchd service while hold is
running, then verify lost/skipped statuses and cleanup before a fresh smoke run.
These scenarios and a post-restart smoke run passed on the target Mac mini.

Build scripts have standard local-user privileges, not a VM sandbox.
Do not put the _p4forge account into admin, Docker, or other privileged groups.
ECR/Docker/Unity signing credentials and generated-file publication are separate
capabilities; this installation does not configure or grant those permissions.

Update/recover an existing installation: sudo sh update.sh
Drain active work before an update. Existing accounts, tokens and Rust tools
are retained. Pre-start process cleanup tests must pass before service startup.

Self patch: update.sh also installs a separate root launchd service named
kr.co.zenogrid.p4forge.updater. After this one-time bootstrap, use Build Agents
in P4Forge to select a release and apply it. Current builds finish first.
Only the Agent binary is replaced, after SHA-256, size and compatibility checks.
The worker requires a server health handshake within 90 seconds or restores the
previous binary. The original drain setting is retained. Credentials, toolchains
and the updater itself are not patched. The protected state/self-patch journal
survives service/OS restarts, and previous.bin remains available for recovery.
Worker status: sudo launchctl print system/kr.co.zenogrid.p4forge.updater
Worker log: sudo tail -n 30 /Library/P4Forge/logs/updater.log
