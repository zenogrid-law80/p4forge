import base64
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch, Mock
import uuid

import executor
import manage


class ExecutorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        root.mkdir(exist_ok=True)
        for module, name, value in [(manage, "STATE_DIR", root), (executor, "DIRECTORY", root), (executor, "JOURNAL", root / "attempt.json")]:
            p = patch.object(module, name, value)
            p.start()
            self.addCleanup(p.stop)
        self.policy = {key: str(uuid.uuid4()) for key in ("environment_id", "pipeline_id", "project_id")}
        data = json.dumps({"schema_version": 1, "version": "v1", "message": "hello"}).encode()
        self.artifact = {"artifact_id": str(uuid.uuid4()), "source_run_id": str(uuid.uuid4()), "name": "demo-release.json", "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
        self.download = {"offset": 0, "data": base64.b64encode(data).decode()}
        self.assignment = {"attempt_id": str(uuid.uuid4()), "run_id": str(uuid.uuid4()), "fence": 1, "artifact_inputs": [self.artifact],
            "workspace_jobs": [{"id": str(uuid.uuid4()), "job": {"name": "compose-" + op, "stage": op, "script": ["p4forge-compose-demo:" + op],
            "shell": "sh", "working_directory": ".", "environment": {}, "runner": {"os": "macos", "arch": "aarch64", "pool": "compose-demo"},
            "timeout_seconds": 120 if op == "apply" else 60}} for op in ("apply", "health")]}
        self.permit = {**self.policy, "run_id": self.assignment["run_id"], "artifact_inputs": [self.artifact], "input_hash": "a" * 64}

    def test_docker_status_reports_engine_access_not_cli_presence(self):
        for result, expected in [(Mock(returncode=0, stdout=b"linux\n"), {"state":"available", "engine_os":"linux"}),
                                 (Mock(returncode=1, stdout=b""), {"state":"engine_unavailable", "engine_os":None}),
                                 (Mock(returncode=0, stdout=b"unexpected"), {"state":"check_failed", "engine_os":None})]:
            with patch.object(executor.subprocess, "run", return_value=result):
                self.assertEqual(executor.docker_capability(), expected)
        for error, expected in [(FileNotFoundError(), "cli_missing"), (executor.subprocess.TimeoutExpired("docker", 5), "check_failed")]:
            with patch.object(executor.subprocess, "run", side_effect=error):
                self.assertEqual(executor.docker_capability()["state"], expected)

    def test_fixed_target_and_commands_reject_unapproved_variations(self):
        executor.validate_assignment(self.assignment, self.policy, self.permit)
        for field, value in [("script", ["rm -rf /"]), ("environment", {"DOCKER_HOST": "elsewhere"}), ("working_directory", "/tmp")]:
            changed = copy.deepcopy(self.assignment)
            changed["workspace_jobs"][0]["job"][field] = value
            with self.assertRaises(ValueError):
                executor.validate_assignment(changed, self.policy, self.permit)
        with self.assertRaises(ValueError):
            executor.validate_assignment(self.assignment, self.policy, {**self.permit, "environment_id": str(uuid.uuid4())})

    def test_artifact_digest_and_schema_cannot_introduce_commands(self):
        self.assertEqual(executor.decode_release(self.download, self.artifact)["version"], "v1")
        with self.assertRaises(ValueError):
            executor.decode_release(self.download, {**self.artifact, "sha256": "0" * 64})
        data = json.dumps({"schema_version": 1, "version": "v1", "message": "ok", "command": "docker run"}).encode()
        with self.assertRaises(ValueError):
            executor.decode_release({"offset": 0, "data": base64.b64encode(data).decode()}, {**self.artifact, "size": len(data), "sha256": hashlib.sha256(data).hexdigest()})

    def test_cancelled_completion_never_advances_local_success(self):
        class Api:
            def call(self, path, body):
                return {"status": "canceled"}
        manage.atomic_json(manage.STATE_DIR / "state.json", {"current": "old", "pending": "new"})
        journal = {"assignment": self.assignment, "log_ack": True, "completion": {"status": "succeeded"}, "successful_state": {"current": "new"}}
        manage.atomic_json(executor.JOURNAL, journal)
        executor.finish(Api(), journal)
        saved = json.loads((manage.STATE_DIR / "state.json").read_text())
        self.assertEqual(saved["current"], "old")
        self.assertEqual(saved["status"], "recovery_required")

    def test_restart_reports_failure_without_replaying_compose(self):
        calls = []
        class Api:
            def call(self, path, body):
                calls.append((path, body))
                return {"status": "failed"}
        manage.atomic_json(executor.JOURNAL, {"phase": "running", "assignment": self.assignment})
        executor.JOURNAL.chmod(0o600)
        with patch.object(manage, "compose", side_effect=AssertionError("must not replay")):
            executor.recover(Api())
        self.assertEqual(calls[-1][1]["status"], "failed")
        self.assertFalse(executor.JOURNAL.exists())

    def test_lost_completion_reply_retries_identical_payload(self):
        class Api:
            def call(self, path, body):
                return {"status": "succeeded"}
        journal = {"phase": "finishing", "assignment": self.assignment, "log_ack": True,
            "completion": {"status": "succeeded", "fence": 1, "last_log_sequence": 0}, "successful_state": {"current": "new", "status": "healthy"}}
        manage.atomic_json(executor.JOURNAL, journal)
        executor.JOURNAL.chmod(0o600)
        with patch.object(manage, "compose", side_effect=AssertionError("must not replay")):
            executor.recover(Api())
        self.assertEqual(json.loads((manage.STATE_DIR / "state.json").read_text())["current"], "new")

    def test_expired_lease_and_cancel_block_work(self):
        guard = executor.Guard(None, self.assignment)
        with self.assertRaises(RuntimeError):
            guard.check()
        guard.deadline = time.monotonic() + 60
        guard.check()
        guard.canceled.set()
        with self.assertRaises(RuntimeError):
            guard.check()


if __name__ == "__main__":
    unittest.main()
