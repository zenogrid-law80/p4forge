import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("manage", Path(__file__).with_name("manage.py"))
manage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(manage)


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = patch.object(manage, "STATE_DIR", Path(self.temp.name))
        self.directory.start()
        self.addCleanup(self.directory.stop)

    def test_release_integrity_and_html_escaping(self):
        release = manage.prepare_release("v1", "<script>alert(1)</script>")
        path = manage.STATE_DIR / "releases" / release
        self.assertIn(b"&lt;script&gt;", (path / "index.html").read_bytes())
        (path / "health.json").write_text('{}')
        with self.assertRaises((ValueError, KeyError)):
            manage.validate_release(path)
        with self.assertRaises(ValueError):
            manage.prepare_release("../../secret", "bad")

    def test_failed_health_retains_success_pointer_and_recovery_can_restore_it(self):
        first = manage.prepare_release("v1", "first")
        second = manage.prepare_release("v2", "second")
        original = {"current": first, "previous": None, "status": "healthy"}
        with patch.object(manage, "compose"), patch.object(manage, "check_health", side_effect=RuntimeError("bad health")):
            with self.assertRaises(RuntimeError):
                manage.switch(original, second, "apply")
        failed = json.loads((manage.STATE_DIR / "state.json").read_text())
        self.assertEqual(failed["current"], first)
        self.assertEqual(failed["pending"], second)
        self.assertEqual(failed["status"], "recovery_required")
        with patch.object(manage, "compose"), patch.object(manage, "check_health", return_value={}), patch("builtins.print"):
            manage.switch(failed, first, "rollback")
        recovered = json.loads((manage.STATE_DIR / "state.json").read_text())
        self.assertEqual(recovered["current"], first)
        self.assertIsNone(recovered["previous"])
        self.assertIsNone(recovered["pending"])


if __name__ == "__main__":
    unittest.main()
