#!/usr/bin/env python3
"""Fail when a workflow references a third-party action without a 40-character commit SHA."""

from __future__ import annotations

import argparse
import re
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github" / "workflows"

# `uses: owner/repo@ref` on a step line, or `- uses: ...` shorthand.
USES_RE = re.compile(
    r"(?m)^(?P<indent>\s*)(?:-\s*)?uses:\s*(?P<ref>[^\s#]+)(?:\s+#\s*(?P<comment>.*))?$"
)
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
BRANCH_COMMENT_RE = re.compile(r"^(stable|master|nightly|beta)\b", re.IGNORECASE)


def _branch_label(comment: str) -> str | None:
    comment = comment.strip()
    match = BRANCH_COMMENT_RE.match(comment)
    if not match:
        return None
    return match.group(1).lower()


def check_workflows(workflows_dir: Path) -> list[str]:
    errors: list[str] = []
    branch_pins: dict[tuple[str, str], dict[str, list[tuple[Path, int]]]] = defaultdict(dict)
    for path in sorted(workflows_dir.glob("*.yml")):
        text = path.read_text(encoding="utf-8")
        for match in USES_RE.finditer(text):
            ref = match.group("ref")
            if ref.startswith("./"):
                continue
            if "@" not in ref:
                errors.append(f"{path}: uses without @ ref: {ref!r}")
                continue
            action, version = ref.rsplit("@", 1)
            line = text.count("\n", 0, match.start()) + 1
            if not SHA_RE.fullmatch(version):
                errors.append(
                    f"{path}:{line}: third-party action must be pinned to a full commit SHA, "
                    f"not @{version}: {ref}"
                )
                continue
            comment = match.group("comment")
            if comment is None:
                continue
            branch = _branch_label(comment)
            if branch is None:
                continue
            key = (action, branch)
            branch_pins[key].setdefault(version, []).append((path, line))

    for (action, branch), by_sha in sorted(branch_pins.items()):
        if len(by_sha) <= 1:
            continue
        shas = sorted(by_sha)
        detail = "; ".join(
            f"{sha} ({', '.join(f'{p}:{ln}' for p, ln in by_sha[sha])})" for sha in shas
        )
        errors.append(
            f"{action} pins for #{branch} must use one commit SHA across workflows, "
            f"found {len(shas)}: {detail}"
        )
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--workflows-dir",
        type=Path,
        default=WORKFLOWS,
        help="Directory containing workflow YAML files (default: .github/workflows)",
    )
    args = parser.parse_args(argv)
    errors = check_workflows(args.workflows_dir)
    if errors:
        for err in errors:
            print(err, file=sys.stderr)
        print(f"::error::Found {len(errors)} unpinned GitHub Action reference(s)", file=sys.stderr)
        return 1
    print(f"All GitHub Actions in {args.workflows_dir} are pinned to commit SHAs.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())