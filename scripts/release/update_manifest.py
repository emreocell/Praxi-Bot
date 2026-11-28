#!/usr/bin/env python3
"""Write and check the in-app update feed (`latest.json`, Tauri static JSON format).

The desktop app (tauri-plugin-updater) reads this file, downloads the installer it names and
verifies the installer's minisign signature against the public key built into the app. Nothing here
replaces that check; these helpers only make sure that what the release publishes is a feed the app
will accept, so a broken one fails the release build instead of the owner's users:

* ``make``    writes ``latest.json`` for one installer and its ``.sig`` file;
* ``check``   re-reads a feed (offline) and checks it against the tag and, optionally, the app's
              public key (the signature must have been made by the matching private key and must be
              bound to this exact version, which the app requires);
* ``newer``   tells whether a feed announces a newer version than another one (used before the
              Preview feed is moved forward, so it can never go back to an older release);
* ``check-config`` checks the updater section of ``tauri.conf.json``;
* ``write-pubkey`` puts the public key into ``tauri.conf.json`` (used by setup-updater-key.ps1).

Only the Python standard library is used. Exit status 0 means ok, 1 means refused (a plain message
says why), 2 is a usage error.
"""

from __future__ import annotations

import argparse
import base64
import binascii
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import quote, urlparse

REPO = "emreocell/Praxi-Bot"
PLATFORM = "windows-x86_64"
MAX_NOTES_CHARS = 8000

NUMBER = r"(?:0|[1-9][0-9]*)"
PRERELEASE_ID = rf"(?:{NUMBER}|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
SEMVER = re.compile(
    rf"(?P<core>{NUMBER}\.{NUMBER}\.{NUMBER})"
    rf"(?:-(?P<pre>{PRERELEASE_ID}(?:\.{PRERELEASE_ID})*))?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
)


class Refused(Exception):
    """A plain-language reason why a feed or a key is not acceptable."""


# ---------------------------------------------------------------- versions


def parse_version(text: str):
    """(major, minor, patch, prerelease parts) for a semantic version, or raise Refused."""
    match = SEMVER.fullmatch(text or "")
    if not match:
        raise Refused(f"{text!r} is not a semantic version such as 0.1.0 or 0.1.0-alpha.2")
    major, minor, patch = (int(part) for part in match.group("core").split("."))
    pre = match.group("pre")
    return major, minor, patch, tuple(pre.split(".")) if pre else ()


def version_key(text: str):
    """Sort key following semver precedence (a prerelease is older than its release)."""
    major, minor, patch, pre = parse_version(text)

    def part(identifier: str):
        # Numeric identifiers sort before text ones, numerically; text ones sort as text.
        return (0, int(identifier), "") if identifier.isdigit() else (1, 0, identifier)

    return (major, minor, patch, 1 if not pre else 0, tuple(part(i) for i in pre))


def is_newer(candidate: str, current: str) -> bool:
    return version_key(candidate) > version_key(current)


def tag_version(tag: str) -> str:
    """`v0.1.0-alpha.2` -> `0.1.0-alpha.2`. The release tag always has the leading `v`."""
    if not tag.startswith("v"):
        raise Refused(f"the release tag {tag!r} must start with v")
    version = tag[1:]
    parse_version(version)
    return version


# ----------------------------------------------------------- minisign data


def _decode_b64(text: str, what: str) -> bytes:
    try:
        return base64.b64decode("".join(text.split()), validate=True)
    except (binascii.Error, ValueError):
        raise Refused(f"{what} is not valid base64") from None


def _minisign_text(blob: str, what: str) -> list[str]:
    """The lines of a minisign file as Tauri stores it (base64 of the whole file)."""
    try:
        text = _decode_b64(blob, what).decode("utf-8")
    except UnicodeDecodeError:
        raise Refused(f"{what} is not text") from None
    return [line for line in text.splitlines() if line.strip()]


def pubkey_key_id(pubkey: str) -> bytes:
    """The 8-byte key id inside a public key (the content of the `.pub` file `tauri signer generate` writes)."""
    lines = _minisign_text(pubkey, "the public key")
    if len(lines) != 2 or not lines[0].startswith("untrusted comment:"):
        raise Refused("the public key is not a minisign public key (expected the content of the .pub file)")
    raw = _decode_b64(lines[1], "the public key")
    if len(raw) != 42 or raw[:2] != b"Ed":
        raise Refused("the public key is not a minisign (Ed25519) public key")
    return raw[2:10]


def parse_signature(signature: str) -> tuple[bytes, str]:
    """(key id, trusted comment) of a `.sig` file's content, without checking the signature itself."""
    lines = _minisign_text(signature, "the signature")
    if len(lines) != 4 or not lines[0].startswith("untrusted comment:") or not lines[2].startswith("trusted comment:"):
        raise Refused("the signature is not a minisign signature (expected the content of the .sig file)")
    raw = _decode_b64(lines[1], "the signature")
    if len(raw) != 74 or raw[:2] not in (b"Ed", b"ED"):
        raise Refused("the signature is not a minisign (Ed25519) signature")
    return raw[2:10], lines[2][len("trusted comment:"):].strip()


def signed_version(trusted_comment: str) -> str | None:
    for field in trusted_comment.split("\t"):
        if field.startswith("version:"):
            return field[len("version:"):]
    return None


def check_signature(signature: str, version: str, pubkey: str | None) -> None:
    """The signature must be bound to `version` (the app refuses it otherwise) and, when the public
    key is given, come from the matching private key. The real cryptographic check is done by the
    app before it installs anything."""
    key_id, trusted = parse_signature(signature)
    bound = signed_version(trusted)
    if bound is None:
        raise Refused(
            "the signature does not say which version it was made for; sign with "
            "`tauri signer sign --app-version <version>` (the app refuses unbound signatures)"
        )
    if bound.lstrip("v") != version:
        raise Refused(f"the signature was made for version {bound}, not {version}")
    if pubkey is not None and pubkey_key_id(pubkey) != key_id:
        raise Refused(
            "the signature was made with a different key than the public key built into the app; "
            "the app would refuse this update"
        )


# ------------------------------------------------------------------- feed


def default_notes(version: str, tag: str, repo: str) -> str:
    link = f"https://github.com/{repo}/releases/tag/{quote(tag)}"
    return (
        f"Praxi-Bot {version} is available. Details: {link}\n"
        f"<!-- lang: en -->\nPraxi-Bot {version} is available. Details: {link}\n"
        f"<!-- lang: tr -->\nPraxi-Bot {version} yayınlandı. Ayrıntılar: {link}\n"
        f"<!-- lang: de -->\nPraxi-Bot {version} ist verfügbar. Details: {link}\n"
    )


def rfc3339_utc(text: str) -> str:
    """Any ISO 8601 time with an offset (what `git show --format=%cI` prints) as `...Z`."""
    try:
        moment = datetime.fromisoformat(text.strip().replace("Z", "+00:00"))
    except ValueError:
        raise Refused(f"{text!r} is not an ISO 8601 date") from None
    if moment.tzinfo is None:
        raise Refused(f"{text!r} has no time zone")
    return moment.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def installer_url(repo: str, tag: str, installer_name: str) -> str:
    return f"https://github.com/{repo}/releases/download/{quote(tag)}/{quote(installer_name)}"


def build_manifest(
    *, version: str, tag: str, installer_name: str, signature: str, pub_date: str,
    notes: str | None = None, repo: str = REPO, pubkey: str | None = None,
) -> dict:
    """The feed for one release. Raises Refused when anything would make the app reject it."""
    parse_version(version)
    if tag_version(tag) != version:
        raise Refused(f"the tag {tag} does not match the version {version}")
    if not installer_name.endswith("-setup.exe") or "/" in installer_name or "\\" in installer_name:
        raise Refused(f"{installer_name!r} is not an NSIS installer file name (*-setup.exe)")
    if version not in installer_name:
        raise Refused(f"the installer {installer_name!r} was not built for version {version}")
    signature = signature.strip()
    check_signature(signature, version, pubkey)
    text = (notes if notes is not None else default_notes(version, tag, repo)).strip()
    if not text:
        raise Refused("the release notes are empty")
    return {
        "version": version,
        "notes": text[:MAX_NOTES_CHARS],
        "pub_date": rfc3339_utc(pub_date),
        "platforms": {
            PLATFORM: {"url": installer_url(repo, tag, installer_name), "signature": signature}
        },
    }


def render(manifest: dict) -> str:
    """Stable text: the same input always gives the same bytes (a release asset is never replaced)."""
    return json.dumps(manifest, indent=2, ensure_ascii=False) + "\n"


def check_manifest(manifest: object, *, tag: str | None, pubkey: str | None, repo: str = REPO) -> dict:
    """Everything the app needs from a feed, checked offline. Returns the manifest."""
    if not isinstance(manifest, dict):
        raise Refused("the feed is not a JSON object")
    version = manifest.get("version")
    if not isinstance(version, str):
        raise Refused("the feed has no version")
    parse_version(version)
    if tag is not None and tag_version(tag) != version:
        raise Refused(f"the feed announces {version} but the release tag is {tag}")
    notes = manifest.get("notes")
    if not isinstance(notes, str) or not notes.strip():
        raise Refused("the feed has no release notes")
    rfc3339_utc(str(manifest.get("pub_date", "")))
    platforms = manifest.get("platforms")
    entry = platforms.get(PLATFORM) if isinstance(platforms, dict) else None
    if not isinstance(entry, dict):
        raise Refused(f"the feed has no {PLATFORM} entry")
    url = urlparse(str(entry.get("url", "")))
    if url.scheme != "https" or url.netloc != "github.com":
        raise Refused("the installer must be downloaded over https from github.com")
    prefix = f"/{repo}/releases/download/"
    if not url.path.startswith(prefix):
        raise Refused(f"the installer must be a release asset of {repo}")
    parts = url.path[len(prefix):].split("/")
    if len(parts) != 2 or not parts[1].endswith("-setup.exe"):
        raise Refused("the installer URL must point at a *-setup.exe release asset")
    if tag is not None and parts[0] != quote(tag):
        raise Refused(f"the installer URL points at release {parts[0]}, not {tag}")
    signature = entry.get("signature")
    if not isinstance(signature, str) or not signature.strip():
        raise Refused("the feed carries no signature for the installer")
    check_signature(signature, version, pubkey)
    return manifest


def load_manifest(path: str) -> dict:
    try:
        return json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise Refused(f"cannot read the feed {path}: {error}") from None


# ----------------------------------------------------------- app config


def check_config(config: object, *, require_pubkey: bool) -> None:
    """The `plugins.updater` section of tauri.conf.json."""
    updater = config.get("plugins", {}).get("updater") if isinstance(config, dict) else None
    if not isinstance(updater, dict):
        raise Refused("tauri.conf.json has no plugins.updater section")
    if updater.get("requireSignedVersion") is not True:
        raise Refused("plugins.updater.requireSignedVersion must stay true")
    for key in updater:
        if key.lower().startswith("dangerous"):
            raise Refused(f"plugins.updater.{key} weakens the update checks and must not be set")
    if updater.get("allowDowngrades") or updater.get("allow-downgrades"):
        raise Refused("plugins.updater.allowDowngrades must not be enabled")
    pubkey = str(updater.get("pubkey", "")).strip()
    if pubkey:
        pubkey_key_id(pubkey)
    elif require_pubkey:
        raise Refused(
            "tauri.conf.json has no updater public key yet. Run scripts/release/setup-updater-key.ps1 "
            "once and commit the change it makes (docs/updates.md)"
        )
    endpoints = list(updater.get("endpoints") or [])
    endpoints += [updater[key] for key in ("praxiPreviewEndpoint", "praxiRollbackEndpoint") if updater.get(key)]
    for endpoint in endpoints:
        if urlparse(str(endpoint)).scheme != "https":
            raise Refused(f"update address {endpoint!r} is not https")
    if require_pubkey and not updater.get("endpoints"):
        raise Refused("plugins.updater.endpoints is empty: the app would not know where to look")


def write_pubkey(config_path: Path, pubkey: str, *, replace: bool) -> None:
    """Sets `plugins.updater.pubkey` without reformatting the file."""
    pubkey_key_id(pubkey)
    text = config_path.read_bytes().decode("utf-8")
    pattern = re.compile(r'("updater"\s*:\s*\{[^{}]*?"pubkey"\s*:\s*)"([^"]*)"', re.S)
    match = pattern.search(text)
    if not match:
        raise Refused(f'{config_path} has no "pubkey" entry in plugins.updater')
    if match.group(2).strip() and not replace:
        raise Refused(
            "tauri.conf.json already has an updater public key. Replacing it would stop every "
            "installed copy from updating; pass --replace only if that is really what you want"
        )
    config_path.write_bytes(
        (text[: match.start()] + match.group(1) + json.dumps(pubkey.strip()) + text[match.end():]).encode("utf-8")
    )


# -------------------------------------------------------------------- cli


def read_text(path: str) -> str:
    try:
        return Path(path).read_text(encoding="utf-8")
    except OSError as error:
        raise Refused(f"cannot read {path}: {error}") from None


def read_pubkey(args) -> str | None:
    if getattr(args, "pubkey_file", None):
        return read_text(args.pubkey_file).strip()
    if getattr(args, "config", None):
        updater = json.loads(read_text(args.config)).get("plugins", {}).get("updater", {})
        pubkey = str(updater.get("pubkey", "")).strip()
        if not pubkey:
            raise Refused(f"{args.config} has no updater public key to check against")
        return pubkey
    return None


def cmd_make(args) -> None:
    installer = Path(args.installer)
    manifest = build_manifest(
        version=tag_version(args.tag) if args.version is None else args.version,
        tag=args.tag,
        installer_name=installer.name,
        signature=read_text(args.signature),
        pub_date=args.pub_date,
        notes=read_text(args.notes_file) if args.notes_file else None,
        repo=args.repo,
        pubkey=read_pubkey(args),
    )
    Path(args.out).write_text(render(manifest), encoding="utf-8", newline="\n")
    print(f"wrote {args.out}: version {manifest['version']}, {installer.name}")


def cmd_check(args) -> None:
    manifest = check_manifest(load_manifest(args.manifest), tag=args.tag, pubkey=read_pubkey(args), repo=args.repo)
    print(f"{args.manifest}: ok, version {manifest['version']}")


def cmd_newer(args) -> None:
    candidate = load_manifest(args.candidate)["version"]
    if not Path(args.current).exists():
        print(f"no current feed: {candidate} is accepted")
        return
    current = load_manifest(args.current)["version"]  # a damaged file stops the move
    if not is_newer(candidate, current):
        raise Refused(f"{candidate} is not newer than {current}; the feed is not moved back")
    print(f"{candidate} is newer than {current}")


def cmd_check_config(args) -> None:
    config = json.loads(read_text(args.config))
    check_config(config, require_pubkey=args.require_pubkey)
    print(f"{args.config}: updater configuration ok")


def cmd_write_pubkey(args) -> None:
    write_pubkey(Path(args.config), read_text(args.pubkey_file), replace=args.replace)
    print(f"public key written to {args.config}")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = root.add_subparsers(dest="command", required=True)

    def key_source(sub):
        group = sub.add_mutually_exclusive_group()
        group.add_argument("--pubkey-file", help="the .pub file of the signing key")
        group.add_argument("--config", help="tauri.conf.json: use its updater public key")

    make = commands.add_parser("make", help="write latest.json")
    make.add_argument("--tag", required=True, help="release tag, e.g. v0.1.0-alpha.3")
    make.add_argument("--version", help="defaults to the tag without its v")
    make.add_argument("--installer", required=True, help="path of the *-setup.exe (only its name is used)")
    make.add_argument("--signature", required=True, help="path of the .sig file")
    make.add_argument("--pub-date", required=True, help="ISO 8601, e.g. the commit time of the tag")
    make.add_argument("--notes-file", help="release notes (plain text, optional <!-- lang: xx --> blocks)")
    make.add_argument("--repo", default=REPO)
    make.add_argument("--out", required=True)
    key_source(make)
    make.set_defaults(run=cmd_make)

    check = commands.add_parser("check", help="check a feed offline")
    check.add_argument("--manifest", required=True)
    check.add_argument("--tag", help="the release tag the feed must belong to")
    check.add_argument("--repo", default=REPO)
    key_source(check)
    check.set_defaults(run=cmd_check)

    newer = commands.add_parser("newer", help="succeed only if the candidate feed is newer than the current one")
    newer.add_argument("--candidate", required=True)
    newer.add_argument("--current", required=True)
    newer.set_defaults(run=cmd_newer)

    cfg = commands.add_parser("check-config", help="check plugins.updater of tauri.conf.json")
    cfg.add_argument("--config", required=True)
    cfg.add_argument("--require-pubkey", action="store_true", help="refuse an empty public key (release builds)")
    cfg.set_defaults(run=cmd_check_config)

    pub = commands.add_parser("write-pubkey", help="store the public key in tauri.conf.json")
    pub.add_argument("--config", required=True)
    pub.add_argument("--pubkey-file", required=True)
    pub.add_argument("--replace", action="store_true")
    pub.set_defaults(run=cmd_write_pubkey)
    return root


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        args.run(args)
    except Refused as reason:
        print(f"refused: {reason}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
