# winget manifest templates

These files are templates for a [winget](https://learn.microsoft.com/windows/package-manager/)
package of the Windows installer. They are not submitted anywhere, and no workflow reads them yet.
The open decisions are listed at the end; tracking issue:
[#178](https://github.com/emreocell/Praxi-Bot/issues/178).

| File | Manifest type (schema 1.10.0) |
| --- | --- |
| `PraxiBot.Praxi-Bot.yaml` | version |
| `PraxiBot.Praxi-Bot.installer.yaml` | installer |
| `PraxiBot.Praxi-Bot.locale.en-US.yaml` | defaultLocale |

The installer manifest describes the per-user NSIS installer built by
[`installer.yml`](../../.github/workflows/installer.yml) (`installMode: currentUser` in
[`tauri.conf.json`](../../apps/desktop/tauri.conf.json)): `InstallerType: nullsoft`,
`Scope: user`, `Architecture: x64`. No `MinimumOSVersion` is set, because the installer does not
check the Windows version.

## Filling in a release

Copy the three `.yaml` files to a scratch folder (without this README, which winget would also try
to read) and replace the placeholders:

| Placeholder | Value | Example (`v0.1.0-alpha.3`) |
| --- | --- | --- |
| `<VERSION>` | Release tag without the leading `v` | `0.1.0-alpha.3` |
| `<INSTALLER_URL>` | Download URL of the `*-setup.exe` release asset | `https://github.com/emreocell/Praxi-Bot/releases/download/v0.1.0-alpha.3/Praxi-0.1.0-alpha.3-2fac862c-setup.exe` |
| `<INSTALLER_SHA256>` | SHA-256 of that exact file, upper or lower case hex | output of `Get-FileHash <file> -Algorithm SHA256` |

`<VERSION>` also appears in `ReleaseNotesUrl`. Compute the hash from the file downloaded from the
release, not from a local build. Remove the template header comments before submitting.

## Validating

On Windows with winget installed:

```powershell
winget validate --manifest <folder with the three filled-in files>
```

To test the install itself, enable local manifests once
(`winget settings --enable LocalManifestFiles`, as administrator) and run
`winget install --manifest <folder>`; then uninstall with `winget uninstall Praxi-Bot`.

## Submitting to winget-pkgs

Packages are published by a pull request to
[microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs), with the files under
`manifests/p/PraxiBot/Praxi-Bot/<VERSION>/`. Two ways:

- **By hand:** fork winget-pkgs from the account that owns the identifier, add the filled-in
  folder and open a pull request. The winget-pkgs bots validate it and install it in a sandbox.
- **With [wingetcreate](https://github.com/microsoft/winget-create):**
  `wingetcreate update PraxiBot.Praxi-Bot --version <VERSION> --urls <INSTALLER_URL> --submit`
  updates an already published package and opens the pull request. It needs a GitHub token from
  that account (`--token`), which is why automating it is the owner's decision.

## Open decisions (blocked until decided)

- **Package identifier owner and token.** `PraxiBot.Praxi-Bot` is a proposed value. Who owns it on
  winget-pkgs, and whether a release step may submit updates with a token from the owner's
  account, is for the project owner to decide.
- **Stable installer asset name and `.sha256`.** The current asset name contains the commit hash
  (`Praxi-0.1.0-alpha.3-2fac862c-setup.exe`) and has no `.sha256` next to it. A name such as
  `Praxi-<version>-x64-setup.exe` plus a `.sha256` file would make the URL and hash easy to fill
  in, but changes the release job and needs the owner's confirmation first.
- **Alphas.** Whether pre-releases are published to winget or only stable tags.
- **Code signing.** The installer is unsigned today (SmartScreen warns). Whether to wait for a
  signing certificate before the first submission.

Until these are decided, nothing in the release workflow uses these files.
