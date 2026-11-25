"""Windows release supply chain: workflow policy, SBOM collector and the download verifier.

Fast and hermetic: no network, no cargo, no real `gh` (the verifier test puts a fake one first on
PATH). The real proof that the SBOMs and attestations work is the Windows Installer workflow itself.
"""

import importlib.util
import json
import os
import re
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github" / "workflows" / "installer.yml"
VERIFY = ROOT / "scripts" / "release" / "verify-windows.ps1"
PWSH = shutil.which("pwsh")


def load_collector():
    spec = importlib.util.spec_from_file_location("collect_sbom", ROOT / "scripts" / "release" / "collect_sbom.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def jobs(text):
    """{job name: body text} for the top-level `jobs:` of a workflow (no YAML library needed)."""
    section = text.split("\njobs:\n", 1)[1]
    parts = re.split(r"(?m)^  ([a-z][a-z0-9-]*):[ \t]*$", section)
    return {parts[i]: parts[i + 1] for i in range(1, len(parts), 2)}


def job_permissions(body):
    block = re.search(r"(?m)^    permissions:[ \t]*\n((?:      [^\n]+\n)+)", body)
    assert block, "job has no permissions block"
    return dict(line.strip().split(": ") for line in block.group(1).splitlines())


class InstallerWorkflowPolicy(unittest.TestCase):
    def setUp(self):
        self.text = WORKFLOW.read_text(encoding="utf-8")
        self.jobs = jobs(self.text)

    def test_every_action_is_pinned_to_a_commit(self):
        uses = re.findall(r"(?m)^\s*(?:-\s*)?uses:\s*(\S+)", self.text)
        self.assertGreater(len(uses), 5)
        for action in uses:
            self.assertRegex(action, r"^[^@\s]+@[0-9a-f]{40}$", f"{action} is not pinned to a full commit SHA")

    def test_nothing_is_granted_by_default_and_every_job_asks_for_its_own(self):
        self.assertRegex(self.text, r"(?m)^permissions: \{\}$")
        self.assertEqual(set(self.jobs), {"windows-installer", "attest", "attach-to-release"})
        self.assertEqual(job_permissions(self.jobs["windows-installer"]), {"contents": "read"})
        self.assertEqual(
            job_permissions(self.jobs["attest"]),
            {"contents": "read", "id-token": "write", "attestations": "write"},
        )
        self.assertEqual(job_permissions(self.jobs["attach-to-release"]), {"contents": "write"})

    def test_secrets_stay_in_the_build_job(self):
        for name in ("attest", "attach-to-release"):
            self.assertNotIn("secrets.", self.jobs[name], name)
        self.assertIn("TAURI_SIGNING_PRIVATE_KEY", self.jobs["windows-installer"])

    def test_attestation_and_publication_never_run_for_pull_requests_or_forks(self):
        for name in ("attest", "attach-to-release"):
            condition = re.search(r"(?ms)^    if: >-\n(.*?)^    runs-on:", self.jobs[name]).group(1)
            self.assertIn("github.repository == 'emreocell/Praxi-Bot'", condition, name)
            self.assertIn("github.event_name == 'push'", condition, name)
            self.assertNotIn("pull_request", condition, name)
            self.assertNotIn("schedule", condition, name)
        # The attach job only handles pre-release tags and builds nothing itself.
        self.assertIn("contains(github.ref_name, '-')", self.jobs["attach-to-release"])
        self.assertNotIn("actions/checkout", self.jobs["attach-to-release"])
        self.assertNotIn("--clobber", self.text)

    def test_provenance_covers_the_installer_sidecar_and_sboms(self):
        attest = self.jobs["attest"]
        self.assertIn("actions/attest-build-provenance@", attest)
        self.assertIn("subject-path: release/*", attest)
        assemble = self.jobs["windows-installer"].split("- name: Assemble the release files", 1)[1]
        assemble = assemble.split("- name: Upload installer artifact", 1)[0]
        for needle in ('-setup.exe', 'bot-*.exe', "sbom/*.cdx.json", "verify-windows.ps1", "build-info.txt", "SHA256SUMS.txt"):
            self.assertIn(needle, assemble)
        # SBOM generation happens before the long build so a broken SBOM fails fast.
        build = self.jobs["windows-installer"]
        self.assertLess(build.index("- name: Generate SBOMs"), build.index("- name: Build and stage bot sidecar"))
        self.assertIn("cargo build --release --locked -p cli", build)

    def test_the_release_files_the_verifier_needs_are_published(self):
        self.assertIn("praxi-bot-windows.sigstore.json", self.jobs["attest"])
        script = VERIFY.read_text(encoding="utf-8")
        self.assertIn("praxi-bot-windows.sigstore.json", script)
        self.assertIn("SHA256SUMS.txt", script)
        self.assertIn(".github/workflows/installer.yml", script)


def valid_sbom(package, components=("serde",)):
    return {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "metadata": {"component": {"name": package}},
        "components": [{"name": name} for name in components],
    }


class SbomCollector(unittest.TestCase):
    def setUp(self):
        self.collect = load_collector()
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)

    def write(self, relative, document):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(document), encoding="utf-8")
        return path

    def test_copies_the_shipped_crates_and_ignores_other_json(self):
        self.write("apps/cli/cli.cdx.json", valid_sbom("cli"))
        self.write("apps/cli/package.json", {"name": "not-an-sbom"})
        self.write("apps/desktop/desktop.cdx.json", valid_sbom("desktop"))
        self.write("apps/desktop/tauri.conf.json", {"productName": "Praxi-Bot"})
        self.write("out/sbom-ui.cdx.json", valid_sbom("desktop-ui"))
        self.assertEqual(self.collect.main(["--root", str(self.root), "--out", str(self.root / "out")]), 0)
        for name in ("sbom-rust-cli.cdx.json", "sbom-rust-desktop.cdx.json", "sbom-ui.cdx.json"):
            self.assertTrue((self.root / "out" / name).is_file(), name)
        copied = json.loads((self.root / "out" / "sbom-rust-cli.cdx.json").read_text(encoding="utf-8"))
        self.assertEqual(copied["metadata"]["component"]["name"], "cli")

    def test_an_empty_or_wrong_sbom_fails_the_build(self):
        self.assertIsNotNone(self.collect.check_sbom(self.write("a.json", valid_sbom("cli", components=())), "cli"))
        self.assertIsNotNone(self.collect.check_sbom(self.write("b.json", valid_sbom("other")), "cli"))
        self.assertIsNotNone(self.collect.check_sbom(self.write("c.json", {"bomFormat": "SPDX"}), "cli"))
        self.assertIsNone(self.collect.check_sbom(self.write("d.json", valid_sbom("cli")), "cli"))
        self.write("apps/cli/cli.cdx.json", valid_sbom("cli", components=()))
        with self.assertRaises(SystemExit) as raised:
            self.collect.find_rust_sbom(self.root / "apps" / "cli", "cli")
        self.assertIn("lists no components", str(raised.exception))


FAKE_GH_POSIX = """#!/bin/sh
if [ "$1" = "--version" ]; then echo "gh version $FAKE_GH_VERSION (fake)"; exit 0; fi
echo "$@" > "$FAKE_GH_LOG"
exit "$FAKE_GH_EXIT"
"""
FAKE_GH_CMD = """@echo off
if "%1"=="--version" goto version
echo %* > "%FAKE_GH_LOG%"
exit /b %FAKE_GH_EXIT%
:version
echo gh version %FAKE_GH_VERSION%
exit /b 0
"""


@unittest.skipUnless(PWSH, "PowerShell (pwsh) is not installed")
class VerifyWindowsScript(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        self.installer = self.dir / "Praxi-0.0.0-setup.exe"
        self.installer.write_bytes(b"pretend installer")
        self.hash = __import__("hashlib").sha256(b"pretend installer").hexdigest()
        (self.dir / "SHA256SUMS.txt").write_text(f"{self.hash}  Praxi-0.0.0-setup.exe\n", encoding="utf-8")
        self.fake_bin = self.dir / "fakebin"
        self.fake_bin.mkdir()
        if os.name == "nt":
            (self.fake_bin / "gh.cmd").write_text(FAKE_GH_CMD, encoding="ascii")
        else:
            fake = self.fake_bin / "gh"
            fake.write_text(FAKE_GH_POSIX, encoding="ascii")
            fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
        self.log = self.dir / "gh.log"

    def run_script(self, *args, gh_exit=0, gh_version="2.80.0"):
        env = dict(os.environ)
        env.update(
            PATH=str(self.fake_bin) + os.pathsep + env.get("PATH", ""),
            FAKE_GH_LOG=str(self.log),
            FAKE_GH_EXIT=str(gh_exit),
            FAKE_GH_VERSION=gh_version,
        )
        return subprocess.run(
            [PWSH, "-NoProfile", "-File", str(VERIFY), "-InstallerPath", str(self.installer), *args],
            capture_output=True, text=True, env=env, timeout=120,
        )

    def test_good_checksum_with_provenance_skipped_says_so(self):
        done = self.run_script("-SkipProvenance")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("Checksum: OK", done.stdout)
        self.assertIn("NOT CHECKED", done.stdout)

    def test_a_changed_file_or_unlisted_file_is_refused(self):
        self.installer.write_bytes(b"pretend installer, edited")
        changed = self.run_script("-SkipProvenance")
        self.assertEqual(changed.returncode, 1, changed.stdout)
        self.assertIn("different from the one in SHA256SUMS.txt", changed.stdout)
        (self.dir / "SHA256SUMS.txt").write_text(f"{self.hash}  other-file.exe\n", encoding="utf-8")
        unlisted = self.run_script("-SkipProvenance")
        self.assertEqual(unlisted.returncode, 1, unlisted.stdout)
        self.assertIn("not listed", unlisted.stdout)

    def test_provenance_is_asked_from_the_release_workflow_of_this_repository(self):
        (self.dir / "praxi-bot-windows.sigstore.json").write_text("{}", encoding="utf-8")
        done = self.run_script("-Tag", "v0.0.0")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        args = self.log.read_text(encoding="utf-8")
        for expected in (
            "attestation verify",
            "--repo emreocell/Praxi-Bot",
            "--signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/installer.yml",
            "--source-ref refs/tags/v0.0.0",
            "--deny-self-hosted-runners",
            "--bundle",
        ):
            self.assertIn(expected, args)

    def test_failed_provenance_or_old_gh_is_refused(self):
        failed = self.run_script(gh_exit=1)
        self.assertEqual(failed.returncode, 1, failed.stdout)
        self.assertIn("could NOT be verified", failed.stdout)
        old = self.run_script(gh_version="2.79.0")
        self.assertEqual(old.returncode, 1, old.stdout)
        self.assertIn("too old", old.stdout)


if __name__ == "__main__":
    unittest.main()
