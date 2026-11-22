# Reading CI timing reports

Use Python 3.10+ and an authenticated [GitHub CLI](https://cli.github.com/) account
with read access to Actions metadata and artifacts. No Python packages are needed.
From the repository root:

```sh
python scripts/ci_report.py --repo emreocell/Praxi-Bot --runs 30 --output ci-report.md
```

The script uses `gh api` to read the last N completed runs of `ci.yml`, including
cancelled runs, every attempt's jobs and steps, and available test artifacts. It
prints Markdown to stdout when `--output` is omitted. To reproduce a particular
comparison, repeat `--run-id ID` for each completed run. Use `--workflow FILE.yml`
for another workflow. `--json-out snapshot.json` preserves the underlying metadata
locally for analysis; review it before publishing. The generated report includes
job and step names but never downloads workflow logs or reads credentials.

The successful-job table shows sample count, median and range. Failed and cancelled
job timings remain visible under individual runs but are excluded from that table.
Compare the same jobs and steps on several successful runs; a short failed or
cancelled run is not a speed improvement. Cache state, changed code, runner load
and Rust versions can all affect the result. Skipped jobs are not timed samples.
Successful jobs carried forward into a partial rerun are excluded when their
timestamps precede that attempt, even if the API gives them a new job ID.

**Queue versus execution:** startup is run creation to first active job start.
Execution span is first job start to last job completion. Per-job wait is measured
from creation (or the rerun attempt's start); it includes upstream dependencies.
GitHub's API does not expose a separate pure runner queue duration. Reruns exclude
the time a human waited before requesting another attempt.

**Tests:** artifacts with `junit`, `nextest` or `test-results` in their name are
searched for JUnit XML in memory, without extracting files. Expired artifacts,
archives over 20 MB compressed or 50 MB uncompressed, and unavailable downloads
are reported as limitations. Nextest's JUnit output is supported; arbitrary
nextest JSON formats are not. The slowest 15 test observations include their run
and artifact IDs. [Nextest retry tags](https://www.nexte.st/docs/features/retries/)
are evidence of retries; a retry that still fails is not proof of a flaky pass.
Without these artifacts the report explicitly falls back to job and step timings.

**Flaky candidates:** a job must fail and later pass in another attempt of the
same run. A later run at a different commit is not treated as a rerun. These are
job-level candidates, not proven flaky tests: runner or network failures can also
recover. Inspect the corresponding logs before proposing a test change. PR tests
are never retried or ignored by this script.

For metadata only, `--no-artifacts` avoids artifact list/download requests. The
default collection costs roughly two API requests per run, plus extra attempt,
pagination and test-artifact requests. Avoid frequent polling. API/authentication
failures stop collection without retrying; inspect `gh api rate_limit` and wait
for reset if needed. Reports use public metadata, but review test and job names
before sharing output from a private repository.

Run the offline parser regression tests with:

```sh
python -m unittest discover -s scripts/tests -p test_ci_report.py -v
```

The first repository measurement and proposed follow-ups are in
[the November 2026 report](ci-report-2026-11.md). Scheduled nextest measurements
and test-level follow-ups remain separate work tracked by
[issue #463](https://github.com/emreocell/Praxi-Bot/issues/463).
