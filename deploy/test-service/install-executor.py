#!/usr/bin/env python3
"""Install the already-provisioned Compose/Docker executor as this user's LaunchAgent."""
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys

import executor
import manage

LABEL = "kr.co.zenogrid.p4forge.compose-demo"


def main():
    if sys.platform != "darwin" or os.getuid() == 0:
        raise RuntimeError("install as the Mac user running Docker Desktop, not root")
    policy = executor.private_json(executor.DIRECTORY / "policy.json")
    executor.private_json(executor.DIRECTORY / "token.json")
    if executor.JOURNAL.exists():
        raise RuntimeError("wait for the current attempt and recovery to finish before reinstalling")
    if policy.get("docker_build"):
        for key in ("helper", "p4_binary"):
            binary = Path(policy["docker_build"][key])
            if not binary.is_absolute() or not binary.is_file() or not os.access(binary, os.X_OK):
                raise RuntimeError(f"docker_build.{key} must be an installed absolute executable")
    docker = shutil.which("docker")
    if not docker:
        raise RuntimeError("Docker CLI is required")
    destination = Path.home() / "Library" / "LaunchAgents" / (LABEL + ".plist")
    destination.parent.mkdir(exist_ok=True)
    content = {
        "Label": LABEL,
        "ProgramArguments": [sys.executable, str(Path(__file__).with_name("executor.py").resolve())],
        "WorkingDirectory": str(manage.ROOT),
        "EnvironmentVariables": {"PATH": str(Path(docker).parent) + ":/usr/bin:/bin:/usr/sbin:/sbin", "PYTHONDONTWRITEBYTECODE": "1"},
        "RunAtLoad": True, "KeepAlive": True, "ThrottleInterval": 10, "ExitTimeOut": 20,
        "Umask": 0o077,
        "StandardOutPath": str(executor.DIRECTORY / "executor.log"),
        "StandardErrorPath": str(executor.DIRECTORY / "executor.log"),
    }
    temporary = destination.with_suffix(".next")
    temporary.write_bytes(plistlib.dumps(content))
    temporary.chmod(0o600)
    temporary.replace(destination)
    target = f"gui/{os.getuid()}/{LABEL}"
    present = subprocess.run(["launchctl", "print", target], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0
    if present:
        subprocess.run(["launchctl", "bootout", target], check=True)
    subprocess.run(["launchctl", "bootstrap", f"gui/{os.getuid()}", str(destination)], check=True)
    print(f"Installed {LABEL}; protocol 8, fixed Compose operations and optional Docker build, no host shell execution")


if __name__ == "__main__":
    main()
