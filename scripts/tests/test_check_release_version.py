"""Check release version validation through its command line."""

from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
from textwrap import dedent
import unittest


CHECKER = Path(__file__).resolve().parents[1] / "release/check_version.py"


class ReleaseVersionCheckerTests(unittest.TestCase):
    def setUp(self):
        directory = TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        files = {
            "Cargo.toml": """
                [workspace]
                members = ["crates/*"]
                exclude = ["crates/fixture"]

                [workspace.package]
                version = "1.2.3"

                [workspace.dependencies]
                private-alias = { package = "private", path = "tools/private", version = "1.2.3" }
                registry-only = "7.5.0"
            """,
            "crates/app/Cargo.toml": """
                [package]
                name = "example-app"
                version.workspace = true

                [dependencies]
                lib = { path = "../lib", version = "1.2.3" }
                fixture = { path = "../fixture", version = "8.0.0" }

                [dev-dependencies]
                private-alias.workspace = true
                lib = { path = "../lib", version = "1.2.3" }

                [target.'cfg(windows)'.build-dependencies]
                lib = { path = "../lib", version = "1.2.3" }
            """,
            "crates/lib/Cargo.toml": '[package]\nname = "lib"\nversion = "1.2.3"\n',
            "crates/fixture/Cargo.toml": '[package]\nname = "fixture"\nversion = "8.0.0"\n',
            "tools/private/Cargo.toml": '[package]\nname = "private"\nversion.workspace = true\n',
            "Cargo.lock": """
                version = 4

                [[package]]
                name = "example-app"
                version = "1.2.3"

                [[package]]
                name = "lib"
                version = "1.2.3"

                [[package]]
                name = "lib"
                version = "5.4.3"
                source = "registry+https://example.com/index"

                [[package]]
                name = "private"
                version = "1.2.3"

                [[package]]
                name = "fixture"
                version = "8.0.0"
            """,
            "apps/desktop/tauri.conf.json": '{"version": "1.2.3"}',
            "apps/desktop/ui/package.json": '{"version": "1.2.3", "dependencies": {"lib": "5.4.3"}}',
            "packages/ui/package.json": '{"version": "1.2.3"}',
            "package.json": '{"name": "workspace-root", "private": true}',
            "docs/guide/whats-new.md": "# v0.1.0-alpha.1\n",
        }
        for name, content in files.items():
            self.write(name, dedent(content).lstrip("\n"))

    def write(self, name, content):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def replace(self, name, old, new):
        self.write(name, (self.root / name).read_text(encoding="utf-8").replace(old, new))

    def run_checker(self, version):
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root), version],
            capture_output=True,
            text=True,
            encoding="utf-8",
        )

    def test_accepts_numeric_versions_and_prerelease_tags(self):
        for version in ("1.2.3", "v1.2.3", "v1.2.3-alpha.2", "v1.2.3-rc.1+build.05"):
            with self.subTest(version=version):
                result = self.run_checker(version)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("Release manifests match 1.2.3", result.stdout)

    def test_rejects_invalid_versions(self):
        for version in ("main", "1.2", "v01.2.3", "1.2.3-alpha.01", "v1.2.3-", "1.2.3-rc..1"):
            with self.subTest(version=version):
                result = self.run_checker(version)
                self.assertEqual(result.returncode, 2)
                self.assertIn("invalid release version", result.stderr)

    def test_reports_drift_across_manifests_in_one_run(self):
        self.replace("Cargo.toml", 'version = "1.2.3"', 'version = "1.2.2"')
        self.replace("crates/app/Cargo.toml", 'version = "1.2.3"', 'version = "1.2.2"')
        self.replace("crates/lib/Cargo.toml", 'version = "1.2.3"', 'version = "1.2.2"')
        self.replace("tools/private/Cargo.toml", "version.workspace = true", 'version = "1.2.2"')
        self.replace("Cargo.lock", 'version = "1.2.3"', 'version = "1.2.2"')
        for name in ("apps/desktop/tauri.conf.json", "apps/desktop/ui/package.json", "packages/ui/package.json"):
            self.replace(name, '"version": "1.2.3"', '"version": "1.2.2"')

        result = self.run_checker("v1.2.3-alpha.2")

        self.assertEqual(result.returncode, 1)
        fields = (
            "Cargo.toml: workspace.package.version",
            "Cargo.toml: workspace.dependencies.private-alias.version",
            "crates/app/Cargo.toml: dependencies.lib.version",
            "crates/app/Cargo.toml: dev-dependencies.lib.version",
            "crates/app/Cargo.toml: target.'cfg(windows)'.build-dependencies.lib.version",
            "crates/lib/Cargo.toml: package.version",
            "tools/private/Cargo.toml: package.version",
            "Cargo.lock: package 'example-app'.version",
            "Cargo.lock: package 'lib'.version",
            "Cargo.lock: package 'private'.version",
            "apps/desktop/tauri.conf.json: version",
            "apps/desktop/ui/package.json: version",
            "packages/ui/package.json: version",
        )
        self.assertCountEqual(
            result.stderr.splitlines(),
            [f"{field}: expected '1.2.3', found '1.2.2'" for field in fields],
        )

    def test_reports_missing_entries_and_parse_errors(self):
        self.replace("Cargo.lock", '[[package]]\nname = "lib"\nversion = "1.2.3"\n', "")
        self.write("tools/private/Cargo.toml", "[package")
        self.write("apps/desktop/tauri.conf.json", "{")
        self.write("apps/desktop/ui/package.json", "{}")
        (self.root / "packages/ui/package.json").unlink()

        result = self.run_checker("1.2.3")

        self.assertEqual(result.returncode, 1)
        self.assertIn("Cargo.lock: package 'lib'.version: expected '1.2.3', found no local package entry", result.stderr)
        self.assertIn("tools/private/Cargo.toml: could not read or parse file", result.stderr)
        self.assertIn("apps/desktop/tauri.conf.json: could not read or parse file", result.stderr)
        self.assertIn("apps/desktop/ui/package.json: version: expected '1.2.3', found missing", result.stderr)
        self.assertIn("packages/ui/package.json: could not read or parse file", result.stderr)


if __name__ == "__main__":
    unittest.main()
