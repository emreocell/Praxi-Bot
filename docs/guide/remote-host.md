# Running Praxi on a remote host (VPS / VM)

This guide sets up Praxi as an always-on host on a cheap Linux server, for
example an Oracle Cloud Always Free VM (Ampere A1, `aarch64`) or any
`x86_64` VPS. The daemon runs under systemd as an unprivileged user and is
reachable only from the machine itself.

> **Status.** The daemon (`bot serve`) and the Telegram channel work on Linux.
> The node-agent (`bot-node`) and the iroh peer network are **experimental**:
> the core side is not connected to the daemon yet, and the Linux desktop
> backend (`desktop-linux`) has no implementation yet, so `bot-node run`
> currently refuses to start on Linux. Xvfb and Chromium are installed for the
> planned desktop and browser automation, not because every tool already uses
> them.

## What you need

- A VM with 6 GB RAM (1 core) or 12 GB (2 cores) for an always-on host with
  browser automation, running Ubuntu 22.04+, Debian 12+, or an Oracle Linux / Fedora
  family system with systemd, and root access.
- Architecture `x86_64` or `aarch64` (the installer detects it).
- An API key for a model provider (see [Providers and models](providers-and-models.md)).

## 1. Get the release package

Binaries are built by the **Linux Server Packages** GitHub Actions workflow
(manual run or a `v*` tag). Each run produces
`praxi-linux-x86_64.tar.gz` and `praxi-linux-aarch64.tar.gz`, each with a
`.sha256` file. Tagged runs attach those archives and `install-linux.sh` from
the same tag to the GitHub release. There are no published releases yet, so
run the workflow yourself and download the artifact for your architecture.

## 2. Install

Copy the tarball to the server, or, once a release exists, fetch the installer
from **that same release tag**. The release job uploads `install-linux.sh`
next to the tarballs. Use that asset; the copy on the default branch can
drift from the binaries you are installing.

```sh
# script and binaries from the same tag
curl -fsSL https://github.com/emreocell/Praxi-Bot/releases/download/v0.1.0/install-linux.sh | sudo bash -s -- --version v0.1.0

# from a local tarball (a sibling .sha256, or --sha256, is required)
sudo ./scripts/install-linux.sh --file ./praxi-linux-aarch64.tar.gz

# local tarball with neither checksum: refused unless you opt out
sudo ./scripts/install-linux.sh --file ./praxi-linux-aarch64.tar.gz --insecure-skip-verify

# with a virtual display and Chromium for browser/desktop automation
sudo ./scripts/install-linux.sh --version v0.1.0 --with-desktop
```

Other options: `--port`, `--display`, `--screen`, `--sha256`,
`--insecure-skip-verify`, `--skip-packages`, `--no-start`, `--without-desktop`.
Run it with `--help` for the full list. Re-running upgrades the binaries and
keeps `/var/lib/praxi`, `/etc/praxi/praxi.env`, and `/etc/praxi/praxi-node.env`.
A custom `BOT_PORT` stays unless you pass `--port`. An existing
`praxi-xvfb.service` and its `DISPLAY` value stay unless you pass
`--without-desktop`. `--with-desktop` installs or refreshes Xvfb and Chromium.

The script:

- installs the runtime libraries the binaries link (`libdbus`, `libssl.so.3`);
- creates a `praxi` system user with no login shell;
- installs binaries to `/opt/praxi` and keeps data in `/var/lib/praxi`;
- lists tarball members with `tar -tzf` and refuses an absolute path or a
  `..` component, then extracts only into a temporary directory;
- writes `/etc/praxi/praxi.env` (mode 0640, root:`praxi`) for `praxi.service`,
  with the data dir, port, and a random passphrase for the encrypted-file
  secret store (headless servers have no OS keychain);
- writes `/etc/praxi/praxi-node.env` (mode 0640, root:`praxi`) for
  `praxi-node.service`, with the data dir, home, and `DISPLAY` when a virtual
  display is in use. That file does not contain `BOT_SECRETS_PASSPHRASE`;
- installs `praxi.service` (`bot serve`, 127.0.0.1 only) and enables it, plus
  `praxi-node.service` (not enabled, see below);
- with `--with-desktop`, installs `praxi-xvfb.service` (virtual display `:99`)
  and a non-snap Chromium browser (source selection below). Prepares the root-owned,
  mode 1777 X11 socket directory inside the shared private `/tmp` before Xvfb starts. A later run keeps that unit until `--without-desktop`;
- applies systemd hardening: `NoNewPrivileges`, `ProtectSystem=strict` with
  `ReadWritePaths=/var/lib/praxi`, `ProtectHome`, `PrivateTmp`,
  `PrivateDevices`, kernel and cgroup protections;
- adds wrapper commands `praxi` (the `bot` CLI, using `praxi.env`) and
  `praxi-node` (using `praxi-node.env`) that run as the `praxi` user.

The installer waits for two consecutive active-unit and valid loopback health
checks, with at most 30 attempts (each HTTP request has a two-second timeout).
Failure prints the last 50 service journal entries and exits nonzero, after
attempting to restore a previously active node. `--no-start` installs/enables
files only and explicitly skips startup and readiness.

A fresh daemon stays running with HTTP 200 health `status: "setup_required"`.
Doctor reports **setup required** for providers. Setup APIs are available;
sending a chat turn returns HTTP 409 `setup_required` with configuration steps.
There is no placeholder provider. Configure a provider and restart the daemon
(`sudo systemctl restart praxi`), or use `POST /v1/providers/reload`, to enable
inference and change health to `status: "ok"`.

## 3. Configure and pair

```sh
sudo praxi setup           # provider, API key, first bot, permission preset
sudo praxi doctor          # check the installation
sudo praxi telegram pair   # one-time code to pair your Telegram account
journalctl -u praxi -f     # logs
```

Chat from your phone through [Telegram](telegram.md), or from your computer
through an SSH tunnel:

```sh
ssh -L 47821:127.0.0.1:47821 user@your-server
```

The API token lives in `/var/lib/praxi` (readable only by `praxi`); clients on
the tunnel authenticate with it like on a local install.

### Letting another core control this machine (experimental)

This is **not functional on Linux yet**. `bot-node run` exits because there is
no Linux desktop backend, and enabling `praxi-node.service` would restart it
every 10 seconds. Leave that unit disabled.

When the backend exists, `bot-node` connects **outward** over iroh (QUIC with
relay fallback), so no inbound port is needed. On the machine that should be
controlled:

```sh
sudo praxi-node join '<bot-pair:...>'     # one-time ticket from the core
```

The ticket is single-use; afterwards the devices use stored keys. See ADR-0006
in the design documents for the model. Enable `praxi-node.service` only after
`bot-node run` works on Linux.

## 4. Firewall notes (Oracle Cloud)

You do **not** need to open any port for Praxi. Keep it that way.

- Leave the VCN security list / network security group with only SSH (22)
  inbound. Do not add an ingress rule for 47821.
- Oracle Ubuntu images also ship iptables rules; do not add an `ACCEPT` for
  the daemon port either. Check with `sudo ss -tlnp | grep 47821`: it must show
  `127.0.0.1`, never `0.0.0.0` or `[::]`.
- Outbound traffic must be allowed (the default): the provider API, Telegram
  and the iroh relays all use outbound connections.
- Always Free ARM capacity and limits change; verify the current terms with
  Oracle. Idle instances can be reclaimed, so keep backups of `/var/lib/praxi`.

## 5. Security

- **Never expose the daemon port publicly**, not through a security list, a
  reverse proxy, or `ssh -R`. The daemon API can run tools on the server; its
  token is the only protection. `bot serve` binds 127.0.0.1 by design.
- Reach it through SSH tunnels, Telegram pairing, or iroh pairing. The
  `praxi-node` link is outbound-only and uses per-device keys after one-time
  pairing.
- Protect `/etc/praxi/praxi.env`: it holds the secret-store passphrase.
  `praxi-node.service` reads `/etc/praxi/praxi-node.env` instead, and that
  file has no `BOT_SECRETS_PASSPHRASE`. Both files are mode 0640. Do not
  commit or paste them.
- Keep the permission preset on **Approve destructive** or stricter (see
  [Chatting and approvals](chatting-and-approvals.md#the-four-presets)). A server with Chromium and a virtual display is a
  powerful machine; use a dedicated VM without personal data.
- Use SSH keys only and keep the OS patched (`unattended-upgrades` on Ubuntu).
### Browser source and service diagnostics

`--with-desktop` uses Debian's native `chromium` candidate on Debian. On
Ubuntu (including 24.04 ARM64), it downloads **Google Chrome for Testing
157.0.8103.38**, directly over HTTPS from Google's official
[Chrome for Testing archive](https://github.com/GoogleChromeLabs/chrome-for-testing),
into `/opt/praxi-browser`. The CPU choice is explicit: `linux64` for x86_64,
`linux-arm64` for aarch64. The
[official download metadata](https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json)
now publishes Linux ARM64 builds (since version 153); older advice that Google's
Linux browser builds are x86_64-only is no longer true for this distribution.
The pinned archive is delivered via HTTPS and verified against pinned SHA-256
checksums for each architecture before extraction. Re-run an updated installer
to receive a newer browser pin; Chrome for Testing does not auto-update.

To update the pinned Chrome for Testing version and checksums in
`scripts/install-linux.sh`, download the new version's archives for both
platforms (`linux64` and `linux-arm64`) from
`https://storage.googleapis.com/chrome-for-testing-public/<version>/<platform>/chrome-<platform>.zip`,
compute their hashes using `sha256sum`, and update `version`,
`sha256_linux64`, and `sha256_linux_arm64` together at the top of
`install_cft`.

Fedora/RHEL uses `chromium` from the administrator's enabled RPM repos, without
weak dependencies; absence is an installation error. No PPA or snap is added.
An xtradeb PPA would add third-party trust and isn't needed with the official
ARM64 archive. Playwright also supports ARM64, but adds a separate runtime and
browser lifecycle that this installer doesn't need.

Ubuntu's `chromium-browser` and virtual `chromium` may resolve to snap. They are
never selected by the installer. Snap-only discovery and snap overrides are
unsupported and produce a clear diagnostic. With `--skip-packages`, provide a
working non-snap browser yourself. `BOT_BROWSER_PATH` overrides discovery;
`PRAXI_BROWSER_PATH` remains a compatible fallback. An invalid override fails
instead of silently choosing a different browser.

The browser needs **libcups**, the client library; it does not need a print
server. Packages use `--no-install-recommends`. The installer disables/stops
`cups.service`, `cups.socket`, `cups.path`, `cups-browsed.service`, and an
installed CUPS snap if present, including after package failure. This installer
is for a dedicated bot host: it also disables pre-existing printing. Do not use
it on a print server. No browser or desktop TCP listener is enabled.

`sudo praxi doctor` launches a short-lived transient unit with the same user,
env file, working directory, hardening and Xvfb private namespace as the daemon.
Its browser row actually launches headless, establishes CDP, and closes the
browser, with a five-second startup/eight-second overall timeout. A failure is
reported as **fail**, with a non-snap/path hint. A direct `bot doctor` probes
only its current user/environment; use the wrapper for service validation.
Ubuntu 24.04's AppArmor user-namespace restriction is accommodated by a named
`praxi-browser` profile permitting `userns` for `/opt/praxi-browser/chrome`,
following Ubuntu's Chrome profile. The Chromium sandbox and systemd hardening
remain enabled; no `--no-sandbox` or broad unit relaxation is applied.

### Small-host resource check

Production hosts run prebuilt release tarballs; don't compile Rust on the
1-core/6-GB (or 2-core/12-GB) machine. Keep Chromium lazy: this installer never
starts a persistent browser. The doctor probe closes its browser after checking.
Configurable idle shutdown/concurrency limits belong to task 054 and are not
implemented by this packaging fix.

For a repeatable check, record `MemoryCurrent` and `MemoryPeak` from
`sudo systemctl show praxi praxi-xvfb -p MemoryCurrent -p MemoryPeak` while idle,
during a representative chat turn, and with one browser tab open. When running
`sudo praxi doctor`, sample the corresponding `praxi-doctor-*.service` too
(`systemctl list-units 'praxi-doctor-*'`). These cgroup counters include browser
children; `/proc/<pid>/status` `VmRSS`/`VmHWM` can separately record process RSS.
Target a combined peak below 2.5 GB, leaving room on a 6-GB host. A doctor probe
is a short launch smoke test, not a representative chat workload measurement.

## Update, logs, uninstall

```sh
sudo ./scripts/install-linux.sh --file ./praxi-linux-aarch64.tar.gz   # update
# an existing praxi-xvfb unit and DISPLAY are kept
sudo ./scripts/install-linux.sh --file ./praxi-linux-aarch64.tar.gz --without-desktop
journalctl -u praxi -n 100
sudo ./scripts/install-linux.sh --uninstall           # keeps data and user
sudo ./scripts/install-linux.sh --uninstall --purge   # deletes data too
```

If you only need a headless desktop session for development, the older
user-level script `scripts/linux/setup-headless.sh` is still available.
