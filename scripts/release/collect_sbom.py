#!/usr/bin/env python3
"""Collect and sanity-check the CycloneDX SBOMs of the Windows release.

`cargo cyclonedx` writes one SBOM next to the Cargo.toml of every workspace member. The Windows
installer ships exactly two of them (the `bot` daemon sidecar, package `cli`, and the desktop
shell, package `desktop`), so only those are copied into the release folder, under fixed names.
The UI SBOM (`pnpm --filter desktop-ui sbom`) is written there directly; it is only validated here.

A file is accepted only when it is a CycloneDX document that names the expected package and lists
at least one component, so an empty or wrong SBOM fails the build instead of being published.

    python scripts/release/collect_sbom.py --root . --out sbom
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path

# (directory of the Cargo.toml, Cargo package name, file name in the release)
RUST_SBOMS = (
    ("apps/cli", "cli", "sbom-rust-cli.cdx.json"),
    ("apps/desktop", "desktop", "sbom-rust-desktop.cdx.json"),
)
UI_SBOM = ("desktop-ui", "sbom-ui.cdx.json")


def check_sbom(path: Path, package: str) -> str | None:
    """Return None when `path` is a usable CycloneDX SBOM of `package`, else the reason it is not."""
    try:
        doc = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as err:
        return f"not readable JSON ({err})"
    if not isinstance(doc, dict) or doc.get("bomFormat") != "CycloneDX":
        return "not a CycloneDX document"
    component = (doc.get("metadata") or {}).get("component") or {}
    if component.get("name") != package:
        return f"describes {component.get('name')!r}, expected {package!r}"
    if not doc.get("components"):
        return "lists no components"
    return None


def find_rust_sbom(package_dir: Path, package: str) -> Path:
    """The SBOM `cargo cyclonedx` wrote for `package` in `package_dir` (any file name it chose)."""
    problems = []
    for candidate in sorted(package_dir.glob("*.json")):
        if candidate.name in ("package.json", "tauri.conf.json"):
            continue
        reason = check_sbom(candidate, package)
        if reason is None:
            return candidate
        problems.append(f"{candidate.name}: {reason}")
    found = "; ".join(problems) or "no JSON files at all"
    raise SystemExit(f"error: no CycloneDX SBOM for package '{package}' in {package_dir} ({found})")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", default=".", help="repository root")
    parser.add_argument("--out", default="sbom", help="release folder for the SBOMs")
    args = parser.parse_args(argv)

    root = Path(args.root)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    for directory, package, name in RUST_SBOMS:
        source = find_rust_sbom(root / directory, package)
        shutil.copyfile(source, out / name)
        print(f"{source} -> {out / name}")

    package, name = UI_SBOM
    reason = check_sbom(out / name, package)
    if reason is not None:
        raise SystemExit(f"error: {out / name}: {reason}")
    print(f"{out / name}: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
