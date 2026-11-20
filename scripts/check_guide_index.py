#!/usr/bin/env python3
"""Fail when a page in docs/guide/ is not linked from docs/guide/index.md.

Run from the repository root:

    python scripts/check_guide_index.py
"""
import os
import subprocess
import sys

from check_markdown_links import INLINE_CODE, LINK, SCHEME, markdown_lines

GUIDE = "docs/guide"
INDEX = f"{GUIDE}/index.md"

# Pages that are intentionally not in the index, one comment each.
ALLOWED_UNLINKED = {
    # Maintainer release process; linked from host-operations.md where users need it.
    "release-linux.md",
}


def main():
    root = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, encoding="utf-8", check=True
    ).stdout.strip()
    files = subprocess.run(
        ["git", "ls-files", "-z", f"{GUIDE}/*.md"], capture_output=True, text=True, encoding="utf-8", check=True
    ).stdout.split("\0")
    # Top-level pages only: the pathspec also matches subfolders.
    pages = {os.path.basename(rel) for rel in files if os.path.dirname(rel) == GUIDE}
    pages.discard("index.md")

    guide = os.path.normpath(os.path.join(root, GUIDE))
    linked = set()
    for _, line in markdown_lines(os.path.join(root, INDEX)):
        for match in LINK.finditer(INLINE_CODE.sub("", line)):
            target = match[1]
            if SCHEME.match(target):
                continue
            relative = target.partition("#")[0].split("?", 1)[0]
            if relative and os.path.dirname(os.path.normpath(os.path.join(guide, relative))) == guide:
                linked.add(os.path.basename(relative))

    missing = sorted(pages - linked - ALLOWED_UNLINKED)
    if missing:
        print(f"Guide pages not linked from {INDEX}:")
        print("\n".join(f"{GUIDE}/{page}: not linked from {INDEX}" for page in missing))
        print(f"\n{len(missing)} unlinked page(s). Add a row to {INDEX}, or to ALLOWED_UNLINKED with a reason.")
        return 1
    print(f"All guide pages are linked from {INDEX}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
