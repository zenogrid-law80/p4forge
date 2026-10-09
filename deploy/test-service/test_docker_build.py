import copy
import json
import os
from pathlib import Path
import sys
import subprocess
import tempfile
import time
import unittest
from unittest.mock import Mock, patch
import uuid

import docker_build as build
import executor
import manage


class DockerBuildTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.config = {"connection": {"port": "localhost:1666", "user": "reader"}, "server_id": "test"}
        job = {"name": "build", "stage": "build", "script": [build.OPERATION], "shell": "sh", "working_directory": ".",
               "runner": {"os": "macos", "arch": "aarch64", "pool": "compose-demo", "docker": "linux"},
               "timeout_seconds": 600, "environment": {}}
        self.assignment = {"attempt_id": str(uuid.uuid4()), "run_id": str(uuid.uuid4()), "fence": 1,
            "source": {**self.config, "changelist": 12}, "workspace_jobs": [{"id": str(uuid.uuid4()), "job": job}],
            "artifacts": [build.ARTIFACT]}
        for module, name, value in [(manage, "STATE_DIR", self.root), (executor, "DIRECTORY", self.root),
                                    (executor, "JOURNAL", self.root / "attempt.json")]:
            p = patch.object(module, name, value)
            p.start()
            self.addCleanup(p.stop)
        self.state = {"current": "keep", "status": "healthy"}
        manage.atomic_json(self.root / "state.json", self.state)
        manage.atomic_json(self.root / "policy.json", {"docker_build": self.config})
        (self.root / "policy.json").chmod(0o600)

    def test_rejects_host_commands_environment_paths_and_wrong_engine(self):
        self.assertEqual(build.validate(self.assignment, self.config)["platform"], "linux/amd64")
        for env in ({"DOCKER_HOST": "elsewhere"}, {"BUILD_CONTEXT": "../"},
                    {"BUILD_DOCKERFILE": "/etc/passwd"}, {"BUILD_IMAGE": "server:latest"},
                    {"BUILD_TARGET": "--push"}, {"BUILD_PLATFORM": "windows/amd64"}):
            changed = copy.deepcopy(self.assignment)
            changed["workspace_jobs"][0]["job"]["environment"] = env
            with self.assertRaises(ValueError):
                build.validate(changed, self.config)
        for field, value in (("script", ["docker run --privileged x"]), ("timeout_seconds", 99999),
                             ("runner", {"os": "macos", "arch": "aarch64", "pool": "compose-demo"})):
            changed = copy.deepcopy(self.assignment)
            changed["workspace_jobs"][0]["job"][field] = value
            with self.assertRaises(ValueError):
                build.validate(changed, self.config)
        with self.assertRaises(ValueError):
            build.validate(self.assignment, {**self.config, "server_id": "other"})

    def test_source_links_and_missing_files_cannot_expose_host(self):
        source = self.root / "source"
        source.mkdir()
        options = build.validate(self.assignment, self.config)
        with self.assertRaises(ValueError):
            build.context_paths(source, options)
        (source / "Dockerfile").write_text("FROM scratch\n")
        self.assertEqual(build.context_paths(source, options)[0], source)
        link = source / "outside"
        link.symlink_to(self.root / "state.json")
        with self.assertRaises(ValueError):
            build.context_paths(source, options)
        link.unlink()
        os.link(self.root / "state.json", link)
        with self.assertRaises(ValueError):
            build.context_paths(source, options)

    def test_process_cancel_stops_child_and_output_is_bounded(self):
        result = build.process([sys.executable, "-c", "print('x'*50000)"], timeout=5)
        self.assertEqual(len(result), 10000)
        pidfile = self.root / "pid"
        deadline = time.monotonic() + 0.5
        def check():
            if time.monotonic() > deadline:
                raise RuntimeError("lease expired")
        with self.assertRaisesRegex(RuntimeError, "lease expired"):
            build.process([sys.executable, "-c", "import os,time,pathlib; pathlib.Path(" + repr(str(pidfile)) + ").write_text(str(os.getpid())); time.sleep(60)"], check=check)
        with self.assertRaises(ProcessLookupError):
            os.kill(int(pidfile.read_text()), 0)

    def test_invalid_grant_preserves_ticket_and_never_reports_ready(self):
        old = self.root / "read.tickets"
        old.write_text("old")
        grant = {"id": str(uuid.uuid4()), "generation": str(uuid.uuid4()), "role": "read", **self.config, "tickets": "new"}
        api = Mock()
        api.call.side_effect = [[grant], {}]
        with patch.object(build, "helper", side_effect=RuntimeError("secret diagnostic")):
            self.assertFalse(build.refresh(api, self.config, self.root))
        self.assertEqual(old.read_text(), "old")
        self.assertFalse((self.root / "read.candidate").exists())
        self.assertFalse(api.call.call_args.args[1][0]["ready"])

    def test_executor_crash_stops_external_operation(self):
        pidfile = self.root / "orphan-pid"
        child_script = "import os,time,pathlib; pathlib.Path(" + repr(str(pidfile)) + ").write_text(str(os.getpid())); time.sleep(60)"
        parent_script = "import sys; sys.path.insert(0," + repr(str(Path(build.__file__).parent)) + "); import docker_build; docker_build.process(" + repr([sys.executable, "-c", child_script]) + ")"
        parent = subprocess.Popen([sys.executable, "-c", parent_script], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            deadline = time.monotonic() + 5
            while not pidfile.exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            self.assertTrue(pidfile.exists())
            pid = int(pidfile.read_text())
            parent.kill()
            parent.wait(timeout=5)
            while time.monotonic() < deadline:
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.05)
            else:
                self.fail("external operation survived executor crash")
        finally:
            if parent.poll() is None:
                parent.kill()
            parent.wait()

    def test_build_failure_and_restart_preserve_deployment_state(self):
        api = Mock()
        api.call.return_value = {"status": "failed"}
        with patch.object(executor, "Guard"), patch.object(build, "build", side_effect=RuntimeError("Docker failure")), patch.object(build, "cleanup"):
            executor.execute(api, {"docker_build": self.config}, self.assignment)
        self.assertEqual(json.loads((self.root / "state.json").read_text()), self.state)
        self.assertEqual(json.loads((self.root / "last-build.json").read_text())["completion"]["status"], "failed")
        journal = {"kind": "docker-build", "phase": "running", "assignment": self.assignment, "cleanup_pending": True}
        manage.atomic_json(executor.JOURNAL, journal)
        executor.JOURNAL.chmod(0o600)
        with patch.object(executor, "private_json", side_effect=lambda path: journal if path == executor.JOURNAL else {"docker_build": self.config}), patch.object(build, "cleanup") as cleanup, patch.object(build, "build", side_effect=AssertionError("no replay")):
            executor.recover(api)
            cleanup.assert_called_once()
        self.assertEqual(json.loads((self.root / "state.json").read_text()), self.state)

    def test_lost_success_reply_retries_identical_completion_without_rebuild(self):
        metadata = {"image": "p4forge-build/server:unique", "image_id": "sha256:" + "a" * 64}
        api = Mock()
        fail_once = [True]
        completions = []
        def call(path, body):
            if path.endswith("/complete"):
                completions.append(copy.deepcopy(body))
                if fail_once and fail_once.pop():
                    raise OSError("reply lost")
                return {"status": "succeeded"}
            return {}
        api.call.side_effect = call
        with patch.object(executor, "Guard"), patch.object(build, "build", return_value=(metadata, "built")), patch.object(build, "upload"), patch.object(build, "cleanup"):
            with self.assertRaises(OSError):
                executor.execute(api, {"docker_build": self.config}, self.assignment)
        executor.JOURNAL.chmod(0o600)
        with patch.object(build, "build", side_effect=AssertionError("no replay")):
            executor.recover(api)
        self.assertEqual(completions[0], completions[1])
        self.assertEqual(json.loads((self.root / "state.json").read_text()), self.state)

    def test_artifacts_upload_after_job_success_before_completion(self):
        events = []
        api = Mock()
        def call(path, body):
            if path.endswith("/step"):
                events.append(body["status"])
            if path.endswith("/complete"):
                events.append("complete")
            return {"status": "succeeded"}
        api.call.side_effect = call
        with patch.object(executor, "Guard"), patch.object(build, "build", return_value=({"image": "example"}, "built")), patch.object(build, "cleanup"), patch.object(build, "upload", side_effect=lambda *_: events.append("upload")):
            executor.execute(api, {"docker_build": self.config}, self.assignment)
        self.assertEqual(events, ["running", "succeeded", "upload", "complete"])


if __name__ == "__main__":
    unittest.main()
