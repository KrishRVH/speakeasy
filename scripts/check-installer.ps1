# Disposable CI runners only: install, upgrade, and uninstall without opening the app or devices.
param([Parameter(Mandatory = $true)][string]$Installer)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or !$env:RUNNER_TEMP) {
    throw 'Installer acceptance requires a disposable GitHub Actions runner.'
}
$Installer = (Resolve-Path $Installer).Path
$work = Join-Path $env:RUNNER_TEMP ('speakeasy-installer-' + [guid]::NewGuid())
$app = Join-Path $work 'app'
$data = Join-Path $env:LOCALAPPDATA 'speakeasy'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$existing = Get-ItemPropertyValue -Path $runKey -Name 'Speakeasy' -ErrorAction SilentlyContinue
if ($existing -or (Test-Path $data)) { throw 'The runner already has Speakeasy state; refusing to change it.' }
New-Item -ItemType Directory -Path $work, $data -Force | Out-Null
$fixture = Join-Path $data 'installer-acceptance.txt'
Set-Content -LiteralPath $fixture -Value 'Settings, models, and Shows belong to the user.' -NoNewline
$before = (Get-FileHash -LiteralPath $fixture).Hash
function Invoke-Setup {
    $process = Start-Process -FilePath $Installer -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/DIR=`"$app`"" -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Installer failed with exit code $($process.ExitCode)." }
}
function Invoke-Uninstall {
    $process = Start-Process -FilePath (Join-Path $app 'unins000.exe') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Uninstall failed with exit code $($process.ExitCode)." }
    if (Test-Path (Join-Path $app 'speakeasy.exe')) { throw 'Uninstall retained the application executable.' }
    if ((Get-FileHash -LiteralPath $fixture).Hash -ne $before) { throw 'Uninstall changed user data.' }
}
try {
    Invoke-Setup
    $version = & (Join-Path $app 'speakeasy-cli.exe') --version
    if ($LASTEXITCODE -ne 0 -or $version -notmatch '^speakeasy \d+\.\d+\.\d+$') { throw 'Installed CLI failed.' }
    Invoke-Setup
    if ((Get-FileHash -LiteralPath $fixture).Hash -ne $before) { throw 'Upgrade changed user data.' }
    if (!(Test-Path $runKey)) { New-Item -Path $runKey -Force | Out-Null }
    New-ItemProperty -Path $runKey -Name 'Speakeasy' -Value ('"' + (Join-Path $app 'speakeasy.exe') + '"') -PropertyType String -Force | Out-Null
    Invoke-Uninstall
    if (Get-ItemPropertyValue -Path $runKey -Name 'Speakeasy' -ErrorAction SilentlyContinue) { throw 'Uninstall left its sign-in command.' }
    Invoke-Setup
    $portable = '"C:\portable\speakeasy.exe"'
    New-ItemProperty -Path $runKey -Name 'Speakeasy' -Value $portable -PropertyType String -Force | Out-Null
    Invoke-Uninstall
    if ((Get-ItemPropertyValue -Path $runKey -Name 'Speakeasy') -ne $portable) { throw 'Uninstall removed another copy''s sign-in command.' }
    Write-Host 'Per-user install, upgrade, CLI launch, and uninstall passed; user data and other copies are preserved.'
} finally {
    if (Test-Path (Join-Path $app 'unins000.exe')) { Invoke-Uninstall }
    Remove-ItemProperty -Path $runKey -Name 'Speakeasy' -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $fixture -Force
    # Remove only our empty data directory, never a recursively populated user directory.
    [IO.Directory]::Delete($data, $false)
    Remove-Item -LiteralPath $work -Force -Recurse
}
