"""Tests for scripts/ci/check_github_actions_pins.py."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CHECK = ROOT / "scripts" / "ci" / "check_github_actions_pins.py"


def load_module():
    spec = importlib.util.spec_from_file_location("check_github_actions_pins", CHECK)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CheckGithubActionsPinsTests(unittest.TestCase):
    def setUp(self):
        self.mod = load_module()

    def test_accepts_sha_pins_and_local_actions(self):
        with tempfile.TemporaryDirectory() as tmp:
            wf = Path(tmp) / "ci.yml"
            wf.write_text(
                "\n".join(
                    [
                        "jobs:",
                        "  check:",
                        "    steps:",
                        "      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262",
                        "      - uses: ./.github/actions/local",
                    ]
                ),
                encoding="utf-8",
            )
            self.assertEqual(self.mod.check_workflows(wf.parent), [])

    def test_rejects_version_tags(self):
        with tempfile.TemporaryDirectory() as tmp:
            wf = Path(tmp) / "bad.yml"
            wf.write_text("jobs:\n  j:\n    steps:\n      - uses: actions/checkout@v4\n", encoding="utf-8")
            errors = self.mod.check_workflows(wf.parent)
            self.assertEqual(len(errors), 1)
            self.assertIn("checkout@v4", errors[0])

    def test_branch_style_pins_must_match_across_workflows(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "a.yml").write_text(
                "jobs:\n  j:\n    steps:\n"
                "      - uses: dtolnay/rust-toolchain@"
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa # stable (2026-01-01)\n",
                encoding="utf-8",
            )
            (root / "b.yml").write_text(
                "jobs:\n  j:\n    steps:\n"
                "      - uses: dtolnay/rust-toolchain@"
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb # stable (2026-02-01)\n",
                encoding="utf-8",
            )
            errors = self.mod.check_workflows(root)
            self.assertEqual(len(errors), 1)
            self.assertIn("dtolnay/rust-toolchain", errors[0])
            self.assertIn("one commit SHA", errors[0])

    def test_branch_style_pins_may_differ_from_version_comments(self):
        with tempfile.TemporaryDirectory() as tmp:
            wf = Path(tmp) / "ci.yml"
            wf.write_text(
                "\n".join(
                    [
                        "jobs:",
                        "  j:",
                        "    steps:",
                        "      - uses: actions/checkout@"
                        "cccccccccccccccccccccccccccccccccccccccc # v4.2.2",
                        "      - uses: actions/checkout@"
                        "dddddddddddddddddddddddddddddddddddddddd # v4.3.0",
                    ]
                ),
                encoding="utf-8",
            )
            self.assertEqual(self.mod.check_workflows(wf.parent), [])

    def test_real_workflows_are_fully_pinned(self):
        errors = self.mod.check_workflows(ROOT / ".github" / "workflows")
        self.assertEqual(errors, [], "\n".join(errors))


if __name__ == "__main__":
    unittest.main()