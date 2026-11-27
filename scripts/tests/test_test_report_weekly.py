"""Weekly test report tests; no network or credentials required."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("weekly", Path(__file__).parents[1] / "test_report_weekly.py")
weekly = importlib.util.module_from_spec(spec)
spec.loader.exec_module(weekly)


def case(name, seconds, binary="demo", failed_attempts=0, message=""):
    return {"binary": binary, "name": name, "seconds": seconds, "status": "passed",
            "failed_attempts": failed_attempts, "message": message}


def report(title, flaky=(), failed=(), tests=(), binaries=()):
    return {"title": title, "tests": 10, "seconds": 100.0, "flaky": list(flaky), "failed": list(failed),
            "slowest_tests": list(tests), "slowest_binaries": list(binaries)}


def write(root, run, artifact, data):
    folder = Path(root) / run / artifact
    folder.mkdir(parents=True)
    (folder / "report.json").write_text(json.dumps(data), encoding="utf-8")


class WeeklyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def test_groups_nights_by_title_and_counts_flaky_nights(self):
        flaky = case("sometimes", 1.0, failed_attempts=1, message="timed out | slow")
        write(self.root, "1", "test-report-workspace-Linux", report("workspace, Linux", flaky=[flaky]))
        write(self.root, "2", "test-report-workspace-Linux", report("workspace, Linux"))
        write(self.root, "3", "test-report-workspace-Linux", report("workspace, Linux", flaky=[flaky]))
        write(self.root, "3", "test-report-workspace-Windows", report("workspace, Windows"))
        by_title = weekly.load(self.root)
        self.assertEqual(sorted(by_title), ["workspace, Linux", "workspace, Windows"])
        self.assertEqual(weekly.occurrences(by_title["workspace, Linux"], "flaky")[0]["nights"], 2)
        text = weekly.render(by_title, "o/r", "2026-11-27", None)
        self.assertIn("| 2 of 3 | `sometimes` | `demo` | timed out \\| slow |", text)
        self.assertIn("### workspace, Windows (1 night(s))", text)
        self.assertTrue(text.startswith(weekly.MARKER))

    def test_slowest_uses_the_median_of_the_nights_it_appears_in(self):
        for run, seconds in (("1", 10.0), ("2", 30.0), ("3", 20.0)):
            write(self.root, run, "a", report("t", tests=[case("slow", seconds), case("quick", 1.0)]))
        row = weekly.slowest(weekly.load(self.root)["t"], "slowest_tests", "name")[0]
        self.assertEqual((row["name"], row["median"], row["max"], row["nights"]), ("slow", 20.0, 30.0, 3))

    def test_a_week_without_reports_says_so(self):
        text = weekly.render(weekly.load(self.root), "o/r", "2026-11-27", None)
        self.assertIn("No nightly test report was available", text)

    def test_ignores_unreadable_or_foreign_json(self):
        write(self.root, "1", "a", {"unrelated": True})
        folder = self.root / "2" / "b"
        folder.mkdir(parents=True)
        (folder / "report.json").write_text("not json", encoding="utf-8")
        self.assertEqual(weekly.load(self.root), {})

    def test_job_timings_only_count_successful_jobs(self):
        def job(name, conclusion, start, end):
            return {"name": name, "conclusion": conclusion, "started_at": start, "completed_at": end}
        ci = {"runs": [{"attempts": [{"jobs": [
            job("Build & Test (windows-latest)", "success", "2026-11-27T00:00:00Z", "2026-11-27T00:20:00Z"),
            job("Build & Test (windows-latest)", "success", "2026-11-27T01:00:00Z", "2026-11-27T01:30:00Z"),
            job("Build & Test (windows-latest)", "failure", "2026-11-27T02:00:00Z", "2026-11-27T02:01:00Z"),
            job("Docs links", "success", "2026-11-27T00:00:00Z", "2026-11-27T00:00:10Z"),
        ]}]}]}
        rows = weekly.job_timings(ci)
        self.assertEqual(rows[0][0], "Build & Test (windows-latest)")
        self.assertEqual((rows[0][1], rows[0][2], rows[0][4]), (2, 1500.0, 1800.0))
        self.assertIn("| Build & Test (windows-latest) | 2 | 25.0 min | 30.0 min | 30.0 min |",
                      weekly.render({}, "o/r", "2026-11-27", ci))

    def test_long_reports_are_cut_below_the_comment_limit(self):
        tests = [case(f"test_{i}", 100 - i * 0.01, message="x" * 300) for i in range(20)]
        flaky = [case(f"flaky_{i}", 1.0, failed_attempts=1, message="x" * 300) for i in range(500)]
        for title in ("a", "b", "c"):
            write(self.root, "1", title, report(title, flaky=flaky, tests=tests))
        weekly.LIMIT = 6000
        try:
            text = weekly.render(weekly.load(self.root), "o/r", "2026-11-27", None)
        finally:
            weekly.LIMIT = 60000
        self.assertLess(len(text), 6000 + 4000)  # shrinks with the row count; never unbounded


if __name__ == "__main__":
    unittest.main()
