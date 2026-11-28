#!/usr/bin/env python3
import importlib.util
import sys
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "ci_pr_path_classifiers",
    Path(__file__).parents[1] / "ci_pr_path_classifiers.py",
)
classifiers = importlib.util.module_from_spec(spec)
sys.modules["ci_pr_path_classifiers"] = classifiers
spec.loader.exec_module(classifiers)
classify_paths = classifiers.classify_paths


class CiPrPathClassifiersTest(unittest.TestCase):
    def test_docs_only_skips_rust_and_smoke(self) -> None:
        flags = classify_paths(["docs/guide/foo.md", "README.md"])
        self.assertFalse(flags.rust)
        self.assertFalse(flags.smoke)
        self.assertFalse(flags.ui)
        self.assertFalse(flags.visual)

    def test_ui_only_skips_rust_keeps_smoke_linux_only(self) -> None:
        flags = classify_paths(["apps/desktop/ui/src/App.tsx"])
        self.assertFalse(flags.rust)
        self.assertTrue(flags.ui)
        self.assertTrue(flags.visual)
        self.assertTrue(flags.smoke)
        self.assertFalse(flags.smoke_windows)

    def test_site_change_runs_ui_not_visual(self) -> None:
        flags = classify_paths(["site/index.html"])
        self.assertFalse(flags.rust)
        self.assertTrue(flags.ui)
        self.assertFalse(flags.visual)

    def test_crate_change_runs_rust_and_windows_smoke(self) -> None:
        flags = classify_paths(["crates/scheduler/src/lib.rs"])
        self.assertTrue(flags.rust)
        self.assertFalse(flags.ui)
        self.assertTrue(flags.smoke)
        self.assertTrue(flags.smoke_windows)

    def test_openapi_triggers_rust_and_ui(self) -> None:
        flags = classify_paths(["docs/api/openapi.json"])
        self.assertTrue(flags.rust)
        self.assertTrue(flags.ui)
        self.assertFalse(flags.visual)


if __name__ == "__main__":
    unittest.main()