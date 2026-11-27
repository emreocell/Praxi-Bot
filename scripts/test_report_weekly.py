#!/usr/bin/env python3
"""Combine the nightly test reports (`report.json` from scripts/test_report.py) of one week
into a single Markdown comment: tests that were flaky or failing on several nights, the
slowest tests and test binaries per platform, and (optionally) how long the pull request
jobs took. Used by .github/workflows/test-report-weekly.yml; also runs locally.

    python3 scripts/test_report_weekly.py reports/ --repo emreocell/Praxi-Bot \
        --ci-json ci.json --week-end 2026-11-27 > comment.md

`reports/` holds one directory per nightly run named after its run id, each with one
directory per artifact that contains a `report.json`:
`reports/<run id>/test-report-workspace-Linux/report.json`. `--ci-json` is the snapshot
written by `scripts/ci_report.py --json-out`.
"""
from __future__ import annotations

import argparse
import json
import statistics
import sys
from collections import defaultdict
from datetime import datetime
from pathlib import Path

MARKER = "<!-- weekly-test-report -->"
TOP = 10
# GitHub rejects a comment over 65 536 characters.
LIMIT = 60000


def cell(text: str, limit: int = 100) -> str:
    text = " ".join(str(text).split()).replace("|", "\\|")
    return text if len(text) <= limit else text[: limit - 1] + "…"


def load(root: Path) -> dict[str, list[tuple[str, dict]]]:
    """Reports by title (for example "workspace, Linux"), each with the run id it came from."""
    by_title: dict[str, list[tuple[str, dict]]] = defaultdict(list)
    for path in sorted(root.glob("*/*/report.json")):
        try:
            report = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        if isinstance(report, dict) and "slowest_tests" in report:
            by_title[str(report.get("title") or path.parent.name)].append((path.parents[1].name, report))
    return by_title


def occurrences(reports: list[tuple[str, dict]], key: str) -> list[dict]:
    """Tests listed under `key` ("flaky" or "failed") on at least one night, most nights first."""
    nights: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for _, report in reports:
        for case in report.get(key, []):
            nights[(case["binary"], case["name"])].append(case)
    rows = [
        {"binary": binary, "name": name, "nights": len(cases), "message": cases[0].get("message", "")}
        for (binary, name), cases in nights.items()
    ]
    return sorted(rows, key=lambda r: (-r["nights"], r["binary"], r["name"]))


def slowest(reports: list[tuple[str, dict]], key: str, label: str) -> list[dict]:
    """The `slowest_tests` or `slowest_binaries` entries, by median over the nights they appear in.
    Each report only keeps its own top 20, so a test missing on a night was simply not that slow."""
    seen: dict[tuple[str, str], list[float]] = defaultdict(list)
    for _, report in reports:
        for entry in report.get(key, []):
            seen[(entry["binary"], entry.get(label, ""))].append(float(entry["seconds"]))
    rows = [
        {"binary": binary, "name": name, "median": statistics.median(v), "max": max(v), "nights": len(v)}
        for (binary, name), v in seen.items()
    ]
    return sorted(rows, key=lambda r: (-r["median"], r["binary"], r["name"]))


def job_timings(ci: dict) -> list[tuple[str, int, float, float, float]]:
    """(job, samples, median, 90th percentile, max) seconds of the successful pull request and main jobs."""
    times: dict[str, list[float]] = defaultdict(list)
    for run in ci.get("runs", []):
        for attempt in run.get("attempts", []):
            for job in attempt.get("jobs", []):
                if job.get("conclusion") != "success" or not job.get("started_at") or not job.get("completed_at"):
                    continue
                start = datetime.fromisoformat(job["started_at"].replace("Z", "+00:00"))
                end = datetime.fromisoformat(job["completed_at"].replace("Z", "+00:00"))
                times[job["name"]].append((end - start).total_seconds())
    rows = []
    for name, v in times.items():
        v.sort()
        p90 = v[min(len(v) - 1, int(0.9 * len(v)))]
        rows.append((name, len(v), statistics.median(v), p90, v[-1]))
    return sorted(rows, key=lambda r: (-r[2], r[0]))


def render(by_title: dict, repo: str, week_end: str, ci: dict | None, top: int = TOP) -> str:
    runs = sorted({run for reports in by_title.values() for run, _ in reports})
    out = [MARKER, f"## Weekly test report, week ending {week_end}", ""]
    if not runs:
        out += ["No nightly test report was available for this week (the `Test report` workflow did not "
                "produce `test-report-*` artifacts), so there is nothing to compare.", ""]
    else:
        links = ", ".join(f"[{r}](https://github.com/{repo}/actions/runs/{r})" for r in runs)
        out += [f"Built from {len(runs)} nightly `Test report` run(s): {links}. "
                "Retries exist only in that workflow; pull request checks do not retry. "
                "How to read it: CONTRIBUTING.md, \"Test report\".", ""]
    for title in sorted(by_title):
        reports = by_title[title]
        out += [f"### {title} ({len(reports)} night(s))", ""]
        total = [float(r.get("seconds", 0)) for _, r in reports]
        out += [f"Test time per night (sum over all tests, not wall time): median {statistics.median(total):.0f} s, "
                f"tests per night: {max(int(r.get('tests', 0)) for _, r in reports)}.", ""]
        for key, heading, empty in (
            ("flaky", "Flaky (failed, then passed on a retry)", "None this week."),
            ("failed", "Failing (every attempt failed)", "None this week."),
        ):
            rows = occurrences(reports, key)
            out += [f"**{heading}**", ""]
            if rows:
                out += ["| Nights | Test | Binary | First failure |", "| ---: | --- | --- | --- |"]
                out += [f"| {r['nights']} of {len(reports)} | `{r['name']}` | `{r['binary']}` | {cell(r['message'])} |"
                        for r in rows[:top]]
                if len(rows) > top:
                    out.append(f"\n…and {len(rows) - top} more (see the `test-report-*` artifacts).")
            else:
                out.append(empty)
            out.append("")
        out += [f"**{top} slowest tests** (median of the nights where it was among the 20 slowest)", "",
                "| Median s | Max s | Test | Binary |", "| ---: | ---: | --- | --- |"]
        out += [f"| {r['median']:.1f} | {r['max']:.1f} | `{r['name']}` | `{r['binary']}` |"
                for r in slowest(reports, "slowest_tests", "name")[:top]]
        out += ["", f"**{top} slowest test binaries** (sum of their tests' time)", "",
                "| Median s | Max s | Binary |", "| ---: | ---: | --- |"]
        out += [f"| {r['median']:.0f} | {r['max']:.0f} | `{r['binary']}` |"
                for r in slowest(reports, "slowest_binaries", "")[:top]]
        out.append("")
    if ci is not None:
        rows = job_timings(ci)
        out += ["### Pull request check timings", "",
                "Successful jobs of the most recent `CI` runs (pull requests and `main`), job duration "
                "without queue time.", ""]
        if rows:
            out += ["| Job | Samples | Median | 90th percentile | Max |", "| --- | ---: | ---: | ---: | ---: |"]
            out += [f"| {cell(n)} | {c} | {m / 60:.1f} min | {p / 60:.1f} min | {x / 60:.1f} min |"
                    for n, c, m, p, x in rows]
        else:
            out.append("No successful job in the sample.")
        out.append("")
    text = "\n".join(out)
    if len(text) > LIMIT and top > 3:
        return render(by_title, repo, week_end, ci, top - 3)
    return text


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("reports", type=Path, help="directory with <run id>/<artifact>/report.json")
    parser.add_argument("--repo", default="emreocell/Praxi-Bot")
    parser.add_argument("--week-end", default=datetime.now().date().isoformat())
    parser.add_argument("--ci-json", type=Path, help="snapshot written by scripts/ci_report.py --json-out")
    args = parser.parse_args(argv)
    ci = json.loads(args.ci_json.read_text(encoding="utf-8")) if args.ci_json else None
    print(render(load(args.reports), args.repo, args.week_end, ci))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
