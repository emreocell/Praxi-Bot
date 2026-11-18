# Running Praxi on a remote host (VPS / VM)

This guide sets up Praxi as an always-on host on a cheap Linux server, for
example an Oracle Cloud Always Free VM (Ampere A1, `aarch64`) or any
`x86_64` VPS. The daemon runs under systemd as an unprivileged user and is
reachable only from the machine itself.

> **Status.** The daemon (`bot serve`) and the Telegram channel work on Linux.
> The node-agent (`bot-node`) and the iroh peer network are **experimental**:
> the Linux desktop backend (`desktop-linux`, X11 with AT-SPI2) exists, so
> `bot-node run` can serve a display on Linux, but the Linux node path has not
> been verified end to end. Xvfb and Chromium are installed for desktop and
> browser automation, not because every tool already uses them.

> **Releasing (maintainers).** Pushing a `v*` tag does **not** publish
> anything. Publishing needs a manual dispatch of **Linux Server Packages** on
> the protected tag with `dry_run=false`, plus approval of the `release`
> environment. The repository administrator must create both of these before
> the first release (details in
> [Release administrator setup](#release-administrator-setup-and-dry-run)):
>
> - an active tag ruleset for `refs/tags/v*` (restrict creation to release
>   maintainers, block updates and deletion, minimal bypass);
> - an environment named `release` with required reviewers, Prevent
>   self-review on, deployment tags limited to `v*`, administrator bypass off.

## What you need

- A VM with 6 GB RAM (1 core) or 12 GB (2 cores) for an always-on host with
  browser automation, running Ubuntu 22.04+, Debian 12+, or an Oracle Linux / Fedora
  family system with systemd, and root access.
- Architecture `x86_64` or `aarch64` (the installer detects it).
- An API key for a model provider (see [Providers and models](providers-and-models.md)).

## 1. Get and verify the release package

Use an independently installed **GitHub CLI 2.80.0 or newer**. The installer
checks this supported minimum before verification or installation. Its required
`--source-ref`, `--source-digest`, `--custom-trusted-root`, and signer-policy
flags are available at that floor. Obtain `gh` from [cli.github.com](https://cli.github.com),
not from a Praxi release asset.

The **Linux Server Packages** workflow builds `praxi-linux-x86_64.tar.gz` and
`praxi-linux-aarch64.tar.gz` with companion `.sha256` files. Only a protected
`v*` tag can produce release attestations. A release contains the archives,
`install-linux.sh` from the same tag, and **`praxi-linux.sigstore.json`**.
Unsigned branch/manual build artifacts cannot be installed by this installer.
A checksum checks integrity; it never replaces signed provenance.

Verify the installer before executing it, including before invoking `--help`
on an installer obtained from an untrusted location. Copy the following from a
reviewed checkout and substitute the release tag you intend to install:

```bash
(
  set -euo pipefail
  version=v0.1.0
  work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
  base="https://github.com/emreocell/Praxi-Bot/releases/download/$version"
  for asset in install-linux.sh praxi-linux.sigstore.json; do
    curl -fSL --proto '=https' --proto-redir '=https' --tlsv1.2 "$base/$asset" -o "$work/$asset"
  done
  gh attestation verify "$work/install-linux.sh" --bundle "$work/praxi-linux.sigstore.json" \
    --repo emreocell/Praxi-Bot --hostname github.com \
    --signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml \
    --source-ref "refs/tags/$version" --deny-self-hosted-runners \
    --cert-oidc-issuer https://token.actions.githubusercontent.com \
    --predicate-type https://slsa.dev/provenance/v1 --digest-alg sha256
  sudo bash "$work/install-linux.sh" --version "$version"
)
```

The verified installer applies the same fixed repository, workflow, tag, OIDC
issuer, predicate, and hosted-runner constraints to the archive **before**
extracting files, installing packages, or modifying services. There is no
verification bypass. `latest` resolves to a concrete tag; pin `--version` for
repeatable deployment.

## 2. Install, including offline hosts

For a local archive, always specify the exact release tag. A sibling
`praxi-linux.sigstore.json` is required unless `--bundle` supplies its path.
A sibling `.sha256` or `--sha256` adds an optional integrity check:

```sh
sudo bash ./install-linux.sh --version v0.1.0 --file ./praxi-linux-aarch64.tar.gz
# Check the signed archive without root, extraction, or installation:
bash ./install-linux.sh --verify-only --version v0.1.0 --file ./praxi-linux-aarch64.tar.gz
```

### Offline trust preparation

On a trusted online machine, independently obtain the current Sigstore trusted
root snapshot with the trusted `gh` installation:

```sh
gh attestation trusted-root > trusted_root.jsonl
```

Transfer the trusted GitHub CLI, verified installer, architecture-specific
archive, provenance bundle, and this snapshot to the offline machine. Transfer
the root through your trusted administrative channel. **Do not automatically
trust a root supplied beside a downloaded archive.** The installer accepts a
custom root only when you explicitly supply it. Recreate the snapshot when
importing new releases; an offline machine cannot discover later key revocation.
Offline bundle verification authenticates the repository/owner names at signing
time, not their current ownership after any rename or name reuse. Preserve the
reviewed repository identity in your release administration. See GitHub's [offline verification guide](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/verify-attestations-offline).

Before executing the transferred installer, verify it offline too:

```sh
gh attestation verify ./install-linux.sh --bundle ./praxi-linux.sigstore.json \
  --custom-trusted-root /trusted/trusted_root.jsonl \
  --repo emreocell/Praxi-Bot --hostname github.com \
  --signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml \
  --source-ref refs/tags/v0.1.0 --deny-self-hosted-runners \
  --cert-oidc-issuer https://token.actions.githubusercontent.com \
  --predicate-type https://slsa.dev/provenance/v1 --digest-alg sha256

bash ./install-linux.sh --verify-only --offline --version v0.1.0 \
  --file ./praxi-linux-aarch64.tar.gz --custom-trusted-root /trusted/trusted_root.jsonl

# Runtime dependencies must already be installed on an offline host.
sudo bash ./install-linux.sh --offline --skip-packages --version v0.1.0 \
  --file ./praxi-linux-aarch64.tar.gz --custom-trusted-root /trusted/trusted_root.jsonl
```

`--offline` refuses `latest`, a missing local archive/root, package installation,
and `--with-desktop` downloads. `--verify-only` needs neither root privileges nor
runtime packages. A local `--file` with `--custom-trusted-root` can also verify
without network; the explicit offline mode additionally checks the installation
options. Missing, corrupt, or mismatched provenance/root material stops the
operation before installation.

Other options: `--port`, `--display`, `--screen`, `--bundle`, `--sha256`,
`--skip-packages`, `--no-start`, `--with-desktop`, `--with-desktop-session`,
`--with-session-apps`, `--without-session-apps`, `--without-desktop-session`,
`--desktop-session <none|xfce>`, `--without-desktop`.
Re-running upgrades the binaries and preserves `/var/lib/praxi`,
`/etc/praxi/praxi.env`, and `/etc/praxi/praxi-node.env`. A custom `BOT_PORT` stays
unless `--port` is supplied. Existing `praxi-xvfb` / `praxi-desktop` units,
`-screen 0` geometry, and `DISPLAY` stay unless `--screen`, `--display`,
`--without-desktop-session`, or `--without-desktop` is supplied (and a bare-Xvfb
host keeps bare Xvfb unless `--with-desktop-session` is passed). On an online
host, `--with-desktop` installs or refreshes Xvfb and Chromium;
`--with-desktop-session` adds a minimal Xfce desktop session on top of Xvfb
(`1280x800x24` on a fresh desktop setup, or preserving existing Xvfb geometry).

### Release administrator setup and dry run

Repository settings are not created by merging a workflow. Before a tagged
release, an administrator must configure:

1. An **active tag ruleset** matching `refs/tags/v*`, restricting tag creation
   to release maintainers and prohibiting updates and deletion. Keep bypass
   permissions limited to the intended release administrators.
2. An environment named **`release`**, with required reviewers, **Prevent
   self-review** enabled, and deployment tags restricted to `v*`. Disable
   administrator bypass where supported.

The attestation job references this environment and fails closed unless GitHub
reports the current tag as protected and the environment API reports required
reviewers with self-review prevention. If the token cannot read that policy,
the job fails; it never assumes approval. The publishing job is gated by the
same environment and the independent real-verification job.

To exercise real Sigstore/`gh` behavior without publishing, create a reviewed,
protected prerelease tag and dispatch **Linux Server Packages** on that tag
with **`dry_run: true`** (the default). The workflow builds both architectures,
attests the tagged inputs, and verifies all signed assets using real `gh`.
It then verifies the archive with an independently fetched root inside a Linux
network namespace with no network and fresh `gh` configuration/cache, and
rejects a tampered artifact, wrong tag and wrong repository. Tag pushes and this dry-run mode never create a GitHub release.
Publishing requires a separate dispatch on that protected tag with
`dry_run: false` and the environment approval. Branch and PR validation remain hermetic and use a
controlled verifier fixture; those checks do not claim a real attestation run.

Publishing uses `gh release create --verify-tag` exactly once. An existing
release fails rather than overwriting or adding assets. Publish a new version
for changed artifacts. The same-run signed bundle is always shipped beside
both tarballs and the tagged installer.

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
- with `--with-desktop`, installs `praxi-xvfb.service` (virtual display `:99`,
  `-nolisten tcp`, private `0600` Xauthority in `/var/lib/praxi/.Xauthority`, no
  `-fbdir` frame files) and a non-snap Chromium browser (source selection below).
  Prepares the root-owned, mode 1777 X11 socket directory inside the shared
  private `/tmp` before Xvfb starts. A later run keeps that unit until `--without-desktop`;
- with `--with-desktop-session`, also installs minimal Xfce (`xfwm4 --compositor=off`,
  `xfce4-panel` with Applications menu, `xfdesktop`, private `praxi` D-Bus
  session bus + AT-SPI2) in `praxi-desktop.service`, initially `1280x800x24` on
  fresh desktop installs (preserving existing geometry on upgrades), with
  screen-lock/power/printer autostarts disabled and no unconditional browser
  autostart (`xfce4-terminal` and `thunar` are hidden and omitted by default
  unless `--with-session-apps` is explicitly passed);
- when desktop mode is active, applies `praxi.slice` (`CPUQuota=100%`,
  `MemoryHigh=3G`, `MemoryMax=4G`) across `praxi.service`,
  `praxi-node.service`, `praxi-xvfb.service`, `praxi-desktop.service`, browser
  children, and applications launched from the menu (hosts without desktop mode
  do not receive `praxi.slice` limits), plus systemd hardening: `NoNewPrivileges`,
  `ProtectSystem=strict` with `ReadWritePaths=/var/lib/praxi`, `ProtectHome`,
  `PrivateTmp`, `PrivateDevices`, kernel and cgroup protections;
- adds wrapper commands `praxi` (the `bot` CLI, using `praxi.env`) and
  `praxi-node` (using `praxi-node.env`) that run as the `praxi` user.

The installer waits for two consecutive active-unit and valid loopback health
checks, with at most 30 attempts (each HTTP request has a two-second timeout).
Failure prints the last 50 service journal entries, rolls back binaries, units,
wrappers, and environment files to the previous working installation, restarts
any previously active services, and exits nonzero. `--no-start` installs/enables
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

### Using the host from the desktop app

The desktop app can show the host's screen (the Xvfb desktop and the bot's
browser), take it over and hand it back, and chat with bots that run **on the
host**. It needs no extra port: the app's own daemon opens an outbound iroh
channel to the host, and every request is checked by the host's peer guard as a
paired *Desktop*.

1. The host must run as a host: `bot serve --host` (this also turns peer
   networking on). An installed `praxi.service` runs `bot serve`; override it
   with a drop-in (`sudo systemctl edit praxi`):

   ```ini
   [Service]
   ExecStart=
   ExecStart=/opt/praxi/bin/bot serve --host
   ```

2. On the host, create a one-time pairing ticket for the desktop and copy it:
   `praxi host invite --role desktop`
3. In the app, open the computer selector in the header, **Add a host…**, and
   paste the ticket. The selector then lists the host next to "This computer".
4. Pairing does not allow screen access by itself. On the host, let the paired
   desktop watch and take over (kept across restarts, local user only):
   `praxi host screen --all-desktops` (add `--view-only` for watching only,
   `--revoke` to remove). `praxi host devices` lists device keys.

On a host, a paired Desktop can operate bots, chats and runs and use the screen
but cannot change secrets or provider keys, MCP servers, skills, email or web
search credentials, Telegram pairing, or import/hand over state; do those on the
host itself. Until the host has a model key of its own, the app says "this host
has no model key yet": the screen and bot management work, chats cannot be
answered. Put the key on the host with `praxi setup` (see section 3).

### Letting another core control this machine (experimental)

`bot-node run` needs a reachable X11 display (a physical one or the Xvfb
display from `--with-desktop`; Wayland is refused) and at least one paired
core. Without them it exits, and `praxi-node.service` would restart it every
10 seconds, so the unit is installed but not enabled. Pair first, then enable
it.

`bot-node` connects **outward** over iroh (QUIC with
relay fallback), so no inbound port is needed. On the machine that should be
controlled:

```sh
sudo praxi-node join '<bot-pair:...>'     # one-time ticket from the core
```

The ticket is single-use; afterwards the devices use stored keys. See ADR-0006
in the design documents for the model. Enable `praxi-node.service` only after
`bot-node join` has succeeded and `bot-node run` starts by hand.

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

### Desktop session, application menu, and browser lifecycle

Xvfb alone does **not** supply a usable desktop (no window manager, panel, or
application launcher). Adding `--with-desktop-session` (or running
`sudo bash scripts/linux/setup-headless.sh --system`) provisions a minimal Xfce
session (`praxi-desktop.service`) on Xvfb (`1280x800x24` on fresh desktop
installs, or preserving the existing `praxi-xvfb.service` geometry unless
`--screen` is passed), running as the same unprivileged `praxi` user:

- **Application menu usage & security posture:** During live-view takeover, open
  the top-left **Applications** menu on `xfce4-panel` to launch **Web Browser**
  (manual launch). `xfce4-terminal` and `thunar` are **not** installed or
  exposed in the menu by default; pass `--with-session-apps` if you explicitly
  need them, or `--without-session-apps` to hide them again. Note that
  `--without-session-apps` only writes `Hidden=true` `.desktop` overrides and
  omits them from `helpers.rc`—it does **not** uninstall `xfce4-terminal` or
  `thunar` if they were already installed by an earlier run or the base OS (their
  binaries stay on disk until removed explicitly with `apt-get remove` / `dnf remove`).
  All launched applications inherit `praxi.slice` containment and run as the `praxi` user
  with `DISPLAY`, `XAUTHORITY`, and `DBUS_SESSION_BUS_ADDRESS`. Note that while
  `praxi-desktop.service` does not place `BOT_SECRETS_PASSPHRASE` in the session
  environment, any application running as `praxi` has filesystem read access to
  `/etc/praxi/praxi.env` (`0640 root:praxi`, containing `BOT_SECRETS_PASSPHRASE`)
  and read/write access to `/var/lib/praxi`. Exposing a terminal or file manager
  with `--with-session-apps` allows a live-view operator—or an unguarded UI
  click—to read those files and run commands as `praxi` outside the daemon's
  shell permission engine.
- **Source & display limitations:** X11/Xvfb only (`XDG_SESSION_TYPE=x11`);
  Wayland is unsupported. Only one display owner is allowed per display (`:99`
  is owned by `praxi-xvfb.service`). No VNC, RDP, X11 TCP (`-nolisten tcp`),
  `xhost +`, `-fbdir` framebuffer dump, or default frame disk storage is enabled,
  and no new listening ports are opened. Keeping a legacy bare-Xvfb unit without
  `-auth` omits Xauthority authentication and leaves Xvfb's abstract Unix socket
  reachable by other local accounts; pass `--with-desktop` or
  `--with-desktop-session` to upgrade it to private `0600` Xauthority.
- **Lazy, idle-closed browser policy:** Chromium is never autostarted by Xfce
  or systemd. Managed `CdpDriver` instances default in the daemon (`bot serve`)
  to a **120 s** idle shutdown (`[limits] browser_idle_seconds = 120` in
  `/var/lib/praxi/host.toml`) and a RAM-based concurrency cap (`1` run below
  8 GiB RAM, `2` runs at 8 GiB+ RAM, or `[limits] max_computer_runs` in
  `host.toml`). The installer does not pin or overwrite
  `BOT_MAX_COMPUTER_RUNS` or `BOT_BROWSER_IDLE_SECONDS` in `/etc/praxi/praxi.env`
  so `host.toml` and daemon defaults remain effective; setting those env vars in
  `praxi.env` overrides `host.toml`. Watch streams do not launch or pin the
  browser; an active bot computer run or active takeover pins it. Named browser
  profiles persist under `/var/lib/praxi/browsers` and are preserved across idle
  shutdown. Any browser opened manually by a human from the Applications menu is
  outside `CdpDriver` idle shutdown and still counts toward `praxi.slice` RAM.
  *(Note: `BrowserSettings::isolated` defaults `headless: true` unless overridden
  for headed takeover/computer-use targets.)*

### Small-host resource check

Production hosts run prebuilt release tarballs; don't compile Rust on the
1-core/6-GB (or 2-core/12-GB) machine. Keep Chromium lazy: this installer never
starts a persistent browser. The doctor probe closes its browser after checking.
Plan targets are ≤500 MiB for display + session idle (the ~300 MB Xfce figure is
a planning estimate, not a guarantee), **<2.5 GB** (`2,500,000,000` bytes /
~2.33 GiB) peak for the complete Praxi process tree, and ≤5% of one core for
idle CPU. `praxi.slice` enforces `CPUQuota=100%`, `MemoryHigh=3G`,
`MemoryMax=4G` as containment, not as a substitute for measuring real RAM.

Run the repeatable measurement tool on the host (outputs text/JSON metrics only;
never reads `/proc/<pid>/cmdline`, `/proc/<pid>/environ`, frames, titles, or
URLs):

```sh
sudo python3 scripts/linux/measure-budget.py \
  --label desktop-idle --duration 30 --interval 0.5 \
  --require-slice-containment --check-budget --format both
```

You can also inspect cgroup counters directly with
`sudo systemctl show praxi.slice praxi praxi-desktop praxi-xvfb -p MemoryCurrent -p MemoryPeak -p CPUUsageNSec`.
Note that summed process `VmRSS` double-counts shared pages across processes;
when `praxi.slice/memory.peak` is writable (Linux 6.12+ as root),
`measure-budget.py` resets it at the start of the window and checks the true
kernel window peak (plus any escaped Praxi processes outside `praxi.slice`); on
older kernels where `memory.peak` is read-only, a lifetime `memory.peak` above
the budget fails `--check-budget` as inconclusive. Absent `praxi.slice`, zero
observed Praxi processes, or any process escaping `praxi.slice` also fails
`--check-budget`.

### Troubleshooting desktop, Xauthority, D-Bus, and namespaces

- **Stopping all Praxi services cleanly:** On systemd 249+, `praxi-xvfb.service`
  sets `Upholds=praxi-desktop.service praxi.service`, so `sudo systemctl stop praxi`
  will immediately be restarted while `praxi-xvfb.service` stays active. Stop
  everything cleanly with `sudo systemctl stop praxi-xvfb.service` (which stops
  `praxi-desktop.service` and `praxi.service` via `BindsTo=`). On older hosts
  with systemd < 249, `Upholds=` is omitted with an installer warning and
  `praxi-xvfb.service` uses an `ExecStartPost=!` uphold fallback
  (`praxi-xvfb-uphold.sh`) to start enabled dependents in the new namespace
  after Xvfb starts or recovers from a crash. `praxi.service`
  uses `Wants=praxi-desktop.service` so a desktop-session failure does not block
  the always-on daemon.
- **Xauthority permission / auth errors (`Invalid MIT-MAGIC-COOKIE-1`):**
  `/var/lib/praxi/.Xauthority` is created with mode `0600` owned by
  `praxi:praxi` and reused across `praxi-xvfb.service` restarts when already
  valid. `praxi-desktop.service` and `praxi.service` bind to `praxi-xvfb.service`
  (`BindsTo=`/`After=`, with `Upholds=` on `praxi-xvfb.service`) so an
  unexpected Xvfb exit stops dependents and brings them back cleanly. To force a
  fresh cookie, remove `/var/lib/praxi/.Xauthority` and run
  `sudo systemctl restart praxi-xvfb.service`.
- **X11 socket / `PrivateTmp` namespace mismatch (`Can't open display :99`):**
  `praxi-xvfb.service`, `praxi-desktop.service`, `praxi.service`, and
  `praxi-node.service` must share the Xvfb mount namespace via
  `JoinsNamespaceOf=praxi-xvfb.service` in `/etc/systemd/system/praxi*.service.d/desktop.conf`.
  Always use `sudo praxi doctor` rather than direct `bot doctor` so the doctor
  probe joins the same namespace and slice.
- **D-Bus / AT-SPI2 missing (`org.a11y.Bus` or `xfconf` errors):** Verify
  `DBUS_SESSION_BUS_ADDRESS=unix:path=/var/lib/praxi/.dbus-session` is present
  in `/etc/praxi/praxi.env` and the mode `0600` socket
  `/var/lib/praxi/.dbus-session` exists (`sudo systemctl status praxi-desktop.service`).
- **XTEST / RANDR missing:** Confirm `praxi-xvfb.service` runs with
  `+extension RANDR +extension XTEST` and `xdpyinfo -display :99` (inside the
  service namespace) lists both extensions.

## Update, logs, rollback, and uninstall

```sh
sudo ./scripts/install-linux.sh --version v0.1.0 --file ./praxi-linux-aarch64.tar.gz   # update (keeps existing desktop mode)
sudo ./scripts/install-linux.sh --version v0.1.0 --file ./praxi-linux-aarch64.tar.gz --without-desktop-session  # remove Xfce, keep Xvfb
sudo ./scripts/install-linux.sh --version v0.1.0 --file ./praxi-linux-aarch64.tar.gz --without-desktop          # remove Xfce and Xvfb
journalctl -u praxi -u praxi-desktop -u praxi-xvfb -n 100
sudo ./scripts/install-linux.sh --uninstall           # keeps data and user
sudo ./scripts/install-linux.sh --uninstall --purge   # deletes data too
```

On source-built hosts without a signed release archive, `scripts/linux/setup-headless.sh`
provisions or rolls back the exact same system units:

```sh
sudo bash scripts/linux/setup-headless.sh --system --display :99 --screen 1280x800x24
sudo bash scripts/linux/setup-headless.sh --system --without-desktop-session
sudo bash scripts/linux/setup-headless.sh --system --without-desktop
```

For user-level development sessions on a separate display (never colliding with
`:99`), run `bash scripts/linux/setup-headless.sh --user --display :98`.
