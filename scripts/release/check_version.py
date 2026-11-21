#!/usr/bin/env python3
"""Check that release manifests agree with the numeric version of a tag."""

import argparse
from glob import glob, has_magic
import json
from pathlib import Path
import re
import sys
import tomllib


NUMBER = r"(?:0|[1-9][0-9]*)"
PRERELEASE_ID = rf"(?:{NUMBER}|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
SEMVER = re.compile(
    rf"({NUMBER}\.{NUMBER}\.{NUMBER})"
    rf"(?:-{PRERELEASE_ID}(?:\.{PRERELEASE_ID})*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
)
DEPENDENCY_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")
JSON_MANIFESTS = (
    "apps/desktop/tauri.conf.json",
    "apps/desktop/ui/package.json",
    "packages/ui/package.json",
)
MISSING = object()


def table(value):
    return value if isinstance(value, dict) else {}


def dependencies(manifest):
    for kind in DEPENDENCY_TABLES:
        for name, spec in table(manifest.get(kind)).items():
            yield f"{kind}.{name}", table(spec)
    for target, settings in table(manifest.get("target")).items():
        for kind in DEPENDENCY_TABLES:
            for name, spec in table(table(settings).get(kind)).items():
                yield f"target.{target!r}.{kind}.{name}", table(spec)


class VersionCheck:
    def __init__(self, root: Path, expected: str):
        self.root = root
        self.expected = expected
        self.errors: list[str] = []

    def report(self, path, message):
        if path.is_relative_to(self.root):
            path = path.relative_to(self.root)
        self.errors.append(f"{path.as_posix()}: {message}")

    def check_version(self, path, field, actual):
        if actual != self.expected:
            value = "missing" if actual is MISSING else repr(actual)
            self.report(path, f"{field}: expected {self.expected!r}, found {value}")

    def load(self, path):
        try:
            if path.suffix == ".json":
                document = json.loads(path.read_text(encoding="utf-8"))
            else:
                with path.open("rb") as stream:
                    document = tomllib.load(stream)
        except (OSError, ValueError) as error:
            self.report(path, f"could not read or parse file ({error})")
            return None
        if not isinstance(document, dict):
            self.report(path, "expected a top-level object")
            return None
        return document

    def workspace_paths(self, root_manifest, workspace, field):
        patterns = workspace.get(field, [])
        if not isinstance(patterns, list) or not all(isinstance(p, str) and p for p in patterns):
            self.report(
                root_manifest,
                f"workspace.{field}: expected an array of nonempty paths, found {patterns!r}",
            )
            return set()
        paths = set()
        for pattern in patterns:
            matches = [pattern]
            if has_magic(pattern):
                matches = glob(pattern, root_dir=self.root, recursive=True)
            if not matches and field == "members":
                self.report(root_manifest, f"workspace.members: no paths match {pattern!r}")
            paths.update((self.root / match / "Cargo.toml").resolve() for match in matches)
        return paths

    def workspace_members(self, root_manifest, document, workspace, workspace_deps):
        members = self.workspace_paths(root_manifest, workspace, "members")
        excluded = self.workspace_paths(root_manifest, workspace, "exclude")
        pending = sorted(members - excluded)
        if "package" in document:
            pending.append(root_manifest)
        manifests = {}
        while pending:
            path = pending.pop()
            if path in manifests:
                continue
            manifest = document if path == root_manifest else self.load(path)
            manifests[path] = manifest
            if manifest is None:
                continue
            # Cargo also treats in-tree path dependencies as workspace members.
            for name, spec in dependencies(manifest):
                base = path.parent
                if spec.get("workspace") is True:
                    spec = table(workspace_deps.get(name.rsplit(".", 1)[-1]))
                    base = self.root
                relative = spec.get("path")
                if isinstance(relative, str):
                    target = (base / relative / "Cargo.toml").resolve()
                    if target.is_relative_to(self.root) and target not in excluded:
                        pending.append(target)
        return manifests

    def check_cargo(self):
        root_manifest = self.root / "Cargo.toml"
        document = self.load(root_manifest)
        names = set()
        if document is not None:
            workspace = table(document.get("workspace"))
            workspace_version = table(workspace.get("package")).get("version", MISSING)
            self.check_version(root_manifest, "workspace.package.version", workspace_version)
            workspace_deps = table(workspace.get("dependencies"))
            members = self.workspace_members(root_manifest, document, workspace, workspace_deps)
            for path, manifest in sorted(members.items()):
                if manifest is None:
                    continue
                package = table(manifest.get("package"))
                name = package.get("name")
                if not isinstance(name, str) or not name:
                    self.report(path, "package.name: expected a nonempty workspace package name")
                else:
                    names.add(name)
                version = package.get("version", MISSING)
                if table(version).get("workspace") is not True:
                    self.check_version(path, "package.version", version)
                self.check_path_dependencies(path, dependencies(manifest), members)
            self.check_path_dependencies(
                root_manifest,
                ((f"workspace.dependencies.{name}", table(spec)) for name, spec in workspace_deps.items()),
                members,
            )
        self.check_lockfile(names)

    def check_path_dependencies(self, path, entries, members):
        for field, spec in entries:
            relative = spec.get("path")
            version = spec.get("version")
            if not isinstance(relative, str) or not isinstance(version, str):
                continue
            # Check repeated release versions while allowing Cargo version ranges.
            if SEMVER.fullmatch(version) and (path.parent / relative / "Cargo.toml").resolve() in members:
                self.check_version(path, f"{field}.version", version)

    def check_lockfile(self, names):
        path = self.root / "Cargo.lock"
        lockfile = self.load(path)
        if lockfile is None:
            return
        packages = lockfile.get("package")
        if not isinstance(packages, list) or not all(isinstance(p, dict) for p in packages):
            self.report(path, "package: expected an array of package entries")
            return
        found = set()
        for package in packages:
            name = package.get("name")
            if isinstance(name, str) and name in names and "source" not in package:
                found.add(name)
                self.check_version(path, f"package {name!r}.version", package.get("version", MISSING))
        for name in sorted(names - found):
            self.report(
                path, f"package {name!r}.version: expected {self.expected!r}, found no local package entry"
            )

    def run(self):
        self.check_cargo()
        for name in JSON_MANIFESTS:
            path = self.root / name
            document = self.load(path)
            if document is not None:
                self.check_version(path, "version", document.get("version", MISSING))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2], help="repository root"
    )
    parser.add_argument("version", metavar="TAG_OR_VERSION", help="SemVer with an optional leading v")
    args = parser.parse_args()
    match = SEMVER.fullmatch(args.version.removeprefix("v"))
    if match is None:
        parser.error(f"invalid release version {args.version!r}; expected SemVer with an optional leading v")
    check = VersionCheck(args.root.resolve(), match[1])
    check.run()
    if check.errors:
        print("\n".join(check.errors), file=sys.stderr)
        return 1
    print(f"Release manifests match {check.expected} ({args.version}).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
