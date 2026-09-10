[CmdletBinding()]
param(
    [string]$InstallDirectory = (Join-Path $env:LOCALAPPDATA 'Programs/speakeasy'),
    [string]$ConfigDirectory = (Join-Path $env:APPDATA 'speakeasy'),
    [switch]$Launch,
    [switch]$DesktopShortcut
)

$ErrorActionPreference = 'Stop'
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$installRoot = [IO.Path]::GetFullPath($InstallDirectory)
$configRoot = [IO.Path]::GetFullPath($ConfigDirectory)
& (Join-Path $PSScriptRoot 'publish.ps1')
$outputRoot = Join-Path $repoRoot 'artifacts/publish'
$installedExecutable = Join-Path $installRoot 'speakeasy.exe'
$running = @(Get-Process -Name speakeasy -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $installedExecutable })
if ($running.Count -gt 0) {
    Start-Process -FilePath $installedExecutable -ArgumentList '--quit' -WindowStyle Hidden -Wait
    foreach ($instance in $running) {
        if (-not $instance.WaitForExit(10000)) { throw 'Close speakeasy before installing this update.' }
    }
}
New-Item -ItemType Directory -Path $installRoot -Force | Out-Null
Copy-Item -Path (Join-Path $outputRoot '*') -Destination $installRoot -Recurse -Force
$configRoot | Set-Content -LiteralPath (Join-Path $installRoot 'config-location.txt') -Encoding utf8

$shellObject = New-Object -ComObject WScript.Shell
$shortcutPath = Join-Path ([Environment]::GetFolderPath('Programs')) 'speakeasy.lnk'
$shortcut = $shellObject.CreateShortcut($shortcutPath)
$shortcut.TargetPath = Join-Path $installRoot 'speakeasy.exe'
$shortcut.Arguments = '--config-dir "' + $configRoot.TrimEnd('\') + '" --settings'
$shortcut.WorkingDirectory = $installRoot
$shortcut.Description = 'Voice dictation, wherever your cursor is.'
$shortcut.Save()
if ($DesktopShortcut) {
    $desktopLink = $shellObject.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Desktop')) 'speakeasy.lnk'))
    $desktopLink.TargetPath = $shortcut.TargetPath
    $desktopLink.Arguments = $shortcut.Arguments
    $desktopLink.WorkingDirectory = $installRoot
    $desktopLink.Description = $shortcut.Description
    $desktopLink.Save()
}
[Runtime.InteropServices.Marshal]::ReleaseComObject($shellObject) | Out-Null

Write-Host "Installed to $installRoot"
Write-Host 'Open speakeasy from the Start menu. Launch at login is available in its tray menu.'
if ($Launch) {
    # Let the desktop shell launch the app independently of a packaged development host.
    $desktopShell = New-Object -ComObject Shell.Application
    $desktopShell.ShellExecute((Join-Path $installRoot 'speakeasy.exe'), ('--config-dir "' + $configRoot.TrimEnd('\') + '"'), $installRoot, 'open', 1)
    [Runtime.InteropServices.Marshal]::ReleaseComObject($desktopShell) | Out-Null
}
