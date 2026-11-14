# Task 054: Linux host runtime

## Implementation and decisions

- The CLI and node share `src/host_runtime.rs` by a source-module include. Linux requires a valid X11 session and an authenticated, reachable display; failure emits one warning and disables daemon desktop tools. `BOT_DESKTOP=off|0|false` wins. The existing Windows UI Automation backend remains selected on Windows.
- `bot-node --display :99 run` re-executes once with DISPLAY and XDG_SESSION_TYPE=x11 before starting Tokio, avoiding unsafe environment mutation in a multithreaded process. A node accepts paired desktop and host cores. No peer-network changes.
- Browser-only work stays headless by default. Embedders can set `BrowserSettings.headless=false` for a desktop target; Linux forces headless if X11 is unreachable. Root is detected by effective UID, never by the spoofable USER environment variable. Only root receives automatic `--no-sandbox`, with a warning. Existing `BOT_BROWSER_PATH`, `--disable-dev-shm-usage`, `--disable-gpu`, and snap rejection are retained.
- Default managed drivers resolve the trusted ToolContext bot id to `<data>/browsers/<bot-id>`. Named profiles are below that bot's `profiles/`; explicit `profile: "isolated"` creates a disposable temporary profile. Profiles survive idle closure. Drivers are lazy; unlocked inactive sibling browsers are reclaimed before launching another bot's Chromium, while active runs, locked sessions, and takeover pins protect them. This does not add card 058's computer-target UI or profile configuration.
- Reused the existing agent-core semaphore, cancellation-aware lifetime permits, root/subagent sharing, and visible `Waiting for computer.` message. Added admission for observation tools and script delegates, and made the run cap independent of whether an embedder registered a desktop control gate. Permissions and control latches remain in their existing paths. Bot-specific activity prevents one bot from pinning every idle profile.
- Defaults: physical RAM below 8 GiB (or unknown) => one computer run, otherwise two. `[limits].max_computer_runs` in `<data>/host.toml`, then `BOT_MAX_COMPUTER_RUNS`, override it (positive values, existing cap 64). Browser idle timeout remains 120 seconds, overridden by `[limits].browser_idle_seconds` or `BOT_BROWSER_IDLE_SECONDS`.
- Added additive health `api_version="1.0"` and host OS/architecture/display/browser/physical RAM in MiB. `version` already existed. Health probes do not launch Chromium. Updated the Rust DTO, handler and OpenAPI only; mode/state/epoch are outside this card.
- Added `bot self-update [--check] [--version X] [--rollback] [--prefix PATH]`. Anonymous, bounded GitHub release requests select `praxi-linux-{x86_64|aarch64}.tar.gz` and its companion checksum. Archive streaming avoids a full tarball RAM buffer. SHA256 and provenance verification precede extraction. Extraction rejects traversal, links, special files, duplicate files, wrong archive roots, excessive entry counts and size. An exclusive installation lock serializes updates. Versions are retained and `current` switches by rename of a staged symlink; `previous` enables rollback. Existing flat installer binaries are preserved as a baseline and their launchers atomically become links through `current`. The updater never restarts itself or a service. Windows prints `use the installer` and succeeds.
- **Security deviation from the old card:** `verify_signature()` performs mandatory `gh attestation verify` with the current installer's fixed repository, workflow, source tag, issuer, predicate and runner constraints. A no-op signature hook would bypass the provenance requirement now on main and violate WORKER-RULES. It fails closed when verification or gh is unavailable. It uses an empty temporary gh config, clears the inherited environment (retaining only PATH and its private configuration), and disables prompts/update notifications. There is no unsigned fallback.

## Scope and reporting deviations

- The user's explicit push/PR instruction supersedes the card's older “Do not commit” wording.
- WORKER-RULES require work only inside this worktree. This report is therefore here, not written to the external `bot/tasks/054-report.md` location. External cards/ADRs were read and were not copied or committed.
- `pnpm --dir apps/desktop/ui gen:api` was executed to verify generation. Its output reveals pre-existing schema drift well beyond health. The generated UI file was restored because this card explicitly forbids edits to apps/desktop and requests health DTO + OpenAPI only. No desktop/UI, scripts, workflow, peer-network, or guide changes are included.

## Validation

All Cargo commands used `CARGO_TARGET_DIR=D:/praxi-target-054` and `CARGO_INCREMENTAL=0`.

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed, no output |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passed |
| Initial `cargo test --workspace` | Passed: 2,139 passed, 0 failed, 37 existing ignored |
| Later `cargo test --workspace --no-fail-fast` | 2,133 passed, 12 failed, 37 existing ignored; all 12 failures were unchanged connector-email local TLS fixtures timing out |
| `cargo test -p connector-email --lib -- --test-threads=1` | Passed: all 82 tests, 0 failed |
| Changed crates + every dependent crate, serial test gate | Passed: 1,018 passed, 0 failed, 22 existing ignored |
| Final `cargo test -p browser-cdp --lib` | Passed: 47 pure/hermetic tests; real browser integration tests remain pre-existing ignored tests in broader runs |
| `cargo build -p cli -p node-agent --features cli/fake-provider` | Passed (Windows test/diagnostic build; no provider credentials needed) |
| Windows native smoke via Python subprocess capture | `self-update --check`, `--rollback`, and `--version 1.2.3 --check` all exit 0 and print `use the installer`; node help includes `--display` |
| `cargo deny check` | Passed: advisories, bans, licenses, sources; existing duplicate warnings |
| `UPDATE_OPENAPI=1 cargo test -p daemon-api openapi_snapshot` | Passed; OpenAPI regenerated |
| Linux target `cargo check -p browser-cdp -p desktop-linux` | Passed |
| Linux target `cargo check -p cli -p node-agent` | Blocked by absent `x86_64-linux-gnu-gcc` in native dependencies |
| UI typecheck / tests / i18n / themes / root lint | Passed; 151 UI tests, 813 translation keys, four themes |
| `pnpm --dir apps/desktop/ui gen:api` | Executed successfully; output restored to preserve the explicit desktop boundary |

The full-workspace timeout failures are reported rather than hidden or marked ignored. Serial connector-email rerun passed without changing its code, tests, or time budgets. A redundant overlapping test rebuild also hit Windows LNK1104 while an older test executable was still running; it was rerun after that process completed. Scoped coverage includes cli, node-agent, browser-cdp, builtin-tools, agent-core, daemon-api, workflow-replay, evals, remote-api (including its server/dev dependencies), and state-bundle.

After the final waiting-message race fix, `cargo test -p agent-core --lib` passed again (181 passed, 0 failed, 1 existing ignored), followed by the final workspace Clippy gate. The scoped suite and focused final checks are reported separately rather than claiming an entirely green final broad workspace run.

Real output tails (fmt emitted no output, exit 0):

```text
cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 11.07s

cargo test <the ten changed/dependent packages above> -- --test-threads=1

running 1 test
test crates\workflow-replay\src\tool.rs - tool (line 11) ... ignored

test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

all doctests ran in 0.33s; merged doctests compilation took 0.28s

cargo test -p agent-core --lib

test result: ok. 181 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 2.68s

cargo test -p browser-cdp --lib

test result: ok. 47 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.27s

cargo test -p connector-email --lib -- --test-threads=1

test result: ok. 82 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 71.58s

cargo build -p cli -p node-agent --features cli/fake-provider
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 53s

cargo check -p browser-cdp -p desktop-linux --target x86_64-unknown-linux-gnu
    Checking browser-cdp v0.1.0 (C:\Users\emreo\OneDrive\Masaüstü\emre\bot\project-wt-054\crates\browser-cdp)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.35s

cargo deny check
advisories ok, bans ok, licenses ok, sources ok

pnpm --dir apps/desktop/ui typecheck
$ tsc --noEmit

pnpm --dir apps/desktop/ui test
ℹ tests 151
ℹ suites 0
ℹ pass 151
ℹ fail 0
ℹ cancelled 0
ℹ skipped 0
ℹ todo 0
ℹ duration_ms 951.3402

pnpm --dir apps/desktop/ui check:i18n
i18n ok (813 keys)

pnpm --dir apps/desktop/ui check:themes
obsidian: 48 tokens; minimum text contrast 5.53:1; states/code/focus AA passed
cobalt: 48 tokens; minimum text contrast 5.69:1; states/code/focus AA passed
raven: 48 tokens; minimum text contrast 5.99:1; states/code/focus AA passed
clay: 48 tokens; minimum text contrast 4.59:1; states/code/focus AA passed

pnpm lint
$ eslint . && stylelint "**/*.css"
```



## Not done / not verified

- No Linux runtime exists locally. Full `cargo check -p cli -p node-agent --target x86_64-unknown-linux-gnu` was attempted because the target is installed; it failed in native dependency compilation because `x86_64-linux-gnu-gcc` is absent. This is not a successful CLI Linux compile. Linux CI must compile and lint the complete CLI/node code, and run Unix update/symlink/rollback tests. The pure-Rust Linux browser/backend check is recorded separately.
- No real GitHub release was downloaded, no installed host was updated, and no service was restarted. Wiremock tests cover downloads/checksums without network credentials. Provenance integration and actual executable/symlink permissions require Linux validation.
- No VM RSS, PSS, CPU, or cgroup measurement was possible. **The <2.5 GB target is pending, not passed.** ARM acceptance requires a real/constrained aarch64 machine with 1 vCPU / 6 GB (also check the 2 vCPU / 12 GB deployment if used). Do not use the temporary 16-core/94-GB host as budget evidence. Use prebuilt releases; do not cargo/npm build on the small host.
- A mixed Wayland environment must explicitly select X11 (`XDG_SESSION_TYPE=x11`) for the existing desktop-linux backend; `bot-node --display` does this. Service environments must provide DISPLAY/XAUTHORITY/session D-Bus correctly. Xvfb/session provisioning remains owned by linux-pkg/card 174.

## Linux CI and server checklist

1. On a Linux CI/build runner, run workspace fmt/clippy/tests and specifically CLI update tests, including `symlink_switch_and_rollback_preserve_versions_and_reject_escape` and `flat_installer_launchers_follow_atomic_current_and_rollback`. Check both x86_64 and aarch64 builds. Check staged directory traversal permissions for the unprivileged service user, including when the updater runs with umask 077.
2. On the VM, install a prebuilt signed release. Use the existing service user and units; confirm Xvfb is up, DISPLAY is :99, the private Xauthority credential works, XDG_SESSION_TYPE is x11, and the user session bus/AT-SPI are reachable in the daemon's private socket namespace. Run `sudo praxi doctor` in the service context and inspect the resources row. It explicitly reports doctor-process RSS/HWM, not whole-daemon RAM.
3. Start `bot serve` through the existing service. Check `/v1/health` for api_version, Linux/aarch64 or x86_64, has_display=true, has_browser=true, and sensible RAM MiB. With DISPLAY removed/unreachable or BOT_DESKTOP=off, confirm one warning (none for explicit off), desktop tools absent, and headless browser work still available. Verify only Wayland disables desktop tools honestly.
4. Open xterm on Xvfb; take a desktop snapshot and screenshot and send non-sensitive keyboard input. Pair a node and run `bot-node run --display :99`; test RPC from both a desktop-role and host-role core. Stop/restart Xvfb and confirm failures are honest; start the daemon after display readiness.
5. Navigate Chromium to a non-sensitive fixture/example page. Verify browser-only headless mode, explicit headed mode on Xvfb, no automatic no-sandbox as the unprivileged user, and root warning/no-sandbox only in a disposable root test environment. Verify BOT_BROWSER_PATH and snap rejection. Check persistent bot A/B profile separation and disposable isolated profiles. Stop work and verify the managed browser and its children exit after 120 s (or the configured timeout), without deleting login profiles. Watch alone must not launch/pin it; active bot work and takeover must keep it alive.
6. Set `BOT_MAX_COMPUTER_RUNS=1`, start computer work in two chats/bots, confirm the second displays `Waiting for computer.`, and confirm an ordinary API/text run proceeds. Exercise observation tools and script delegates too. Cancel a waiting run and verify no launch or permit leak; finish the first and confirm the second proceeds.
7. Run `/opt/praxi/bin/bot self-update --check --prefix /opt/praxi` anonymously. As an administrator, test `self-update --version <signed-tag> --prefix /opt/praxi`; verify checksum/provenance failures leave current/launchers unchanged. Verify `current`, `previous`, and both flat `bin/` launchers; the running daemon must keep running and output must say `restart required`. Restart explicitly, check health, then exercise `self-update --rollback --prefix /opt/praxi` and explicitly restart again. Test an interrupted update/lock contention and unavailable gh. No unsigned bypass is supported.

## Repeatable resource check (pending measurements)

At each phase (OS baseline, Xvfb idle, daemon idle, a chat turn, and Xvfb + one-tab Chromium), sample at one-second intervals for a documented duration. Include every Praxi descendant and the separate Xvfb/session cgroup; do not count only the daemon's own RSS. Commands below avoid process arguments, page titles, URLs and image data:

```sh
sudo praxi doctor
free -b
systemctl show praxi.service praxi-xvfb.service -p MainPID -p ControlGroup -p MemoryCurrent -p MemoryPeak -p CPUUsageNSec
ps -e -o pid=,ppid=,comm=,rss=
# For each ControlGroup above, sample the corresponding cgroup-v2 files:
# /sys/fs/cgroup/<ControlGroup>/memory.current
# /sys/fs/cgroup/<ControlGroup>/memory.peak
# /sys/fs/cgroup/<ControlGroup>/memory.events
# /sys/fs/cgroup/<ControlGroup>/cpu.stat
# Read /proc/<pid>/smaps_rollup for content-free Pss/Rss when permitted.
```

Record architecture, kernel/browser versions, sample duration/interval, startup and idle-close times, MemTotal/MemAvailable, swap/OOM counters, peak summed process-tree RSS/PSS and cgroup peak. RSS from `ps` is KiB; cgroup values are bytes. Summed RSS double-counts shared pages, so retain PSS/cgroup measurements too. Add any desktop-session unit supplied by card 174. Compare complete Praxi peak with **2,500,000,000 bytes (~2.33 GiB)** while leaving OS headroom on the 6-GB host. Report every phase as measured or failed; currently all real-host phases are **not measured**.

## Noticed, not fixed

- The desktop TypeScript API schema was already stale beyond this task's additive health fields; a full generation produces unrelated changes. Follow up under the desktop/API owner's scope.
- Existing workspace examples named `inspect` collide in Cargo output paths (desktop-linux and desktop-windows). Cargo warns; no example names were changed here.
- Existing cargo-deny duplicate-dependency warnings remain; the final deny verdict is recorded below.
- Existing managed-browser/view registry integrations that supply an explicit BrowserDriver retain control of that driver's lifetime and profiles. Card 058's target selection and card 170's live-view binding must choose the matching bot driver rather than silently sharing profiles.

- The computer semaphore is process-local. This task does not add cross-process desktop arbitration or ADR-0025 subagent tab lanes; per-bot profile separation does not claim per-run page isolation.
