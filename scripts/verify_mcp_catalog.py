#!/usr/bin/env python3
"""Verify every entry of the MCP catalog: offline by default, live on request.

Offline (default, no network, safe for pull requests): every entry has a valid shape; a stdio
entry runs a package pinned to an exact version; the licence and the sign-in type are noted and
agree with the install recipe; the logo is one whose licence is known (a Simple Icons mark under
CC0, a note in THIRD_PARTY_NOTICES.md, or none); and the guide's "what do I need" table is the one
generated from the catalog. Any of these failing exits 2.

Live (`--live`, a scheduled job or by hand): additionally asks the outside world, with no
credentials, whether the pinned package exists on npm / PyPI (and says so when its registry licence
differs from the noted one), whether the homepage answers, and, for remote entries, what the
credential-free endpoint check of `check_mcp_catalog.py` finds (reachable, OAuth metadata, dynamic
client registration) and whether that matches the declared sign-in. A dead or changed vendor is an
"attention" row, never a failed run; `--strict` turns attention into exit 1 for manual use.

    python3 scripts/verify_mcp_catalog.py                      # offline
    python3 scripts/verify_mcp_catalog.py --live --timeout 5   # also ask the registries and vendors
    python3 scripts/verify_mcp_catalog.py --render-docs en     # the generated guide table

See docs/contributing/MAINTAINING.md.
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import sys
import urllib.parse

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_mcp_catalog as endpoint_check  # noqa: E402  (sibling script; shares its safe fetcher)

ROOT = Path(__file__).resolve().parents[1]
AUTH_TYPES = ("none", "oauth-dcr", "oauth-own-client", "api-key")
HOSTED = "hosted"
NPM_REGISTRY = "https://registry.npmjs.org"
PYPI_REGISTRY = "https://pypi.org/pypi"
MAX_REDIRECTS = 3
BRAND_ICONS = Path("apps/desktop/ui/src/lib/brandIcons.ts")
# Marks that are not Simple Icons (CC0), with the file that records where their artwork comes from.
LOGO_NOTED_IN_NOTICES = {"slack": "Slack"}
NOTICES = Path("THIRD_PARTY_NOTICES.md")
DOCS = {"en": Path("docs/guide/app-catalog.md"), "tr": Path("docs/guide/tr/app-catalog.md")}
TABLE_BEGIN = "<!-- BEGIN:app-catalog-table -->"
TABLE_END = "<!-- END:app-catalog-table -->"
EXACT_VERSION = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?(?:\+[0-9A-Za-z.]+)?$")
ID_PATTERN = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
# An SPDX-looking expression: ids joined by AND / OR / WITH, optional parentheses.
SPDX_PATTERN = re.compile(r"^[A-Za-z0-9.+-]+(?:\s+(?:AND|OR|WITH)\s+[A-Za-z0-9.+-]+)*$")
ATTENTION = endpoint_check.ATTENTION | {
    "package-missing",
    "package-unreachable",
    "homepage-unreachable",
    "auth-mismatch",
    "license-mismatch",
}

LABELS = {
    "en": {
        "auth": {
            "none": "Nothing to set up",
            "oauth-dcr": "Sign in with your account in the browser",
            "oauth-own-client": "Sign in with an OAuth client you create yourself",
            "api-key": "Paste an API key, access key or connection string",
        },
        "hosted": "Hosted by the vendor",
        "head": ["App", "What you need", "Runs", "Server licence"],
        "runs": {"http": "Vendor's server", "stdio": "On this computer"},
    },
    "tr": {
        "auth": {
            "none": "Ayar gerekmez",
            "oauth-dcr": "Tarayıcıda hesabınla oturum aç",
            "oauth-own-client": "Kendi oluşturduğun bir OAuth istemcisiyle oturum aç",
            "api-key": "Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır",
        },
        "hosted": "Sağlayıcı barındırır",
        "head": ["Uygulama", "Ne gerekir", "Nerede çalışır", "Sunucu lisansı"],
        "runs": {"http": "Sağlayıcının sunucusu", "stdio": "Bu bilgisayar"},
    },
}


# ---------------------------------------------------------------- package pins


def command_base(command: str) -> str:
    base = command.replace("\\", "/").rsplit("/", 1)[-1].lower()
    for ext in (".exe", ".cmd", ".bat"):
        if base.endswith(ext):
            return base[: -len(ext)]
    return base


def _runner(command: str, args: list[str]):
    """(ecosystem, arguments the runner parses), or None when the command is not a package runner."""
    base = command_base(command)
    first = args[0] if args else None
    if base in ("npx", "bunx", "dlx"):
        return "npm", args
    if base == "npm" and first in ("exec", "x"):
        return "npm", args[1:]
    if base in ("pnpm", "yarn") and first == "dlx":
        return "npm", args[1:]
    if base == "uvx":
        return "pypi", args
    if base == "uv" and args[:2] == ["tool", "run"]:
        return "pypi", args[2:]
    if base == "pipx" and first == "run":
        return "pypi", args[1:]
    return None


def split_pin(spec: str, ecosystem: str) -> tuple[str, str] | None:
    """(name, version) of `name@1.2.3` / `name==1.2.3`, or None when it is not an exact pin."""
    if ecosystem == "pypi" and "==" in spec:
        name, _, version = spec.partition("==")
        name = name.split("[", 1)[0]
    elif "@" in spec[1:]:
        name, _, version = spec.rpartition("@")
    else:
        return None
    if not name or name.startswith(("-", "=")) or not EXACT_VERSION.match(version):
        return None
    return name, version


def package_of(install: dict) -> tuple[str, str, str] | None | str:
    """(ecosystem, name, version) of the pinned package a stdio entry runs.

    None: the command is not a package runner (nothing to look up). A string: the reason it is
    not an exact pin.
    """
    command = install.get("command") or ""
    args = [a for a in install.get("args") or [] if isinstance(a, str)]
    runner = _runner(command, args)
    if runner is None:
        return None
    ecosystem, rest = runner
    spec = None
    index = 0
    while index < len(rest):
        arg = rest[index]
        if arg in ("-p", "--package", "--from"):
            spec = rest[index + 1] if index + 1 < len(rest) else None
            break
        if arg.startswith(("--package=", "--from=")):
            spec = arg.split("=", 1)[1]
            break
        if not arg.startswith("-"):
            spec = arg
            break
        index += 1
    if not spec:
        return "no package named"
    pin = split_pin(spec, ecosystem)
    if pin is None:
        return f"`{spec}` is not pinned to an exact version"
    return ecosystem, pin[0], pin[1]


# ----------------------------------------------------------------- offline part


def known_logo_slugs(root: Path) -> set[str]:
    """Slugs of the Simple Icons (CC0) marks the desktop bundles, read from brandIcons.ts."""
    source = (root / BRAND_ICONS).read_text(encoding="utf-8")
    match = re.search(r"import \{([^}]*)\} from 'simple-icons'", source)
    if not match:
        raise ValueError(f"no simple-icons import found in {BRAND_ICONS}")
    return {name.strip()[2:].lower() for name in match.group(1).split(",") if name.strip().startswith("si")}


def logo_problem(entry: dict, slugs: set[str], notices: str) -> str | None:
    icon = entry.get("icon")
    if icon is None:
        return None  # no artwork at all: the app draws neutral initials
    if not isinstance(icon, str):
        return "icon is not a string or null"
    if icon in slugs:
        return None  # Simple Icons, CC0 1.0
    needle = LOGO_NOTED_IN_NOTICES.get(icon)
    if needle and needle in notices:
        return None
    return f"logo `{icon}` has no licence noted: use a Simple Icons slug (CC0), record it in THIRD_PARTY_NOTICES.md, or set icon to null"


def declared_auth_problem(entry: dict) -> str | None:
    """The licence is noted, the sign-in is declared, and the declaration agrees with the recipe."""
    license_ = entry.get("license")
    if not isinstance(license_, str) or not license_.strip():
        return "license is not noted (an SPDX expression, or `hosted`)"
    if license_ != HOSTED and not SPDX_PATTERN.match(license_):
        return f"license {license_!r} is neither `hosted` nor an SPDX expression"
    auth = entry.get("auth")
    if auth not in AUTH_TYPES:
        return f"auth {auth!r} is not declared (one of {', '.join(AUTH_TYPES)})"
    install = entry["install"]
    secrets = bool(install.get("required_secrets"))
    own_client = bool(install.get("oauth_client"))
    remote = install.get("transport") == "http"
    agrees = {
        "api-key": secrets and not own_client,
        "oauth-own-client": own_client and not secrets,
        "oauth-dcr": remote and not secrets and not own_client,
        "none": not secrets and not own_client,
    }[auth]
    if not agrees:
        return f"auth {auth!r} does not match required_secrets / oauth_client / transport"
    if remote and license_ != HOSTED:
        return "a remote entry installs nothing: its license must be `hosted`"
    if not remote and license_ == HOSTED:
        return "a local entry installs software: note its real license, not `hosted`"
    return None


def offline_errors(catalog: list, root: Path = ROOT) -> list[str]:
    """Everything that can be said about the catalog without the network."""
    errors = list(endpoint_check.structural_errors(catalog))
    slugs = known_logo_slugs(root)
    notices = (root / NOTICES).read_text(encoding="utf-8")
    for entry in catalog:
        if not isinstance(entry, dict) or not isinstance(entry.get("install"), dict):
            continue  # already reported by the structural check
        label = str(entry.get("id"))
        if isinstance(entry.get("id"), str) and not ID_PATTERN.match(entry["id"]):
            errors.append(f"{label}: id must be lower-case words joined by `-`")
        for field in ("name", "description", "category"):
            if not isinstance(entry.get(field), str) or not entry[field].strip():
                errors.append(f"{label}: missing {field}")
        homepage = entry.get("homepage")
        if not isinstance(homepage, str) or not homepage.startswith("https://") or not urllib.parse.urlsplit(homepage).hostname:
            errors.append(f"{label}: homepage must be an https URL")
        if (problem := declared_auth_problem(entry)) is not None:
            errors.append(f"{label}: {problem}")
        if (problem := logo_problem(entry, slugs, notices)) is not None:
            errors.append(f"{label}: {problem}")
        if entry["install"].get("transport") == "stdio":
            package = package_of(entry["install"])
            if isinstance(package, str):
                errors.append(f"{label}: {package}")
    return errors


# ---------------------------------------------------------------- generated docs


def render_docs_table(catalog: list, lang: str) -> str:
    text = LABELS[lang]
    rows = ["| " + " | ".join(text["head"]) + " |", "| --- | --- | --- | --- |"]
    for entry in sorted(catalog, key=lambda e: e["name"].lower()):
        install = entry["install"]
        license_ = text["hosted"] if entry["license"] == HOSTED else entry["license"]
        rows.append(
            f"| [{entry['name']}]({entry['homepage']}) | {text['auth'][entry['auth']]} "
            f"| {text['runs'][install['transport']]} | {license_} |"
        )
    return "\n".join(rows)


def docs_errors(catalog: list, root: Path = ROOT) -> list[str]:
    errors = []
    for lang, relative in DOCS.items():
        path = root / relative
        if not path.is_file():
            errors.append(f"{relative}: guide page is missing (run --render-docs {lang})")
            continue
        text = path.read_text(encoding="utf-8")
        begin, end = text.find(TABLE_BEGIN), text.find(TABLE_END)
        if begin < 0 or end < begin:
            errors.append(f"{relative}: table markers {TABLE_BEGIN} / {TABLE_END} not found")
            continue
        current = text[begin + len(TABLE_BEGIN) : end].strip()
        if current != render_docs_table(catalog, lang):
            errors.append(f"{relative}: the table is out of date (paste the output of --render-docs {lang})")
    return errors


# -------------------------------------------------------------------- live part


def follow_homepage(url: str, timeout: float, allow_loopback: bool = False):
    """GET a homepage, following up to MAX_REDIRECTS redirects; each hop is validated again."""
    result = None
    for _ in range(MAX_REDIRECTS + 1):
        result = endpoint_check.fetch(url, timeout, allow_loopback=allow_loopback)
        location = result.headers.get("Location")
        if result.error and "redirect refused" in result.error and location:
            url = urllib.parse.urljoin(url, location)
            continue
        return result
    result.error = "unreachable: too many redirects"
    return result


def check_homepage(url: str, timeout: float, allow_loopback: bool = False) -> tuple[str, str]:
    result = follow_homepage(url, timeout, allow_loopback)
    if result.error:
        return "homepage-unreachable", result.error
    if 200 <= result.status < 400:
        return "ok", f"HTTP {result.status}"
    if result.status in (401, 403, 429, 999):
        return "ok", f"HTTP {result.status} (the site answers but turns automated clients away)"
    return "homepage-unreachable", f"HTTP {result.status}"


def registry_url(ecosystem: str, name: str, version: str) -> str:
    if ecosystem == "npm":
        return f"{NPM_REGISTRY}/{urllib.parse.quote(name, safe='@').replace('/', '%2F')}/{urllib.parse.quote(version)}"
    return f"{PYPI_REGISTRY}/{urllib.parse.quote(name)}/{urllib.parse.quote(version)}/json"


def registry_license(ecosystem: str, body: dict | None) -> str | None:
    if not body:
        return None
    if ecosystem == "npm":
        value = body.get("license")
    else:
        info = body.get("info") or {}
        value = info.get("license_expression") or info.get("license")
    return value.strip() if isinstance(value, str) and value.strip() else None


def _spdx_ids(expression: str) -> set[str]:
    return {part for part in re.split(r"[\s()]+", expression) if part and part not in ("AND", "OR", "WITH")}


def check_package(install: dict, declared_license: str, timeout: float, allow_loopback: bool = False) -> tuple[str, str]:
    package = package_of(install)
    if package is None:
        return "ok", "not a package runner: nothing to look up"
    if isinstance(package, str):
        return "package-missing", package
    ecosystem, name, version = package
    result = endpoint_check.fetch(registry_url(ecosystem, name, version), timeout, allow_loopback=allow_loopback)
    label = f"{name}@{version} on {ecosystem}"
    if result.error:
        return "package-unreachable", f"{label}: {result.error}"
    if result.status == 404:
        return "package-missing", f"{label} does not exist"
    if result.status != 200:
        return "package-unreachable", f"{label}: HTTP {result.status}"
    published = registry_license(ecosystem, endpoint_check.parse_body(result))
    # Only a registry value that is itself an SPDX expression can be compared: "SEE LICENSE IN ..."
    # and an empty field say nothing, and a long PyPI body may be cut before its licence field.
    if published and SPDX_PATTERN.match(published) and _spdx_ids(published) != _spdx_ids(declared_license):
        return "license-mismatch", f"{label} says {published}; the catalog notes {declared_license}"
    return "ok", f"{label} exists" + (f", licence {published}" if published else "")


def auth_mismatch(declared: str, observed: dict) -> str | None:
    """A remote entry whose declared sign-in the vendor's endpoint contradicts."""
    status = observed.get("status")
    if declared == "oauth-dcr" and status in ("oauth-no-dcr", "api-key", "open"):
        return f"declared oauth-dcr, the endpoint reports {status}"
    if declared == "none" and status in ("auth-required", "oauth-dcr", "oauth-no-dcr", "api-key"):
        return f"declared none, the endpoint reports {status}"
    if declared == "api-key" and status in ("oauth-dcr",):
        return "declared api-key, but the endpoint offers dynamic client registration"
    return None


def verify_entry_live(entry: dict, timeout: float, allow_loopback: bool) -> dict:
    """One entry can only ever become a result row, never an exception that ends the run."""
    install = entry["install"]
    row = {"status": "ok", "observed_auth": None, "dcr": None, "package": None, "homepage": None, "notes": []}
    findings: list[tuple[str, str]] = []
    try:
        homepage_status, homepage_note = check_homepage(entry["homepage"], timeout, allow_loopback)
        row["homepage"] = homepage_note
        if homepage_status != "ok":
            findings.append((homepage_status, f"homepage: {homepage_note}"))
        if install.get("transport") == "http":
            observed = endpoint_check.check_endpoint(
                install["url"], timeout, bool(install.get("required_secrets")), allow_loopback=allow_loopback
            )
            row["observed_auth"] = observed.get("auth")
            row["dcr"] = observed.get("dcr")
            row["endpoint"] = observed["status"]
            if observed["status"] in endpoint_check.ATTENTION:
                findings.append((observed["status"], f"endpoint: {observed['note']}"))
            elif (mismatch := auth_mismatch(entry.get("auth"), observed)) is not None:
                findings.append(("auth-mismatch", mismatch))
        else:
            status, note = check_package(install, entry.get("license", ""), timeout, allow_loopback)
            row["package"] = note
            if status != "ok":
                findings.append((status, note))
    except Exception as exc:  # noqa: BLE001
        findings.append(("bad-response", f"checker error: {type(exc).__name__}"))
    if findings:
        row["status"] = findings[0][0]
        row["notes"] = [note for _, note in findings]
    return row


# ----------------------------------------------------------------------- report


def run_verify(
    catalog_path: Path,
    root: Path = ROOT,
    live: bool = False,
    timeout: float = 10.0,
    only: str | None = None,
    allow_loopback: bool = False,
) -> dict:
    catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
    if not isinstance(catalog, list):
        raise ValueError("catalog must be a JSON list")
    errors = offline_errors(catalog, root)
    if not errors:
        errors.extend(docs_errors(catalog, root))
    selected = [e for e in catalog if isinstance(e, dict) and (only is None or e.get("id") == only)]
    live_rows: dict[int, dict] = {}
    if live:
        usable = [e for e in selected if isinstance(e.get("install"), dict) and isinstance(e.get("homepage"), str)]
        with ThreadPoolExecutor(max_workers=4) as pool:
            futures = {id(e): pool.submit(verify_entry_live, e, timeout, allow_loopback) for e in usable}
            live_rows = {key: future.result() for key, future in futures.items()}
    entries = []
    for entry in selected:
        install = entry.get("install") if isinstance(entry.get("install"), dict) else {}
        row = {
            "id": entry.get("id"),
            "transport": install.get("transport"),
            "license": entry.get("license"),
            "auth": entry.get("auth"),
            "status": "checked-offline",
            "note": "",
        }
        extra = live_rows.get(id(entry))
        if extra:
            notes = extra.pop("notes")
            row.update(extra)
            row["note"] = "; ".join(notes) if notes else (row.get("package") or "reachable")
        entries.append(row)
    attention = [{"id": r["id"], "status": r["status"], "note": r["note"]} for r in entries if r["status"] in ATTENTION]
    return {
        "checked_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "mode": "live" if live else "offline",
        "catalog": str(catalog_path),
        "entries": entries,
        "attention": attention,
        "structural_errors": errors,
    }


def render_table(report: dict) -> str:
    lines = [f"mode: {report['mode']}, entries: {len(report['entries'])}"]
    lines.append(f"{'id':<16} {'transport':<8} {'status':<20} {'auth':<17} {'license':<20} note")
    for item in report["entries"]:
        lines.append(
            f"{str(item['id']):<16} {str(item['transport']):<8} {str(item['status']):<20} "
            f"{str(item['auth']):<17} {str(item['license']):<20} {item['note']}"
        )
    lines.extend(f"STRUCTURAL ERROR: {error}" for error in report["structural_errors"])
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--catalog", type=Path, default=ROOT / "crates/mcp-bridge/catalog.json")
    parser.add_argument("--live", action="store_true", help="also ask the registries and vendors (no credentials)")
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--only", help="check a single catalog id")
    parser.add_argument("--json", dest="json_path", type=Path, help="write the machine-readable report here")
    parser.add_argument("--strict", action="store_true", help="also fail on live attention rows")
    parser.add_argument("--render-docs", choices=sorted(DOCS), help="print the generated guide table and exit")
    parser.add_argument("--root", type=Path, default=ROOT, help=argparse.SUPPRESS)  # hermetic tests only
    parser.add_argument("--allow-loopback", action="store_true", help=argparse.SUPPRESS)  # hermetic tests only
    args = parser.parse_args(argv)
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8")  # the Turkish table must survive a Windows console
    try:
        if args.render_docs:
            catalog = json.loads(args.catalog.read_text(encoding="utf-8"))
            if (problems := offline_errors(catalog, args.root)):
                print("\n".join(problems), file=sys.stderr)
                return 2
            print(render_docs_table(catalog, args.render_docs))
            return 0
        report = run_verify(args.catalog, args.root, args.live, args.timeout, args.only, args.allow_loopback)
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"verify_mcp_catalog: internal error: {exc}", file=sys.stderr)
        return 1
    if args.json_path:
        args.json_path.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(render_table(report))
    if report["structural_errors"]:
        return 2
    if args.strict and report["attention"]:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
