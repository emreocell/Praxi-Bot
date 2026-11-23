# Desktop sidecar lifecycle

The Windows desktop shell owns a `bot serve` process. Closing the window quits
by default. If the owner enabled **Keep running in the tray** and the tray icon
exists, closing only hides the window: the shell, daemon and their jobs stay alive.
Tray Quit, uninstall and Windows sign-out stop the daemon regardless of this choice.

## One daemon per data folder

`bot serve` takes an exclusive OS lock on `.daemon.lock` before opening the
database, loading secrets or starting tools. Backup restore uses the same file.
Failure to create or take the lock stops startup. The file is never deleted:
deleting a locked file could allow two different lock objects for one folder.
Windows releases the lock on exit or crash; a leftover file does not block restart.

While holding the lock, a daemon reserves a loopback socket and writes its PID and
actual port to `.daemon-endpoint.json`. The record contains no token or owner key.
Discovery ignores it when the lock is free. A new lock owner removes the old record
before publishing its own. A competing desktop launch waits for the winner and
reuses its healthy listener. The Windows single-instance plugin also focuses the
existing desktop window instead of starting a second shell.

Discovery is a hint, not authentication. Before handing the token to a recorded
listener, the desktop checks both the exact PID and its OS user. For a daemon it
started, the existing exact-child check applies. An explicit loopback `BOT_URL`
keeps the existing listener identity checks. Owner routes, approvals, redaction,
egress checks and untrusted-content handling are unchanged. Reusing a daemon does
not grant a new desktop its old in-memory owner key; a CLI-owned daemon retains its
existing owner-key policy. A reused daemon is not stopped on desktop quit.

## Ports and readiness

The daemon reserves the requested port (normally 47821) before assembly. If it is
busy or reserved by Windows, it asks the OS for a free loopback port, keeping the
socket open throughout startup. It announces the selected port on stderr and in
the discovery record; stdout retains the existing text/JSON listening announcement.
If neither bind succeeds, startup returns a plain connection error.

The desktop and default CLI clients discover the same port from the data folder.
Explicit CLI URLs and ports that differ from the recorded requested port continue
to override discovery; `BOT_PORT` follows the fallback for its requested port. Desktop
diagnostics and prerequisite checks show the discovered port. The webview CSP
already allows loopback ports, so no origin or network boundary is widened.

The desktop polls the health readiness signal, not an assumed startup delay.
`starting` keeps the connection pending; `ok`, `setup_required`, or the API's
bounded-startup `degraded` state permits the UI to connect. The latter lets the
owner see and repair startup failures. A 20-second readiness deadline stops an
unresponsive owned sidecar. Poll intervals schedule another observation; they are
not used as evidence that startup finished. Restarts wait for actual process exit.

## Shutdown and process trees

Before releasing the sidecar's private stdin handshake, the desktop assigns it
to a Windows job with `KILL_ON_JOB_CLOSE`, without either breakaway flag. The owner
key is sent over that pipe only, never argv, logs, an environment variable or a
file. The pipe stays open: a stop line or EOF requests normal daemon shutdown.
The daemon also contains itself and future children in its own kill-on-close job
before loading secrets/tools. The OS owns that job handle until process exit,
including a standalone CLI daemon's crash. Job handles are not inheritable.
Assignment failure stops startup and reaps the uncontained child.

Quit requests graceful shutdown with a three-second tool grace period, waits up
to five seconds for daemon exit, then closes the desktop job and terminates/reaps
the child if necessary. Closing a job also removes descendants left after normal
daemon exit. A shell crash closes its job in the kernel. Windows `WM_ENDSESSION`
uses the same bounded shutdown path; an OS-enforced early exit still closes jobs.
The CLI retains its existing console/logoff/shutdown signal handlers. Logs record
restart requests, provider fallback, recovery, shutdown and forced-stop reasons.

## Installer and data retention

NSIS invokes the desktop's `--stop-for-uninstall` helper before removing
files. Updates extract this version's helper to NSIS's private plugin folder and
pass the installation path; they never ask an older desktop to interpret a new
flag. The extraction uses `MAINBINARYSRCPATH` from the upstream
[Tauri installer template](https://github.com/tauri-apps/tauri/blob/dev/crates/tauri-bundler/src/bundle/windows/nsis/installer.nsi).
The helper matches full executable paths in that installation and the current OS
user, holds process handles to prevent PID reuse, and signals protected native
stop events. All matched processes are signalled before waiting. After eight
seconds, a remaining matched process is terminated; jobs remove its descendants.
The helper never kills by image name or touches unrelated installations. Failure
aborts removal with English, Turkish or German guidance. If another installation
still owns the default data folder, removal is refused instead of deleting live
data or terminating that unrelated installation. A matching CLI process without
the daemon stop event (for example, a running backup) also blocks removal rather
than being forcibly interrupted.

User data is retained by default. The existing post-uninstall data deletion runs
only when Tauri's **Delete application data** checkbox is checked and this is not
an update. Installer stopping never deletes data or lock files. The helper is
part of this version. An older running desktop without a stop event must be
closed manually; upgrading from it needs an explicit installer smoke check before release.

## Verification

Hermetic tests cover lock exclusion and recovery, unwritable lock failure, busy
port selection and error propagation, startup readiness, recorded-listener
identity, job flags and the existing opt-in tray-close policy. Integration
checks include a daemon smoke test for actual fallback-port discovery,
duplicate-start refusal and graceful private-pipe shutdown. Windows job/tree
integration tests are ignored by default and can be run explicitly on Windows:

```powershell
cargo test -p process-guard supervision::tests::closing_job -- --ignored
```

A release still needs interactive tests of app crash/relaunch, two simultaneous
launches, a busy 47821, tray opt-in, active tool children, Windows sign-out,
silent uninstall, and uninstall with the data checkbox both off and on. Native
integration and installer behaviour must not be inferred from the pure tests.
