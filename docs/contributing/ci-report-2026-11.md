# CI report: November 2026

Collected on 2026-11-22 at 14:37 UTC with `scripts/ci_report.py --runs 50`
against the public `emreocell/Praxi-Bot` CI workflow. The sample contains completed
runs created between 11:26:15 and 14:11:59 UTC that day, including cancellations
and four additional attempts across three rerun workflows. This is a busy-day
sample, not a month-long baseline. Workflow contents and source commits vary.
All durations below are seconds; successful jobs only, without counting jobs
carried forward from an earlier attempt twice.

## Where time goes

| Job | Samples | Median | Min–max |
| --- | ---: | ---: | ---: |
| Build & Test (windows-latest) | 24 | 2138 | 1601–2444 |
| Build & Test (ubuntu-latest) | 25 | 1227 | 1035–1447 |
| Local Embeddings (windows-latest) | 27 | 699 | 382–1011 |
| UI Lint & Typecheck | 17 | 135 | 99–169 |
| Browser integration (Chrome for Testing) | 32 | 72 | 60–112 |
| License Check | 32 | 66 | 52–85 |

No readable JUnit/nextest test artifacts were available. There is consequently no
defensible slowest-15-tests list or test-level flaky list. The slowest 15 **steps**
are the available fallback (compilation and execution are combined in test steps):

| Job | Step | Samples | Median |
| --- | --- | ---: | ---: |
| Windows workspace | Run tests | 24 | 1729.5 |
| Linux workspace | Run tests | 25 | 767 |
| Windows embeddings | Offline embedding tests | 27 | 450 |
| Historical Windows smoke | Build daemon and CLI fixture | 1 | 418 |
| Historical Windows smoke | Real-app smoke | 1 | 313 |
| Windows workspace | Post Cache Rust dependencies | 24 | 176.5 |
| Linux workspace | Offline embedding tests | 25 | 166 |
| Windows workspace | Run Clippy | 24 | 161 |
| Windows embeddings | Embedding Clippy | 27 | 98 |
| Linux workspace | Run Clippy | 25 | 84 |
| Linux workspace | Embedding Clippy | 25 | 64 |
| Historical Windows smoke | Post Cache Rust dependencies | 1 | 52 |
| Windows workspace | Cache Rust dependencies | 24 | 48.5 |
| Browser integration | Integration tests | 32 | 45 |
| Linux workspace | Remote API Clippy without server feature | 25 | 40 |

The historical smoke lane appears in an older workflow revision in this sample;
it is absent from the baseline workflow at commit `99f0e670`. Its single sample
does not describe current PR coverage.

Initial startup delay (creation to first active job) has a 3-second median across
37 attempts with active jobs, and a 2–1976 second range. The successful full
Windows-containing first attempts have a 2153-second execution-span median
(23 samples; 1616–2461 seconds). Startup can therefore dominate an otherwise fast
run. These are separate metrics: per-job waits also contain dependencies, and the
API cannot isolate pure runner queue time. Cancelled jobs were not treated as
fast successful runs.

## Rerun candidates

Each candidate failed and passed in different attempts of the same run:

- [45027110250](https://github.com/emreocell/Praxi-Bot/actions/runs/45027110250):
  Browser integration, attempt 1 → 2.
- [45026050368](https://github.com/emreocell/Praxi-Bot/actions/runs/45026050368):
  Windows workspace, attempt 1 → 2. The workflow still failed on another job;
  job recovery does not imply that the whole run passed.
- [45020761910](https://github.com/emreocell/Praxi-Bot/actions/runs/45020761910):
  Windows embeddings, attempt 1 → 2.

`ci-result` also recovered in the first and third runs; that is downstream evidence,
not another flaky test. Without test artifacts/log diagnosis, these observations
do not distinguish test flakiness from infrastructure failures. No test is ignored,
retried or quarantined by this change. Follow-up: obtain scheduled nextest JUnit
measurements in [#463](https://github.com/emreocell/Praxi-Bot/issues/463) before
changing any individual test.

## Two conservative changes

1. Save the build matrix's Rust dependency caches only on `main`. PR jobs still
   restore compatible caches and execute all commands. The Windows post-cache
   median is 176.5 seconds (range 0–284), while Linux's is 31 seconds. Avoiding
   PR-only uploads should shorten teardown and reduce cache churn. This is an
   inference, not a measured end-to-end saving. A PR introducing new dependency
   versions cannot warm its own cache for later attempts, a deliberate tradeoff.
   The action's [save-if input](https://github.com/Swatinem/rust-cache/blob/master/action.yml)
   controls writes independently of restores; existing compiler/manifest/lockfile
   cache hashing and the embeddings feature key are retained.
2. Move Linux's explicit-artifact feature graph, embeddings Clippy, dependency
   prefetch and offline tests into a parallel `Local Embeddings (ubuntu-latest)`
   matrix lane. The old serial Clippy and offline-test medians alone total 230
   seconds. The new lane repeats setup and may have a cold cache initially, so
   that sum is not a promised wall-time saving. Windows already splits these suites.

The existing PR concurrency cancellation and docs/UI filtering were already
enabled. They are retained. All prior check commands still run on their original
operating systems. Existing check names remain, and `ci-result` still depends on
the entire `build-and-test` matrix, including the new Linux embeddings lane.
No timeouts, failure propagation or permission settings are relaxed.

## Before / after comparison

Three recent successful full PR runs establish an explicit before sample:

| Run | Windows workspace | Linux workspace | Windows embeddings |
| --- | ---: | ---: | ---: |
| [45030761934](https://github.com/emreocell/Praxi-Bot/actions/runs/45030761934) | 1731 | 1237 | 993 |
| [45029618710](https://github.com/emreocell/Praxi-Bot/actions/runs/45029618710) | 2208 | 1232 | 1011 |
| [45028386190](https://github.com/emreocell/Praxi-Bot/actions/runs/45028386190) | 2270 | 1447 | 815 |
| Median | 2208 | 1237 | 993 |

The PR body records the PR's own after-run timings once available. Three successful
after samples and a controlled same-commit warm-cache comparison have not yet been
collected, so this report does not claim a proven speedup. Scheduled measurements
are separate contributor work; this PR supplies the historical reporter and the
two workflow changes requested for this task.

## Reproduce

Follow [the reporter guide](ci-report.md). The moving last-N window can be replaced
with repeated `--run-id` flags. The exact 50 sampled run IDs, newest first, were:

```text
45031505367 45031200337 45031193364 45031185655 45031061630
45030761934 45030742248 45029992063 45029618710 45028386190
45027718696 45027661922 45027633261 45027389837 45027166855
45027110250 45026640077 45026378828 45026050368 45025981840
45025417880 45025397918 45025134364 45025037054 45024955842
45024940388 45024533494 45024301980 45023913424 45023713672
45022817496 45022371776 45022074661 45021997660 45021945489
45021874211 45021555673 45021492607 45020761910 45020097923
45020014061 45019621099 45019396138 45019029160 45018594062
45018329622 45017929744 45017748616 45017471919 45017314729
```
