#!/usr/bin/env python3
"""Read GitHub Actions metadata and optional JUnit artifacts using gh only."""
from __future__ import annotations

import argparse
from collections import defaultdict
from datetime import datetime, timezone
import io
import json
from pathlib import Path
import statistics
import subprocess
import sys
import xml.etree.ElementTree as ET
import zipfile


def api(endpoint, *, binary=False):
    result = subprocess.run(["gh", "api", endpoint], check=True, capture_output=True)
    return result.stdout if binary else json.loads(result.stdout)


def pages(endpoint, key):
    separator = "&" if "?" in endpoint else "?"
    page = 1
    while True:
        items = api(f"{endpoint}{separator}per_page=100&page={page}")[key]
        yield from items
        if len(items) < 100:
            return
        page += 1


def seconds(start, end):
    if not start or not end or start.startswith("0001") or end.startswith("0001"):
        return None
    return max(0, (datetime.fromisoformat(end.replace("Z", "+00:00"))
                   - datetime.fromisoformat(start.replace("Z", "+00:00"))).total_seconds())


def junit_tests(data, source):
    root = ET.fromstring(data)
    tests = []
    for case in root.iter("testcase"):
        if case.get("time") is None or case.find("skipped") is not None:
            continue
        tests.append({"source": source,
                      "name": "::".join(filter(None, [case.get("classname"), case.get("name")])),
                      "seconds": float(case.get("time")),
                      "retried": any(case.find(tag) is not None for tag in
                                     ("flakyFailure", "flakyError", "rerunFailure", "rerunError"))})
    return tests


def artifact_tests(repo, run_id, notes):
    tests = []
    for artifact in pages(f"repos/{repo}/actions/runs/{run_id}/artifacts", "artifacts"):
        # Do not download unrelated application bundles, screenshots or build artifacts.
        if not any(word in artifact["name"].lower() for word in ("junit", "nextest", "test-results")):
            continue
        if artifact["expired"] or artifact["size_in_bytes"] > 20_000_000:
            notes.append(f"Run {run_id}: skipped expired/oversized test artifact {artifact['id']}.")
            continue
        try:
            data = api(f"repos/{repo}/actions/artifacts/{artifact['id']}/zip", binary=True)
            with zipfile.ZipFile(io.BytesIO(data)) as archive:
                # Read in memory; never extract artifact-controlled paths or execute content.
                budget = 50_000_000
                for member in archive.infolist():
                    budget -= member.file_size
                    if budget < 0:
                        raise ValueError("uncompressed artifact exceeds 50 MB")
                    if member.filename.lower().endswith(".xml"):
                        tests.extend(junit_tests(archive.read(member), f"{run_id}/{artifact['id']}"))
        except (subprocess.CalledProcessError, zipfile.BadZipFile, ET.ParseError, ValueError) as error:
            if isinstance(error, subprocess.CalledProcessError) and b"rate limit" in (error.stderr or b"").lower():
                raise
            notes.append(f"Run {run_id}: artifact {artifact['id']} unavailable/invalid ({type(error).__name__}).")
    return tests


def flaky_jobs(attempts):
    """Match the same job within the same run; new commits are never reruns."""
    failed = {}
    candidates = []
    for attempt in sorted(attempts, key=lambda item: item["number"]):
        for job in attempt["jobs"]:
            if job["conclusion"] == "failure":
                failed[job["name"]] = attempt["number"]
            elif job["conclusion"] == "success" and job["name"] in failed:
                candidates.append((job["name"], failed.pop(job["name"]), attempt["number"]))
    return candidates


def attempt_record(number, origin, jobs):
    # GitHub may return successful jobs carried forward from the previous attempt,
    # even with a new job id/run_attempt. Their timestamps precede this attempt.
    jobs = [job for job in jobs if number == 1 or not job["started_at"]
            or job["started_at"] >= origin]
    active = [job for job in jobs if job["conclusion"] != "skipped" and job["started_at"]]
    first = min((job["started_at"] for job in active), default=None)
    last = max((job["completed_at"] for job in active if job["completed_at"]), default=None)
    return {"number": number, "origin": origin, "startup_seconds": seconds(origin, first),
            "execution_span_seconds": seconds(first, last), "jobs": jobs}


def collect(repo, runs, artifacts=True):
    records, tests, notes = [], [], []
    for run in runs:
        print(f"Reading run {run['id']} ({run['run_attempt']} attempt(s))", file=sys.stderr)
        attempts = []
        for number in range(1, run["run_attempt"] + 1):
            metadata = run if number == run["run_attempt"] else api(
                f"repos/{repo}/actions/runs/{run['id']}/attempts/{number}")
            jobs = list(pages(f"repos/{repo}/actions/runs/{run['id']}/attempts/{number}/jobs", "jobs"))
            # GitHub keeps the original created_at across reruns. Avoid claiming that
            # the human delay before requesting a rerun was runner queue time.
            origin = metadata["created_at"] if number == 1 else metadata["run_started_at"]
            attempts.append(attempt_record(number, origin, jobs))
        records.append({"id": run["id"], "url": run["html_url"], "sha": run["head_sha"],
                        "conclusion": run["conclusion"], "attempts": attempts})
        if artifacts:
            tests.extend(artifact_tests(repo, run["id"], notes))
    return {"repo": repo, "collected_at": datetime.now(timezone.utc).isoformat(),
            "runs": records, "tests": tests, "notes": notes}


def cell(value):
    return str(value).replace("|", "\\|").replace("\n", " ").replace("\r", " ")


def duration(value):
    return "n/a" if value is None else f"{value:.0f}s"


def render(report):
    lines = ["# CI timing report", "", f"Repository: {report['repo']}; collected {report['collected_at']}; "
             f"{len(report['runs'])} completed runs.", "",
             "Startup is creation → first active job (reruns: attempt start → first job). "
             "Execution span is first job start → last job completion, including gaps. "
             "Job wait is attempt origin → job start: it includes dependencies and runner queue; "
             "the API does not expose pure runner queue time. Cancelled/failed samples are partial.", ""]
    grouped = defaultdict(list)
    for run in report["runs"]:
        for attempt in run["attempts"]:
            for job in attempt["jobs"]:
                if job["conclusion"] == "success":
                    value = seconds(job["started_at"], job["completed_at"])
                    if value is not None:
                        grouped[job["name"]].append(value)
    lines += ["## Successful job durations", "", "| Job | Samples | Median | Min | Max |",
              "| --- | ---: | ---: | ---: | ---: |"]
    for name, values in sorted(grouped.items(), key=lambda item: statistics.median(item[1]), reverse=True):
        lines.append(f"| {cell(name)} | {len(values)} | {duration(statistics.median(values))} | "
                     f"{duration(min(values))} | {duration(max(values))} |")
    lines += ["", "## Failed then passed on rerun (candidates, not proven flaky tests)", ""]
    candidates = [f"- Run {run['id']}: {cell(name)}, attempt {before} failed → {after} passed."
                  for run in report["runs"] for name, before, after in flaky_jobs(run["attempts"])]
    lines += candidates or ["No failed → passed jobs observed within the sampled run attempts."]
    lines += ["", "## Slowest 15 test observations", ""]
    if report["tests"]:
        lines += ["| Test | Duration | Retried in JUnit | Run/artifact |", "| --- | ---: | --- | --- |"]
        for test in sorted(report["tests"], key=lambda item: item["seconds"], reverse=True)[:15]:
            lines.append(f"| {cell(test['name'])} | {test['seconds']:.3f}s | {test['retried']} | {test['source']} |")
    else:
        lines.append("No readable JUnit/nextest JUnit data available; use the per-job and per-step timings below. "
                     "No test-level flakiness or slowest-test claim can be made.")
    lines += ["", "## Runs, attempts, jobs and steps", ""]
    for run in report["runs"]:
        lines += [f"### [Run {run['id']}]({run['url']}) ({run['conclusion']})", ""]
        for attempt in run["attempts"]:
            lines += [f"Attempt {attempt['number']}: startup {duration(attempt['startup_seconds'])}; "
                      f"execution span {duration(attempt['execution_span_seconds'])}.", "",
                      "| Job / step | Result | Job wait | Duration |", "| --- | --- | ---: | ---: |"]
            for job in attempt["jobs"]:
                lines.append(f"| **{cell(job['name'])}** | {job['conclusion']} | "
                             f"{duration(seconds(attempt['origin'], job['started_at']))} | "
                             f"{duration(seconds(job['started_at'], job['completed_at']))} |")
                for step in job.get("steps", []):
                    lines.append(f"| ↳ {cell(step['name'])} | {step['conclusion']} | | "
                                 f"{duration(seconds(step.get('started_at'), step.get('completed_at')))} |")
        lines.append("")
    if report["notes"]:
        lines += ["## Collection limitations", ""] + [f"- {cell(note)}" for note in report["notes"]]
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default="emreocell/Praxi-Bot")
    parser.add_argument("--workflow", default="ci.yml")
    parser.add_argument("--runs", type=int, default=30)
    parser.add_argument("--run-id", type=int, action="append", help="select specific runs instead (repeatable)")
    parser.add_argument("--no-artifacts", action="store_true")
    parser.add_argument("--json-out", type=Path, help="optional metadata snapshot (no logs or credentials)")
    parser.add_argument("--output", type=Path, help="Markdown output file; default stdout")
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    if args.run_id:
        runs = [api(f"repos/{args.repo}/actions/runs/{run_id}") for run_id in args.run_id]
        if any(run["status"] != "completed" for run in runs):
            parser.error("selected runs must be completed")
    else:
        from itertools import islice
        runs = list(islice(pages(f"repos/{args.repo}/actions/workflows/{args.workflow}/runs?status=completed",
                                "workflow_runs"), args.runs))
    report = collect(args.repo, runs, not args.no_artifacts)
    if args.json_out:
        args.json_out.write_text(json.dumps(report, indent=2), encoding="utf-8")
    markdown = render(report)
    if args.output:
        args.output.write_text(markdown, encoding="utf-8")
    else:
        sys.stdout.buffer.write(markdown.encode("utf-8"))


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        # gh may include sensitive server details in stderr; let users run gh directly
        # to diagnose authentication/rate limits without copying those into public reports.
        sys.exit(f"gh api failed (exit {error.returncode}); check gh auth status / gh api rate_limit. No retry attempted.")
