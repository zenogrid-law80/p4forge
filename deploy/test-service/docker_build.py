"""Perforce snapshot -> bounded Docker build. Pipeline values are never shell code."""
import base64
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import uuid

import manage

OPERATION = "p4forge-docker:build"
ARTIFACT = {"name": "docker-image.json", "path": ".p4forge/docker-image.json"}
PREFIX = "BUILD_"
OPTIONS = {PREFIX + name for name in ("CONTEXT", "DOCKERFILE", "PLATFORM", "IMAGE", "TARGET")}


def process(command, *, check=lambda: None, data=None, timeout=60, cwd=None):
    """Bounded tail, no inherited stdin, and cancellation of the entire child group."""
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(("P4", "DOCKER_", "BUILDX_", "BUILDKIT_"))}
    tail = bytearray()
    deadline = time.monotonic() + timeout
    with tempfile.TemporaryFile() as stdin, selectors.DefaultSelector() as reader:
        if data is not None:
            stdin.write(manage.encoded(data))
            stdin.seek(0)
        check()
        owner_read, owner_write = os.pipe()
        try:
            child = subprocess.Popen([sys.executable, str(Path(__file__).with_name("process_guard.py")), str(owner_read), *command],
                                     stdin=stdin, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                     cwd=cwd or "/", env=environment, start_new_session=True, pass_fds=(owner_read,))
        except Exception:
            os.close(owner_write)
            raise
        finally:
            os.close(owner_read)
        reader.register(child.stdout, selectors.EVENT_READ)
        try:
            while reader.get_map():
                check()
                if time.monotonic() >= deadline:
                    raise RuntimeError("operation timed out")
                for key, _ in reader.select(0.2):
                    chunk = os.read(key.fileobj.fileno(), 8192)
                    if chunk:
                        tail.extend(chunk)
                        del tail[:-10000]
                    else:
                        reader.unregister(key.fileobj)
            child.wait(timeout=5)
            check()
            output = tail.decode("utf-8", errors="replace")
            if child.returncode:
                raise RuntimeError(f"operation failed (exit {child.returncode})\n{output}")
            return output
        finally:
            os.close(owner_write)
            # Also kill descendants if the foreground process exited first.
            try:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=3)
                finally:
                    try:
                        os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            except ProcessLookupError:
                pass
            child.wait()
            child.stdout.close()


def helper(config, tickets, body, **kwargs):
    return json.loads(process([config["helper"], config["p4_binary"], str(tickets)], data=body, **kwargs))


def refresh(api, config, directory):
    grants = api.call("agent/credentials", None)
    if len(grants) > 2:
        raise ValueError("too many Perforce grants")
    health = []
    for grant in grants:
        remaining = 0
        candidate = directory / "read.candidate"
        try:
            if (grant["role"] != "read" or grant["connection"] != config["connection"]
                    or grant["server_id"] != config["server_id"]):
                raise ValueError("grant outside local read policy")
            ticket = grant.get("tickets")
            if not isinstance(ticket, str) or not 1 <= len(ticket.encode()) <= 16384:
                raise ValueError("invalid ticket size")
            with candidate.open("w", encoding="utf-8") as file:
                os.chmod(candidate, 0o600)
                file.write(ticket)
            result = helper(config, candidate, {"operation": "check", "connection": config["connection"],
                                              "server_id": config["server_id"]}, timeout=30)
            remaining = result["remaining_seconds"]
            candidate.replace(directory / "read.tickets")
        except Exception:
            pass  # Never copy credential diagnostics into executor logs.
        finally:
            candidate.unlink(missing_ok=True)
        health.append({"id": grant["id"], "generation": grant["generation"],
                       "ready": remaining > 300, "remaining_seconds": remaining})
    api.call("agent/credentials/status", health)
    return any(item["ready"] for item in health)


def relative(value):
    if (not isinstance(value, str) or not value or len(value) > 240 or value.startswith("-")
            or any(c in value for c in "\\\n\r\0:") or PurePosixPath(value).is_absolute()
            or ".." in PurePosixPath(value).parts):
        raise ValueError("Docker paths must stay inside the checked-out source")
    return value


def validate(assignment, config):
    jobs = assignment.get("workspace_jobs", [])
    if (len(jobs) != 1 or assignment.get("publish_generated") or assignment.get("artifact_inputs")
            or assignment.get("artifacts", []) not in ([], [ARTIFACT])):
        raise ValueError("Docker build requires one pipeline-workspace job and optional docker-image.json")
    job = jobs[0]["job"]
    if (job["script"] != [OPERATION] or job["shell"] != "sh" or job["working_directory"] != "."
            or job["runner"] != {"os": "macos", "arch": "aarch64", "pool": "compose-demo", "docker": "linux"}
            or not 30 <= job["timeout_seconds"] <= 3600):
        raise ValueError("unsupported Docker build command, runner or timeout")
    source = assignment["source"]
    if source["connection"] != config["connection"] or source["server_id"] != config["server_id"]:
        raise ValueError("source outside executor's local Perforce read policy")
    env = job.get("environment", {})
    if set(env) - OPTIONS or any(not isinstance(value, str) for value in env.values()):
        raise ValueError("only documented P4FORGE_DOCKER options are accepted")
    options = {"context": relative(env.get(PREFIX + "CONTEXT", ".")),
               "dockerfile": relative(env.get(PREFIX + "DOCKERFILE", "Dockerfile")),
               "platform": env.get(PREFIX + "PLATFORM", "linux/amd64"),
               "image": env.get(PREFIX + "IMAGE", "server"),
               "target": env.get(PREFIX + "TARGET", "")}
    if options["platform"] not in ("linux/amd64", "linux/arm64"):
        raise ValueError("choose one supported Linux platform")
    if not re.fullmatch(r"[a-z0-9][a-z0-9_.-]{0,63}", options["image"]):
        raise ValueError("image must be a short lowercase name without registry or tag")
    if options["target"] and not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,63}", options["target"]):
        raise ValueError("invalid Docker target stage")
    return options


def context_paths(source, options):
    # No source symlink, device or hardlink can expose host files to Docker.
    for directory, dirs, files in os.walk(source, followlinks=False):
        for name in dirs + files:
            info = (Path(directory) / name).lstat()
            if not (stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode)) or (stat.S_ISREG(info.st_mode) and info.st_nlink != 1):
                raise ValueError("Docker source must contain ordinary directories and files only")
    context = source / options["context"]
    dockerfile = source / options["dockerfile"]
    if not context.is_dir() or not dockerfile.is_file():
        raise ValueError("Docker context directory or Dockerfile is missing in the selected Perforce View")
    if not dockerfile.resolve().is_relative_to(context.resolve()):
        raise ValueError("Dockerfile must be inside the chosen build context")
    return context, dockerfile


def upload(api, assignment, metadata):
    if not assignment.get("artifacts"):
        return
    data = manage.encoded(metadata)
    prefix = f"agent/attempts/{assignment['attempt_id']}/artifacts"
    files = [{**ARTIFACT, "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}]
    entries = api.call(prefix, {"fence": assignment["fence"], "files": files})
    if len(entries) != 1 or entries[0]["name"] != ARTIFACT["name"]:
        raise ValueError("artifact manifest mismatch")
    entry = entries[0]
    uuid.UUID(entry["id"])
    offset = entry["uploaded_size"]
    if not 0 <= offset <= len(data):
        raise ValueError("invalid artifact offset")
    if entry["status"] != "verified":
        result = api.call(prefix + "/" + entry["id"], {"fence": assignment["fence"], "offset": offset,
                           "data": base64.b64encode(data[offset:]).decode()})
        if result["status"] != "verified" or result["uploaded_size"] != len(data):
            raise ValueError("artifact verification failed")


def workspace(directory, assignment):
    return directory / "builds" / str(uuid.UUID(assignment["attempt_id"]))


def cleanup(config, directory, assignment):
    path = workspace(directory, assignment)
    # Connection is always local policy, never recovered network-provided values.
    helper(config, directory / "read.tickets", {"operation": "cleanup", "connection": config["connection"],
           "attempt_id": assignment["attempt_id"]}, timeout=15)
    if path.exists():
        shutil.rmtree(path)


def build(config, directory, assignment, options, guard):
    path = workspace(directory, assignment)
    source = path / "source"
    source.mkdir(parents=True, exist_ok=False, mode=0o700)
    helper(config, directory / "read.tickets", {"operation": "checkout", "snapshot": assignment["source"],
           "attempt_id": assignment["attempt_id"], "directory": str(source)}, check=guard.check, timeout=300)
    context, dockerfile = context_paths(source, options)
    tag = f"p4forge-build/{options['image']}:cl-{assignment['source']['changelist']}-{uuid.UUID(assignment['attempt_id']).hex}"
    iidfile = path / "image-id"
    command = ["docker", "--context", "desktop-linux", "buildx", "build", "--builder", "desktop-linux",
               "--platform", options["platform"], "--load", "--progress", "plain", "--tag", tag,
               "--iidfile", str(iidfile), "--file", str(dockerfile),
               "--label", "kr.co.zenogrid.p4forge.run=" + str(uuid.UUID(assignment["run_id"]))]
    if options["target"]:
        command += ["--target", options["target"]]
    output = process(command + [str(context)], check=guard.check, timeout=3600, cwd=source)
    digest = iidfile.read_text().strip()
    if not re.fullmatch(r"sha256:[a-f0-9]{64}", digest):
        raise ValueError("Docker did not return a valid image digest")
    return {"schema_version": 1, "run_id": assignment["run_id"], "attempt_id": assignment["attempt_id"],
            "changelist": assignment["source"]["changelist"], "image": tag, "image_id": digest,
            "platform": options["platform"], "context": options["context"], "dockerfile": options["dockerfile"],
            "target": options["target"], "registry_pushed": False}, output
