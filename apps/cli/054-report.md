# Task 054: Linux host runtime — PR #44 follow-up

The current gate/CI results and output tails are recorded in [PR #44](https://github.com/emreocell/Praxi-Bot/pull/44) and the explicitly requested external task report `bot/tasks/054-report.md`. These notes replace the earlier report that predated native Linux validation.

## Review fixes

| Finding | Change and regression coverage |
| --- | --- |
| 1: interrupted installation/adoption | An existing version directory no longer blocks repair. Updates always download and verify again, including same-tag repairs. A synced binary-hash seal detects missing and nonempty truncated binaries. Interrupted flat-layout adoption rebuilds the preserved baseline under the update lock. Unix regression covers adoption, damage, repair, and interrupted switching. |
| 2: planted PATH verifier | The updater resolves only `/usr/bin/gh`, checks root ownership and absence of group/other write permission for its canonical path and every ancestor, and uses a fixed PATH with an empty private gh config. Missing/untrusted/failed verification fails closed. A subprocess regression plants a success-returning gh on PATH and checks that unverified bytes are rejected and the planted program is never selected. |
| 3: blocking display checks | One Xauthority-aware probe runs on a dedicated OS thread, with a 200 ms caller timeout and a process-wide cached result. Health, browser launch, CLI startup, and node startup reuse it. Timeout/cache regression uses a stalled probe. Restart the process to reprobe after display configuration changes. |
| 4: workflow success checks | All three SuccessCheck variants authorize their corresponding computer read through the same ToolGate/LoopGate path as workflow steps, acquiring the run semaphore before backend access. Regression denies each read and checks that replay cannot bypass admission. |
| 5: sudo maintenance entry point | Only root's exact `praxi self-update ...` entry retains root. It execs the installed binary using `/usr/bin/env -i`, without sourcing the service environment. Other commands, node commands, and non-root callers keep the existing service-user route. A nonwritable prefix reports the direct sudo binary command. `scripts/tests/self-update-wrapper.py` executes the actual generated wrappers with harmless routing stubs and a temporary prefix. No ownership or sudoers permissions are broadened. |
| 6: waiting-message race | The semaphore regression awaits the actual visible waiting event with a bounded timeout instead of assuming that observing an atomic flag means the event has been emitted. |
| 7: Windows Session 0 | Health checks the current window station's visibility using Win32. A flags regression covers noninteractive and interactive stations; Windows backend selection is preserved. |
| 8: checksum filename | Require exactly a digest and the matching asset name; reject bare digests, mismatched names, and trailing fields. Named-checksum regression covers both GNU text/binary forms. |

The original Linux CI failure was `clippy::map_unwrap_or` in the Linux-only installed-version reader. It is fixed. Native Linux checking also caught and fixed two new Unix-only lints (semicolon and import placement). An initial regression fixture used an invalid workflow slug; the fixture was corrected and the workflow suite rerun.

## Native Ubuntu 24.04 aarch64 validation

The branch was transferred as a git bundle to `/home/ubuntu/praxi-build-054`. Release builds used `cargo build --release -p cli -p node-agent -j2` under nohup and were polled. Native Cargo output uses `/home/ubuntu/praxi-build/target` to reuse the existing build cache, with `CARGO_INCREMENTAL=0`. Local Windows Cargo output always uses `D:/praxi-target-054`.

A temporary systemd unit shared only Xvfb's private `/tmp` namespace, ran a fresh daemon on `127.0.0.1:48954` with a temporary data directory, and used a keyless loopback OpenAI-compatible fixture at `127.0.0.1:48955`. No installed daemon/service configuration, credentials, provider API keys, firewall rules, or public listeners were changed. The validation daemon ran as root to exercise the required root browser flag policy; the installed service remained under its original ownership model.

Observed output:

```text
health={"status":"ok","version":"0.1.0","api_version":"1.0","host":{"os":"linux","arch":"aarch64","has_display":true,"has_browser":true,"total_ram_mb":11925}}
health_20_requests_seconds=0.022
GTK desktop_launch, desktop_list_apps, desktop_get_state: tool_call_completed, error=null
screenshot=.jpg; bytes=4773
validation_profile_dirs=['01a1d9de-64c9-754d-9584-e9ade1c10027']
browser_open and browser_snapshot: tool_call_completed, error=null; fixture text found
Observed browser flags: --disable-dev-shm-usage --disable-gpu --headless=new --no-sandbox
computer_cap=1; second run emitted Waiting for computer while first held slot
cap_finished=succeeded
cap_finished=succeeded
browser_processes_after_idle=0
self_update_check_exit=1
error: HTTP status client error (404 Not Found) for url (https://api.github.com/repos/emreocell/Praxi-Bot/releases/latest)
installed_services=active
active
```

The desktop fixture uses GTK with a private D-Bus accessibility session. The earlier xmessage attempt could launch/list windows but could not supply an AT-SPI tree; its failed snapshot was not counted as successful validation. Chromium's live command line was read only for descendants of the validation daemon. Chrome rewrites its process title, so the selected flags were extracted from that observed title.

Content-free RSS sampling every ~100 ms, in KiB (`daemon`, `daemon and descendants`, `daemon tree plus existing Xvfb`):

```text
idle:            [31744,   31744,   52908]
desktop-turn:    [39196,  135952,  157900]
browser-one-tab: [39884, 1295672, 1317620]
computer-cap:    [39960, 1295868, 1317816]
browser-idle:    [39988, 1372508, 1394456]
```

The observed combined peak was 1.33 GiB, below the 2.5 GiB budget. These are sampled RSS sums (shared mappings can be counted more than once), not a cgroup peak or a measurement of a real paid model turn. The GTK fixture remains in the daemon's descendant count during the browser check. Idle cleanup was checked after 26 seconds with a 15-second configured browser idle timeout.

The generated maintenance wrapper was also exercised with the real release binary in a temporary root-owned prefix: root reached the GitHub release lookup (404 for the intentionally nonexistent v0.0.0), while the actual `praxi` service user was denied writes to `/opt/praxi` and received `run sudo /opt/praxi/bin/bot self-update (with the same options)`. The installed wrapper and binaries were not replaced. Installer `bash -n` and generated-wrapper tests passed.

Native Unix regressions passed, including interrupted adoption/repair, symlink rollback, planted-PATH verifier rejection, checksum filenames, display timeout/cache, and workflow admission. Native Clippy and the final release build passed. The first native seven-package test batch had one unrelated `daemon-api --test cli_only_turn` fixture failure (`signed_out` expected, `cli_io_error` returned); that entire test file then passed three serial reruns unchanged. Final workspace and CI evidence is in the PR body and external report.

## Limits and decisions

- `self-update --check` checks release metadata and installed binary integrity; it does not download or verify release bytes. This repository currently returns 404 for the latest published release. No live signed install, repair, or rollback against a published release could be demonstrated. Unix tests cover the local installation transaction; missing trusted gh fails closed.
- The installer's root-only wrapper exception is an explicit scope extension requested by the follow-up review. Root prefix ownership and the service user's privileges remain unchanged. Existing installations need the updated wrapper (or the direct sudo binary maintenance command).
- The live browser exercise covered headless/root mode on Xvfb. A sandboxed unprivileged browser, headed mode, real node pairing, and the Windows Session 0 station API on an actual service remain unverified live.
- Limits remain process-local; cross-process desktop arbitration and per-subagent tab lanes remain outside this card.
- No new ignored tests, softened approval checks, skipped provenance, or widened ownership were introduced.
