<#
.SYNOPSIS
  One-time setup of the key that signs Praxi-Bot's in-app updates.

.DESCRIPTION
  Run this ONCE, on the owner's computer, from anywhere inside a clone of the repository:

      pwsh scripts/release/setup-updater-key.ps1

  It does three things and prints what is left to do:

    1. creates the signing key pair (the PRIVATE key stays in $KeyPath, outside the repository);
    2. adds the private key to the repository as the GitHub Actions secret TAURI_SIGNING_PRIVATE_KEY
       (it is sent straight from the file to GitHub; it is never printed or put on a command line);
    3. writes the PUBLIC key into apps/desktop/tauri.conf.json (plugins.updater.pubkey), which you
       then commit through a normal pull request. That public key is built into the app; it is not secret.

  Needs: git, the GitHub CLI (gh, signed in as someone who may add repository secrets), pnpm
  (`pnpm install` done once in this clone) and python. It never overwrites an existing private key
  and refuses to replace a public key that is already in the configuration: replacing it would
  stop every installed copy from updating (use -Replace only if that is what you want).

  Back the private key file up (a password manager works). If it is lost, existing installs can
  never update again. It has no password: it protects nothing beyond the file permissions, so keep
  the file private and out of the repository. See docs/updates.md.
#>
[CmdletBinding()]
param(
  [string]$Repo = 'emreocell/Praxi-Bot',
  [string]$KeyPath = (Join-Path $HOME '.tauri/praxi-updater.key'),
  [switch]$Replace
)

$ErrorActionPreference = 'Stop'
$root = (git -C $PSScriptRoot rev-parse --show-toplevel).Trim()
$config = Join-Path $root 'apps/desktop/tauri.conf.json'
$tool = Join-Path $root 'scripts/release/update_manifest.py'
$secretName = 'TAURI_SIGNING_PRIVATE_KEY'

foreach ($needed in 'gh', 'pnpm', 'python') {
  if (-not (Get-Command $needed -ErrorAction SilentlyContinue)) { throw "$needed is not installed or not on PATH." }
}
gh auth status *> $null
if ($LASTEXITCODE -ne 0) { throw 'The GitHub CLI is not signed in. Run: gh auth login' }

# Stop before doing anything if the configuration already has a public key.
$existing = (Get-Content -Raw -LiteralPath $config | ConvertFrom-Json).plugins.updater.pubkey
if ($existing -and -not $Replace) {
  throw 'apps/desktop/tauri.conf.json already has an updater public key, so the key was set up before. Nothing was changed. (Replacing it would stop all installed copies from updating; add -Replace only if you are sure.)'
}

$pubPath = "$KeyPath.pub"
if (Test-Path -LiteralPath $KeyPath) {
  if (-not (Test-Path -LiteralPath $pubPath)) { throw "$KeyPath exists but $pubPath does not. Nothing was changed." }
  Write-Host "Using the existing key at $KeyPath (it is not overwritten)."
} else {
  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $KeyPath) | Out-Null
  pnpm --dir (Join-Path $root 'apps/desktop') tauri signer generate --ci -w $KeyPath | Out-Null
  if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $KeyPath) -or -not (Test-Path -LiteralPath $pubPath)) {
    throw 'The signing key could not be created.'
  }
  Write-Host "Created the signing key pair at $KeyPath (+ .pub)."
}

# Send the private key from the file to GitHub on stdin (never on the command line, never printed).
$gh = (Get-Command gh).Source
$run = Start-Process -FilePath $gh -ArgumentList 'secret', 'set', $secretName, '--repo', $Repo, '--app', 'actions' `
  -RedirectStandardInput $KeyPath -NoNewWindow -Wait -PassThru
if ($run.ExitCode -ne 0) {
  throw "GitHub refused the secret (exit code $($run.ExitCode)). Nothing was written to the configuration: run this script again once gh can add secrets, or add $secretName by hand from $KeyPath."
}

# Last step: put the public key into the configuration. (The secret goes first, so a failure there
# leaves the configuration untouched and this script can simply be run again.)
$writeArgs = @('write-pubkey', '--config', $config, '--pubkey-file', $pubPath)
if ($Replace) { $writeArgs += '--replace' }
python -B $tool @writeArgs
if ($LASTEXITCODE -ne 0) { throw 'The public key could not be written to the configuration. The GitHub secret IS set; fix the problem above and run this script again.' }

Write-Host ''
Write-Host "Done: GitHub Actions secret $secretName is set on $Repo." -ForegroundColor Green
Write-Host ''
Write-Host 'Two things are left:'
Write-Host "  1. BACK UP $KeyPath now (password manager or an offline copy). Do not commit it."
Write-Host '  2. Commit the public key through a pull request, then tag the next release:'
Write-Host '       git switch -c chore/updater-public-key'
Write-Host '       git add apps/desktop/tauri.conf.json'
Write-Host '       git commit -m "chore(updater): add the update public key"'
Write-Host '       gh pr create --fill'
Write-Host 'Release builds refuse to run without the secret and the public key, so no installer can ship with updates silently off.'
