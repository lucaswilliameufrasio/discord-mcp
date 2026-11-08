import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import report


class ReportTests(unittest.TestCase):
    def test_source_digest_changes_and_rejects_symlink(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "src").mkdir()
            (root / "examples").mkdir()
            for name in ("Cargo.toml", "Cargo.lock", "examples/mcp_benchmark.rs", "src/lib.rs"):
                (root / name).write_text("first")
            before = report.source_digest(root)
            (root / "src/lib.rs").write_text("changed")
            self.assertNotEqual(before, report.source_digest(root))
            (root / "src/lib.rs").unlink()
            (root / "src/lib.rs").symlink_to(root / "Cargo.toml")
            with self.assertRaises(ValueError):
                report.source_digest(root)

    def test_report_never_serializes_environment_or_path(self):
        values = ["a" * 40, "private-path-marker", "rustc 1.99.1 (58cf8f9c3 2026-10-13)",
                  "cargo 1.99.1 (58cf8f9c3 2026-10-13)", '{"schema":3,"scenarios":[]}']
        output = io.StringIO()
        with patch.object(report, "source_digest", return_value="b" * 64), \
             patch.object(report, "run_capture", side_effect=values), \
             patch.dict("os.environ", {"SECRET_MARKER": "do-not-output", "HOME": "/private-path-marker"}), \
             patch.object(report.platform, "release", return_value="7.3.0-private-marker"), \
             contextlib.redirect_stdout(output):
            report.main()
        result = json.loads(output.getvalue())
        self.assertTrue(result["provenance"]["working_tree_dirty"])
        self.assertEqual(result["provenance"]["kernel_version"], "7.3.0")
        self.assertNotIn("private-marker", output.getvalue())
        self.assertNotIn("do-not-output", output.getvalue())

    def test_changed_source_produces_no_report(self):
        values = ["a" * 40, "", "rustc 1.99.1 (58cf8f9c3 2026-10-13)",
                  "cargo 1.99.1 (58cf8f9c3 2026-10-13)", '{"schema":3,"scenarios":[]}']
        output = io.StringIO()
        with patch.object(report, "source_digest", side_effect=["a" * 64, "b" * 64]), \
             patch.object(report, "run_capture", side_effect=values), \
             contextlib.redirect_stdout(output):
            with self.assertRaises(ValueError):
                report.main()
        self.assertEqual(output.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
