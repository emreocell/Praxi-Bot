# Install and first run

There are two ways to get Praxi: the Windows installer (no terminal needed) or
a build from source (any OS for the CLI and daemon). The only release so far is
the pre-release `v0.1.0-alpha.1` (a Windows installer), so otherwise you build
the installer yourself or build the CLI.

## Option A: Windows desktop app

The installer is a single per-user NSIS `.exe`. It bundles the desktop shell
(`bot-desktop.exe`) and the daemon (`bot.exe`, used as a sidecar).

1. Run the Praxi-Bot setup `.exe`. It installs under
   `%LOCALAPPDATA%\Programs\Praxi-Bot` and needs no administrator rights.
2. Until builds are code-signed, Windows SmartScreen may warn you. Choose
   **More info**, then **Run anyway**.
3. On first launch the app starts the bundled daemon and opens a five-step
   wizard:
   1. **Welcome**: interface language (English or Turkish) and theme (light,
      dark, system). Praxi is local-first with no telemetry.
   2. **Provider**: pick Google Gemini, Anthropic, an OpenAI-compatible
      service or local Ollama, paste the API key, and press **Test
      connection**. The key goes straight to Windows Credential Manager and is
      never shown again. Google Gemini has a free API tier through Google AI
      Studio, with no credit card.
   3. **Safety**: choose the default permission preset. The default is
      **Approve destructive** (see [Chatting and approvals](chatting-and-approvals.md)):
      reads are free, and anything that changes files, clicks or types on the
      desktop, runs commands or sends messages asks first. Financial actions
      always ask, critical credential and system paths are always blocked, and
      `Ctrl+Alt+Esc` stops the bot at any time.
   4. **Your first bot**: a name, an avatar/colour and model roles (`executor`,
      `planner`, `classifier`) filled in from the provider you picked.
   5. **Try it live**: click *"What's on my screen?"* or *"Open Notepad and
      write hello"*, watch the bot respond, and approve or deny a live permission
      card in real time.
4. You can skip the wizard at any step and re-run it later from
   **Settings, First-run setup wizard**.

Autostart at Windows sign-in is **off** unless you switch on *Launch at Windows
sign-in* in Settings (or pass `/AUTOSTART=1` to the installer). Uninstalling
keeps your chats, bots and settings unless you tick **Delete application data**;
see [Installing on Windows](install-windows.md).

The global emergency stop hotkey of the app is `Ctrl+Alt+Esc` (configurable in
the app).

Desktop control (clicking and typing in other apps) is implemented for Windows
only. On other systems the daemon runs, but without desktop tools.

## Option B: build the CLI from source

Requirements: Rust 1.89 or newer, a C toolchain (MSVC build tools on Windows),
and for the desktop app Node 24+ and pnpm 10+.

```sh
cargo build --release -p cli      # produces target/release/bot
./target/release/bot --version
```

To run the desktop app in development with the deterministic fake provider (no API
key needed), use `PRAXIBOT_FAKE_PROVIDER=1 pnpm dev:desktop`. To build the Windows
installer yourself, see
[Building the Windows installer yourself](#building-the-windows-installer-yourself).

## Building the Windows installer yourself

Tauri bundles the `bot` CLI binary via `bundle.externalBin: ["binaries/bot"]`
in [`apps/desktop/tauri.conf.json`](../../apps/desktop/tauri.conf.json). Tauri expects
the sidecar binary to be named with the Rust target triple (`rustc -Vv | grep host`)
inside `apps/desktop/binaries/` before `tauri build` runs:

```powershell
# 1. Determine target triple (e.g. x86_64-pc-windows-msvc on 64-bit Windows MSVC)
$triple = (rustc -Vv | Select-String "host:").ToString().Split(":")[1].Trim()

# 2. Build the CLI daemon in release mode
cargo build --release -p cli

# 3. Stage the sidecar binary with the target-triple suffix expected by Tauri externalBin
New-Item -ItemType Directory -Force -Path apps/desktop/binaries | Out-Null
Copy-Item "target/release/bot.exe" "apps/desktop/binaries/bot-$triple.exe" -Force

# 4. Build the desktop UI and package the per-user NSIS installer
pnpm build:desktop
```

The resulting installer is written to
`target/release/bundle/nsis/Praxi-Bot_<version>_x64-setup.exe`. At runtime,
[`apps/desktop/src/daemon.rs`](../../apps/desktop/src/daemon.rs) resolves the daemon
binary in priority order:

1. `PRAXIBOT_BOT_BIN` environment variable (if set)
2. Bundled sidecar next to `bot-desktop.exe` (`bot-<target-triple>.exe` or `bot.exe`)
3. Staged sidecar in `apps/desktop/binaries/bot-<target-triple>.exe`
4. `bot` on `PATH`

Code-signing fields (`certificateThumbprint`, `digestAlgorithm`, `timestampUrl`)
are pre-configured as placeholders under `bundle.windows` in
[`apps/desktop/tauri.conf.json`](../../apps/desktop/tauri.conf.json).

## First run from the terminal

```sh
bot setup          # interactive: provider, key, first bot, permission preset
bot serve          # start the daemon (leave it running)
```

In a second terminal:

```sh
bot status                      # daemon health, pending approvals
bot bots list                   # shows your bots and their ids
bot chat <bot-id> "What files are in my Documents folder?"
```

`bot setup` asks, in order: language of messages, provider, API key (hidden
input), model, first-bot preset (`general`, `browser` or `desktop`), bot name
and default permission preset. Nothing is saved until the wizard finishes. The key is
stored in the secret store, never in a plain-text file.

Scripted setup (for example in CI or a dotfiles script):

```sh
export GEMINI_API_KEY=...        # your key
bot setup --provider gemini --key-env GEMINI_API_KEY --yes \
          --bot-preset general --permission approve-destructive
```

`--yes` is required together with `--provider`; it means "never prompt".

### What `bot serve` does

- Listens on `127.0.0.1:47821` (change with `--port` or `BOT_PORT`). It never
  binds another network interface.
- Creates a random 256-bit API token in `<data dir>/api-token` on first start
  and prints its path. The CLI and the desktop app read it from there.
- Pauses any run that was interrupted by a previous shutdown. Resume it through
  the API (`POST /v1/runs/{id}/resume`); there is no CLI command for it yet.
- Starts the scheduler and, if configured, the Telegram channel.
- Stops cleanly on Ctrl-C: running tool steps get `--shutdown-grace` seconds
  (default 10) to finish, then all live runs are paused for the next start.

`bot serve` needs at least one usable provider. Add one with `bot setup` or
`bot providers add`; a provider added while the daemon runs is picked up after
`POST /v1/providers/reload` (the desktop app does this for you).

### Where your files live

Inside the data directory:

| File or folder | Contents |
| --- | --- |
| `providers.toml` | Which providers are enabled, default model, where each key is stored |
| `api-token` | The local API token (treat it like a password) |
| `skills/` | Your skills, see [Skills](skills.md) |
| `workflows/` | Saved workflows, see [Workflows](workflows.md) |
| `channels.toml`, `telegram_state.json` | Telegram on/off switch and paired users |
| SQLite database and per-run event logs | Bots, chats, messages, runs, approvals, audit log |

## Environment variables

| Variable | Effect |
| --- | --- |
| `BOT_DATA_DIR` | Data directory |
| `BOT_PORT`, `BOT_URL` | Daemon port, or full daemon URL for client commands |
| `BOT_CATALOG_DIR` | Directory of provider catalog `*.toml` files instead of the built-in set |
| `BOT_SECRETS_BACKEND` | `auto` (default), `keyring` or `file` |
| `BOT_SECRETS_PASSPHRASE` | Passphrase for the encrypted-file secret backend (otherwise prompted) |
| `BOT_DESKTOP=off` | Start the daemon without desktop tools |
| `BOT_ALLOWED_ORIGINS` | Extra browser origins allowed to call the API (comma separated; for developing a UI) |
