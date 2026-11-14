#!/usr/bin/env python3
"""Repeatable Praxi Linux host resource & budget measurement tool (Task 174-B).

Collects per-process RSS/PSS/CPU from procfs, cgroup v2 memory/CPU/OOM stats,
and host memory/swap counters. Never reads /proc/<pid>/cmdline, /proc/<pid>/environ,
frames, titles, passwords, or URLs.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

try:
    import pwd
except ImportError:  # Windows host running --fixture-dir tests
    pwd = None  # type: ignore[assignment]

MIB = 1024 * 1024
GIB = 1024 * 1024 * 1024
GB_DEC = 1_000_000_000

DEFAULT_UNITS = [
    "praxi.slice",
    "praxi-xvfb.service",
    "praxi-desktop.service",
    "praxi.service",
    "praxi-node.service",
]

CAVEATS = [
    "Summed process VmRSS double-counts shared memory pages across processes; "
    "prefer cgroup memory.current/peak and summed PSS when smaps_rollup is readable.",
    "Units are labeled explicitly: MiB = 1024^2 bytes, GiB = 1024^3 bytes, "
    "provider GB (decimal) = 10^9 bytes.",
]


def fmt_bytes(b: int | None) -> dict[str, Any] | None:
    if b is None:
        return None
    return {
        "bytes": b,
        "mib": round(b / MIB, 2),
        "gib": round(b / GIB, 3),
        "gb_decimal": round(b / GB_DEC, 3),
    }


def read_text(path: Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None


def parse_kv_kib(text: str | None) -> dict[str, int]:
    out: dict[str, int] = {}
    if not text:
        return out
    for line in text.splitlines():
        if ":" not in line:
            continue
        k, v = line.split(":", 1)
        m = re.match(r"\s*([0-9]+)\s*(kB|KiB|B)?", v, flags=re.IGNORECASE)
        if not m:
            continue
        num = int(m.group(1))
        unit = (m.group(2) or "").lower()
        if unit in ("kb", "kib"):
            num *= 1024
        out[k.strip()] = num
    return out


def parse_plain_kv(text: str | None) -> dict[str, int]:
    out: dict[str, int] = {}
    if not text:
        return out
    for line in text.splitlines():
        parts = line.split()
        if len(parts) >= 2:
            try:
                out[parts[0]] = int(parts[1])
            except ValueError:
                continue
    return out


def parse_proc_stat(text: str) -> dict[str, Any] | None:
    """Parse /proc/<pid>/stat safely even if comm contains spaces or parens."""
    rparen = text.rfind(")")
    lparen = text.find("(")
    if lparen == -1 or rparen == -1 or rparen <= lparen:
        return None
    pid_str = text[:lparen].strip()
    comm = text[lparen + 1 : rparen]
    rest = text[rparen + 2 :].split()
    # Field 3 is state (rest[0]); field 4 is ppid (rest[1]);
    # field 14 is utime (rest[11]); field 15 is stime (rest[12]);
    # field 22 is starttime (rest[19]).
    if len(rest) < 20:
        return None
    try:
        return {
            "pid": int(pid_str),
            "comm": comm,
            "state": rest[0],
            "ppid": int(rest[1]),
            "utime": int(rest[11]),
            "stime": int(rest[12]),
            "starttime": int(rest[19]),
        }
    except ValueError:
        return None


def parse_uid_from_status(status_text: str | None) -> int | None:
    if not status_text:
        return None
    for line in status_text.splitlines():
        if line.startswith("Uid:"):
            parts = line.split()
            if len(parts) >= 2:
                try:
                    return int(parts[1])
                except ValueError:
                    return None
    return None


def read_meminfo(proc_root: Path) -> dict[str, Any]:
    kv = parse_kv_kib(read_text(proc_root / "meminfo"))
    total = kv.get("MemTotal")
    avail = kv.get("MemAvailable")
    if avail is None and total is not None:
        free = kv.get("MemFree", 0)
        buffers = kv.get("Buffers", 0)
        cached = kv.get("Cached", 0)
        avail = free + buffers + cached
    used = (total - avail) if (total is not None and avail is not None) else None
    swap_total = kv.get("SwapTotal", 0)
    swap_free = kv.get("SwapFree", 0)
    swap_used = max(0, swap_total - swap_free)
    return {
        "mem_total_bytes": total,
        "mem_available_bytes": avail,
        "host_used_bytes": used,
        "swap_total_bytes": swap_total,
        "swap_used_bytes": swap_used,
    }


def read_vmstat(proc_root: Path) -> dict[str, int]:
    kv = parse_plain_kv(read_text(proc_root / "vmstat"))
    return {
        "pswpin": kv.get("pswpin", 0),
        "pswpout": kv.get("pswpout", 0),
        "oom_kill": kv.get("oom_kill", 0),
    }


def read_cgroup_dir(cg_dir: Path) -> dict[str, Any]:
    if not cg_dir.exists():
        return {"exists": False}

    def read_int_or_max(name: str) -> int | str | None:
        raw = read_text(cg_dir / name)
        if raw is None:
            return None
        s = raw.strip()
        if s == "max":
            return "max"
        try:
            return int(s)
        except ValueError:
            return None

    mem_cur = read_int_or_max("memory.current")
    mem_peak = read_int_or_max("memory.peak")
    mem_high = read_int_or_max("memory.high")
    mem_max = read_int_or_max("memory.max")
    events = parse_plain_kv(read_text(cg_dir / "memory.events"))
    cpu_stat = parse_plain_kv(read_text(cg_dir / "cpu.stat"))

    pids: list[int] = []
    seen_pids: set[int] = set()
    procs_files = [cg_dir / "cgroup.procs"]
    try:
        for sub_procs in sorted(cg_dir.rglob("cgroup.procs")):
            if sub_procs not in procs_files:
                procs_files.append(sub_procs)
    except OSError:
        pass
    for pf in procs_files:
        procs_raw = read_text(pf)
        if not procs_raw:
            continue
        for line in procs_raw.splitlines():
            line = line.strip()
            if line:
                try:
                    pid_val = int(line)
                    if pid_val not in seen_pids:
                        seen_pids.add(pid_val)
                        pids.append(pid_val)
                except ValueError:
                    continue

    return {
        "exists": True,
        "memory_current_bytes": mem_cur if isinstance(mem_cur, int) else None,
        "memory_peak_bytes": mem_peak if isinstance(mem_peak, int) else None,
        "memory_peak_available": isinstance(mem_peak, int),
        "memory_high": mem_high,
        "memory_max": mem_max,
        "memory_events": {
            "high": events.get("high", 0),
            "max": events.get("max", 0),
            "oom": events.get("oom", 0),
            "oom_kill": events.get("oom_kill", 0),
            "oom_group_kill": events.get("oom_group_kill", 0),
        },
        "cpu_stat": {
            "usage_usec": cpu_stat.get("usage_usec"),
            "user_usec": cpu_stat.get("user_usec"),
            "system_usec": cpu_stat.get("system_usec"),
            "nr_throttled": cpu_stat.get("nr_throttled", 0),
            "throttled_usec": cpu_stat.get("throttled_usec", 0),
        },
        "pids": pids,
    }


def resolve_live_unit_cgroups(units: list[str]) -> dict[str, str]:
    mapping: dict[str, str] = {}
    for unit in units:
        try:
            res = subprocess.run(
                ["systemctl", "show", unit, "-p", "ControlGroup", "--value"],
                capture_output=True,
                text=True,
                timeout=3,
                check=False,
            )
            cg = res.stdout.strip()
            if cg:
                mapping[unit] = cg
        except OSError:
            continue
    return mapping


def resolve_praxi_uid(proc_root: Path) -> int | None:
    if pwd is None:
        return None
    try:
        return pwd.getpwnam("praxi").pw_uid
    except KeyError:
        return None


def read_all_processes(proc_root: Path) -> tuple[dict[tuple[int, int], dict[str, Any]], int, bool]:
    """Read /proc/<pid> entries without cmdline/environ.

    Returns (processes keyed by (pid, starttime), vanished_count, any_pss_available).
    """
    procs: dict[tuple[int, int], dict[str, Any]] = {}
    vanished = 0
    pss_any = False

    try:
        entries = list(proc_root.iterdir())
    except OSError:
        return procs, vanished, False

    for entry in entries:
        if not entry.name.isdigit():
            continue
        stat_text = read_text(entry / "stat")
        if stat_text is None:
            vanished += 1
            continue
        parsed = parse_proc_stat(stat_text)
        if parsed is None:
            vanished += 1
            continue
        status_text = read_text(entry / "status")
        if status_text is None:
            vanished += 1
            continue
        status_kv = parse_kv_kib(status_text)
        uid = parse_uid_from_status(status_text)

        smaps_text = read_text(entry / "smaps_rollup")
        smaps_kv = parse_kv_kib(smaps_text)
        pss = smaps_kv.get("Pss")
        if pss is not None:
            pss_any = True

        key = (parsed["pid"], parsed["starttime"])
        procs[key] = {
            "pid": parsed["pid"],
            "starttime": parsed["starttime"],
            "ppid": parsed["ppid"],
            "comm": parsed["comm"],
            "uid": uid,
            "utime": parsed["utime"],
            "stime": parsed["stime"],
            "rss_bytes": status_kv.get("VmRSS", 0),
            "hwm_bytes": status_kv.get("VmHWM"),
            "pss_bytes": pss,
            "pss_anon_bytes": smaps_kv.get("Pss_Anon"),
            "pss_file_bytes": smaps_kv.get("Pss_File"),
            "pss_shmem_bytes": smaps_kv.get("Pss_Shmem"),
        }
    return procs, vanished, pss_any


def detect_browser_version(browser_path: str | None) -> str:
    candidates = [
        browser_path,
        os.environ.get("BOT_BROWSER_PATH"),
        os.environ.get("PRAXI_BROWSER_PATH"),
        "/opt/praxi-browser/chrome",
        "/usr/bin/chromium",
        "/usr/bin/google-chrome",
    ]
    for cand in candidates:
        if not cand:
            continue
        p = Path(cand)
        if p.is_file() and os.access(p, os.X_OK):
            try:
                res = subprocess.run(
                    [str(p), "--version"],
                    capture_output=True,
                    text=True,
                    timeout=3,
                    check=False,
                )
                out = res.stdout.strip()
                if out:
                    return out.splitlines()[0]
            except OSError:
                continue
    return "unavailable"


def detect_os_pretty() -> str:
    text = read_text(Path("/etc/os-release"))
    if text:
        for line in text.splitlines():
            if line.startswith("PRETTY_NAME="):
                return line.split("=", 1)[1].strip().strip('"')
    return platform.platform()


def collect_tick(
    proc_root: Path,
    cgroup_root: Path,
    unit_cgroups: dict[str, str],
    praxi_uid: int | None,
) -> dict[str, Any]:
    meminfo = read_meminfo(proc_root)
    vmstat = read_vmstat(proc_root)
    all_procs, vanished, pss_any = read_all_processes(proc_root)

    cg_data: dict[str, Any] = {}
    slice_pids: set[int] = set()
    for unit, cg_rel in unit_cgroups.items():
        rel = cg_rel.lstrip("/")
        cg_path = cgroup_root / rel if rel else cgroup_root
        info = read_cgroup_dir(cg_path)
        cg_data[unit] = info
        if unit == "praxi.slice" and info.get("exists"):
            slice_pids.update(info.get("pids", []))

    # Identify Praxi processes: inside praxi.slice OR owned by praxi UID (if known)
    # OR matching unit cgroup PIDs + descendants.
    unit_pids: set[int] = set(slice_pids)
    for info in cg_data.values():
        if info.get("exists"):
            unit_pids.update(info.get("pids", []))

    # Expand descendants in case fixture/system lacks slice PIDs for a unit
    changed = True
    while changed:
        changed = False
        for p in all_procs.values():
            if p["pid"] not in unit_pids and p["ppid"] in unit_pids:
                unit_pids.add(p["pid"])
                changed = True

    praxi_procs: dict[tuple[int, int], dict[str, Any]] = {}
    escaped_slice_pids: list[dict[str, Any]] = []
    other_host_procs: list[dict[str, Any]] = []

    for key, p in all_procs.items():
        in_praxi_uid = praxi_uid is not None and p["uid"] == praxi_uid
        in_unit_tree = p["pid"] in unit_pids
        if in_praxi_uid or in_unit_tree:
            in_slice = (p["pid"] in slice_pids) if slice_pids else None
            proc_copy = dict(p)
            proc_copy["in_praxi_slice"] = in_slice
            praxi_procs[key] = proc_copy
            if slice_pids and in_praxi_uid and p["pid"] not in slice_pids:
                escaped_slice_pids.append({"pid": p["pid"], "comm": p["comm"]})
        else:
            other_host_procs.append(p)

    sum_rss = sum(p["rss_bytes"] for p in praxi_procs.values())
    pss_vals = [p["pss_bytes"] for p in praxi_procs.values() if p["pss_bytes"] is not None]
    sum_pss = sum(pss_vals) if (pss_vals and len(pss_vals) == len(praxi_procs)) else (sum(pss_vals) if pss_vals else None)

    other_host_procs.sort(key=lambda x: x["rss_bytes"], reverse=True)
    top_other = [
        {"pid": p["pid"], "comm": p["comm"], "uid": p["uid"], "rss_bytes": p["rss_bytes"]}
        for p in other_host_procs[:15]
    ]

    return {
        "meminfo": meminfo,
        "vmstat": vmstat,
        "cgroups": cg_data,
        "praxi_procs": praxi_procs,
        "sum_praxi_rss_bytes": sum_rss,
        "sum_praxi_pss_bytes": sum_pss,
        "pss_available": pss_any,
        "vanished_pids": vanished,
        "escaped_slice_pids": escaped_slice_pids,
        "top_other_services": top_other,
    }


def run_measurement(args: argparse.Namespace) -> tuple[dict[str, Any], int]:
    fixture_dir = Path(args.fixture_dir) if args.fixture_dir else None
    meta: dict[str, Any] = {}
    tick_dirs: list[tuple[Path, Path, float | None]] = []

    if fixture_dir:
        meta_file = fixture_dir / "meta.json"
        if meta_file.is_file():
            meta = json.loads(meta_file.read_text(encoding="utf-8"))
        samples_root = fixture_dir / "samples"
        if samples_root.is_dir():
            for sub in sorted(p for p in samples_root.iterdir() if p.is_dir()):
                tick_meta_path = sub / "tick.json"
                tick_elapsed = None
                if tick_meta_path.is_file():
                    tick_elapsed = json.loads(tick_meta_path.read_text(encoding="utf-8")).get("elapsed_s")
                tick_dirs.append((sub / "proc", sub / "cgroup", tick_elapsed))
        else:
            tick_dirs.append((fixture_dir / "proc", fixture_dir / "cgroup", 0.0))

    units = list(args.units)
    if fixture_dir:
        unit_cgroups = meta.get(
            "unit_cgroups",
            {
                "praxi.slice": "/praxi.slice",
                "praxi-xvfb.service": "/praxi.slice/praxi-xvfb.service",
                "praxi-desktop.service": "/praxi.slice/praxi-desktop.service",
                "praxi.service": "/praxi.slice/praxi.service",
                "praxi-node.service": "/praxi.slice/praxi-node.service",
            },
        )
        praxi_uid = meta.get("praxi_uid")
        clk_tck = int(meta.get("clk_tck", 100))
        arch = meta.get("arch", platform.machine())
        kernel = meta.get("kernel", platform.release())
        os_name = meta.get("os", "fixture")
        browser_version = meta.get("browser_version", "fixture-browser")
    else:
        unit_cgroups = resolve_live_unit_cgroups(units)
        if "praxi.slice" not in unit_cgroups and (Path(args.cgroup_root) / "praxi.slice").is_dir():
            unit_cgroups["praxi.slice"] = "/praxi.slice"
        praxi_uid = resolve_praxi_uid(Path(args.proc_root))
        clk_tck = os.sysconf("SC_CLK_TCK") if hasattr(os, "sysconf") else 100
        arch = platform.machine()
        kernel = platform.release()
        os_name = detect_os_pretty()
        browser_version = detect_browser_version(args.browser_path)

    ticks: list[dict[str, Any]] = []
    timestamps: list[float] = []

    if fixture_dir:
        for idx, (p_root, c_root, t_elapsed) in enumerate(tick_dirs):
            t_val = float(t_elapsed) if t_elapsed is not None else idx * args.interval
            timestamps.append(t_val)
            ticks.append(collect_tick(p_root, c_root, unit_cgroups, praxi_uid))
    else:
        start_mono = time.monotonic()
        end_mono = start_mono + max(0.0, args.duration)
        while True:
            now = time.monotonic()
            timestamps.append(now - start_mono)
            ticks.append(
                collect_tick(
                    Path(args.proc_root),
                    Path(args.cgroup_root),
                    unit_cgroups,
                    praxi_uid,
                )
            )
            if args.duration <= 0 or now >= end_mono:
                break
            sleep_for = min(args.interval, max(0.0, end_mono - time.monotonic()))
            if sleep_for > 0:
                time.sleep(sleep_for)

    elapsed_s = max(0.0, timestamps[-1] - timestamps[0]) if len(timestamps) >= 2 else float(args.duration)
    total_vanished = sum(t["vanished_pids"] for t in ticks)

    # Aggregate host memory & swap & OOM
    host_used_series = [t["meminfo"]["host_used_bytes"] for t in ticks if t["meminfo"]["host_used_bytes"] is not None]
    mem_avail_series = [t["meminfo"]["mem_available_bytes"] for t in ticks if t["meminfo"]["mem_available_bytes"] is not None]
    swap_used_series = [t["meminfo"]["swap_used_bytes"] for t in ticks]
    mem_total = ticks[-1]["meminfo"]["mem_total_bytes"]
    swap_total = ticks[-1]["meminfo"]["swap_total_bytes"]

    swap_in_delta = max(0, ticks[-1]["vmstat"]["pswpin"] - ticks[0]["vmstat"]["pswpin"])
    swap_out_delta = max(0, ticks[-1]["vmstat"]["pswpout"] - ticks[0]["vmstat"]["pswpout"])
    vmstat_oom_delta = max(0, ticks[-1]["vmstat"]["oom_kill"] - ticks[0]["vmstat"]["oom_kill"])

    # Aggregate cgroups
    cgroups_summary: dict[str, Any] = {}
    total_cg_oom_kills = 0
    for unit in unit_cgroups:
        u_ticks = [t["cgroups"].get(unit, {"exists": False}) for t in ticks]
        if not any(u.get("exists") for u in u_ticks):
            cgroups_summary[unit] = {"exists": False}
            continue
        cur_vals = [u["memory_current_bytes"] for u in u_ticks if u.get("memory_current_bytes") is not None]
        peak_vals = [u["memory_peak_bytes"] for u in u_ticks if u.get("memory_peak_bytes") is not None]
        last_u = next(u for u in reversed(u_ticks) if u.get("exists"))
        first_u = next(u for u in u_ticks if u.get("exists"))

        obs_max = max(cur_vals) if cur_vals else None
        cg_peak = max(peak_vals) if peak_vals else None
        effective_peak = cg_peak if cg_peak is not None else obs_max

        ev_first = first_u.get("memory_events", {})
        ev_last = last_u.get("memory_events", {})
        ev_delta = {
            k: max(0, ev_last.get(k, 0) - ev_first.get(k, 0))
            for k in ("high", "max", "oom", "oom_kill", "oom_group_kill")
        }
        # In single-sample fixtures, also respect absolute oom_kill if > 0
        oom_events_seen = max(ev_delta["oom_kill"] + ev_delta["oom_group_kill"], ev_last.get("oom_kill", 0) if len(u_ticks) == 1 else 0)
        if unit == "praxi.slice" or "praxi.slice" not in unit_cgroups:
            total_cg_oom_kills += oom_events_seen

        cpu_pct = None
        cpu_first = first_u.get("cpu_stat", {}).get("usage_usec")
        cpu_last = last_u.get("cpu_stat", {}).get("usage_usec")
        if cpu_first is not None and cpu_last is not None and elapsed_s > 0:
            delta_usec = max(0, cpu_last - cpu_first)
            cpu_pct = round((delta_usec / (elapsed_s * 1_000_000.0)) * 100.0, 2)

        cgroups_summary[unit] = {
            "exists": True,
            "cgroup_path": unit_cgroups[unit],
            "memory_current_last": fmt_bytes(cur_vals[-1] if cur_vals else None),
            "memory_current_max": fmt_bytes(obs_max),
            "memory_peak": fmt_bytes(cg_peak),
            "memory_peak_available": cg_peak is not None,
            "effective_peak": fmt_bytes(effective_peak),
            "memory_high": last_u.get("memory_high"),
            "memory_max": last_u.get("memory_max"),
            "memory_events_last": ev_last,
            "memory_events_delta": ev_delta,
            "cpu_pct_of_one_core": cpu_pct,
            "nr_throttled_delta": max(
                0,
                last_u.get("cpu_stat", {}).get("nr_throttled", 0)
                - first_u.get("cpu_stat", {}).get("nr_throttled", 0),
            ),
            "pids_last": last_u.get("pids", []),
        }

    # Aggregate per-process tree keyed by (pid, starttime) so PID reuse is safe.
    proc_first: dict[tuple[int, int], tuple[float, dict[str, Any]]] = {}
    proc_last: dict[tuple[int, int], tuple[float, dict[str, Any]]] = {}
    proc_peak_rss: dict[tuple[int, int], int] = {}
    proc_peak_pss: dict[tuple[int, int], int] = {}

    for t_val, tick in zip(timestamps, ticks):
        for key, p in tick["praxi_procs"].items():
            if key not in proc_first:
                proc_first[key] = (t_val, p)
            proc_last[key] = (t_val, p)
            proc_peak_rss[key] = max(proc_peak_rss.get(key, 0), p["rss_bytes"])
            if p["pss_bytes"] is not None:
                proc_peak_pss[key] = max(proc_peak_pss.get(key, 0), p["pss_bytes"])

    processes_summary: list[dict[str, Any]] = []
    total_proc_cpu_pct = 0.0
    any_proc_cpu = False
    for key in sorted(proc_last.keys(), key=lambda k: proc_peak_rss[k], reverse=True):
        t0, p0 = proc_first[key]
        t1, p1 = proc_last[key]
        dt = t1 - t0
        cpu_pct = None
        if dt > 0:
            tick_delta = max(0, (p1["utime"] + p1["stime"]) - (p0["utime"] + p0["stime"]))
            cpu_pct = round(((tick_delta / clk_tck) / dt) * 100.0, 2)
            total_proc_cpu_pct += cpu_pct
            any_proc_cpu = True
        processes_summary.append(
            {
                "pid": p1["pid"],
                "starttime": p1["starttime"],
                "ppid": p1["ppid"],
                "comm": p1["comm"],
                "in_praxi_slice": p1.get("in_praxi_slice"),
                "rss_last": fmt_bytes(p1["rss_bytes"]),
                "rss_peak": fmt_bytes(proc_peak_rss[key]),
                "hwm": fmt_bytes(p1["hwm_bytes"]),
                "pss_last": fmt_bytes(p1["pss_bytes"]),
                "pss_peak": fmt_bytes(proc_peak_pss.get(key)),
                "cpu_pct_of_one_core": cpu_pct,
            }
        )

    sum_rss_series = [t["sum_praxi_rss_bytes"] for t in ticks]
    sum_pss_series = [t["sum_praxi_pss_bytes"] for t in ticks if t["sum_praxi_pss_bytes"] is not None]
    sum_rss_max = max(sum_rss_series) if sum_rss_series else 0
    sum_pss_max = max(sum_pss_series) if sum_pss_series else None

    # Determine Praxi process-tree peak in bytes (prefer cgroup slice peak -> slice max -> sum PSS -> sum RSS)
    slice_cg = cgroups_summary.get("praxi.slice", {})
    if slice_cg.get("exists") and slice_cg.get("effective_peak"):
        praxi_tree_peak_bytes = slice_cg["effective_peak"]["bytes"]
        praxi_tree_peak_source = (
            "praxi.slice/memory.peak" if slice_cg.get("memory_peak_available") else "praxi.slice/memory.current_max"
        )
    elif sum_pss_max is not None:
        praxi_tree_peak_bytes = sum_pss_max
        praxi_tree_peak_source = "sum_proc_pss_max"
    else:
        praxi_tree_peak_bytes = sum_rss_max
        praxi_tree_peak_source = "sum_proc_rss_max (double-counts shared pages)"

    # Determine Praxi CPU % (prefer cgroup slice cpu.stat -> summed process CPU)
    if slice_cg.get("exists") and slice_cg.get("cpu_pct_of_one_core") is not None:
        praxi_cpu_pct = slice_cg["cpu_pct_of_one_core"]
    elif any_proc_cpu:
        praxi_cpu_pct = round(total_proc_cpu_pct, 2)
    else:
        praxi_cpu_pct = None

    # Bound stored sample timeline
    timeline_raw = [
        {
            "t_s": round(t_val, 3),
            "host_used_bytes": tick["meminfo"]["host_used_bytes"],
            "mem_available_bytes": tick["meminfo"]["mem_available_bytes"],
            "swap_used_bytes": tick["meminfo"]["swap_used_bytes"],
            "slice_memory_current_bytes": tick["cgroups"].get("praxi.slice", {}).get("memory_current_bytes"),
            "sum_praxi_rss_bytes": tick["sum_praxi_rss_bytes"],
            "sum_praxi_pss_bytes": tick["sum_praxi_pss_bytes"],
        }
        for t_val, tick in zip(timestamps, ticks)
    ]
    max_samples = max(1, args.max_samples)
    samples_truncated = len(timeline_raw) > max_samples
    if samples_truncated:
        step = max(1, len(timeline_raw) // max_samples)
        timeline = timeline_raw[::step][:max_samples]
    else:
        timeline = timeline_raw

    # Collect display+session idle combined bytes if requested
    session_bytes = None
    xvfb_cur = cgroups_summary.get("praxi-xvfb.service", {}).get("effective_peak")
    desk_cur = cgroups_summary.get("praxi-desktop.service", {}).get("effective_peak")
    if xvfb_cur or desk_cur:
        session_bytes = (xvfb_cur["bytes"] if xvfb_cur else 0) + (desk_cur["bytes"] if desk_cur else 0)

    escaped_union: list[dict[str, Any]] = []
    seen_escaped: set[tuple[int, str]] = set()
    for tick in ticks:
        for item in tick["escaped_slice_pids"]:
            k = (item["pid"], item["comm"])
            if k not in seen_escaped:
                seen_escaped.add(k)
                escaped_union.append(item)

    oom_total = max(vmstat_oom_delta, total_cg_oom_kills)
    host_used_peak = max(host_used_series) if host_used_series else None

    # Budget evaluation
    violations: list[str] = []
    if praxi_tree_peak_bytes > args.budget_tree_bytes:
        violations.append(
            f"praxi_tree_peak_bytes ({praxi_tree_peak_bytes}) > budget ({args.budget_tree_bytes})"
        )
    if host_used_peak is not None and host_used_peak > args.budget_host_bytes:
        violations.append(
            f"host_used_peak_bytes ({host_used_peak}) > budget ({args.budget_host_bytes})"
        )
    if oom_total > 0:
        violations.append(f"oom_kill_events ({oom_total}) > 0")
    if args.budget_session_mib is not None and session_bytes is not None:
        limit = args.budget_session_mib * MIB
        if session_bytes > limit:
            violations.append(
                f"display_session_bytes ({session_bytes}) > budget ({limit} = {args.budget_session_mib} MiB)"
            )
    if args.budget_idle_cpu_pct is not None and praxi_cpu_pct is not None:
        if praxi_cpu_pct > args.budget_idle_cpu_pct:
            violations.append(
                f"praxi_cpu_pct ({praxi_cpu_pct}%) > budget ({args.budget_idle_cpu_pct}%)"
            )
    if args.require_slice_containment and escaped_union:
        violations.append(f"processes escaped praxi.slice: {escaped_union}")

    budget_passed = len(violations) == 0

    report: dict[str, Any] = {
        "schema_version": 1,
        "timestamp_utc": datetime.now(timezone.utc).isoformat(),
        "label": args.label,
        "mode": "fixture" if fixture_dir else "live",
        "system": {
            "arch": arch,
            "kernel": kernel,
            "os": os_name,
            "browser_version": browser_version,
        },
        "sampling": {
            "interval_s": args.interval,
            "duration_requested_s": args.duration,
            "elapsed_s": round(elapsed_s, 3),
            "sample_count": len(ticks),
            "stored_samples": len(timeline),
            "max_samples": max_samples,
            "samples_truncated": samples_truncated,
            "vanished_pid_events": total_vanished,
            "pss_available": any(t["pss_available"] for t in ticks),
        },
        "host_memory": {
            "mem_total": fmt_bytes(mem_total),
            "mem_available_min": fmt_bytes(min(mem_avail_series) if mem_avail_series else None),
            "mem_available_last": fmt_bytes(mem_avail_series[-1] if mem_avail_series else None),
            "host_used_peak": fmt_bytes(host_used_peak),
            "host_used_last": fmt_bytes(host_used_series[-1] if host_used_series else None),
            "swap_total": fmt_bytes(swap_total),
            "swap_used_peak": fmt_bytes(max(swap_used_series) if swap_used_series else 0),
            "swap_in_pages_delta": swap_in_delta,
            "swap_out_pages_delta": swap_out_delta,
            "oom_kill_events": oom_total,
        },
        "praxi_tree": {
            "effective_peak": fmt_bytes(praxi_tree_peak_bytes),
            "effective_peak_source": praxi_tree_peak_source,
            "sum_process_rss_max": fmt_bytes(sum_rss_max),
            "sum_process_pss_max": fmt_bytes(sum_pss_max),
            "display_and_session_peak": fmt_bytes(session_bytes),
            "cpu_pct_of_one_core": praxi_cpu_pct,
            "escaped_slice_pids": escaped_union,
        },
        "cgroups": cgroups_summary,
        "processes": processes_summary,
        "top_other_host_services": ticks[-1]["top_other_services"] if ticks else [],
        "samples": timeline,
        "budget": {
            "tree_limit": fmt_bytes(args.budget_tree_bytes),
            "host_limit": fmt_bytes(args.budget_host_bytes),
            "session_limit_mib": args.budget_session_mib,
            "idle_cpu_limit_pct": args.budget_idle_cpu_pct,
            "passed": budget_passed,
            "violations": violations,
        },
        "caveats": CAVEATS,
    }

    exit_code = 0
    if args.check_budget and not budget_passed:
        exit_code = 2
    return report, exit_code


def format_text(report: dict[str, Any]) -> str:
    sys_info = report["system"]
    samp = report["sampling"]
    hm = report["host_memory"]
    pt = report["praxi_tree"]
    b = report["budget"]

    def fb(d: dict[str, Any] | None) -> str:
        if not d:
            return "unavailable"
        return f"{d['mib']} MiB / {d['gib']} GiB ({d['gb_decimal']} GB dec, {d['bytes']} B)"

    lines = [
        f"=== Praxi Budget Report [{report['label']}] ({report['mode']}) ===",
        f"System   : arch={sys_info['arch']} kernel={sys_info['kernel']} os={sys_info['os']}",
        f"Browser  : {sys_info['browser_version']}",
        f"Sampling : interval={samp['interval_s']}s elapsed={samp['elapsed_s']}s "
        f"samples={samp['sample_count']} stored={samp['stored_samples']} "
        f"truncated={samp['samples_truncated']} vanished_pids={samp['vanished_pid_events']} "
        f"pss_available={samp['pss_available']}",
        "",
        "Host Memory & Swap:",
        f"  MemTotal        : {fb(hm['mem_total'])}",
        f"  MemAvailable min: {fb(hm['mem_available_min'])}",
        f"  Host used peak  : {fb(hm['host_used_peak'])}",
        f"  Swap used peak  : {fb(hm['swap_used_peak'])} (in_delta={hm['swap_in_pages_delta']}, out_delta={hm['swap_out_pages_delta']})",
        f"  OOM kills       : {hm['oom_kill_events']}",
        "",
        "Praxi Process Tree:",
        f"  Effective peak  : {fb(pt['effective_peak'])} [source: {pt['effective_peak_source']}]",
        f"  Display+Session : {fb(pt['display_and_session_peak'])}",
        f"  Summed RSS max  : {fb(pt['sum_process_rss_max'])} (NOTE: double-counts shared pages)",
        f"  Summed PSS max  : {fb(pt['sum_process_pss_max'])}",
        f"  CPU (1-core %)  : {pt['cpu_pct_of_one_core']}",
        f"  Escaped slice   : {pt['escaped_slice_pids']}",
        "",
        "Systemd Cgroups:",
    ]
    for unit, cg in report["cgroups"].items():
        if not cg.get("exists"):
            lines.append(f"  {unit}: absent")
            continue
        lines.append(
            f"  {unit}: cur={fb(cg['memory_current_last'])} peak={fb(cg['effective_peak'])} "
            f"peak_avail={cg['memory_peak_available']} high={cg['memory_high']} max={cg['memory_max']} "
            f"cpu_1core={cg['cpu_pct_of_one_core']}% pids={len(cg['pids_last'])}"
        )

    lines.append("")
    lines.append("Praxi Processes (sorted by peak RSS):")
    for p in report["processes"][:20]:
        lines.append(
            f"  pid={p['pid']} comm={p['comm']} slice={p['in_praxi_slice']} "
            f"rss_peak={fb(p['rss_peak'])} pss_peak={fb(p['pss_peak'])} cpu={p['cpu_pct_of_one_core']}%"
        )

    lines.append("")
    lines.append("Top Non-Praxi Host Processes (by RSS):")
    for p in report["top_other_host_services"][:10]:
        lines.append(f"  pid={p['pid']} uid={p['uid']} comm={p['comm']} rss={round(p['rss_bytes'] / MIB, 2)} MiB")

    lines.append("")
    lines.append(f"Budget Result: {'PASS' if b['passed'] else 'FAIL'}")
    for v in b["violations"]:
        lines.append(f"  VIOLATION: {v}")
    lines.append("Caveats:")
    for c in report["caveats"]:
        lines.append(f"  - {c}")
    return "\n".join(lines)


def build_arg_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description="Praxi Linux host resource & budget measurement tool")
    p.add_argument("--label", default="ad-hoc", help="Scenario label")
    p.add_argument("--duration", type=float, default=5.0, help="Sampling duration in seconds (0 = single snapshot)")
    p.add_argument("--interval", type=float, default=0.5, help="Sampling interval in seconds")
    p.add_argument("--max-samples", type=int, default=1200, help="Max stored sample timeline entries")
    p.add_argument("--units", nargs="+", default=DEFAULT_UNITS, help="Systemd units/slice to track")
    p.add_argument("--proc-root", default="/proc", help="procfs root path")
    p.add_argument("--cgroup-root", default="/sys/fs/cgroup", help="cgroup v2 root path")
    p.add_argument("--browser-path", default=None, help="Explicit browser binary path for --version")
    p.add_argument("--fixture-dir", default=None, help="Replay synthetic proc/cgroup fixture directory")
    p.add_argument("--dry-run", action="store_true", help="Print measurement plan without sampling")
    p.add_argument("--budget-tree-bytes", type=int, default=2_500_000_000, help="Praxi tree peak limit in bytes (default: 2.5 GB)")
    p.add_argument("--budget-host-bytes", type=int, default=6_000_000_000, help="Host used peak limit in bytes (default: 6.0 GB)")
    p.add_argument("--budget-session-mib", type=int, default=None, help="Optional display+session peak limit in MiB")
    p.add_argument("--budget-idle-cpu-pct", type=float, default=None, help="Optional idle CPU limit (% of one core)")
    p.add_argument("--require-slice-containment", action="store_true", help="Fail budget check if praxi UID processes exist outside praxi.slice")
    p.add_argument("--check-budget", action="store_true", help="Exit nonzero (2) if any budget is exceeded")
    p.add_argument("--format", choices=("text", "json", "both"), default="text", help="Stdout format")
    p.add_argument("--json-out", default=None, help="Write JSON report to file path")
    return p


def main() -> int:
    parser = build_arg_parser()
    args = parser.parse_args()

    if args.dry_run:
        plan = {
            "dry_run": True,
            "label": args.label,
            "duration_s": args.duration,
            "interval_s": args.interval,
            "max_samples": args.max_samples,
            "units": args.units,
            "proc_root": args.proc_root,
            "cgroup_root": args.cgroup_root,
            "fixture_dir": args.fixture_dir,
            "budget_tree_bytes": args.budget_tree_bytes,
            "budget_host_bytes": args.budget_host_bytes,
            "budget_session_mib": args.budget_session_mib,
            "budget_idle_cpu_pct": args.budget_idle_cpu_pct,
            "privacy": "reads comm/stat/status/smaps_rollup/meminfo/vmstat/cgroup only; never cmdline/environ/frames/urls",
        }
        print(json.dumps(plan, indent=2))
        return 0

    report, exit_code = run_measurement(args)
    if args.json_out:
        out_path = Path(args.json_out)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    if args.format in ("text", "both"):
        print(format_text(report))
    if args.format in ("json", "both"):
        print(json.dumps(report, indent=2))
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
