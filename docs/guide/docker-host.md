# Running Praxi in Docker (always-on host)

This guide runs the Praxi always-on host (`bot serve --host`) inside a Linux
container using [`packaging/docker/Dockerfile`](../../packaging/docker/Dockerfile)
and [`packaging/docker/docker-compose.yml`](../../packaging/docker/docker-compose.yml).

> **Status.** The Docker setup builds `bot` from source in a multi-stage image
> and runs `bot serve --host` as the non-root `praxi` user with `tini` process
> reaping and a `/v1/health` healthcheck. On a Linux VM with systemd, the
> signed installer in [Running Praxi on a remote host](remote-host.md) remains
> the primary installation path.

## Image variants

[`packaging/docker/Dockerfile`](../../packaging/docker/Dockerfile) produces two
variants from the same multi-stage build:

- **Base headless daemon (`WITH_DESKTOP=0`, default)**: Installs only the
  runtime libraries (`libdbus-1-3`, `libssl3`, `ca-certificates`, `curl`,
  `tini`) and runs `bot serve --host` with `/var/lib/praxi` as a volume. Use this
  when you want an always-on host for chat, Telegram, schedules, MCP servers,
  and web tools without a virtual display or browser.
- **Desktop and browser variant (`WITH_DESKTOP=1`)**: Adds `Xvfb` (`:99`,
  `1920x1080x24`, `-nolisten tcp`, private `0600` Xauthority), a minimal Xfce
  session (`xfwm4`, `xfce4-panel`, `xfdesktop`, private D-Bus session bus,
  AT-SPI2), and the same pinned **Google Chrome for Testing `157.0.8103.38`**
  binary (`/opt/praxi-browser/chrome`, SHA-256 verified) used by
  `scripts/install-linux.sh`.

Both variants run as the unprivileged `praxi` user (`UID 10001`, `GID 10001`)
and bake **no secrets** into the image.

## First run

1. **Create the environment file** from
   [`packaging/docker/.env.example`](../../packaging/docker/.env.example) and
   generate a random passphrase for `/var/lib/praxi/secrets.enc`:

   ```sh
   cp packaging/docker/.env.example packaging/docker/.env
   # Replace BOT_SECRETS_PASSPHRASE with a strong random value, e.g.:
   openssl rand -hex 32
   ```

   Containers have no OS keychain, so `BOT_SECRETS_BACKEND=file` requires
   `BOT_SECRETS_PASSPHRASE` (or `BOT_SECRETS_PASSPHRASE_FILE` pointing to a
   mounted Docker secret file). Keep `.env` out of git and off shared disks.

2. **Build and start the container** with Docker Compose:

   ```sh
   docker compose -f packaging/docker/docker-compose.yml up -d --build
   ```

   To build the desktop + browser variant instead, set `WITH_DESKTOP: "1"` in
   `docker-compose.yml` (and review the `--security-opt` notes below) or build
   directly:

   ```sh
   docker build --build-arg WITH_DESKTOP=1 -f packaging/docker/Dockerfile -t praxi-host:desktop .
   ```

3. **Check health and logs**:

   ```sh
   docker compose -f packaging/docker/docker-compose.yml ps
   docker compose -f packaging/docker/docker-compose.yml logs -f praxi
   docker exec praxi-host curl -fsS http://127.0.0.1:47821/v1/health
   ```

   A fresh container answers `"status":"setup_required"` with HTTP 200 until a
   model provider is configured, then `"status":"ok"`.

4. **Configure a provider, bot, and optional Telegram channel**:

   ```sh
   docker exec -it praxi-host bot setup
   docker exec -it praxi-host bot doctor
   docker exec -it praxi-host bot channels telegram setup   # optional
   docker exec -it praxi-host bot channels telegram pair    # optional
   ```

   If you set provider API key environment variables (such as
   `ANTHROPIC_API_KEY` or `GEMINI_API_KEY`) in `.env`, still run `bot setup` (or
   `bot providers add`) so the default provider and first bot are configured.

## Pairing the desktop app with the container host

The Praxi desktop app connects to a container host over an outbound iroh peer
channel (QUIC with relay fallback). You do **not** need to expose port `47821`
on the host firewall or router.

1. Confirm the container is running in host mode (`bot serve --host`, which is
   the image's default `CMD`), and has outbound HTTPS/UDP egress for relays.
2. Generate a single-use pairing ticket inside the container:

   ```sh
   docker exec -it praxi-host bot host invite --role desktop
   ```

3. In the Praxi desktop app, open the computer selector in the header, choose
   **Add a host…**, and paste the ticket (tickets expire after two minutes and
   work once).

Because `docker exec` runs as `praxi` in `/var/lib/praxi`, `bot host invite`
reads `/var/lib/praxi/owner-key` and issues an **admin desktop** invite
(matching `sudo praxi host invite --role desktop` on systemd hosts). That admin
desktop can manage bots, chats, runs, and live screen takeover (when built with
`WITH_DESKTOP=1`), while secrets, execution policy, MCP servers, skills, email
and web search credentials, and Telegram pairing remain managed locally inside
the container (`docker exec -it praxi-host bot ...`).

To list, grant, or revoke paired devices:

```sh
docker exec praxi-host bot host devices
docker exec praxi-host bot host screen --all-desktops
docker exec praxi-host bot host screen <key> --view-only
docker exec praxi-host bot host screen <key> --revoke
```

## Upgrades

1. **Back up the volume first** (see [Backups of the volume](#backups-of-the-volume)).
   Opening the SQLite database in a newer `bot` release can migrate it forward,
   so keep a pre-upgrade snapshot before replacing the image.
2. **Pull source changes and rebuild**:

   ```sh
   git pull
   docker compose -f packaging/docker/docker-compose.yml build --pull
   ```

3. **Stop cleanly and recreate**:

   ```sh
   docker compose -f packaging/docker/docker-compose.yml stop
   docker compose -f packaging/docker/docker-compose.yml up -d
   ```

   Stopping sends `SIGTERM` through `tini`, allowing `bot serve` to pause live
   runs cleanly during its `--shutdown-grace` window (10 seconds by default) and
   close SQLite before the new container starts.

4. **Verify**:

   ```sh
   docker exec praxi-host bot doctor
   ```

## Backups of the volume

All persistent host state lives in the `/var/lib/praxi` volume (`praxi-data` in
`docker-compose.yml`): the SQLite database, `secrets.enc`, `api-token`,
`owner-key`, peer identity, `host.toml`, and browser profiles.

### Full volume snapshot (recommended before upgrades)

Stop the container first so SQLite is idle and the snapshot is consistent:

```sh
docker compose -f packaging/docker/docker-compose.yml stop
docker run --rm \
  -v praxi-data:/data:ro \
  -v "$PWD":/backup \
  debian:bookworm-20261104-slim \
  tar -C /data -czf "/backup/praxi-data-$(date -u +%Y%m%dT%H%M%SZ).tgz" .
docker compose -f packaging/docker/docker-compose.yml start
```

> [!IMPORTANT]
> The volume archive contains `secrets.enc`, `api-token`, `owner-key`, and
> peer keys, **not** `BOT_SECRETS_PASSPHRASE`. Store the passphrase from `.env`
> (or your Docker secret file) separately from the backup archive; without it,
> `secrets.enc` cannot be decrypted.

To restore a volume snapshot into a clean volume:

```sh
docker compose -f packaging/docker/docker-compose.yml stop
docker run --rm \
  -v praxi-data:/data \
  -v "$PWD":/backup:ro \
  debian:bookworm-20261104-slim \
  sh -c 'rm -rf /data/* /data/.[!.]* && tar -C /data -xzf /backup/praxi-data-YYYYMMDDTHHMMSSZ.tgz'
docker compose -f packaging/docker/docker-compose.yml up -d
```

### Portable state backup (`bot backup` / `bot restore`)

You can also export portable state (bots, chats, memory, skills, workflows,
schedules, provider settings — excluding API keys, tokens, paired devices, and
browser profiles) as described in [Encrypted backup and restore](backup.md). Keep the passphrase in a file or Docker secret, never on the command line:

The image does not define `BACKUP_PASSPHRASE`; you set it for the command
(`docker exec -e BACKUP_PASSPHRASE ...` passes the value from your shell without
writing it on the command line, or use `--passphrase-file` with a mounted Docker
secret). `--passphrase-env` takes the **name** of the variable, never the passphrase.

```sh
export BACKUP_PASSPHRASE='...'   # your shell only; not stored in the image
docker exec -e BACKUP_PASSPHRASE praxi-host bot backup create /tmp/praxi-backup.bak --passphrase-env BACKUP_PASSPHRASE
docker cp praxi-host:/tmp/praxi-backup.bak ./praxi-backup.bak
docker exec praxi-host rm -f /tmp/praxi-backup.bak
```

Because `bot backup restore` refuses to run while `bot serve` is listening, restore
with the daemon stopped using a one-off container on the same volume, then
start the service and re-enter secrets.

## Security notes

### Never expose the daemon port publicly

- `bot serve` strictly binds `127.0.0.1:47821` (loopback only) and rejects
  requests whose `Host` header is not loopback (`127.0.0.1`, `localhost`,
  `[::1]`) or whose `Origin` is not allowed.
- Never publish `0.0.0.0:47821:47821` or put a public reverse proxy in front of
  the daemon API. Anyone who reaches the API with a token can drive bots and
  read host state.
- In `docker-compose.yml`, the port mapping is `127.0.0.1:47821:47821`. Note
  that on Docker's default bridge network, `bot serve` listens on the
  container's own `127.0.0.1` (not the container `eth0` interface), so
  bridge-forwarded host connections to `127.0.0.1:47821` are refused too. Keep
  bridge mode when pairing via iroh or Telegram and administer locally via
  `docker exec -it praxi-host bot ...`. If you specifically need local HTTP API
  access from the Linux Docker host itself, use `network_mode: host` (still
  loopback-only) and reach it over an SSH tunnel (`ssh -L 47821:127.0.0.1:47821 user@server`).

### Who can reach the owner key and passphrase

Praxi separates the general daemon bearer token (`/var/lib/praxi/api-token`,
mode `0600`) from the **owner key** (`/var/lib/praxi/owner-key`, mode `0600`).
Owner-only operations — approving tool calls, changing permission policy,
modifying secrets, pairing/revoking hosts, creating admin invites — require
`owner-key` (`x-praxi-owner-key`), not just `api-token`.

Inside the container:

- **Who can read `owner-key`**: Anyone who can run `docker exec` against
  `praxi-host`, anyone with `root` (or Docker socket access) on the Docker host,
  and any process running as the `praxi` user inside the container (including an
  approved `shell_run` or `script_run` command executed by a bot). Those same
  parties can also inspect `/proc/<pid>/environ` for `BOT_SECRETS_PASSPHRASE`
  or read `/var/lib/praxi/secrets.enc`.
- **Who cannot**: Bots without shell/script access, CLI coding engines, MCP
  clients, and callers holding only `api-token` are never handed `owner-key`.
- **Operational posture**: Treat Docker daemon / `docker exec` access as root
  host administration, keep the container on a dedicated host/VM without
  unrelated personal data, and keep the bot permission preset on **Approve
  destructive** or stricter (see [Security model](security.md)).

### Chrome sandbox and `--security-opt` for `WITH_DESKTOP=1`

`browser-cdp` refuses `--no-sandbox` for non-root Linux processes so Chromium
keeps its renderer sandbox enabled. On Linux, Chromium's sandbox relies on
unprivileged user namespaces (`CLONE_NEWUSER`).

On systemd hosts, `scripts/install-linux.sh` installs a named AppArmor profile
(`/etc/apparmor.d/praxi-browser`) granting `userns` only to
`/opt/praxi-browser/chrome`. Inside a Docker container, `docker build` cannot
load kernel AppArmor profiles, and Docker's default `docker-default` AppArmor
and `seccomp` profiles block unprivileged user namespaces inside the container.
In addition, on Ubuntu 24.04+ hosts where
`kernel.apparmor_restrict_unprivileged_userns=1` is enabled in the host kernel,
even `--security-opt apparmor=unconfined` is denied user namespaces unless the
host either loads a custom AppArmor profile permitting `userns` for the
container or disables that sysctl (`sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`).

As a result, running the `WITH_DESKTOP=1` variant with browser automation
typically requires passing container security overrides at `docker run` /
Compose runtime (plus a `userns`-permitting AppArmor profile or sysctl on
Ubuntu 24.04+ hosts):

```sh
docker run -d \
  --security-opt seccomp=unconfined \
  --security-opt apparmor=unconfined \
  --env-file packaging/docker/.env \
  -v praxi-data:/var/lib/praxi \
  praxi-host:desktop
```

Prefer a custom, narrowly scoped seccomp/AppArmor profile allowing `userns`
over `unconfined` on shared or multi-tenant hosts, and avoid `--privileged` or
`--cap-add=SYS_ADMIN`. If you do not need browser or X11 desktop automation,
use the default `WITH_DESKTOP=0` image so no X server, desktop session, or
`--security-opt` relaxation is needed.
