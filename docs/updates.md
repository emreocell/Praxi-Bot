# In-app updates and rollback

Settings > System > About can check for a new version, show what is new (in the
user's language when the release has it), install it with one click and go back
to the previous version if the new one does not start.

**Nothing is applied unless its signature verifies** against the public key built into
the app (minisign, checked by `tauri-plugin-updater` before the installer is launched), and the
signature must be bound to the exact version the feed announces (`requireSignedVersion`).
Without a key and an https feed in the app's configuration, the feature is off: the Updates
page says so, no request is made, nothing can be installed.

## How it fits together

```
owner key pair ── private key ──> GitHub secret TAURI_SIGNING_PRIVATE_KEY   (never in the repo)
               └─ public key  ──> apps/desktop/tauri.conf.json  plugins.updater.pubkey  (built into the app)

tag v0.1.0-alpha.3 ─> Windows Installer workflow
   builds the installer ─> signs it (tauri signer sign --app-version 0.1.0-alpha.3)
   ─> writes latest.json (scripts/release/update_manifest.py) and checks it offline
   ─> attests + attaches  *-setup.exe, *-setup.exe.sig, latest.json, ... to the GitHub release

Stable  feed: https://github.com/emreocell/Praxi-Bot/releases/latest/download/latest.json
              (GitHub's "latest release": the newest FINAL release)
Preview feed: https://raw.githubusercontent.com/emreocell/Praxi-Bot/update-feed/preview.json
              (the "Update feed" workflow copies a release's latest.json there, forward only)
```

Both addresses are in `apps/desktop/tauri.conf.json` (`plugins.updater.endpoints[0]` and
`praxiPreviewEndpoint`), so nothing has to be configured in GitHub besides the one secret.
`PRAXI_UPDATER_*` build-time variables still override them (only the maintainers' own builds can
set them; debug builds also read them from the environment for tests).

## What the maintainer must do (one time)

Run one command on the owner's computer, from a clone of the repository (needs `gh` signed in as
someone who can add repository secrets, `pnpm install` done once, and python):

```powershell
pwsh scripts/release/setup-updater-key.ps1
```

It creates the key pair in `~/.tauri/praxi-updater.key` (outside the repository), adds the private
key as the Actions secret `TAURI_SIGNING_PRIVATE_KEY`, and writes the public key into
`apps/desktop/tauri.conf.json`. Then:

1. **Back up `~/.tauri/praxi-updater.key`** (password manager or an offline copy). Losing it means
   existing installs can never update again; leaking it lets someone sign updates.
   Never commit it. The key has no password: GitHub stores it encrypted, and a password kept in the
   same place would add nothing. (To use one anyway, create the key by hand with
   `pnpm --dir apps/desktop tauri signer generate -p <password> -w <file>`, add it as the secret and
   also add `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`; the workflow passes both.)
2. Commit the one-line public-key change through a pull request (the script prints the commands).
   The public key is not secret.
3. Tag the next release as usual (docs/contributing/releasing.md).

From then on a **tag build refuses to run** without the secret or the public key, so no release can
ship with updates silently off. Builds for pull requests, the weekly build and manual runs
rehearse the whole path (sign, write `latest.json`, check it) with a throwaway key and never use the
real secret.

Replacing the public key later is possible (`-Replace`) but only makes sense together with a
release that every install can still verify, because installed copies only trust the key they were
built with.

## The feed (`latest.json`)

`scripts/release/update_manifest.py` writes and checks it (tests:
`scripts/tests/test_update_manifest.py`; format: Tauri static JSON):

```json
{
  "version": "0.2.0-alpha.1",
  "notes": "Default text\n<!-- lang: en -->\nWhat is new.\n<!-- lang: tr -->\nYenilikler.",
  "pub_date": "2026-12-01T00:00:00Z",
  "platforms": {
    "windows-x86_64": {
      "url": "https://github.com/emreocell/Praxi-Bot/releases/download/v0.2.0-alpha.1/Praxi-Bot_0.2.0-alpha.1_x64-setup.exe",
      "signature": "<contents of the .sig file>"
    }
  }
}
```

* `version` is the release tag without its `v` and equals the app's own version (the same string
  `bot host add` and the Linux updater use as `v<version>`).
* `pub_date` is the commit time of the tag, so a re-run writes the same bytes (a release asset is
  never replaced).
* Notes: put plain text in `docs/release-notes/<tag>.md` (optional `<!-- lang: xx -->` blocks, text
  before the first marker is the default) and the workflow uses it. Otherwise the feed carries a
  one-line "Praxi-Bot X is available" with a link to the release, in English, Turkish and German.
  Notes are shown as plain text.
* The workflow refuses to publish a feed the app would reject: wrong tag or version, a signature
  made with another key, a signature that is not bound to the version, an installer URL outside
  this repository's release assets.

`rollback.json` (optional, same format, `praxiRollbackEndpoint` in the configuration) is still
written by hand and points at the last release known to work. Without it "Go back to the previous
version" stays disabled.

Tagged installer builds use the complete release version (including `-alpha.2`, for example) in
Tauri and the signed update metadata. The updater and About use that same packaged version, so
successive previews of one numeric version can update correctly and each release gets its own
“What's new” dismissal.

### Publishing a release

* **Pre-release tag** (`v1.2.3-alpha.1`): *Windows Installer* attaches the files, including
  `latest.json` and the `.sig`, to the pre-release (if it exists) and *Update feed* then moves the
  Preview feed to it. Nothing else to do.
* **Final tag** (`v1.2.3`): attach the files by hand as for every final release (the complete set,
  `latest.json` and `*.sig` included). Stable users get it as soon as it is GitHub's latest release.
  Then run `gh workflow run update-feed.yml -f tag=v1.2.3` so Preview users get it too.
* The Preview feed moves forward only; asking for an older tag fails.
* GitHub's CDN may serve the previous `preview.json` for a few minutes.

## Channels and release notes

About shows the app version, source build ID, channel and last attempted check time.
Stable is the default and rejects prereleases even if the stable feed lists one.
Choosing Preview explicitly uses the Preview feed (`praxiPreviewEndpoint`), which may include
prereleases and stable releases. A missing or invalid Preview feed disables that choice;
it never falls back to Stable while claiming to check Preview. Both feeds use the same
built-in signing key. Release builds ignore environment overrides for every feed.

While only pre-releases exist (GitHub has no "latest release" yet) the Stable feed does not
exist, so a check on Stable says plainly that nothing is published for this channel, and About
points a preview build to the Preview channel. A preview build never moves to Stable by itself.

Checks happen only on request and do not download the installer. “Skip this version”
remembers the offered version across restarts; newer versions are still offered.
Changing channels clears the skipped version and any pending installer candidate.
A failed check also clears any previous candidate so it cannot install stale results.

After signature verification, notes are saved locally before the installer starts.
The existing “What's new” dialog shows those notes for the running version once,
after onboarding, and remembers dismissal in local settings. Notes survive health
confirmation, use the user's en/tr/de block with English fallback, and render as
inert plain text (including HTML and links). Bundled notes remain the fallback for
manual upgrades or releases without feed notes. No network request is needed to
show saved notes.

## How rollback works

Just before the installer runs, the app writes `update-state.json` in its data folder.
Every start of the new version without a healthy run (60 seconds up) is counted:

* 2nd failed start: Settings > System > About shows "Go back to the previous version";
* 3rd failed start: the rollback recommendation remains visible. The user must click
  “Go back to the previous version”; no installer, including rollback, runs automatically.

Going back downloads the installer named by `rollback.json`, verifies its signature with
the same key, and only accepts a version **older** than the running one. A healthy run
clears the bookkeeping.

## Limits

* Windows installer (NSIS) only; other platforms are not wired up.
* The bundled `requireSignedVersion` option makes the app refuse an installer whose
  signature was made for a different version (downgrade replay). It needs releases built
  with a current Tauri CLI; re-sign older releases before listing them in a feed.
* Rollback needs the previous installer to stay available at the URL in `rollback.json`.
