#!/usr/bin/env python3
"""Fail if an NSIS source kills processes by image name (`taskkill ... /IM`).

Killing by image name hits every process called bot.exe on the machine (other programs, other
user sessions). The installer must stop only the processes running from $INSTDIR; see
PRAXI_STOP_OUR_PROCESSES in apps/desktop/nsis/installer-hooks.nsh.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2] / "apps" / "desktop" / "nsis"
BAD = re.compile(r"taskkill[^\r\n]*/IM\b|/IM\b[^\r\n]*taskkill", re.IGNORECASE)

bad = []
for f in sorted(ROOT.rglob("*")):
    if f.suffix.lower() in (".nsh", ".nsi"):
        for n, line in enumerate(f.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
            if BAD.search(line):
                bad.append(f"{f.relative_to(ROOT.parents[2])}:{n}: {line.strip()}")
if bad:
    print("taskkill /IM (kill by image name) is forbidden in the NSIS sources:")
    print("\n".join(bad))
    sys.exit(1)
print("ok: no taskkill /IM in NSIS sources")
