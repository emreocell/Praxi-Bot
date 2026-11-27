#!/usr/bin/env python3
"""Classify pull-request changed paths for the CI workflow (see .github/workflows/ci.yml)."""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass


@dataclass(frozen=True)
class CiPathFlags:
    rust: bool
    ui: bool
    visual: bool
    smoke: bool
    smoke_windows: bool


DOCS_ONLY = re.compile(
    r"^((docs|docs-site|site|packaging)/.*|[^/]+\.md|LICENSE|NOTICE|"
    r"\.github/(ISSUE_TEMPLATE/.*|CODEOWNERS|labeler\.yml|pull_request_template\.md|"
    r"workflows/(docs-site|labeler|installer|linux-release|linux-scripts|"
    r"test-report|test-report-weekly)\.yml))$"
)
UI_ONLY = re.compile(r"^(apps/desktop/ui/|packages/)")
VISUAL_PATHS = re.compile(r"^(apps/desktop/ui/|packages/)")
UI_JOB_PATHS = re.compile(
    r"^(apps/desktop/ui/|packages/|site/|eslint\.config\.js$|\.stylelintrc\.json$|"
    r"package\.json$|pnpm-lock\.yaml$|pnpm-workspace\.yaml$|docs/api/openapi\.json$|"
    r"\.github/workflows/ci\.yml$)"
)
SMOKE_PATHS = re.compile(
    r"^(apps/(desktop|cli)/|crates/|packages/|providers/|skills/|\.cargo/|"
    r"(Cargo\.(toml|lock)|rust-toolchain\.toml|package\.json|pnpm-lock\.yaml|"
    r"pnpm-workspace\.yaml|tsconfig\.base\.json|docs/api/openapi\.json|"
    r"\.github/workflows/ci\.yml)$)"
)


def classify_paths(paths: list[str]) -> CiPathFlags:
    """Return which CI lanes a pull request needs from its changed file paths."""
    rust = False
    ui = False
    visual = False
    smoke = False
    smoke_windows = False

    for path in paths:
        path = path.strip()
        if not path:
            continue
        if path == "docs/api/openapi.json" or (
            not DOCS_ONLY.search(path) and not UI_ONLY.search(path)
        ):
            rust = True
        if UI_JOB_PATHS.search(path):
            ui = True
        if VISUAL_PATHS.search(path):
            visual = True
        if SMOKE_PATHS.search(path):
            smoke = True
            if not UI_ONLY.search(path):
                smoke_windows = True

    return CiPathFlags(
        rust=rust,
        ui=ui,
        visual=visual,
        smoke=smoke,
        smoke_windows=smoke_windows,
    )


def full_matrix() -> CiPathFlags:
    return CiPathFlags(
        rust=True,
        ui=True,
        visual=True,
        smoke=True,
        smoke_windows=True,
    )


def github_output_lines(flags: CiPathFlags) -> list[str]:
    smoke_os = (
        '["ubuntu-latest","windows-latest"]'
        if flags.smoke_windows
        else '["ubuntu-latest"]'
    )
    return [
        f"rust={'true' if flags.rust else 'false'}",
        f"ui={'true' if flags.ui else 'false'}",
        f"visual={'true' if flags.visual else 'false'}",
        f"smoke={'true' if flags.smoke else 'false'}",
        f"smoke_os={smoke_os}",
    ]


def main() -> int:
    if len(sys.argv) > 1 and sys.argv[1] == "--full":
        flags = full_matrix()
    else:
        paths = [line.rstrip("\n") for line in sys.stdin]
        flags = classify_paths(paths)
    for line in github_output_lines(flags):
        print(line)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())