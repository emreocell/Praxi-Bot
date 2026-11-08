param(
  [Parameter(Mandatory = $true)][string]$OutputDirectory,
  [string]$BaseUrl = 'http://localhost:5181/',
  [string]$BrowserPath = 'C:/Program Files/Google/Chrome/Application/chrome.exe'
)

$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$captureRun = [Guid]::NewGuid().ToString('N')
foreach ($theme in @('obsidian', 'cobalt', 'raven', 'clay')) {
  foreach ($screen in @('chat', 'settings', 'apps')) {
    $screenshotPath = Join-Path (Resolve-Path -LiteralPath $OutputDirectory) "$theme-$screen.png"
    $profilePath = Join-Path $env:TEMP "praxi-design-$captureRun-$theme-$screen"
    New-Item -ItemType Directory -Force -Path $profilePath | Out-Null
    $captureArguments = @(
      '--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check',
      ('"--user-data-dir=' + $profilePath + '"'),
      ('"--screenshot=' + $screenshotPath + '"'),
      '--window-size=1440,900', '--virtual-time-budget=5000',
      ('"' + $BaseUrl + '?mock=1&theme=' + $theme + '&screen=' + $screen + '"')
    )
    Start-Process -FilePath $BrowserPath -WindowStyle Hidden -ArgumentList $captureArguments -Wait -RedirectStandardError (Join-Path $profilePath 'chrome.log')
    if (!(Test-Path -LiteralPath $screenshotPath)) { throw "Screenshot missing: $screenshotPath" }
    Write-Output $screenshotPath
  }
}
