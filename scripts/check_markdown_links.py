#!/usr/bin/env python3
"""Fail when a tracked Markdown file links to a relative path that does not exist.

Only relative file links are checked (no network, no anchors). Run from anywhere:

    python scripts/check_markdown_links.py
"""
import os
import re
import subprocess
import sys

ROOT = subprocess.run(
    ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, encoding="utf-8", check=True
).stdout.strip()
FILES = subprocess.run(
    ["git", "ls-files", "-z", "*.md"], capture_output=True, text=True, encoding="utf-8", check=True
).stdout.split("\0")

# [text](target) and [text](target "title"); skips images' alt text handling (same syntax).
LINK = re.compile(r"\[[^\]]*\]\(\s*<?([^)\s>]+)>?(?:\s+\"[^\"]*\")?\s*\)")
FENCE = re.compile(r"^\s*(```|~~~)")

bad = []
for rel in FILES:
    if not rel:
        continue
    path = os.path.join(ROOT, rel)
    in_fence = False
    with open(path, encoding="utf-8") as fh:
        for n, line in enumerate(fh, 1):
            if FENCE.match(line):
                in_fence = not in_fence
                continue
            if in_fence:
                continue
            line = re.sub(r"`[^`]*`", "", line)  # inline code is not a link
            for m in LINK.finditer(line):
                target = m.group(1)
                if re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*:", target) or target.startswith("#"):
                    continue  # http:, https:, mailto:, in-page anchors
                target = target.split("#", 1)[0].split("?", 1)[0]
                if not target:
                    continue
                if target.startswith("/"):
                    resolved = os.path.join(ROOT, target.lstrip("/"))
                else:
                    resolved = os.path.normpath(os.path.join(os.path.dirname(path), target))
                if not os.path.exists(resolved):
                    bad.append(f"{rel}:{n}: broken link -> {m.group(1)}")

if bad:
    print("Broken relative Markdown links:")
    print("\n".join(bad))
    print(f"\n{len(bad)} broken link(s). Fix the path or remove the link.")
    sys.exit(1)
print("All relative Markdown links resolve.")
