"""The in-app update feed: manifest generation, offline checks and the app's updater configuration.

Hermetic: no network, no Tauri CLI. `fixtures/update_signatures.json` holds PUBLIC values only
(public keys and signatures made with two throwaway keys whose private halves were discarded; the
same file is used by apps/desktop/tests/update_server.rs). The real cryptographic check is done by
the app; these tests cover everything around it.
"""

import base64
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "release" / "update_manifest.py"
TAURI_CONF = ROOT / "apps" / "desktop" / "tauri.conf.json"
FIXTURES = json.loads((Path(__file__).parent / "fixtures" / "update_signatures.json").read_text(encoding="utf-8"))

sys.path.insert(0, str(SCRIPT.parent))
import update_manifest as um  # noqa: E402

VERSION = "0.1.0-alpha.2"
TAG = "v0.1.0-alpha.2"
INSTALLER = "Praxi-Bot_0.1.0-alpha.2_x64-setup.exe"
PUB_DATE = "2026-11-27T10:15:00+02:00"


def make(**overrides):
    args = dict(version=VERSION, tag=TAG, installer_name=INSTALLER, signature=FIXTURES["a_ok"],
                pub_date=PUB_DATE, pubkey=FIXTURES["pub_a"])
    args.update(overrides)
    return um.build_manifest(**args)


class Versions(unittest.TestCase):
    def test_semver_precedence(self):
        ordered = ["0.1.0-alpha.1", "0.1.0-alpha.2", "0.1.0-alpha.10", "0.1.0-beta.1", "0.1.0-rc.1", "0.1.0", "0.1.1", "0.2.0", "1.0.0"]
        for older, newer in zip(ordered, ordered[1:]):
            self.assertTrue(um.is_newer(newer, older), f"{newer} should be newer than {older}")
            self.assertFalse(um.is_newer(older, newer), f"{older} should not be newer than {newer}")
        self.assertFalse(um.is_newer("0.1.0", "0.1.0"))

    def test_bad_versions_and_tags_are_refused(self):
        for bad in ("", "1.0", "01.0.0", "1.0.0-", "latest", "1.0.0\n"):
            with self.assertRaises(um.Refused, msg=bad):
                um.parse_version(bad)
        with self.assertRaises(um.Refused):
            um.tag_version("0.1.0")  # the release tag always starts with v
        self.assertEqual(um.tag_version(TAG), VERSION)


class Signatures(unittest.TestCase):
    def test_the_key_id_of_a_signature_matches_its_public_key(self):
        key_a, trusted = um.parse_signature(FIXTURES["a_ok"])
        self.assertEqual(key_a, um.pubkey_key_id(FIXTURES["pub_a"]))
        self.assertNotEqual(key_a, um.pubkey_key_id(FIXTURES["pub_b"]))
        self.assertEqual(um.signed_version(trusted), VERSION)

    def test_a_signature_from_another_key_is_refused(self):
        with self.assertRaisesRegex(um.Refused, "different key"):
            um.check_signature(FIXTURES["b_ok"], VERSION, FIXTURES["pub_a"])

    def test_a_signature_for_another_version_or_without_one_is_refused(self):
        with self.assertRaisesRegex(um.Refused, "made for version 0.1.0-alpha.1"):
            um.check_signature(FIXTURES["a_old"], VERSION, FIXTURES["pub_a"])
        with self.assertRaisesRegex(um.Refused, "does not say which version"):
            um.check_signature(FIXTURES["a_legacy"], VERSION, FIXTURES["pub_a"])

    def test_garbage_is_refused_not_crashed_on(self):
        for bad in ("", "not base64!", base64.b64encode(b"hello").decode(),
                    base64.b64encode(b"untrusted comment: x\nAAAA\ntrusted comment: y\nBBBB").decode()):
            with self.assertRaises(um.Refused, msg=bad):
                um.parse_signature(bad)
            with self.assertRaises(um.Refused, msg=bad):
                um.pubkey_key_id(bad)


class Manifest(unittest.TestCase):
    def test_the_feed_has_what_the_app_reads(self):
        manifest = make()
        self.assertEqual(manifest["version"], VERSION)
        self.assertEqual(manifest["pub_date"], "2026-11-27T08:15:00Z")
        entry = manifest["platforms"]["windows-x86_64"]
        self.assertEqual(entry["url"], f"https://github.com/emreocell/Praxi-Bot/releases/download/{TAG}/{INSTALLER}")
        self.assertEqual(entry["signature"], FIXTURES["a_ok"])
        # Notes carry the language blocks the app's pick_notes() understands.
        for marker in ("<!-- lang: en -->", "<!-- lang: tr -->", "<!-- lang: de -->"):
            self.assertIn(marker, manifest["notes"])
        um.check_manifest(manifest, tag=TAG, pubkey=FIXTURES["pub_a"])

    def test_the_same_input_gives_the_same_bytes(self):
        # A release asset is never replaced, so a re-run must not change the file.
        self.assertEqual(um.render(make()), um.render(make()))
        self.assertTrue(um.render(make()).endswith("}\n"))

    def test_custom_notes_replace_the_default(self):
        notes = "Default\n<!-- lang: tr -->\nYenilikler\n"
        self.assertEqual(make(notes=notes)["notes"], notes.strip())
        with self.assertRaisesRegex(um.Refused, "notes are empty"):
            make(notes="  \n")

    def test_a_feed_that_the_app_would_refuse_is_not_written(self):
        with self.assertRaisesRegex(um.Refused, "different key"):
            make(signature=FIXTURES["b_ok"])
        with self.assertRaisesRegex(um.Refused, "tag v0.1.0-alpha.3 does not match"):
            make(tag="v0.1.0-alpha.3")
        with self.assertRaisesRegex(um.Refused, "not built for version"):
            make(installer_name="Praxi-Bot_0.1.0-alpha.1_x64-setup.exe")
        with self.assertRaisesRegex(um.Refused, "NSIS installer"):
            make(installer_name="Praxi-Bot_0.1.0-alpha.2_x64.zip")
        with self.assertRaisesRegex(um.Refused, "NSIS installer"):
            make(installer_name="../Praxi-Bot_0.1.0-alpha.2_x64-setup.exe")

    def test_check_refuses_a_tampered_feed(self):
        good = make()

        def tampered(change):
            manifest = json.loads(json.dumps(good))
            change(manifest)
            return manifest

        cases = {
            "announces": lambda m: m.update(version="0.1.0-alpha.9"),
            "no release notes": lambda m: m.update(notes=" "),
            "windows-x86_64 entry": lambda m: m["platforms"].clear(),
            "https from github.com": lambda m: m["platforms"]["windows-x86_64"].update(url="http://github.com/emreocell/Praxi-Bot/releases/download/v0.1.0-alpha.2/" + INSTALLER),
            "https from github.com ": lambda m: m["platforms"]["windows-x86_64"].update(url="https://example.com/" + INSTALLER),
            "release asset of": lambda m: m["platforms"]["windows-x86_64"].update(url="https://github.com/someone/else/releases/download/v0.1.0-alpha.2/" + INSTALLER),
            "points at release": lambda m: m["platforms"]["windows-x86_64"].update(url=f"https://github.com/emreocell/Praxi-Bot/releases/download/v0.1.0-alpha.1/{INSTALLER}"),
            "no signature": lambda m: m["platforms"]["windows-x86_64"].update(signature=""),
            "different key": lambda m: m["platforms"]["windows-x86_64"].update(signature=FIXTURES["b_ok"]),
        }
        for message, change in cases.items():
            with self.assertRaisesRegex(um.Refused, message.strip(), msg=message):
                um.check_manifest(tampered(change), tag=TAG, pubkey=FIXTURES["pub_a"])
        with self.assertRaises(um.Refused):
            um.check_manifest([], tag=TAG, pubkey=None)

    def test_the_announced_version_matches_what_the_server_side_update_expects(self):
        # `bot host add` and the managed Linux updater work with the release TAG (`v` + version, see
        # valid_release_tag in crates/daemon-api/src/host.rs). The desktop's own version is the tag
        # without its v, so a desktop that updates itself also knows which tag the server must reach.
        manifest = make()
        self.assertEqual("v" + manifest["version"], TAG)
        self.assertRegex(TAG, r"^v[A-Za-z0-9.+-]{1,99}$")


class Config(unittest.TestCase):
    def test_the_checked_in_configuration_is_valid_and_keeps_verification_on(self):
        config = json.loads(TAURI_CONF.read_text(encoding="utf-8"))
        um.check_config(config, require_pubkey=False)
        updater = config["plugins"]["updater"]
        self.assertTrue(updater["requireSignedVersion"])
        for endpoint in updater["endpoints"] + [updater["praxiPreviewEndpoint"]]:
            self.assertTrue(endpoint.startswith("https://"), endpoint)

    def test_a_release_build_needs_a_public_key(self):
        config = json.loads(TAURI_CONF.read_text(encoding="utf-8"))
        config["plugins"]["updater"]["pubkey"] = ""
        with self.assertRaisesRegex(um.Refused, "no updater public key yet"):
            um.check_config(config, require_pubkey=True)
        config["plugins"]["updater"]["pubkey"] = FIXTURES["pub_a"]
        um.check_config(config, require_pubkey=True)
        config["plugins"]["updater"]["pubkey"] = "garbage"
        with self.assertRaises(um.Refused):
            um.check_config(config, require_pubkey=True)

    def test_signature_checking_cannot_be_switched_off_or_pointed_at_plain_http(self):
        config = json.loads(TAURI_CONF.read_text(encoding="utf-8"))
        config["plugins"]["updater"]["requireSignedVersion"] = False
        with self.assertRaisesRegex(um.Refused, "requireSignedVersion"):
            um.check_config(config, require_pubkey=False)
        config["plugins"]["updater"]["requireSignedVersion"] = True
        for weakening in ({"dangerousInsecureTransportProtocol": True}, {"dangerousAcceptInvalidCerts": False},
                           {"allowDowngrades": True}):
            weak = json.loads(json.dumps(config))
            weak["plugins"]["updater"].update(weakening)
            with self.assertRaises(um.Refused, msg=str(weakening)):
                um.check_config(weak, require_pubkey=False)
        config["plugins"]["updater"]["praxiPreviewEndpoint"] = "http://example.com/preview.json"
        with self.assertRaisesRegex(um.Refused, "not https"):
            um.check_config(config, require_pubkey=False)

    def test_the_public_key_is_written_without_reformatting_and_never_replaced_silently(self):
        original = TAURI_CONF.read_bytes().decode("utf-8")
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "tauri.conf.json"
            path.write_bytes(original.encode("utf-8"))
            um.write_pubkey(path, FIXTURES["pub_a"], replace=False)
            written = path.read_bytes().decode("utf-8")
            self.assertEqual(json.loads(written)["plugins"]["updater"]["pubkey"], FIXTURES["pub_a"])
            # Only the one value changed (line endings and layout are kept).
            self.assertEqual(written.replace(FIXTURES["pub_a"], ""), original)
            with self.assertRaisesRegex(um.Refused, "already has an updater public key"):
                um.write_pubkey(path, FIXTURES["pub_b"], replace=False)
            um.write_pubkey(path, FIXTURES["pub_b"], replace=True)
            self.assertEqual(json.loads(path.read_text(encoding="utf-8"))["plugins"]["updater"]["pubkey"], FIXTURES["pub_b"])
            with self.assertRaises(um.Refused):
                um.write_pubkey(path, "not a key", replace=True)


class CommandLine(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        (self.dir / "a.sig").write_text(FIXTURES["a_ok"] + "\n", encoding="utf-8")
        (self.dir / "a.pub").write_text(FIXTURES["pub_a"] + "\n", encoding="utf-8")
        (self.dir / "b.pub").write_text(FIXTURES["pub_b"] + "\n", encoding="utf-8")

    def run_cli(self, *args):
        return subprocess.run([sys.executable, "-B", str(SCRIPT), *map(str, args)], capture_output=True, text=True, timeout=60)

    def make_args(self, pub="a.pub"):
        return ["make", "--tag", TAG, "--installer", self.dir / INSTALLER, "--signature", self.dir / "a.sig",
                "--pub-date", PUB_DATE, "--pubkey-file", self.dir / pub, "--out", self.dir / "latest.json"]

    def test_make_then_check(self):
        done = self.run_cli(*self.make_args())
        self.assertEqual(done.returncode, 0, done.stderr)
        feed = json.loads((self.dir / "latest.json").read_text(encoding="utf-8"))
        self.assertEqual(feed["version"], VERSION)
        for pub, code in (("a.pub", 0), ("b.pub", 1)):
            checked = self.run_cli("check", "--manifest", self.dir / "latest.json", "--tag", TAG, "--pubkey-file", self.dir / pub)
            self.assertEqual(checked.returncode, code, checked.stdout + checked.stderr)
        self.assertIn("different key", checked.stderr)

    def test_a_config_without_a_public_key_cannot_be_used_to_check(self):
        conf = self.dir / "tauri.conf.json"
        conf.write_text(TAURI_CONF.read_text(encoding="utf-8"), encoding="utf-8")  # pubkey is empty here
        self.assertEqual(self.run_cli(*self.make_args()).returncode, 0)
        checked = self.run_cli("check", "--manifest", self.dir / "latest.json", "--tag", TAG, "--config", conf)
        self.assertEqual(checked.returncode, 1)
        self.assertIn("no updater public key", checked.stderr)

    def test_make_refuses_the_wrong_key_and_writes_nothing(self):
        done = self.run_cli(*self.make_args(pub="b.pub"))
        self.assertEqual(done.returncode, 1)
        self.assertIn("refused:", done.stderr)
        self.assertFalse((self.dir / "latest.json").exists())

    def test_the_feed_only_moves_forward(self):
        def feed(name, version):
            manifest = make()
            manifest["version"] = version
            (self.dir / name).write_text(json.dumps(manifest), encoding="utf-8")

        feed("old.json", "0.1.0-alpha.2")
        feed("new.json", "0.1.0-alpha.3")
        newer = self.run_cli("newer", "--candidate", self.dir / "new.json", "--current", self.dir / "old.json")
        self.assertEqual(newer.returncode, 0, newer.stderr)
        for candidate, current in (("old.json", "new.json"), ("old.json", "old.json")):
            back = self.run_cli("newer", "--candidate", self.dir / candidate, "--current", self.dir / current)
            self.assertEqual(back.returncode, 1)
            self.assertIn("not moved back", back.stderr)
        first = self.run_cli("newer", "--candidate", self.dir / "new.json", "--current", self.dir / "missing.json")
        self.assertEqual(first.returncode, 0)
        (self.dir / "broken.json").write_text("{not json", encoding="utf-8")
        broken = self.run_cli("newer", "--candidate", self.dir / "new.json", "--current", self.dir / "broken.json")
        self.assertEqual(broken.returncode, 1, "a damaged current feed must stop the move, not open it")

    def test_check_config_on_the_real_file(self):
        self.assertEqual(self.run_cli("check-config", "--config", TAURI_CONF).returncode, 0)


if __name__ == "__main__":
    unittest.main()
