# In-app updates and rollback

Settings > System > Updates & version can check for a new version, show what is new (in the
user's language when the release has it), install it with one click and go back
to the previous version if the new one does not start.

**Nothing is applied unless its signature verifies** against the public key built into
the app (minisign, checked by `tauri-plugin-updater` before the installer is launched).
Without a key, an https feed and a build that was given them, the feature is off: the
Updates page says so, no request is made, nothing can be installed.

## What the maintainer must configure (one time)

1. Create a signing key pair: `pnpm --dir apps/desktop tauri signer generate -w ~/.tauri/praxi-updater.key`.
   Keep the private key and its password secret and backed up; losing it means existing
   installs can never update again.
2. GitHub > Settings > Secrets and variables > Actions:
   * secret `TAURI_SIGNING_PRIVATE_KEY` (contents of the private key file),
     secret `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`;
   * variable `PRAXI_UPDATER_PUBKEY` (contents of the `.pub` file),
     variable `PRAXI_UPDATER_ENDPOINT` (https URL of `latest.json`, e.g. the `latest.json`
     asset of the latest GitHub release),
     variable `PRAXI_UPDATER_ROLLBACK_ENDPOINT` (https URL of `rollback.json`, optional).
3. `.github/workflows/installer.yml` then also produces `*-setup.exe` + `*-setup.exe.sig`.
   Publish them with the release and write the two feeds (Tauri static JSON format):

   ```json
   {
     "version": "0.2.0",
     "notes": "Default text\n<!-- lang: en -->\nWhat is new.\n<!-- lang: tr -->\nYenilikler.\n<!-- lang: de -->\nNeuigkeiten.",
     "pub_date": "2026-12-01T00:00:00Z",
     "platforms": {
       "windows-x86_64": { "url": "https://.../Praxi-Bot_0.2.0_x64-setup.exe", "signature": "<contents of the .sig file>" }
     }
   }
   ```

   `latest.json` points at the newest release; `rollback.json` has the same format and
   points at the last release known to work (older than the newest one). Notes may hold
   one `<!-- lang: xx -->` block per language; text before the first marker is the default.
   Notes are shown as plain text.

## How rollback works

Just before the installer runs, the app writes `update-state.json` in its data folder.
Every start of the new version without a healthy run (60 seconds up) is counted:

* 2nd failed start: Settings > System > Updates & version shows "Go back to the previous version";
* 3rd failed start: the app goes back by itself (the screen may not even load).

Going back downloads the installer named by `rollback.json`, verifies its signature with
the same key, and only accepts a version **older** than the running one. A healthy run
clears the bookkeeping.

## Limits

* Windows installer (NSIS) only; other platforms are not wired up.
* The bundled `requireSignedVersion` option makes the app refuse an installer whose
  signature was made for a different version (downgrade replay). It needs releases built
  with a current Tauri CLI; re-sign older releases before listing them in a feed.
* Rollback needs the previous installer to stay available at the URL in `rollback.json`.
