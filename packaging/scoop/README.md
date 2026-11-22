# Scoop manifest template

`praxi-bot.json` is a [Scoop](https://scoop.sh/) manifest for the Windows installer. Like the
[winget templates](../winget/README.md), it is not published to any bucket yet, and no workflow
reads it. It is filled in for `v0.1.0-alpha.3` so it validates and installs as is; `checkver` and
`autoupdate` move it to newer releases.

## How it installs

The manifest does not run the installer. It downloads the release's
`*-setup.exe`, extracts it with 7-Zip (`#/dl.7z`), and removes the NSIS leftovers (`$PLUGINSDIR`,
`uninstall.exe`). What remains is `bot-desktop.exe` and the `bot.exe` daemon sidecar in Scoop's app
folder, plus a Start-menu shortcut named Praxi-Bot.

Differences from the setup `.exe` (none of them touch user data):

- The installer's hooks don't run: no `/AUTOSTART=1` option (autostart can still be turned on in
  Settings) and no icon-cache refresh.
- The installer downloads the WebView2 Runtime when it is missing; Scoop does not. Windows 11 and
  updated Windows 10 already have it. Elsewhere, install the
  [Evergreen WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/) or use the
  setup `.exe`. The manifest's `notes` say this after `scoop install`.
- Updates come from `scoop update praxi-bot`, not from running a newer setup `.exe`.
- `scoop uninstall praxi-bot` stops `bot-desktop.exe` and `bot.exe` (a `pre_uninstall` step) and
  removes the app folder only. Data in `%APPDATA%\praxi\praxi-bot` and
  `%LOCALAPPDATA%\praxi\praxi-bot` is kept, as with the uninstaller's default. The installer does
  not manage Credential Manager entries either way. If you turned autostart on in Settings, the
  entry in the Windows Run key (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, value
  `Praxi-Bot`) is not removed by `scoop uninstall`, so turn autostart off in Settings before
  uninstalling.

## Updating to a new release

`checkver` reads the GitHub releases API, so it also finds pre-releases, and takes the newest
`*-setup.exe` asset. The version comes from the tag, not the file name, so both
`Praxi-<version>-<commit>-setup.exe` and the unrenamed build output
`Praxi-Bot_<version>_x64-setup.exe` work; `autoupdate` reuses the matched file name. There is no
`.sha256` asset, so Scoop downloads the installer to compute the hash.

From a clone of [Scoop](https://github.com/ScoopInstaller/Scoop), on Windows:

```powershell
.\bin\checkver.ps1 -App praxi-bot -Dir <path to packaging\scoop> -Update
```

This writes the new `version`, `url` and `hash` into the manifest.

## Validating

- Schema: check the manifest against Scoop's
  [`schema.json`](https://raw.githubusercontent.com/ScoopInstaller/Scoop/master/schema.json), for
  example `uvx check-jsonschema --schemafile schema.json praxi-bot.json`.
- Install test: `scoop install .\packaging\scoop\praxi-bot.json`, start Praxi-Bot from the Start
  menu, then `scoop uninstall praxi-bot`.

## Publishing

Scoop reads manifests from buckets (git repositories). Two options, both the owner's decision:

- **Own bucket:** a repository such as `emreocell/scoop-praxi` with `bucket/praxi-bot.json`. Users
  run `scoop bucket add praxi https://github.com/emreocell/scoop-praxi` and
  `scoop install praxi/praxi-bot`. Scoop's
  [bucket template](https://github.com/ScoopInstaller/BucketTemplate) includes a scheduled
  workflow that runs `checkver -Update` and commits new versions.
- **Extras bucket:** a pull request to [ScoopInstaller/Extras](https://github.com/ScoopInstaller/Extras),
  whose bots then update the manifest. Extras prefers stable releases, so this fits better after
  the open winget questions (alpha releases, code signing) are settled.
