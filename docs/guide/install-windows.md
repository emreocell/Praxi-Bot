# Installing on Windows

This page covers the Windows installer in detail: what it installs, where your
data lives, how upgrading and uninstalling behave, and what to do when
something goes wrong. For the first-run wizard see [Install and first run](install.md).

## Before you start

- Windows 10 or 11, 64-bit. No administrator rights are needed: the installer
  is per-user.
- **WebView2.** The app window is drawn by Microsoft Edge WebView2, which is
  already present on current Windows 10/11. If it is missing the installer
  downloads the small WebView2 bootstrapper (needs an internet connection once).
  If the app window stays blank, install the "Evergreen Runtime" from Microsoft
  and start Praxi again.
- **SmartScreen.** Releases are not code-signed yet, so Windows may show
  "Windows protected your PC". Choose **More info**, then **Run anyway**. To be
  safe, compare the SHA-256 published next to the download with
  `Get-FileHash .\Praxi-...-setup.exe`.

## What gets installed

| What | Where |
|------|-------|
| Program files (`bot-desktop.exe`, `bot.exe` daemon sidecar) | `%LOCALAPPDATA%\Programs\Praxi-Bot` |
| Your data: `bot.db`, `config`, logs, `backups` | `%APPDATA%\praxi\praxi-bot` |
| API keys | Windows Credential Manager (never in files) |
| Start menu entry | `Praxi-Bot` |

Autostart at sign-in is off by default; switch it on in **Settings** (or pass
`/AUTOSTART=1` to the installer). Only one copy of the app runs at a time: starting it again just brings the existing
window to the front.

## Upgrading

Run the new setup `.exe` over the old install; running Praxi processes are
stopped first and your data is kept. When the new version needs a newer
database schema, the app first writes a copy of your database to
`%APPDATA%\praxi\praxi-bot\backups\bot-v<old>-<timestamp>.db` (the newest three
are kept). To roll back, close Praxi, install the older version and copy the
backup over `bot.db`.

## Uninstalling

Use **Settings, Apps** (or the Start menu entry). The uninstaller stops any
running `bot-desktop.exe` and `bot.exe` processes and removes the autostart
registry value. Your chats, bots and settings (in `%APPDATA%\praxi\praxi-bot`
and `%LOCALAPPDATA%\praxi\praxi-bot`) are kept unless you explicitly tick
**Delete application data**. Keys stored in
Credential Manager are not removed by the uninstaller; delete the `Praxi-Bot`
entries there if you want them gone.

## When something goes wrong

- The app shows a start-up problem screen with the reason and a button to copy
  diagnostics. Attach that text when you report a bug (it contains no keys).
- Logs: `%APPDATA%\praxi\praxi-bot\desktop-daemon.log` (previous run in
  `desktop-daemon.log.1`).
- "Address already in use": another program uses the daemon port. Close it or
  quit a stray `bot.exe` in Task Manager, then restart Praxi.
- Antivirus quarantined `bot.exe`: restore it and add the install folder as an
  exclusion; unsigned binaries are sometimes flagged.

See also [Troubleshooting](troubleshooting.md).
