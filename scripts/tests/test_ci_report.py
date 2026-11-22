"""Focused report tests; no network or credentials required."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("ci_report", Path(__file__).parents[1] / "ci_report.py")
report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(report)


class ReportTests(unittest.TestCase):
    def test_carried_forward_jobs_do_not_inflate_rerun_timings(self):
        jobs = [{"name": "Windows", "conclusion": "success", "run_attempt": 2,
                 "started_at": "2026-11-22T00:00:00Z", "completed_at": "2026-11-22T00:10:00Z"},
                {"name": "Browser", "conclusion": "success", "run_attempt": 2,
                 "started_at": "2026-11-22T01:00:05Z", "completed_at": "2026-11-22T01:01:05Z"}]
        attempt = report.attempt_record(2, "2026-11-22T01:00:00Z", jobs)
        self.assertEqual([job["name"] for job in attempt["jobs"]], ["Browser"])
        self.assertEqual((attempt["startup_seconds"], attempt["execution_span_seconds"]), (5, 60))

    def test_rerun_candidates_require_failure_then_success_in_same_job(self):
        attempts = [
            {"number": 1, "jobs": [{"name": "Windows", "conclusion": "failure"},
                                    {"name": "Linux", "conclusion": "cancelled"}]},
            {"number": 2, "jobs": [{"name": "Windows", "conclusion": "success"},
                                    {"name": "Linux", "conclusion": "success"}]},
        ]
        self.assertEqual(report.flaky_jobs(attempts), [("Windows", 1, 2)])
        self.assertEqual(report.flaky_jobs(attempts[1:]), [])

    def test_junit_timings_skip_unexecuted_tests_and_preserve_retry_evidence(self):
        tests = report.junit_tests(b'''<testsuites><testsuite>
          <testcase classname="crate" name="slow" time="12.5"><flakyFailure/></testcase>
          <testcase classname="crate" name="failed" time="3"><failure/></testcase>
          <testcase name="ignored" time="0"><skipped/></testcase>
        </testsuite></testsuites>''', "123/456")
        self.assertEqual([(test["name"], test["seconds"], test["retried"]) for test in tests],
                         [("crate::slow", 12.5, True), ("crate::failed", 3.0, False)])

    def test_missing_timing_is_not_reported_as_zero_and_fallback_is_explicit(self):
        self.assertIsNone(report.seconds(None, "2026-11-22T00:00:05Z"))
        self.assertEqual(report.seconds("2026-11-22T00:00:00Z", "2026-11-22T00:00:05Z"), 5)
        text = report.render({"repo": "example/repo", "collected_at": "2026-11-22",
                              "runs": [], "tests": [], "notes": []})
        self.assertIn("No readable JUnit/nextest JUnit data available", text)
        self.assertIn("includes dependencies and runner queue", text)


if __name__ == "__main__":
    unittest.main()
