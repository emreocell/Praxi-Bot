# Operating a Praxi host

Day-to-day operation of the always-on Linux host (systemd, unprivileged user
`praxi`). Installing the host is [Running Praxi on a remote host](remote-host.md).
This page is what to do after it is installed.

On the server, `sudo praxi ...` is the `bot` CLI as the `praxi` user, with the
service environment. From the computer that owns the host, `bot host add`,
`repair`, `upgrade` and `remove` run in that terminal over SSH. They are not
daemon routes. SSH uses a key file or `ssh-agent`. Passwords are not supported.
`<TARGET>` is `user@server` or the saved name.

The behaviour below is what `apps/cli` and `crates/host-provision` do. Nothing
in this page was executed on a live server (**not verified**).

## Logs

The host daemon has no log file of its own. `praxi.service` does not set
`StandardOutput`, so stdout and stderr go to the journal. Start-up writes one
stderr line per step, `startup: <step> (+N ms)`. The API answers once the
journal shows `startup: listening`. The leadership check runs after that and
logs `startup: leader components started in background` when it finishes.
Scheduled work and channels start only then. See
[Host leadership](host-leadership.md).

```sh
journalctl -u praxi -n 200 --no-pager
journalctl -u praxi -u praxi-xvfb -u praxi-desktop -u praxi-node -f
sudo systemctl status praxi.service praxi-xvfb.service praxi-desktop.service
```

`bot host add`, `repair` and `upgrade` print a step table and
`Log (redacted):` plus a path. That file is
`<data directory>/hosts/<saved-name>/provision.log` on the computer where you
ran the command, not on the server. The log drops pairing tickets, private-key
blocks, and lines that carry a passphrase, password, token or secret.

`desktop-daemon.log` is written by the desktop app for the daemon it starts on
that computer. It is not the host's log.

Loopback health (default port 47821, or `BOT_PORT` in `/etc/praxi/praxi.env`):

```sh
curl -fsS --noproxy '*' --max-time 2 http://127.0.0.1:47821/v1/health
```

A fresh host answers `"status":"setup_required"` until a provider is
configured, then `"status":"ok"`. Do not print `praxi.env`. It holds the
secret-store passphrase.

## Start, stop, and restart

Units:

| Unit | Role | Enabled by the installer |
| --- | --- | --- |
| `praxi.service` | `bot serve` or `bot serve --host`, loopback only. `Type=notify`, `TimeoutStartSec=600`, `WatchdogSec=180`, `Restart=on-failure`, `RestartSec=5` | yes |
| `praxi-xvfb.service` | virtual display | with desktop |
| `praxi-desktop.service` | Xfce session on that display | with a desktop session |
| `praxi-node.service` | `bot-node run`. `RestartSec=10` | installed, not enabled |

Host mode is the last `ExecStart=` of the merged unit. `bot host repair` and
`bot host upgrade` install with `--host-mode`, so that line is
`bot serve --host`. Check with `systemctl cat praxi.service`.

On systemd 249 and newer, `praxi-xvfb.service` upholds `praxi.service` and
`praxi-desktop.service`. Stopping `praxi` while Xvfb is still active starts
`praxi` again. Stop the set by stopping Xvfb (it binds the others, so they
stop with it):

```sh
sudo systemctl stop praxi-xvfb.service
sudo systemctl start praxi-xvfb.service
sudo systemctl start praxi-desktop.service
sudo systemctl start praxi.service
systemctl is-active praxi.service praxi-xvfb.service praxi-desktop.service
```

Restart only the daemon with `sudo systemctl restart praxi.service`.
Without a virtual display, start and stop `praxi.service` directly.

The installer exports `NEEDRESTART_MODE=l` for its own package installs, so
Ubuntu's needrestart hook does not restart Praxi during those installs. For
your own `apt upgrade`, set `NEEDRESTART_MODE=l` as well if those hooks should
list units and leave them alone. On systemd older than 249 the installer omits
`Upholds=` and uses a start-up helper; details are in the
[remote-host troubleshooting](remote-host.md#troubleshooting-desktop-xauthority-d-bus-and-namespaces).

## When something inside the daemon breaks

Three things keep a host running without you:

- **A crashed background loop is restarted.** The scheduler, the mission wake loop and the
  mission drivers each run under a supervisor. A panic restarts the loop after a short, growing
  wait (1 s, 2 s, 4 s, up to 30 s) and you get an owner notice ("Schedules stopped and were
  restarted"). Five crashes in a row without a quiet five minutes between them stop the loop for
  good: it is then *degraded*, you get a second notice ("Schedules are not running"),
  `GET /v1/health` shows `background.status: "degraded"`, and `bot doctor` shows
  `background loops` as failed. Restart the daemon (`sudo systemctl restart praxi.service`) to
  bring it back.
- **A start is "up" when the daemon answers, and a hung daemon is restarted by systemd.** The
  unit is `Type=notify`: the daemon tells systemd it is ready (`READY=1`) only after its own
  `GET /v1/health` answered over the loopback, so `systemctl start praxi` (and the installer)
  return when the daemon really serves. Everything before that, a long database migration
  included, runs under `TimeoutStartSec=600` and not under the watchdog, which systemd starts
  only after `READY=1`. From then on the daemon sends a heartbeat every 45 seconds
  (a quarter of `WatchdogSec=180`), each one only after another `GET /v1/health` answered within
  10 seconds. A probe that fails is retried after 5 seconds, not after a whole interval, so a
  short hiccup costs nothing; a daemon that stays stuck for the 180 seconds is killed and
  restarted (`Restart=on-failure`). Outside systemd (no `NOTIFY_SOCKET`) none of this runs.
- **Notices survive a restart.** The notices behind the app's notification panel and the
  Telegram / Discord pushes are kept in `owner-notices.json` in the data directory (the newest
  200, readable by the daemon's user only), written in the background. The daemon also keeps,
  per bridge, the newest notice its stream handed over. A notice held back by quiet hours is
  still pushed when the window ends. A bridge that connects without a position of its own (it
  just started) gets what it was not handed yet and is less than 12 hours old; a bridge the
  daemon has never seen gets what was pushed since the daemon started.

`/v1/health` says only whether the background loops are `ok` or `degraded`, because it answers
without a token. Which loop crashed and how often is in the app's Settings > Doctor
(`GET /v1/doctor`, owner only). The crash itself is in the support log (`daemon.jsonl`, also in
the journal) as `loop_panicked`, `loop_ended` or `loop_gave_up` with the loop's name and the
count, never the panic message.

A degraded loop does not stop the heartbeat on purpose: restarting the daemon over and over
would not fix a bug that comes back at every start, and chat keeps working meanwhile.

The installer writes `Type=notify` only when the installed `bot` binary sends the readiness
message. An older release picked with `install-linux.sh --version` gets the plain unit
(`Type=simple`, no watchdog) and a warning, because under `Type=notify` it would be killed after
`TimeoutStartSec` at every start. The same goes for putting an older `bot` back by hand
(`<prefix>.backup-*`): run that release's own `install-linux.sh --update` so the unit matches the
program.

## Upgrade and repair

Both commands SSH to the server, upload the install scripts, and on success
print `Done`. A failed step stops the run. Fix the cause and run the same
command again. Steps that are already true report `skip`.

Version comparison uses major.minor.patch only. A pre-release suffix does not
count. If the server's triple is newer than this app's, every mode warns
`not downgrading` and leaves the server as it is.

| Server state | `bot host add` | `bot host upgrade` | `bot host repair` |
| --- | --- | --- | --- |
| Nothing installed | installs and pairs | fails: use `add` | installs and pairs |
| Plain `bot serve` (not `--host`) | fails: use `repair` | reinstalls in place | reinstalls in place |
| Host mode, same version, service active | skips install, still pairs and checks health | skips install, still pairs and checks health | reinstalls in place |
| Host mode, older version, or host mode but not active | installs this app's version | installs this app's version | installs this app's version |

"This app's version" is the release tag baked into the binary
(`PRAXI_RELEASE_TAG`), otherwise `v` plus the binary's version. See
[which tag the server installs](release-linux.md#which-tag-the-server-installs).

When an install actually runs and `/var/lib/praxi` already exists, the helper
first writes a root-only archive (see [Backup and restore](#backup-and-restore)).
It then runs `install-linux.sh --version <tag> --host-mode`. Unless you passed
`--no-desktop`, it also passes `--with-desktop-session`. `--no-desktop` omits
that flag. It does not pass `--without-desktop`, so a display that is already
there stays. The installer keeps `/var/lib/praxi`, `/etc/praxi/praxi.env` and
`/etc/praxi/praxi-node.env`.

Before swapping files the installer stops active units (Xvfb first, so an
uphold cannot start the daemon mid-install) and copies the previous
`/opt/praxi`, wrappers, env files and units aside. If the install then fails,
that copy is restored and previously active units are started again. The
script says `rolling back to previous working installation`. That rollback
does not restore `/var/lib/praxi`. Opening the database migrates it forward,
and that open happens before the health check, so a failed start can already
have migrated data. Put the data back from the snapshot when you need the
previous bytes. A live failed upgrade was **not verified**.

After install, the command pairs this computer if it is not already paired,
grants that device screen view and input, and checks health over the peer
link (six attempts, five seconds apart). Health fails when the running host
version's major.minor.patch differs from this app, and the message says to
run `bot host upgrade` again. A changed SSH host key stops the command before
any change (`host key mismatch`). A new key asks you to type `yes`, or pass
`--yes`.

`bot host remove <name>` forgets the server locally and revokes this computer
on it. `--uninstall` also uninstalls and keeps the data. `--purge` deletes
`/var/lib/praxi`, `/etc/praxi` and the `praxi` user. `--yes` skips the typed
confirmation for `--purge`.

On a Linux host installed with the managed updater, the desktop's **Update server**
button requests the exact published desktop release over the paired connection.
The host administrator pairing is required, and only the daemon's own process can pass
the request on to the root updater. The updater never installs a release older than the one
on the server, and it keeps to the server's channel (see below): a server set to `stable`
does not move to a pre-release. It checks both SHA-256 and GitHub build provenance (for the
release tag itself, never a branch), stops the daemon, copies its SQLite databases to
`/var/lib/praxi-updates/db-snapshot` (root only; the next update replaces it), switches the
program files, starts `praxi.service`, and waits up to six minutes for the new process to
report ready. If it does not, the updater stops it, puts the databases and the previous
program files back, and checks that the previous daemon starts. Data the new release wrote in
that time is lost with the rollback. The update needs free disk space for the copy and stops
before it changes anything when there is not enough.
A host installed before the managed updater needs one verified installer upgrade.
Use the SSH upgrade or the commands the app shows if it cannot start the update.

Scheduled updates are off by default. To change that, create `/etc/praxi/updates.toml`
as root and set the mode and channel.

```toml
mode = "notify"         # off, notify, auto
channel = "stable"      # stable, pre-release
```

The systemd timer checks daily. `notify` shows available releases on the selected
server in the desktop app. `auto` also installs them and can restart the daemon
while bots are working. `stable` waits for a stable release. `pre-release` also
considers preview releases. A `channel` you set also applies to the desktop's update
button. Without one, a server that runs a pre-release follows pre-releases and a server
that runs a final release follows stable releases. The file must be a regular file owned
by root, not a link, and under 16 KiB. Run `sudo systemctl start praxi-update-check.service`
to apply a mode change immediately. Use `sudo journalctl -u 'praxi-update*.service'`
to inspect failures. To turn off update requests from paired desktops, disable
and stop `praxi-update.socket` on the host.

Managed rollback restores the program files and the databases copied just before the
update. A killed update run (time limit, crash) is marked failed and the daemon is
started again, so the update button does not stay off. `praxi self-update --rollback`
and the installer rollback below only switch program files. Before upgrading a host
with important data, take a backup as described below so you can also recover from a
failed data migration.

The database copy is a rollback aid, not a backup. It holds only the SQLite databases in
`/var/lib/praxi` and `/var/lib/praxi/bots/<id>`, taken for this one update and replaced by the
next. Before the copy the updater stops the daemon and then every other process of the `praxi`
user (a leftover one is ended; if one will not stop, the update stops before changing
anything). It reads and writes the data directory only through directory handles and refuses
a link anywhere inside it. The restore first prepares every database beside its place, then
replaces them all, so a failure while preparing leaves the data as it was. If the updater is
killed during the last few renames, its recovery step (it runs after every updater run)
finishes the restore before the daemon starts; if that fails the daemon stays stopped and
the next update refuses until `sudo systemctl start praxi-update-check.service` has finished
it. A power loss in that moment is not covered. Keep your own backup of everything else, as
described below: the encrypted secret file, browser profiles, host config and the API token
are not in the copy.

## Roll back

Use the path that matches how far the upgrade got.

- **Install failed after the services were stopped.** The installer restores
  the previous binaries, units, wrappers and env files itself. Confirm with
  `systemctl is-active praxi.service` and `journalctl -u praxi -n 50`.
- **Upgrade succeeded and you want the previous release.** Install that tag
  again with the signed installer (`--version <old tag>`), the same way as an
  update in [Running Praxi on a remote host](remote-host.md#update-logs-rollback-and-uninstall).
  `bot host upgrade` will not do this when the server's major.minor.patch is
  newer than the app you run it from.
- **Data must return to the pre-upgrade copy.** Restore the snapshot from
  `/var/backups/praxi` (below). Do that with the binaries you want to run,
  because a newer daemon migrates SQLite when it opens the data directory.

`praxi self-update` and `praxi self-update --rollback` are a different
command. They switch a version directory inside the install prefix and do not
restart systemd. They are not `bot host upgrade`.

## Backup and restore

Two different copies exist. Do not treat them as the same file.

**Data-directory snapshot.** Before `repair` or `upgrade` changes an existing
`/var/lib/praxi`, the helper runs, as root:

```sh
install -d -m 700 /var/backups/praxi
tar -C /var/lib -czf /var/backups/praxi/praxi-data-YYYYMMDDTHHMMSSZ.tgz praxi
```

The timestamp is UTC. The archive is the whole data directory (database,
encrypted secret file, API token, browser profiles, host config). It does
not include `/etc/praxi/praxi.env`. Without that passphrase the restored
secret file cannot be opened. Keep the env file with the archive, and keep
both off the host's public disk. The same `tar` is what you run by hand for
a copy of your own. Stop the services first so SQLite is quiet. The helper
does not stop them before its own snapshot (**not verified** whether that
copy is crash-consistent).

Restore follows that layout. It was **not verified** by extracting an archive:

```sh
sudo systemctl stop praxi-xvfb.service || sudo systemctl stop praxi.service
sudo mv /var/lib/praxi /var/lib/praxi.aside
sudo tar -C /var/lib -xzf /var/backups/praxi/praxi-data-YYYYMMDDTHHMMSSZ.tgz
sudo systemctl start praxi-xvfb.service
sudo systemctl start praxi-desktop.service
sudo systemctl start praxi.service
```

Skip the Xvfb and desktop lines when those units are not installed. The aside
directory is your undo. Ownership is whatever the archive recorded (the
helper's `tar` runs as root).

**Portable state file.** `sudo praxi backup create <file> --passphrase-file <pass>` and
`sudo praxi backup restore <file> --passphrase-file <pass>` copy bots, chats, memory,
skills, workflows, schedules and provider settings into one encrypted file. API keys are
included only with `--include-secrets`; the API token, paired-device state and browser
profiles are never included. `restore` refuses to run while a daemon holds the data
directory or answers on the service port, so stop the units first. The `praxi` user must
be able to read the file. Details: [Encrypted backup and restore](backup.md). Enter keys
again after a restore without `--include-secrets` (`sudo praxi setup` or
`sudo praxi secrets set`).

## Pairing the desktop app

The desktop app talks to the host over an outbound peer channel. The host
does not open an extra listening port for it.

**From the server provider's console:** open **Add server** in the desktop app
and choose **I have a server**. Paste the setup command into the server console.
It verifies the installer, sets up the service and prints a QR and a `bot-pair:`
line. Paste the full line into the app within 30 minutes. If it expired, run
`sudo praxi host invite --role desktop` on the server to get a new one.

**From the owning computer** (the usual path): leave the desktop app or
`bot serve` running, then `bot host add user@server`. The command installs
with `--host-mode`, creates a one-time desktop invite on the server, pairs
this computer through the local daemon, and grants screen view and input.
The invite ticket is not written to `provision.log`.

**From a host you already installed:** on the server,
`sudo praxi host invite --role desktop`. That invite lasts two minutes, works
once, and (because the wrapper sends the host's owner key) includes host
administration. In the app, open the computer selector, **Add a host**, and
paste the ticket. The ticket stays in that field. The app does not store it.

An admin desktop may watch and drive the screen without a further command.
Any other paired device cannot, until you grant it on the host. The grant
survives restarts:

```sh
sudo praxi host devices
sudo praxi host screen --all-desktops
sudo praxi host screen <key> --view-only
sudo praxi host screen <key> --revoke
```

`--revoke` also removes the admin desktop's default. A paired desktop can
operate bots, chats, runs and the screen. Secrets, the execution policy,
MCP servers, skills, email and web-search credentials, and Telegram pairing
stay on the host (`sudo praxi setup` and the other local commands). The
longer install-time notes are in
[Using the host from the desktop app](remote-host.md#using-the-host-from-the-desktop-app).

Until the host has a model key, chats have nothing to answer with. The
screen and bot list still work.

The **Praxi desktop** peer represents the computer you paired from. Its offline
status affects work that targets that computer. Jobs targeting the server can
keep running there while the desktop is closed.

To measure whether live screen view is using a direct path or a relay (RTT and
frame rate), open a tunnel with `bot --remote <ENDPOINT> proxy` and follow
[Measuring the live-view link](../design/live-view-link-measurement.md).

## Firewall and ports

Leave the daemon on loopback. Do not publish it.

| Port | Direction | What to do |
| --- | --- | --- |
| 22/tcp (or your SSH port) | inbound | the only inbound rule the host needs for Praxi |
| 47821/tcp (or `BOT_PORT`) | none on the public interface | `bot serve` binds `127.0.0.1` only. `sudo ss -tlnp` must show `127.0.0.1` for that port, never `0.0.0.0` or `[::]` |
| provider API, Telegram, peer relays | outbound | leave default egress open. The peer link dials out. No inbound peer port is required |

Reach the API from another machine with an SSH tunnel
(`ssh -L 47821:127.0.0.1:47821 user@server`), or use the paired app. Oracle
security-list notes are in
[Firewall notes](remote-host.md#4-firewall-notes-oracle-cloud).

## Troubleshooting

Server coding CLIs are installed and checked as the `praxi` service account.
Use the server settings' sign-in action. If a CLI needs its own interactive
setup, open a terminal on the server and use the same account and executable paths.

```sh
sudo -Hu praxi env HOME=/var/lib/praxi NPM_CONFIG_PREFIX=/var/lib/praxi/.npm-global \
  PATH=/var/lib/praxi/.local/bin:/var/lib/praxi/.npm-global/bin:/opt/praxi-nodejs/bin:/usr/local/bin:/usr/bin:/bin \
  /bin/bash --noprofile --norc
```

| What you see | What to do |
| --- | --- |
| `npm: command not found`, `EACCES` on `npm install -g`, or `are you root?` in the server terminal | Node.js and npm come from the installer (`/opt/praxi-nodejs`) and install globally into `/var/lib/praxi/.npm-global`, no root needed. On an older install run the installer with `--update`; `sudo praxi doctor` shows the **node / npm** row. Commands that really need root (`apt`, `systemctl`) are for the server admin or `sudo` over SSH. See [Node.js and npm for the coding CLIs](remote-host.md#nodejs-and-npm-for-the-coding-clis-no-root-needed). |
| Host offline (app or `bot status` cannot see it) | On the server, `systemctl is-active praxi.service`. If it is inactive, read `journalctl -u praxi -n 200 --no-pager` and start it as in [Start, stop, and restart](#start-stop-and-restart). Then `curl` loopback `/v1/health`. From outside, check SSH, not port 47821. A paired desktop also needs its own daemon running. |
| Slow start | Follow the last `startup:` line in `journalctl -u praxi`. Time is milliseconds since `startup: begin`. The API is up at `startup: listening`. Components stay closed until `startup: leader components started in background`. A host with paired devices waits up to ten seconds for peer heartbeats before that line. See [Host leadership](host-leadership.md). |
| Peer cannot connect | Confirm the host unit is `bot serve --host` and `praxi.service` is active. The desktop daemon must be running. A pasted ticket works once and expires after two minutes. Create another with `sudo praxi host invite --role desktop`. Outbound traffic to the peer relays must be allowed. `bot host repair` reports `Paired channel` failed when the local daemon cannot reach the host, and says to try again in a minute. An SSH host-key mismatch stops `add` / `repair` / `upgrade` before they change the server. |

Display, Xauthority and D-Bus faults are in the
[remote-host troubleshooting](remote-host.md#troubleshooting-desktop-xauthority-d-bus-and-namespaces).
General daemon errors are in [Troubleshooting](troubleshooting.md).
