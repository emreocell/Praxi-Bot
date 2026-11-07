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

- A VM with 1 GB RAM or more (2 GB or more if you add the virtual display and
  Chromium), running Ubuntu 22.04+, Debian 12+, or an Oracle Linux / Fedora
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
  and Chromium. A later run keeps that unit until `--without-desktop`;
- applies systemd hardening: `NoNewPrivileges`, `ProtectSystem=strict` with
  `ReadWritePaths=/var/lib/praxi`, `ProtectHome`, `PrivateTmp`,
  `PrivateDevices`, kernel and cgroup protections;
- adds wrapper commands `praxi` (the `bot` CLI, using `praxi.env`) and
  `praxi-node` (using `praxi-node.env`) that run as the `praxi` user.

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
- Chromium: Ubuntu's `chromium-browser` package is a snap, which does not work
  well inside a hardened system service. Prefer Debian's `chromium` deb, or
  another non-snap Chromium build.
- Chromium under `ProtectSystem=strict` and `PrivateDevices=true` is untested.
  Those settings make the filesystem read-only except `/var/lib/praxi` and
  replace `/dev` with a private set of pseudo-devices, so a browser can fail
  to reach `/dev/shm` or `/dev/dri`. Relax them with a drop-in. The installer
  only rewrites `desktop.conf` in the drop-in directory, so this file stays
  across updates:

  ```ini
  # /etc/systemd/system/praxi.service.d/chromium.conf
  # Repeat under praxi-node.service.d if the node should launch Chromium.
  [Service]
  ProtectSystem=full
  PrivateDevices=false
  ```

  Apply it with `sudo systemctl daemon-reload` and
  `sudo systemctl restart praxi`.

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
