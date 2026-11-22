#!/usr/bin/env python3
"""Summarise a cargo-nextest JUnit file: slowest tests, slowest test binaries, flaky and
failing tests. Used by .github/workflows/test-report.yml; also runs locally.

    python3 scripts/test_report.py target/nextest/ci-report/junit.xml --title Linux \
        --markdown report.md --json report.json
    python3 scripts/test_report.py --self-test

A test is *flaky* when it failed and then passed on a retry (nextest writes a
`flakyFailure` element), and *failing* when every attempt failed (`failure`).
"""
from __future__ import annotations

import argparse
import json
import sys
import unittest
import xml.etree.ElementTree as ET
from dataclasses import asdict, dataclass

SLOWEST = 20


@dataclass
class Case:
    binary: str
    name: str
    seconds: float
    status: str  # "passed", "flaky", "failed" or "skipped"
    failed_attempts: int
    message: str

    @property
    def crate(self) -> str:
        # nextest names binaries "<crate>" (lib), "<crate>::<test>" or "<crate>::bin/<bin>".
        return self.binary.split("::", 1)[0]


def parse(xml_text: str) -> list[Case]:
    root = ET.fromstring(xml_text)
    cases = []
    for suite in root.iter("testsuite"):
        binary = suite.get("name", "")
        for case in suite.iter("testcase"):
            flaky = case.findall("flakyFailure")
            failure = case.find("failure")
            if failure is None:
                failure = case.find("error")
            reruns = case.findall("rerunFailure") + case.findall("rerunError")
            if failure is not None:
                status, attempts, first = "failed", 1 + len(reruns), failure
            elif flaky:
                status, attempts, first = "flaky", len(flaky), flaky[0]
            elif case.find("skipped") is not None:
                status, attempts, first = "skipped", 0, None
            else:
                status, attempts, first = "passed", 0, None
            message = (first.get("message") or "") if first is not None else ""
            cases.append(
                Case(binary, case.get("name", ""), float(case.get("time") or 0), status, attempts, message)
            )
    return cases


def summarise(cases: list[Case]) -> dict:
    ran = [c for c in cases if c.status != "skipped"]
    binaries: dict[str, list[float]] = {}
    for case in ran:
        binaries.setdefault(case.binary, []).append(case.seconds)
    slow_binaries = sorted(
        ((name, sum(times), len(times)) for name, times in binaries.items()),
        key=lambda item: (-item[1], item[0]),
    )
    return {
        "tests": len(ran),
        "seconds": round(sum(c.seconds for c in ran), 3),
        "flaky": [asdict(c) for c in ran if c.status == "flaky"],
        "failed": [asdict(c) for c in ran if c.status == "failed"],
        "slowest_tests": [asdict(c) for c in sorted(ran, key=lambda c: (-c.seconds, c.binary, c.name))[:SLOWEST]],
        "slowest_binaries": [
            {"binary": name, "seconds": round(total, 3), "tests": count}
            for name, total, count in slow_binaries[:SLOWEST]
        ],
    }


def cell(text: str, limit: int = 120) -> str:
    text = " ".join(text.split()).replace("|", "\\|")
    return text if len(text) <= limit else text[: limit - 1] + "…"


def markdown(summary: dict, title: str) -> str:
    out = [f"## Test report: {title}", ""]
    out.append(
        f"{summary['tests']} tests, {summary['seconds']:.1f} s of test time in total, "
        f"{len(summary['flaky'])} flaky, {len(summary['failed'])} failing."
    )
    out.append("")
    out.append("### Flaky tests (failed, then passed on a retry)")
    out.append("")
    if summary["flaky"]:
        out += ["| Test | Binary | Failed attempts | First failure |", "| --- | --- | ---: | --- |"]
        for c in summary["flaky"]:
            out.append(f"| `{c['name']}` | `{c['binary']}` | {c['failed_attempts']} | {cell(c['message'])} |")
    else:
        out.append("None in this run.")
    out.append("")
    out.append("### Failing tests (every attempt failed)")
    out.append("")
    if summary["failed"]:
        out += ["| Test | Binary | Attempts | Failure |", "| --- | --- | ---: | --- |"]
        for c in summary["failed"]:
            out.append(f"| `{c['name']}` | `{c['binary']}` | {c['failed_attempts']} | {cell(c['message'])} |")
    else:
        out.append("None in this run.")
    out.append("")
    out.append(f"### {SLOWEST} slowest tests")
    out.append("")
    out += ["| Seconds | Test | Binary |", "| ---: | --- | --- |"]
    for c in summary["slowest_tests"]:
        out.append(f"| {c['seconds']:.2f} | `{c['name']}` | `{c['binary']}` |")
    out.append("")
    out.append(f"### {SLOWEST} slowest test binaries (sum of their tests)")
    out.append("")
    out += ["| Seconds | Tests | Binary |", "| ---: | ---: | --- |"]
    for b in summary["slowest_binaries"]:
        out.append(f"| {b['seconds']:.1f} | {b['tests']} | `{b['binary']}` |")
    out.append("")
    return "\n".join(out)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("junit", help="JUnit XML written by cargo nextest")
    parser.add_argument("--title", default="local run")
    parser.add_argument("--markdown", help="write the Markdown report here (default: stdout)")
    parser.add_argument("--json", help="also write the summary as JSON here")
    args = parser.parse_args(argv)
    with open(args.junit, encoding="utf-8") as f:
        summary = summarise(parse(f.read()))
    report = markdown(summary, args.title)
    if args.markdown:
        with open(args.markdown, "a", encoding="utf-8") as f:
            f.write(report + "\n")
    else:
        print(report)
    if args.json:
        with open(args.json, "w", encoding="utf-8") as f:
            json.dump({"title": args.title, **summary}, f, indent=2)
    return 0


SAMPLE = """<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="4" failures="1">
  <testsuite name="demo" tests="3">
    <testcase name="steady" classname="demo" time="0.010"/>
    <testcase name="flaky_once" classname="demo" time="0.009">
      <flakyFailure time="0.010" message="panicked at src/lib.rs:6:5" type="test failure">x</flakyFailure>
    </testcase>
    <testcase name="always_fails" classname="demo" time="0.010">
      <failure message="panicked at src/lib.rs:9:21 | broken" type="test failure">x</failure>
      <rerunFailure time="0.009" message="again" type="test failure">x</rerunFailure>
      <rerunFailure time="0.008" message="again" type="test failure">x</rerunFailure>
    </testcase>
  </testsuite>
  <testsuite name="demo::integration" tests="1">
    <testcase name="slow" classname="demo::integration" time="2.500"/>
  </testsuite>
</testsuites>
"""


class ReportTests(unittest.TestCase):
    def setUp(self) -> None:
        self.cases = parse(SAMPLE)
        self.summary = summarise(self.cases)

    def test_classifies_flaky_and_failing(self) -> None:
        by_name = {c.name: c for c in self.cases}
        self.assertEqual(by_name["steady"].status, "passed")
        self.assertEqual((by_name["flaky_once"].status, by_name["flaky_once"].failed_attempts), ("flaky", 1))
        self.assertEqual((by_name["always_fails"].status, by_name["always_fails"].failed_attempts), ("failed", 3))
        self.assertEqual([c["name"] for c in self.summary["flaky"]], ["flaky_once"])
        self.assertEqual([c["name"] for c in self.summary["failed"]], ["always_fails"])

    def test_orders_slowest_tests_and_binaries(self) -> None:
        self.assertEqual(self.summary["slowest_tests"][0]["name"], "slow")
        self.assertEqual(self.summary["slowest_binaries"][0]["binary"], "demo::integration")
        self.assertEqual(self.summary["slowest_binaries"][1]["tests"], 3)
        self.assertEqual(self.cases[-1].crate, "demo")

    def test_markdown_escapes_table_cells(self) -> None:
        text = markdown(self.summary, "sample")
        self.assertIn("4 tests, 2.5 s of test time in total, 1 flaky, 1 failing.", text)
        self.assertIn("src/lib.rs:9:21 \\| broken", text)


if __name__ == "__main__":
    if "--self-test" in sys.argv:
        unittest.main(argv=[sys.argv[0]])
    sys.exit(main(sys.argv[1:]))
