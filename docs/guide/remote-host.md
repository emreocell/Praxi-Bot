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
`.sha256` file; tagged runs attach them to the GitHub release. There are no
published releases yet, so run the workflow yourself and download the artifact
for your architecture.

## 2. Install

Copy the script and tarball to the server (or, once a release exists, let the
script download it):

```sh
# from a local tarball (the .sha256 next to it is verified automatically)
sudo ./scripts/install-linux.sh --file ./praxi-linux-aarch64.tar.gz

# from a published release
sudo ./scripts/install-linux.sh --version v0.1.0

# with a virtual display and Chromium for browser/desktop automation
sudo ./scripts/install-linux.sh --file ./praxi-linux-aarch64.tar.gz --with-desktop
```

Other options: `--port`, `--display`, `--screen`, `--skip-packages`,
`--no-start`. Run it with `--help` for the full list. Re-running upgrades the
binaries and keeps `/var/lib/praxi` and `/etc/praxi/praxi.env` (a custom
`BOT_PORT` stays unless you pass `--port` again). Pass `--with-desktop` on
that re-run to keep the virtual display; omitting it stops `praxi-xvfb` and
removes `DISPLAY`.

The script:

- installs the runtime libraries the binaries link (`libdbus`, `libssl.so.3`);
- creates a `praxi` system user with no login shell;
- installs binaries to `/opt/praxi` and keeps data in `/var/lib/praxi`;
- writes `/etc/praxi/praxi.env` (mode 0640) with the data dir, port and a
  random passphrase for the encrypted-file secret store (headless servers have
  no OS keychain);
- installs `praxi.service` (`bot serve`, 127.0.0.1 only) and enables it, plus
  `praxi-node.service` (not enabled, see below);
- with `--with-desktop`, installs `praxi-xvfb.service` (virtual display `:99`)
  and Chromium;
- applies systemd hardening: `NoNewPrivileges`, `ProtectSystem=strict` with
  `ReadWritePaths=/var/lib/praxi`, `ProtectHome`, `PrivateTmp`,
  `PrivateDevices`, kernel and cgroup protections;
- adds wrapper commands `praxi` (the `bot` CLI) and `praxi-node` that run as
  the `praxi` user with the service environment.

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
- Protect `/etc/praxi/praxi.env`: it holds the secret-store passphrase. Do not
  commit or paste it.
- Keep the permission preset on **Approve destructive** or stricter (see
  [Chatting and approvals](chatting-and-approvals.md#the-four-presets)). A server with Chromium and a virtual display is a
  powerful machine; use a dedicated VM without personal data.
- Use SSH keys only and keep the OS patched (`unattended-upgrades` on Ubuntu).
- Chromium: Ubuntu's `chromium-browser` package is a snap, which does not work
  well inside a hardened system service. Prefer Debian's `chromium` deb, or
  another non-snap Chromium build.

## Update, logs, uninstall

```sh
sudo ./scripts/install-linux.sh --file ./praxi-linux-aarch64.tar.gz   # update
# pass --with-desktop again if this host should keep the virtual display
journalctl -u praxi -n 100
sudo ./scripts/install-linux.sh --uninstall           # keeps data and user
sudo ./scripts/install-linux.sh --uninstall --purge   # deletes data too
```

If you only need a headless desktop session for development, the older
user-level script `scripts/linux/setup-headless.sh` is still available.
