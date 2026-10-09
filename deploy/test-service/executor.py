#!/usr/bin/env python3
"""Compose deployment and Docker build executor; protocol 8, never host shell scripts."""
import base64
import fcntl
import hashlib
import json
import os
import re
import signal
import stat
import subprocess
import threading
import time
import urllib.error
import urllib.request
import uuid

import manage
import docker_build

SERVER = "https://p4forge.zenogrid.co.kr:8444/"
DIRECTORY = manage.STATE_DIR / "executor"
JOURNAL = DIRECTORY / "attempt.json"
STOP = threading.Event()
POOL = "compose-demo"


class ApiError(RuntimeError):
    def __init__(self, code):
        self.code = code
        super().__init__(f"P4Forge request failed (HTTP {code})")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


class Api:
    def __init__(self, token):
        self.token = token

    def call(self, path, body):
        if not path.startswith("agent/") or ".." in path or "?" in path:
            raise ValueError("invalid API path")
        request = urllib.request.Request(SERVER + path, data=manage.encoded(body), headers={
            "Authorization": "Bearer " + self.token, "Content-Type": "application/json"})
        # No redirect can forward the private bearer credential elsewhere.
        try:
            with urllib.request.build_opener(NoRedirect()).open(request, timeout=5) as response:
                data = response.read(512 * 1024 + 1)
                if len(data) > 512 * 1024:
                    raise ValueError("oversized API response")
                return json.loads(data)
        except urllib.error.HTTPError as error:
            raise ApiError(error.code) from None


def docker_capability():
    # Inherit the same account/environment used by manage.compose, without mutations.
    try:
        result = subprocess.run(["docker", "--context", "desktop-linux", "info", "--format", "{{.OSType}}"],
                                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=False)
    except FileNotFoundError:
        return {"state": "cli_missing", "engine_os": None}
    except (OSError, subprocess.TimeoutExpired):
        return {"state": "check_failed", "engine_os": None}
    if result.returncode:
        return {"state": "engine_unavailable", "engine_os": None}
    engine = result.stdout.strip()
    if engine not in (b"linux", b"windows"):
        return {"state": "check_failed", "engine_os": None}
    return {"state": "available", "engine_os": engine.decode("ascii")}


def private_json(path):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError("executor files must be owner-only regular files")
    return json.loads(path.read_text())


def validate_assignment(assignment, policy, permit):
    for key in ("project_id", "pipeline_id", "environment_id"):
        if permit.get(key) != policy[key]:
            raise ValueError("deployment is outside this executor's fixed target")
    if permit.get("run_id") != assignment["run_id"] or permit.get("artifact_inputs") != assignment.get("artifact_inputs"):
        raise ValueError("deployment identity or frozen inputs changed")
    if not re.fullmatch(r"[a-f0-9]{64}", permit.get("input_hash", "")):
        raise ValueError("missing input digest")
    jobs = assignment.get("workspace_jobs", [])
    if len(jobs) != 2 or assignment.get("publish_generated") or assignment.get("artifacts"):
        raise ValueError("executor accepts only its two fixed deployment operations")
    for assigned, operation in zip(jobs, ("apply", "health")):
        job = assigned["job"]
        if (job["name"] != "compose-" + operation or job["stage"] != operation
                or job["script"] != ["p4forge-compose-demo:" + operation]
                or job["shell"] != "sh" or job["working_directory"] != "."
                or job.get("environment") or job["runner"] != {"os": "macos", "arch": "aarch64", "pool": POOL}
                or job["timeout_seconds"] != (120 if operation == "apply" else 60)):
            raise ValueError("arbitrary commands, environment or runner settings are forbidden")
    inputs = assignment.get("artifact_inputs", [])
    if len(inputs) != 1 or inputs[0]["name"] != "demo-release.json" or not 1 <= inputs[0]["size"] <= 4096:
        raise ValueError("exactly one bounded demo-release.json input is required")
    uuid.UUID(inputs[0]["artifact_id"])
    if not re.fullmatch(r"[a-f0-9]{64}", inputs[0]["sha256"]):
        raise ValueError("invalid artifact digest")


def decode_release(value, artifact):
    if value["offset"] != 0:
        raise ValueError("incorrect artifact offset")
    data = base64.b64decode(value["data"], validate=True)
    if len(data) != artifact["size"] or hashlib.sha256(data).hexdigest() != artifact["sha256"]:
        raise ValueError("artifact content does not match the approved digest")
    payload = json.loads(data)
    if not isinstance(payload, dict) or set(payload) != {"schema_version", "version", "message"} or type(payload["schema_version"]) is not int or payload["schema_version"] != 1:
        raise ValueError("unsupported release manifest")
    if not isinstance(payload["version"], str) or not isinstance(payload["message"], str):
        raise ValueError("release fields must be strings")
    manage.release_files(payload["version"], payload["message"])
    return payload


class Guard:
    def __init__(self, api, assignment, timeout=180):
        self.api = api
        self.assignment = assignment
        self.deadline = time.monotonic()
        self.hard_deadline = time.monotonic() + timeout
        self.canceled = threading.Event()
        self.done = threading.Event()
        self.thread = threading.Thread(target=self.loop, daemon=True)

    def pulse(self):
        started = time.monotonic()
        response = self.api.call(f"agent/attempts/{self.assignment['attempt_id']}/heartbeat", {"fence": self.assignment["fence"]})
        if response["cancel_requested"]:
            self.canceled.set()
        self.deadline = started + min(response["lease_seconds"], 60) - 5

    def loop(self):
        while not self.done.wait(5):
            try:
                self.pulse()
            except ApiError as error:
                if 400 <= error.code < 500:
                    self.canceled.set()
            except (OSError, ValueError):
                pass  # A failed request never extends the last confirmed lease.

    def check(self):
        if STOP.is_set() or self.canceled.is_set() or time.monotonic() >= min(self.deadline, self.hard_deadline):
            raise RuntimeError("executor stopped, canceled, timed out or lost its lease")

    def __enter__(self):
        self.pulse()
        self.check()
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.done.set()
        if self.thread.is_alive():
            self.thread.join(timeout=6)


def authorize(api, assignment, policy):
    permit = api.call(f"agent/attempts/{assignment['attempt_id']}/deployment", {"fence": assignment["fence"]})
    validate_assignment(assignment, policy, permit)
    return permit


def step(api, assignment, job, status):
    return api.call(f"agent/attempts/{assignment['attempt_id']}/step", {
        "fence": assignment["fence"], "job_id": job["id"], "status": status,
        "exit_code": None if status == "running" else 0, "reason": ""})


def finish(api, journal):
    assignment = journal["assignment"]
    prefix = f"agent/attempts/{assignment['attempt_id']}/"
    if journal.get("kind") == "docker-build" and journal.get("cleanup_pending"):
        policy = private_json(DIRECTORY / "policy.json")
        docker_build.cleanup(policy["docker_build"], DIRECTORY, assignment)
        journal["cleanup_pending"] = False
        manage.atomic_json(JOURNAL, journal)
    if not journal.get("log_ack"):
        api.call(prefix + "logs", journal["log"])
        journal["log_ack"] = True
        manage.atomic_json(JOURNAL, journal)
    # Exact payload persisted before sending: a lost reply is safe to retry.
    outcome = api.call(prefix + "complete", journal["completion"])
    journal["server_status"] = outcome["status"]
    if journal.get("kind") == "docker-build":
        manage.atomic_json(DIRECTORY / "last-build.json", journal)
    elif outcome["status"] == "succeeded" and journal["completion"]["status"] == "succeeded":
        manage.atomic_json(manage.STATE_DIR / "state.json", journal["successful_state"])
    else:
        state_path = manage.STATE_DIR / "state.json"
        state = json.loads(state_path.read_text()) if state_path.exists() else {}
        manage.atomic_json(state_path, {**state, "status": "recovery_required"})
    manage.atomic_json(DIRECTORY / "last-result.json", journal)
    JOURNAL.unlink()


def execute(api, policy, assignment):
    jobs = assignment.get("workspace_jobs", [])
    if not jobs or jobs[0]["job"].get("script") != ["p4forge-compose-demo:apply"]:
        return execute_build(api, policy, assignment)
    uuid.UUID(assignment["attempt_id"])
    journal = {"phase": "running", "assignment": assignment}
    manage.atomic_json(JOURNAL, journal)
    result = {"run_id": assignment["run_id"]}
    try:
        with Guard(api, assignment) as guard:
            permit = authorize(api, assignment, policy)
            artifact = assignment["artifact_inputs"][0]
            downloaded = api.call(f"agent/attempts/{assignment['attempt_id']}/inputs/{artifact['artifact_id']}",
                                  {"fence": assignment["fence"], "offset": 0})
            payload = decode_release(downloaded, artifact)
            target = manage.prepare_release(payload["version"], payload["message"])
            state_path = manage.STATE_DIR / "state.json"
            state = json.loads(state_path.read_text()) if state_path.exists() else {}
            previous = state.get("current")
            apply_job, health_job = assignment["workspace_jobs"]
            step(api, assignment, apply_job, "running")
            guard.check()
            authorize(api, assignment, policy)
            pending = {**state, "pending": target, "status": "checking", "run_id": assignment["run_id"]}
            manage.atomic_json(state_path, pending)
            manage.compose(target, "up", "-d", "--no-deps", "--wait", "--wait-timeout", "30", "test-service", check=guard.check)
            step(api, assignment, apply_job, "succeeded")
            step(api, assignment, health_job, "running")
            result.update({"input_hash": permit["input_hash"], "artifact": artifact,
                           "health": manage.check_health(target, check=guard.check)})
            authorize(api, assignment, policy)
            guard.check()
            step(api, assignment, health_job, "succeeded")
            journal["successful_state"] = {"current": target, "previous": previous if target != previous else state.get("previous"),
                "pending": None, "status": "healthy", "checked_at": int(time.time()), "run_id": assignment["run_id"], "operation": "p4forge"}
            journal["completion"] = {"fence": assignment["fence"], "status": "succeeded", "exit_code": 0, "reason": "", "last_log_sequence": 0}
            journal["log"] = {"fence": assignment["fence"], "sequence": 0, "stream": "system", "content": json.dumps(result, ensure_ascii=False) + "\n"}
            journal["phase"] = "finishing"
            manage.atomic_json(JOURNAL, journal)
            finish(api, journal)
    except Exception as error:
        # Do not replace an already-persisted completion after an uncertain reply.
        if journal["phase"] == "finishing":
            raise
        state_path = manage.STATE_DIR / "state.json"
        state = json.loads(state_path.read_text()) if state_path.exists() else {}
        manage.atomic_json(state_path, {**state, "status": "recovery_required"})
        reason = str(error)[:500]
        journal.update({"phase": "finishing", "completion": {"fence": assignment["fence"], "status": "failed", "exit_code": None,
                        "reason": reason, "last_log_sequence": 0},
                        "log": {"fence": assignment["fence"], "sequence": 0, "stream": "system", "content": "Deployment stopped: " + reason + "\n"}})
        manage.atomic_json(JOURNAL, journal)
        finish(api, journal)


def execute_build(api, policy, assignment):
    uuid.UUID(assignment["attempt_id"])
    journal = {"kind": "docker-build", "phase": "running", "assignment": assignment}
    manage.atomic_json(JOURNAL, journal)
    output = ""
    try:
        config = policy.get("docker_build")
        if not config:
            raise ValueError("Docker build is not configured on this executor")
        options = docker_build.validate(assignment, config)
        job = assignment["workspace_jobs"][0]
        with Guard(api, assignment, timeout=job["job"]["timeout_seconds"]) as guard:
            step(api, assignment, job, "running")
            journal["cleanup_pending"] = True
            manage.atomic_json(JOURNAL, journal)
            metadata, output = docker_build.build(config, DIRECTORY, assignment, options, guard)
            journal["image"] = metadata
            guard.check()
            docker_build.cleanup(config, DIRECTORY, assignment)
            journal["cleanup_pending"] = False
            guard.check()
            step(api, assignment, job, "succeeded")
            # Artifact endpoints require every workspace job to have succeeded.
            docker_build.upload(api, assignment, metadata)
            guard.check()
        status, code, reason = "succeeded", 0, ""
        output += "\nDocker image built: " + json.dumps(metadata, ensure_ascii=False) + "\n"
    except Exception as error:
        status, code, reason = "failed", None, str(error).replace("\0", "").encode("utf-8")[:900].decode("utf-8", errors="ignore")
        output += "\nDocker build stopped: " + str(error) + "\n"
    journal.update({"phase": "finishing", "completion": {"fence": assignment["fence"], "status": status,
                   "exit_code": code, "reason": reason, "last_log_sequence": 0},
                   "log": {"fence": assignment["fence"], "sequence": 0, "stream": "system",
                           "content": output.replace("\0", "").encode("utf-8")[-12000:].decode("utf-8", errors="replace")}})
    manage.atomic_json(JOURNAL, journal)
    finish(api, journal)


def recover(api):
    journal = private_json(JOURNAL)
    if journal["phase"] == "running":
        assignment = journal["assignment"]
        journal.update({"phase": "finishing", "completion": {"fence": assignment["fence"], "status": "failed", "exit_code": None,
            "reason": "executor restarted; verify target before environment recovery", "last_log_sequence": 0},
            "log": {"fence": assignment["fence"], "sequence": 0, "stream": "system", "content": "Executor restarted; no deployment replayed.\n"}})
        if journal.get("kind") == "docker-build":
            journal["completion"]["reason"] = "executor restarted; Docker build was not replayed"
            journal["log"]["content"] = "Executor restarted; Docker build was not replayed.\n"
        manage.atomic_json(JOURNAL, journal)
    try:
        finish(api, journal)
    except ApiError as error:
        if error.code != 409:
            raise
        # Expired/finalized attempt cannot authorize further work. The server
        # retains its environment lock; only a newly approved Run may proceed.
        manage.atomic_json(DIRECTORY / "interrupted.json", journal)
        if journal.get("kind") != "docker-build":
            state_path = manage.STATE_DIR / "state.json"
            state = json.loads(state_path.read_text()) if state_path.exists() else {}
            manage.atomic_json(state_path, {**state, "status": "recovery_required"})
        JOURNAL.unlink()


def main():
    os.umask(0o077)
    policy = private_json(DIRECTORY / "policy.json")
    for key in ("project_id", "pipeline_id", "environment_id"):
        uuid.UUID(policy[key])
    token_path = DIRECTORY / "token.json"
    token = private_json(token_path)["token"]
    if not re.fullmatch(r"[A-Za-z0-9_-]{43}", token):
        raise ValueError("invalid private agent token")
    api = Api(token)
    for number in (signal.SIGINT, signal.SIGTERM):
        signal.signal(number, lambda *_: STOP.set())
    with (DIRECTORY / "executor.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        request_path = DIRECTORY / "claim.json"
        next_docker_check = 0.0
        next_credentials = 0.0
        while not STOP.is_set():
            try:
                if policy.get("docker_build") and time.monotonic() >= next_credentials:
                    try:
                        docker_build.refresh(api, policy["docker_build"], DIRECTORY)
                    except Exception:
                        print("Perforce read credential check unavailable", flush=True)
                    next_credentials = time.monotonic() + 60
                if JOURNAL.exists():
                    with manage.locked():
                        recover(api)
                if time.monotonic() >= next_docker_check:
                    try:
                        api.call("agent/docker-status", docker_capability())
                    except Exception:
                        print("Docker status report unavailable", flush=True)
                    next_docker_check = time.monotonic() + 60
                if not request_path.exists():
                    manage.atomic_json(request_path, {"request_id": str(uuid.uuid4()), "os": "macos", "arch": "aarch64", "protocol_version": 8})
                elif private_json(request_path).get("protocol_version") != 8:
                    # Preserve the idempotency key across executor upgrades.
                    manage.atomic_json(request_path, {**private_json(request_path), "protocol_version": 8})
                response = api.call("agent/claim", private_json(request_path))
                assignment = response["assignment"]
                if assignment:
                    with manage.locked():
                        execute(api, policy, assignment)
                    request_path.unlink()
            except ApiError as error:
                print(str(error), flush=True)
                if error.code == 409 and not JOURNAL.exists() and request_path.exists():
                    request_path.unlink()
            except Exception as error:
                print(type(error).__name__ + ": " + str(error)[:500], flush=True)
            STOP.wait(2)


if __name__ == "__main__":
    main()
