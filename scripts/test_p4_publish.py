import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("p4_publish", Path(__file__).with_name("p4-publish.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class PublisherTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.previous = Path.cwd()
        os.chdir(self.directory.name)
        Path("generated").mkdir()
        self.env = dict(P4PORT="p4.example:1666", P4USER="publisher", P4CLIENT="p4forge_test",
                        P4TICKETS="/protected/tickets", P4FORGE_CHANGELIST="2")
        self.calls = []
        self.opened = [{"code": "stat", "depotFile": "//streams/main/generated/file"}]
        self.preexisting = False
        self.fail_submit = False

    def tearDown(self):
        os.chdir(self.previous)
        self.directory.cleanup()

    def run_p4(self, *args, **kwargs):
        self.calls.append((args, kwargs))
        if args == ("opened", "-C", "p4forge_test"):
            return self.opened if self.preexisting else []
        if args == ("change", "-i"):
            return [{"code": "info", "data": "Change 41 created."}]
        if args == ("opened", "-c", "41"):
            return self.opened
        if args == ("submit", "-c", "41"):
            if self.fail_submit:
                raise RuntimeError("submit failed")
            return [{"code": "stat", "submittedChange": "42"}]
        if args == ("change", "-o", "41"):
            return [{"code": "stat", "Status": "pending"}]
        return []

    def test_only_explicit_paths_and_numbered_change_are_published(self):
        self.assertEqual(module.publish(["generated/..."], self.env, self.run_p4), "42")
        calls = [c[0] for c in self.calls]
        self.assertIn(("reconcile", "-a", "-e", "-d", "-c", "41", "generated/..."), calls)
        self.assertIn(("submit", "-c", "41"), calls)
        self.assertFalse(any(c[0] == "revert" for c in calls))

    def test_unchanged_outputs_delete_empty_pending_change_without_submit(self):
        self.opened = []
        self.assertIsNone(module.publish(["generated/..."], self.env, self.run_p4))
        self.assertIn(("change", "-d", "41"), [c[0] for c in self.calls])
        self.assertFalse(any(c[0][0] == "submit" for c in self.calls))

    def test_unrelated_open_files_prevent_creating_a_change(self):
        self.preexisting = True
        with self.assertRaisesRegex(RuntimeError, "unrelated"):
            module.publish(["generated/..."], self.env, self.run_p4)
        self.assertEqual(len(self.calls), 1)

    def test_failure_cleans_only_own_change_and_keeps_workspace_files(self):
        self.fail_submit = True
        with self.assertRaisesRegex(RuntimeError, "submit failed"):
            module.publish(["generated/..."], self.env, self.run_p4)
        self.assertIn(("revert", "-k", "-c", "41", "//p4forge_test/..."), [c[0] for c in self.calls])

    def test_unsafe_or_missing_directories_fail_before_any_p4_command(self):
        for path in ["...", "../...", "//depot/...", "generated/...@head", "missing/..."]:
            with self.assertRaises(ValueError):
                module.publish([path], self.env, self.run_p4)
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
