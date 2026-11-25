# Check that your Windows download is genuine

Before you run an installer you downloaded, you can check two things:

1. **The file is complete and is the one the project listed.** (a SHA-256 checksum)
2. **GitHub confirms the file was built by the Praxi-Bot release workflow**, from the project's
   own source code, and not on someone else's computer. (a build provenance "attestation")

Both checks take a minute. You do not need to be a programmer, and checking never starts the
installer.

> **Not signed yet.** The installer has no code-signing certificate, so Windows SmartScreen may
> still say "Windows protected your PC" (see [Installing on Windows](install-windows.md#before-you-start)).
> The checks on this page do not remove that warning. They tell you whether the file really is
> the project's own build, which is the thing SmartScreen cannot tell you today.

## What each release contains

Every release built after this page was added lists these files next to the installer. Download
the ones you need into the same folder.

| File | What it is |
| --- | --- |
| `Praxi-...-setup.exe` | The Windows installer. |
| `SHA256SUMS.txt` | The expected SHA-256 of every file below and above. |
| `praxi-bot-windows.sigstore.json` | The signed proof of where the files were built (the attestation). |
| `verify-windows.ps1` | A script that runs both checks for you (optional). |
| `bot-x86_64-pc-windows-msvc.exe` | The `bot` program that is inside the installer (daemon and command line). |
| `sbom-rust-cli.cdx.json`, `sbom-rust-desktop.cdx.json`, `sbom-ui.cdx.json` | Lists of every library the program is made of (see [the parts list](#the-parts-list-sbom)). |
| `build-info.txt` | The commit, the tools and the date the files were built with. |

Releases published before this feature existed (the earliest alphas) have none of these files.
For those you can only compute the file's hash yourself and ask the project to confirm it.

## The easy way: one script

Open **PowerShell** in the folder with the downloaded files and run (replace the file name and
the tag with yours):

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\verify-windows.ps1 `
  -InstallerPath .\Praxi-0.1.0-alpha.4-setup.exe -Tag v0.1.0-alpha.4
```

It prints `Checksum: OK` and `Provenance: OK` when everything is fine. If anything is wrong it
says `NOT SAFE`, stops, and you should not run the installer. The provenance check needs the
[GitHub CLI](https://cli.github.com) (`gh`, version 2.80 or newer). Without it you can run only
the checksum with `-SkipProvenance`; the script then says plainly that provenance was not checked.

The script is also in the repository: [`scripts/release/verify-windows.ps1`](../../scripts/release/verify-windows.ps1).
It only reads files and runs `gh attestation verify`.

## The manual way

### 1. The checksum

```powershell
Get-FileHash .\Praxi-0.1.0-alpha.4-setup.exe -Algorithm SHA256
```

Open `SHA256SUMS.txt` and compare the long hexadecimal text with the line for your file. Upper
or lower case does not matter. They must be identical, every character. If they are not, the
download is damaged or has been changed: delete it and download it again.

A checksum on its own only protects against a broken download. Anyone who can replace the
installer on a website can replace the checksum next to it. That is why the second check exists.

### 2. The provenance

Install the [GitHub CLI](https://cli.github.com) (version 2.80 or newer), then:

```powershell
gh attestation verify .\Praxi-0.1.0-alpha.4-setup.exe `
  --bundle .\praxi-bot-windows.sigstore.json `
  --repo emreocell/Praxi-Bot `
  --signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/installer.yml `
  --source-ref refs/tags/v0.1.0-alpha.4 `
  --deny-self-hosted-runners
```

`Verification succeeded` means: GitHub's signing service recorded that exactly
this file (the check is on its SHA-256, so the file name does not matter) was produced by the
`installer.yml` workflow of `emreocell/Praxi-Bot`, on GitHub's own computers, from the tag you
named. Anything else prints an error and a non-zero exit code.

- **Without the bundle file:** leave out the `--bundle` line. `gh` then asks GitHub directly and
  needs you to sign in once with `gh auth login`.
- **Another file of the release** (the sidecar, an SBOM, `SHA256SUMS.txt`) is checked the same
  way: put its path first and keep the other options.
- **If you only know the project's newest build on `main`,** not a tag, use
  `--source-ref refs/heads/main`. Use a tag whenever you have one: it is the stricter check.

## Is the program on my PC the attested one?

The installer puts the `bot` program in `%LOCALAPPDATA%\Programs\Praxi-Bot`. It is the very same
file as `bot-x86_64-pc-windows-msvc.exe` in the release (the build checks this on every run), so
you can compare:

```powershell
Get-FileHash "$env:LOCALAPPDATA\Programs\Praxi-Bot\bot.exe" -Algorithm SHA256
```

The result must match the `bot-x86_64-pc-windows-msvc.exe` line in `SHA256SUMS.txt`. (A per-user
install is assumed; an install "for everyone" lives under `C:\Program Files`.)

## The parts list (SBOM)

An SBOM ("software bill of materials") is the parts list of the program: every library that went
into it, with its version and licence. The release has three, in the
[CycloneDX](https://cyclonedx.org) format that security tools can read:

- `sbom-rust-cli.cdx.json`: what the `bot` program (daemon and command line) is built from.
- `sbom-rust-desktop.cdx.json`: what the desktop window shell is built from.
- `sbom-ui.cdx.json`: the JavaScript packages the app's screens need when it runs.

You do not need them to use Praxi. They are for people who want to check whether a library they
care about, or one with a newly found security problem, is inside. Tools such as
[Grype](https://github.com/anchore/grype) or [OSV-Scanner](https://google.github.io/osv-scanner/)
accept these files directly. They list what Praxi was built from; they are not a promise that
nothing in the list has a flaw. Development-only tools are left out, and a few build-time
dependencies of the Rust parts may be listed even though they are not part of the final program.

## What these checks do not tell you

- **They do not prove the source code is harmless.** They prove the file was built from the
  project's own repository by its own workflow. The code is open source: read it, or wait for
  other people to.
- **They are only as strong as the repository's settings.** If someone with write access to the
  project builds from a branch or edits the workflow, the provenance is still "valid" for that
  build. This is why the stricter `--source-ref refs/tags/<tag>` form is recommended and why final
  release tags are protected on the Linux side; see [Releasing](../contributing/releasing.md).
- **They are not a code-signing certificate.** There is no publisher name for Windows to show.
  A certificate is planned; until then this page is the way to check.

## Can I rebuild it myself?

You can build the same source, but **do not expect the same file**. The builds are *not*
bit-for-bit reproducible today, and this has not been tested on a second machine. What the project
does pin:

- the exact source commit (named in `build-info.txt` and in the attestation),
- the Rust libraries: `Cargo.lock` is used with `--locked`, so a stale lock file fails the build,
- the JavaScript packages: `pnpm install --frozen-lockfile`,
- every GitHub Action the workflow uses, by its full commit hash,
- the SBOM tool (`cargo-cyclonedx`) version.

What is **not** pinned, and can change the bytes: the Rust compiler (the repository asks for the
`stable` channel, so the version moves), Node and pnpm patch versions, the `windows-latest`
GitHub runner image, and the NSIS installer tooling that Tauri downloads. `build-info.txt` records
the versions and the lock-file hashes that were actually used, so a rebuild can at least be
compared like for like.

To rebuild: check out the tag, follow the build steps in
[`installer.yml`](../../.github/workflows/installer.yml) (they are plain commands), and compare the
two builds' SBOMs and the `bot.exe` behaviour (`bot --version`). A different hash is normal; the
*attestation* (not a rebuild) is what ties a downloaded file to the project's workflow.

## If a check fails

Do not run the installer. Download it again from the
[releases page](https://github.com/emreocell/Praxi-Bot/releases), only from the repository
`emreocell/Praxi-Bot`, and check again. If it still fails, please
[open an issue](https://github.com/emreocell/Praxi-Bot/issues) and say which release and which
check failed; for a suspected attack, follow [SECURITY.md](../../SECURITY.md) instead of a public
issue.
