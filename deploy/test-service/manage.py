#!/usr/bin/env python3
"""Operator-controlled Compose rehearsal; never grants Docker access to Agents."""
import argparse
import contextlib
import fcntl
import hashlib
import html
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
STATE_DIR = ROOT / ".local" / "deployment-demo"
HEALTH_URL = "https://p4forge.zenogrid.co.kr:8444/deployment-demo/healthz"
SERVICE = "p4forge-deployment-demo"


def encoded(value):
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode()


def atomic_json(path, value):
    temporary = path.with_suffix(".next")
    with temporary.open("wb") as output:
        output.write(encoded(value))
        output.flush()
        os.fsync(output.fileno())
    temporary.replace(path)


def release_files(version, message):
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}", version):
        raise ValueError("version must contain 1–64 letters, digits, dots, underscores or hyphens")
    if not message.strip() or len(message) > 500:
        raise ValueError("message must contain 1–500 characters")
    payload = {"service": SERVICE, "status": "ok", "version": version, "message": message}
    release = hashlib.sha256(encoded(payload)).hexdigest()
    payload["release"] = release
    page = f'''<!doctype html><html lang="ko"><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>P4Forge · 배포 테스트</title>
<style>body{{background:#101827;color:#eef3fa;font:16px/1.6 system-ui;margin:0;padding:8vh 24px}}
main{{max-width:720px;margin:auto}}small{{color:#80a8d2;letter-spacing:.1em}}h1{{font-size:38px;line-height:1.2}}
section{{background:#1a2638;border:1px solid #30435d;border-radius:16px;padding:28px;margin:28px 0}}
.status{{color:#73e0af}}code{{overflow-wrap:anywhere;color:#afc7e2}}a{{color:#94c6ff}}p{{margin:12px 0}}</style>
<main><small>P4FORGE / SELF-HOSTED</small><h1>배포 테스트 서비스</h1>
<p>Docker Compose로 배포한 별도 테스트 서비스입니다.</p><section>
<p class="status">● 정상 응답</p><h2>{html.escape(version)}</h2><p>{html.escape(message)}</p>
<p>릴리스 SHA-256<br><code>{release}</code></p></section>
<p><a href="/deployment-demo/healthz">상태 확인 JSON</a> · <a href="/">P4Forge</a></p>
<p>표시된 버전과 SHA-256으로 배포 전환 및 이전 버전 복구를 확인합니다.</p></main></html>'''
    return release, {"health.json": encoded(payload), "index.html": page.encode()}


def validate_release(directory):
    if directory.is_symlink() or not re.fullmatch(r"[a-f0-9]{64}", directory.name):
        raise ValueError("invalid release directory")
    health = directory / "health.json"
    if health.is_symlink() or health.stat().st_size > 4096:
        raise ValueError("invalid release metadata")
    payload = json.loads(health.read_bytes())
    release, expected = release_files(payload["version"], payload["message"])
    if release != directory.name:
        raise ValueError("release digest mismatch")
    for name, content in expected.items():
        path = directory / name
        if path.is_symlink() or path.read_bytes() != content:
            raise ValueError(f"release file mismatch: {name}")
    return payload


def prepare_release(version, message):
    release, files = release_files(version, message)
    directory = STATE_DIR / "releases" / release
    if not directory.exists():
        directory.mkdir(parents=True)
        directory.chmod(0o755)
        for name, content in files.items():
            path = directory / name
            path.write_bytes(content)
            path.chmod(0o644)
    validate_release(directory)
    return release


def compose(release, *arguments, check=None):
    directory = STATE_DIR / "releases" / release
    validate_release(directory)
    environment = os.environ.copy()
    environment["P4FORGE_DEMO_RELEASE_DIR"] = str(directory)
    command = ["docker", "compose", "-p", SERVICE, "--env-file", "/dev/null", "-f", str(ROOT / "compose.demo.yaml"), *arguments]
    # Never execute pipeline text or use its environment. The executor supplies
    # only fixed arguments and a checksum-verified release directory.
    with tempfile.TemporaryFile() as output:
        process = subprocess.Popen(command, cwd=ROOT, env=environment, stdout=output, stderr=output, start_new_session=True)
        try:
            deadline = time.monotonic() + 60
            while process.poll() is None:
                if check:
                    check()
                if time.monotonic() >= deadline:
                    raise RuntimeError("Compose operation timed out; target needs recovery verification")
                time.sleep(0.2)
            if process.returncode:
                raise RuntimeError(f"Compose failed (exit {process.returncode}); target needs recovery verification")
            if check:
                check()
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()


def check_health(release, timeout=30, check=None):
    expected = validate_release(STATE_DIR / "releases" / release)
    deadline = time.monotonic() + timeout
    last_error = "no response"
    while True:
        if check:
            check()
        try:
            request = urllib.request.Request(HEALTH_URL, headers={"Cache-Control": "no-cache"})
            with urllib.request.urlopen(request, timeout=4) as response:
                if response.geturl() != HEALTH_URL or response.status != 200:
                    raise ValueError("unexpected health response or redirect")
                actual = json.loads(response.read(4097))
            if actual != expected:
                raise ValueError("health response does not match the complete target release")
            if check:
                check()
            return actual
        except (OSError, ValueError) as error:
            last_error = str(error)
        if time.monotonic() >= deadline:
            raise RuntimeError(f"HTTPS health check failed: {last_error}")
        time.sleep(1)


def switch(state, target, action):
    validate_release(STATE_DIR / "releases" / target)
    previous = state.get("current")
    pending = {**state, "pending": target, "operation": action, "status": "checking"}
    atomic_json(STATE_DIR / "state.json", pending)
    try:
        compose(target, "up", "-d", "--no-deps", "--wait", "--wait-timeout", "30", "test-service")
        result = check_health(target)
    except Exception:
        atomic_json(STATE_DIR / "state.json", {**pending, "status": "recovery_required"})
        raise
    completed = {"current": target, "previous": previous if target != previous else state.get("previous"),
                 "pending": None, "status": "healthy", "checked_at": int(time.time()), "operation": action}
    atomic_json(STATE_DIR / "state.json", completed)
    print(json.dumps({"state": completed, "health": result}, ensure_ascii=False))


@contextlib.contextmanager
def locked():
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    with (STATE_DIR / "operation.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    actions = parser.add_subparsers(dest="action", required=True)
    apply = actions.add_parser("apply")
    apply.add_argument("--version", required=True)
    apply.add_argument("--message", required=True)
    actions.add_parser("rollback")
    actions.add_parser("retry")
    actions.add_parser("status")
    args = parser.parse_args()
    if args.action != "status" and (STATE_DIR / "executor" / "policy.json").exists():
        raise RuntimeError("managed by P4Forge: create and approve a deployment or rollback Run in the UI")
    with locked():
        path = STATE_DIR / "state.json"
        state = json.loads(path.read_text()) if path.exists() else {}
        if args.action == "status":
            target = state.get("pending") or state.get("current")
            print(json.dumps({"state": state, "health": check_health(target, timeout=0) if target else None}, ensure_ascii=False))
        elif args.action == "apply":
            if state.get("pending"):
                raise RuntimeError("unresolved deployment; retry the same release or rollback to the last healthy release")
            switch(state, prepare_release(args.version, args.message), "apply")
        elif args.action == "retry":
            if not state.get("pending"):
                raise RuntimeError("no pending deployment to retry")
            switch(state, state["pending"], "retry")
        else:
            target = state.get("current") if state.get("pending") else state.get("previous")
            if not target:
                raise RuntimeError("no successful release available for rollback")
            switch(state, target, "rollback")


if __name__ == "__main__":
    main()
