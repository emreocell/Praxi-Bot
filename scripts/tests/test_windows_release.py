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
FEED_WORKFLOW = ROOT / ".github" / "workflows" / "update-feed.yml"
VERIFY = ROOT / "scripts" / "release" / "verify-windows.ps1"
SCRIPT_MANIFEST = ROOT / "scripts" / "release" / "update_manifest.py"
# Public keys and signatures of throwaway keys (see test_update_manifest.py).
FIXTURES = json.loads((Path(__file__).parent / "fixtures" / "update_signatures.json").read_text(encoding="utf-8"))
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

    def step_text(self, name, text=None):
        """One step of the installer job: from its name to the next step (or the comment above it)."""
        body = (text or self.text).split(f"- name: {name}\n", 1)[1]
        return re.split(r"\n      (?:- name:|#)", body, maxsplit=1)[0]

    def step_script(self, name, text=None):
        """The `run:` script of one step of the installer job, unindented."""
        step = self.step_text(name, text)
        script = step.split("        run: |\n", 1)[1]
        return "\n".join(line[10:] for line in script.splitlines())

    @unittest.skipUnless(PWSH, "PowerShell (pwsh) is not installed")
    def test_preview_build_keeps_the_full_release_version_for_signed_updates(self):
        script = self.step_script("Build installer")
        # Exercise the actual workflow script, replacing only the expensive build command.
        stub = """
function pnpm {
    param([Parameter(ValueFromRemainingArguments=$true)][string[]]$BuildArgs)
    if ($BuildArgs.Count -ne 3 -or $BuildArgs[0] -ne 'build:desktop' -or $BuildArgs[1] -ne '--config') {
        throw 'Expected desktop build with a configuration file'
    }
    $config = Get-Content -Raw -LiteralPath $BuildArgs[2] | ConvertFrom-Json
    Write-Output ('CONFIG=' + ($config | ConvertTo-Json -Compress -Depth 4))
}
"""
        with tempfile.TemporaryDirectory() as tmp:
            env = {**os.environ, "RUNNER_TEMP": tmp, "GITHUB_REF_TYPE": "tag", "GITHUB_REF_NAME": "v0.1.0-alpha.2"}
            result = subprocess.run([PWSH, "-NoProfile", "-NonInteractive", "-Command", stub + script],
                                    env=env, capture_output=True, text=True, timeout=20, check=True)
            config = json.loads(next(line[7:] for line in result.stdout.splitlines() if line.startswith("CONFIG=")))
            self.assertEqual(config["version"], "0.1.0-alpha.2")
            # A release build trusts the public key of tauri.conf.json, never one handed in at build time.
            self.assertNotIn("plugins", config)

    @unittest.skipUnless(PWSH, "PowerShell (pwsh) is not installed")
    def test_a_rehearsal_build_trusts_only_its_throwaway_key(self):
        script = self.step_script("Build installer")
        stub = """
function pnpm {
    param([Parameter(ValueFromRemainingArguments=$true)][string[]]$BuildArgs)
    $config = Get-Content -Raw -LiteralPath $BuildArgs[2] | ConvertFrom-Json
    Write-Output ('CONFIG=' + ($config | ConvertTo-Json -Compress -Depth 4))
}
"""
        with tempfile.TemporaryDirectory() as tmp:
            key = Path(tmp) / "throwaway.key"
            Path(f"{key}.pub").write_text(FIXTURES["pub_b"] + "\n", encoding="utf-8")
            env = {**os.environ, "RUNNER_TEMP": tmp, "GITHUB_REF_TYPE": "branch",
                   "PRAXI_REHEARSAL_UPDATER_KEY": str(key)}
            result = subprocess.run([PWSH, "-NoProfile", "-NonInteractive", "-Command", stub + script],
                                    env=env, capture_output=True, text=True, timeout=20, check=True)
            config = json.loads(next(line[7:] for line in result.stdout.splitlines() if line.startswith("CONFIG=")))
            self.assertEqual(config["plugins"]["updater"]["pubkey"], FIXTURES["pub_b"])
            self.assertNotIn("version", config)

    def test_the_build_never_sees_the_signing_key_and_real_secrets_are_for_tags_only(self):
        build = self.step_text("Build installer")
        self.assertNotIn("secrets.", build)
        self.assertNotIn("TAURI_SIGNING", build)
        self.assertNotIn("createUpdaterArtifacts", self.text)
        for name in ("Sign the installer and write the update feed (latest.json)",):
            step = self.step_text(name)
            for secret in ("TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"):
                self.assertIn(f"${{{{ github.ref_type == 'tag' && secrets.{secret} || '' }}}}", step)
        # The only other place that mentions the secret asks "is it set?" and never reads its value.
        early = self.step_text("Check the updater configuration and signing key")
        self.assertIn("secrets.TAURI_SIGNING_PRIVATE_KEY != ''", early)
        self.assertEqual(
            set(re.findall(r"secrets\.([A-Z_]+)", self.text)),
            {"TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
             "WINDOWS_CERTIFICATE", "WINDOWS_CERTIFICATE_PASSWORD"},
        )

    def test_the_updater_check_runs_before_the_long_build_and_the_signing_step_can_be_skipped(self):
        self.assertLess(self.text.index("Check the updater configuration"), self.text.index("- name: Build installer"))
        sign = self.step_text("Sign the installer and write the update feed (latest.json)")
        self.assertIn("if: env.PRAXI_UPDATER_MODE != 'off'", sign)
        # The release files are assembled with or without the feed.
        self.assertIn("if (Test-Path update-assets)", self.text)

    def run_updater_check(self, *, tag, have_key, pubkey):
        """Runs the real decision step in a scratch checkout; returns (process, mode written to GITHUB_ENV, summary)."""
        script = self.step_script("Check the updater configuration and signing key")
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            (work / "scripts" / "release").mkdir(parents=True)
            shutil.copy(SCRIPT_MANIFEST, work / "scripts" / "release")
            (work / "apps" / "desktop").mkdir(parents=True)
            conf = json.loads((ROOT / "apps" / "desktop" / "tauri.conf.json").read_text(encoding="utf-8"))
            conf["plugins"]["updater"]["pubkey"] = pubkey
            (work / "apps" / "desktop" / "tauri.conf.json").write_text(json.dumps(conf), encoding="utf-8")
            github_env, summary = work / "github_env.txt", work / "summary.md"
            env = {**os.environ, "GITHUB_REF_TYPE": "tag" if tag else "branch", "GITHUB_ENV": str(github_env),
                   "GITHUB_STEP_SUMMARY": str(summary), "HAVE_UPDATER_KEY": "true" if have_key else "false"}
            done = subprocess.run([PWSH, "-NoProfile", "-NonInteractive", "-Command", script],
                                  env=env, cwd=work, capture_output=True, encoding="utf-8", errors="replace", timeout=60)
            read = lambda path: path.read_text(encoding="utf-8-sig") if path.exists() else ""
            return done, read(github_env), read(summary)

    @unittest.skipUnless(PWSH, "PowerShell (pwsh) is not installed")
    def test_a_tag_build_without_either_key_part_is_released_unsigned_with_a_warning(self):
        done, github_env, summary = self.run_updater_check(tag=True, have_key=False, pubkey="")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("PRAXI_UPDATER_MODE=off", github_env)
        self.assertIn("::warning", done.stdout)
        self.assertIn("in-app updates not enabled: run scripts/release/setup-updater-key.ps1", summary)

    @unittest.skipUnless(PWSH, "PowerShell (pwsh) is not installed")
    def test_a_tag_build_with_both_key_parts_signs(self):
        done, github_env, summary = self.run_updater_check(tag=True, have_key=True, pubkey=FIXTURES["pub_a"])
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("PRAXI_UPDATER_MODE=release", github_env)
        self.assertEqual(summary, "")

    @unittest.skipUnless(PWSH, "PowerShell (pwsh) is not installed")
    def test_a_tag_build_with_only_one_key_part_fails(self):
        for have_key, pubkey, expected in ((True, "", "has no updater public key"),
                                           (False, FIXTURES["pub_a"], "secret TAURI_SIGNING_PRIVATE_KEY is not set")):
            with self.subTest(have_key=have_key):
                done, github_env, _ = self.run_updater_check(tag=True, have_key=have_key, pubkey=pubkey)
                self.assertNotEqual(done.returncode, 0, done.stdout)
                self.assertIn(expected, done.stdout + done.stderr)
                self.assertEqual(github_env, "")

    @unittest.skipUnless(PWSH, "PowerShell (pwsh) is not installed")
    def test_a_rehearsal_never_depends_on_the_real_key_and_always_rehearses(self):
        for have_key, pubkey in ((False, ""), (True, FIXTURES["pub_a"])):
            done, github_env, summary = self.run_updater_check(tag=False, have_key=have_key, pubkey=pubkey)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            self.assertIn("PRAXI_UPDATER_MODE=rehearsal", github_env)
            self.assertEqual(summary, "")

    @unittest.skipUnless(PWSH, "PowerShell (pwsh) is not installed")
    def test_the_signing_step_writes_a_feed_the_app_accepts(self):
        """Runs the real signing step against stand-ins for pnpm/git and the fixture signature and key."""
        script = self.step_script("Sign the installer and write the update feed (latest.json)")
        stub = f"""
function git {{ '2026-11-27T10:15:00+02:00' }}
function pnpm {{
    param([Parameter(ValueFromRemainingArguments=$true)][string[]]$A)
    # `pnpm exec tauri signer sign --app-version <v> <file>`: the signing key must arrive trimmed.
    if ($A[0] -ne 'exec' -or $A[1] -ne 'tauri' -or $A[2] -ne 'signer' -or $A[3] -ne 'sign' -or $A[4] -ne '--app-version') {{ throw "unexpected: $A" }}
    if ($env:TAURI_SIGNING_PRIVATE_KEY -ne $env:TAURI_SIGNING_PRIVATE_KEY.Trim()) {{ throw 'key not trimmed' }}
    Set-Content -LiteralPath ($A[6] + '.sig') -Value $env:FIXTURE_SIGNATURE -NoNewline
    $global:LASTEXITCODE = 0
}}
"""
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            (work / "scripts" / "release").mkdir(parents=True)
            shutil.copy(SCRIPT_MANIFEST, work / "scripts" / "release")
            (work / "apps" / "desktop").mkdir(parents=True)
            conf = json.loads((ROOT / "apps" / "desktop" / "tauri.conf.json").read_text(encoding="utf-8"))
            conf["plugins"]["updater"]["pubkey"] = FIXTURES["pub_a"]
            (work / "apps" / "desktop" / "tauri.conf.json").write_text(json.dumps(conf), encoding="utf-8")
            nsis = work / "target" / "release" / "bundle" / "nsis"
            nsis.mkdir(parents=True)
            (nsis / "Praxi-Bot_0.1.0-alpha.2_x64-setup.exe").write_bytes(b"installer")
            env = {**os.environ, "GITHUB_REF_TYPE": "tag", "GITHUB_REF_NAME": "v0.1.0-alpha.2", "GITHUB_SHA": "0" * 40,
                   "TAURI_SIGNING_PRIVATE_KEY": "fixture-private-key\n", "FIXTURE_SIGNATURE": FIXTURES["a_ok"]}
            done = subprocess.run([PWSH, "-NoProfile", "-NonInteractive", "-Command", stub + script],
                                  env=env, cwd=work, capture_output=True, encoding="utf-8", errors="replace", timeout=60)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            feed = json.loads((work / "update-assets" / "latest.json").read_text(encoding="utf-8"))
            self.assertEqual(feed["version"], "0.1.0-alpha.2")
            self.assertEqual(feed["pub_date"], "2026-11-27T08:15:00Z")
            self.assertTrue((work / "update-assets" / "Praxi-Bot_0.1.0-alpha.2_x64-setup.exe.sig").is_file())
            # A signature from another key (a leaked or mismatched secret) stops the release.
            env["FIXTURE_SIGNATURE"] = FIXTURES["b_ok"]
            shutil.rmtree(work / "update-assets")
            wrong = subprocess.run([PWSH, "-NoProfile", "-NonInteractive", "-Command", stub + script],
                                   env=env, cwd=work, capture_output=True, encoding="utf-8", errors="replace", timeout=60)
            self.assertNotEqual(wrong.returncode, 0, wrong.stdout)
            self.assertIn("different key", wrong.stdout + wrong.stderr)
            self.assertFalse((work / "update-assets" / "latest.json").exists())

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
        for needle in ('-setup.exe', 'bot-*.exe', "sbom/*.cdx.json", "verify-windows.ps1", "build-info.txt", "SHA256SUMS.txt",
                       "update-assets/*"):
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


class UpdateFeedWorkflowPolicy(unittest.TestCase):
    """The Preview feed publisher: no secrets, nothing replaced, forward only, only for this repository."""

    def setUp(self):
        self.text = FEED_WORKFLOW.read_text(encoding="utf-8")

    def test_every_action_is_pinned_and_nothing_is_granted_by_default(self):
        for action in re.findall(r"(?m)^\s*(?:-\s*)?uses:\s*(\S+)", self.text):
            self.assertRegex(action, r"^[^@\s]+@[0-9a-f]{40}$", action)
        self.assertRegex(self.text, r"(?m)^permissions: \{\}$")
        self.assertEqual(set(jobs(self.text)), {"preview-feed"})
        self.assertEqual(job_permissions(jobs(self.text)["preview-feed"]), {"contents": "write"})

    def test_it_uses_no_secret_and_never_replaces_or_creates_a_release(self):
        self.assertNotIn("secrets.", self.text)
        self.assertNotIn("--clobber", self.text)
        self.assertNotIn("gh release create", self.text)
        self.assertNotIn("gh release upload", self.text)
        self.assertNotIn("--force", self.text)

    def test_it_only_runs_here_for_a_finished_tag_build_or_by_hand_and_moves_forward_only(self):
        condition = re.search(r"(?ms)^    if: >-\n(.*?)^    runs-on:", self.text).group(1)
        for needle in ("github.repository == 'emreocell/Praxi-Bot'", "workflow_run.conclusion == 'success'",
                       "workflow_run.event == 'push'", "head_repository.full_name == github.repository",
                       "startsWith(github.event.workflow_run.head_branch, 'v')"):
            self.assertIn(needle, condition)
        self.assertIn('workflows: ["Windows Installer"]', self.text)
        self.assertIn("update_manifest.py newer", self.text)
        self.assertIn("update_manifest.py check ", self.text)
        # The tag is validated before it is used and only ever read from the environment.
        self.assertNotIn("${{ inputs.tag }}", self.text.split("steps:", 1)[1])
        self.assertIn("^v(0|[1-9]", self.text)

    def test_the_address_the_app_reads_is_the_one_this_workflow_writes(self):
        conf = json.loads((ROOT / "apps" / "desktop" / "tauri.conf.json").read_text(encoding="utf-8"))
        preview = conf["plugins"]["updater"]["praxiPreviewEndpoint"]
        self.assertEqual(preview, "https://raw.githubusercontent.com/emreocell/Praxi-Bot/update-feed/preview.json")
        self.assertIn("update-feed", self.text)
        self.assertIn("preview.json", self.text)


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

    def run_script(self, *args, gh_exit=0, gh_version="2.80.0", tag="v0.0.0"):
        env = dict(os.environ)
        env.update(
            PATH=str(self.fake_bin) + os.pathsep + env.get("PATH", ""),
            FAKE_GH_LOG=str(self.log),
            FAKE_GH_EXIT=str(gh_exit),
            FAKE_GH_VERSION=gh_version,
        )
        return subprocess.run(
            [PWSH, "-NoProfile", "-File", str(VERIFY), "-InstallerPath", str(self.installer),
             *(["-Tag", tag] if tag else []), *args],
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
        done = self.run_script()
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

    def test_the_release_tag_is_required(self):
        # No -Tag: stop before reading or checking anything, with a plain message and no gh call.
        for extra in ((), ("-SkipProvenance",)):
            done = self.run_script(*extra, tag=None)
            self.assertEqual(done.returncode, 2, done.stdout + done.stderr)
            self.assertIn("-Tag", done.stdout)
            self.assertIn("Nothing was checked", done.stdout)
            self.assertNotIn("Checksum", done.stdout)
        self.assertFalse(self.log.exists())

    def test_failed_provenance_or_old_gh_is_refused(self):
        failed = self.run_script(gh_exit=1)
        self.assertEqual(failed.returncode, 1, failed.stdout)
        self.assertIn("could NOT be verified", failed.stdout)
        old = self.run_script(gh_version="2.79.0")
        self.assertEqual(old.returncode, 1, old.stdout)
        self.assertIn("too old", old.stdout)


if __name__ == "__main__":
    unittest.main()
