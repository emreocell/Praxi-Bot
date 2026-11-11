# Praxi-Bot

A self-hosted, open-source AI agent that controls desktops, manages tasks, and
connects to external services. It runs on your own machines and talks to the
LLM providers you choose (cloud APIs or local models).
The core daemon is written in Rust (tokio + axum); node-agents dial out to it
from every machine you want to control, requiring no inbound ports.

**Status: early development — not usable yet.**
No stable API, no releases. Expect breaking changes at any time.

## Documentation

The **[user guide](docs/guide/index.md)** explains how to install Praxi, create
bots, chat and approve actions, schedule tasks, use workflows and skills, pick a
provider (including the Gemini free tier), connect Telegram, and understand the
security model. It also has a [CLI reference](docs/guide/cli-reference.md),
[troubleshooting](docs/guide/troubleshooting.md) and an [FAQ](docs/guide/faq.md).
Features that are not available yet are marked **Planned**.

## Windows Installer & First-Run Onboarding

Praxi-Bot ships as a single per-user Windows NSIS installer that bundles both
the desktop shell (`bot-desktop.exe`) and the local background daemon (`bot.exe`
sidecar). No terminal is required:

1. **Install:** Run the Praxi-Bot setup `.exe`. It installs into your user
   profile (`%LOCALAPPDATA%\Programs\Praxi-Bot`) without requiring
   administrator rights and adds a **Praxi-Bot** Start-menu shortcut.
2. **Windows SmartScreen note:** Until release builds are signed with an
   Authenticode certificate, Windows Defender SmartScreen may display a
   *"Windows protected your PC"* prompt on first run. Click **More info** →
   **Run anyway** to continue.
3. **Guided 5-step setup wizard:** On first launch, the desktop app starts the
   bundled daemon automatically and opens the onboarding wizard:
   - **Step 1 — Welcome:** Pick interface language (**English** / **Türkçe**)
     and theme (**Light** / **Dark** / **System**). Praxi-Bot is 100%
     local-first with zero telemetry.
   - **Step 2 — Provider:** Choose a preset (**Google Gemini** — which offers a
     free API tier via Google AI Studio with no credit card — **Anthropic**,
     **OpenAI-compatible**, or **Local Ollama**), paste your API key (saved
     directly to Windows Credential Manager and never shown again), and click
     **Test connection**.
   - **Step 3 — Safety:** Select your default permission preset (defaults to
     **Approve destructive**, where reads are free and any action that changes
     files, clicks/types on the desktop, runs commands, or sends messages asks
     first). Financial actions always ask, critical credential/system paths are
     always blocked, and `Ctrl+Alt+Esc` stops the bot at any time.
   - **Step 4 — Create your first bot:** Pick a name, avatar/color, and model
     roles (`executor`, `planner`, `classifier`) pre-filled from your chosen
     provider.
   - **Step 5 — Try it live:** Click *"What's on my screen?"* or *"Open Notepad
     and write hello"* to watch the bot respond and approve/deny a live
     permission card in real time.
   - You can skip the wizard at any step and resume or re-run it later from
     **Settings → First-run setup wizard**.

### Autostart & Uninstalling

- **Autostart (off by default):** Praxi-Bot does not register itself to run at
  Windows sign-in unless you enable **Launch at Windows sign-in** in
  **Settings** (or pass `/AUTOSTART=1` to the NSIS installer).
- **Uninstaller keeps your data by default:** Uninstalling Praxi-Bot stops any
  running `bot-desktop.exe` / `bot.exe` processes, removes the autostart
  registry value, and **preserves** your chats, bots, and settings in
  `%APPDATA%\praxibot` and `%LOCALAPPDATA%\praxibot` unless you explicitly tick
  **Delete application data** in the uninstaller.

## Build

Requirements: Rust 1.89+, MSVC build tools (Windows) or a standard C toolchain
(Linux/macOS), Node 24+, and pnpm 10+.

```sh
# Check everything compiles
cargo build --workspace

# Build only the CLI
cargo build -p cli

# Run the CLI
./target/debug/bot --version

# Run the desktop app in development (with the deterministic fake provider)
PRAXIBOT_FAKE_PROVIDER=1 pnpm dev:desktop
```

### Producing the Bundled Sidecar & Windows NSIS Installer (Release Build)

Tauri bundles the `bot` CLI binary via `bundle.externalBin: ["binaries/bot"]`
in [`apps/desktop/tauri.conf.json`](apps/desktop/tauri.conf.json). Tauri expects
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
[`apps/desktop/src/daemon.rs`](apps/desktop/src/daemon.rs) resolves the daemon
binary in priority order:

1. `PRAXIBOT_BOT_BIN` environment variable (if set)
2. Bundled sidecar next to `bot-desktop.exe` (`bot-<target-triple>.exe` or `bot.exe`)
3. Staged sidecar in `apps/desktop/binaries/bot-<target-triple>.exe`
4. `bot` on `PATH`

Code-signing fields (`certificateThumbprint`, `digestAlgorithm`, `timestampUrl`)
are pre-configured as placeholders under `bundle.windows` in
[`apps/desktop/tauri.conf.json`](apps/desktop/tauri.conf.json).

## Telegram

Talk to your bots from Telegram (`bot channels telegram setup` / `pair`); see [docs/telegram.md](docs/telegram.md)
or the [user guide page](docs/guide/telegram.md).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
