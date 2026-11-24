<#
.SYNOPSIS
  Checks a downloaded Praxi-Bot Windows installer before you run it.

.DESCRIPTION
  Two checks, in plain words:
    1. Checksum (SHA-256): is the file complete and the same one the project listed in
       SHA256SUMS.txt? This catches a broken or swapped download.
    2. Build provenance: was this exact file built by the Praxi-Bot release workflow on GitHub?
       This is done with the GitHub CLI (`gh attestation verify`) and catches a file that was
       built or edited somewhere else.
  The installer is never started by this script. It only reads files.

  Nothing here is a code-signing check: the installer is not signed with a certificate yet, so
  Windows SmartScreen may still warn. Passing both checks is what tells you the file is the
  project's own build. See docs/guide/verify-download.md.

.PARAMETER InstallerPath
  The downloaded installer, for example .\Praxi-0.1.0-alpha.4-setup.exe

.PARAMETER Tag
  The release you downloaded from, for example v0.1.0-alpha.4. When given, the provenance check
  also insists that the file was built from exactly that tag. Recommended.

.PARAMETER ChecksumsPath
  SHA256SUMS.txt from the same release. Default: the file with that name next to the installer.

.PARAMETER BundlePath
  praxi-bot-windows.sigstore.json from the same release. Default: the file with that name next to
  the installer. Without it the check asks GitHub online, which needs `gh auth login` first.

.PARAMETER SkipProvenance
  Only check the checksum. The result then says clearly that provenance was NOT checked.

.PARAMETER Repository
  The repository the build must come from. Do not change this unless you run your own fork.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File .\verify-windows.ps1 `
    -InstallerPath .\Praxi-0.1.0-alpha.4-setup.exe -Tag v0.1.0-alpha.4

.NOTES
  Works in Windows PowerShell 5.1 and PowerShell 7. Exit code 0 means every check that was run
  passed; anything else means do not run the installer.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$InstallerPath,
    [string]$Tag,
    [string]$ChecksumsPath,
    [string]$BundlePath,
    [switch]$SkipProvenance,
    [string]$Repository = 'emreocell/Praxi-Bot'
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$MinGhVersion = [version]'2.80.0'
$DefaultBundleName = 'praxi-bot-windows.sigstore.json'
$SignerWorkflow = "github.com/$Repository/.github/workflows/installer.yml"

function Stop-Unsafe([string]$Message) {
    Write-Host ''
    Write-Host "NOT SAFE: $Message" -ForegroundColor Red
    Write-Host 'Do not run this installer. Download it again from the official release page.' -ForegroundColor Red
    exit 1
}

function Read-ChecksumLines([string]$Path) {
    # "<64 hex>  <name>" or "<64 hex> *<name>" per line; anything else is ignored.
    $entries = @()
    foreach ($line in [IO.File]::ReadAllLines($Path)) {
        if ($line -match '^([0-9a-fA-F]{64})\s+\*?(.+?)\s*$') {
            $entries += [pscustomobject]@{ Hash = $Matches[1].ToLowerInvariant(); Name = $Matches[2] }
        }
    }
    return $entries
}

if (-not (Test-Path -LiteralPath $InstallerPath -PathType Leaf)) {
    Stop-Unsafe "The file '$InstallerPath' does not exist."
}
$installer = Get-Item -LiteralPath $InstallerPath
$folder = $installer.DirectoryName

Write-Host "File:   $($installer.Name)"
$hash = (Get-FileHash -LiteralPath $installer.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Host "SHA-256: $hash"

# --- 1. Checksum -------------------------------------------------------------------------------
if (-not $ChecksumsPath) { $ChecksumsPath = Join-Path $folder 'SHA256SUMS.txt' }
if (-not (Test-Path -LiteralPath $ChecksumsPath -PathType Leaf)) {
    Stop-Unsafe "No checksum list found at '$ChecksumsPath'. Download SHA256SUMS.txt from the same release page and put it next to the installer (or pass -ChecksumsPath)."
}
$entries = Read-ChecksumLines $ChecksumsPath
if ($entries.Count -eq 0) { Stop-Unsafe "'$ChecksumsPath' does not look like a SHA-256 list." }

$byHash = @($entries | Where-Object { $_.Hash -eq $hash })
$byName = @($entries | Where-Object { $_.Name -eq $installer.Name })
if ($byHash.Count -gt 0) {
    Write-Host "Checksum: OK, matches '$($byHash[0].Name)' in SHA256SUMS.txt." -ForegroundColor Green
} elseif ($byName.Count -gt 0) {
    Stop-Unsafe "The checksum of '$($installer.Name)' is different from the one in SHA256SUMS.txt. The download is damaged or has been changed."
} else {
    Stop-Unsafe "This file is not listed in SHA256SUMS.txt. Check that the checksum list and the installer come from the same release."
}

# --- 2. Build provenance -----------------------------------------------------------------------
if ($SkipProvenance) {
    Write-Host ''
    Write-Host 'Provenance: NOT CHECKED (you asked to skip it). The checksum only proves the file matches the list, not who built it.' -ForegroundColor Yellow
    exit 0
}

$gh = Get-Command gh -ErrorAction SilentlyContinue
if (-not $gh) {
    Stop-Unsafe 'The GitHub CLI (gh) is needed for the provenance check. Install it from https://cli.github.com and run this again, or pass -SkipProvenance to check only the checksum.'
}
$versionText = (& gh --version | Select-Object -First 1)
if ($versionText -notmatch 'gh version (\d+\.\d+\.\d+)') {
    Stop-Unsafe "Could not read the GitHub CLI version from '$versionText'."
}
if ([version]$Matches[1] -lt $MinGhVersion) {
    Stop-Unsafe "The GitHub CLI $($Matches[1]) is too old; version $MinGhVersion or newer is needed (update from https://cli.github.com)."
}

if (-not $BundlePath) {
    $candidate = Join-Path $folder $DefaultBundleName
    if (Test-Path -LiteralPath $candidate -PathType Leaf) { $BundlePath = $candidate }
}

$verifyArgs = @(
    'attestation', 'verify', $installer.FullName,
    '--repo', $Repository,
    '--hostname', 'github.com',
    '--signer-workflow', $SignerWorkflow,
    '--cert-oidc-issuer', 'https://token.actions.githubusercontent.com',
    '--predicate-type', 'https://slsa.dev/provenance/v1',
    '--deny-self-hosted-runners'
)
if ($BundlePath) {
    if (-not (Test-Path -LiteralPath $BundlePath -PathType Leaf)) {
        Stop-Unsafe "The provenance bundle '$BundlePath' does not exist."
    }
    $verifyArgs += @('--bundle', $BundlePath)
    Write-Host "Provenance bundle: $BundlePath"
} else {
    Write-Host "No $DefaultBundleName next to the installer: asking GitHub online (needs 'gh auth login')."
}
if ($Tag) {
    $verifyArgs += @('--source-ref', "refs/tags/$Tag")
    Write-Host "Required source: tag $Tag"
} else {
    Write-Host 'No -Tag given: the file must come from this project''s release workflow, but not necessarily from one particular release.' -ForegroundColor Yellow
}

& gh @verifyArgs
if ($LASTEXITCODE -ne 0) {
    Stop-Unsafe 'The build provenance could NOT be verified. The file may not come from the Praxi-Bot release workflow.'
}

Write-Host ''
Write-Host 'Provenance: OK. GitHub confirms this exact file was built by the Praxi-Bot release workflow.' -ForegroundColor Green
Write-Host 'Both checks passed. (The installer is still not code-signed, so Windows SmartScreen may warn: that is expected.)' -ForegroundColor Green
exit 0
