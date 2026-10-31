# Disposable CI runners only: install, upgrade, and uninstall; demo opens no devices.
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
function Get-SignInCommand {
    # Get-ItemPropertyValue throws for a missing value even with SilentlyContinue.
    return [Microsoft.Win32.Registry]::GetValue('HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run', 'Speakeasy', $null)
}
$existing = Get-SignInCommand
if ($null -ne $existing -or (Test-Path $data)) { throw 'The runner already has Speakeasy state; refusing to change it.' }
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
function Check-RunningAppProtection {
    $previewConfig = Join-Path $work 'preview.json'
    [IO.File]::WriteAllText($previewConfig, 'demo preserves this file')
    $image = Join-Path $app 'speakeasy.exe'
    $imageHash = (Get-FileHash -LiteralPath $image).Hash
    $preview = Start-Process -FilePath $image -ArgumentList '--demo-tray', "--config `"$previewConfig`"" -PassThru
    $checked = $false
    try {
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        do {
            $preview.Refresh()
            if ($preview.HasExited) { throw 'The installed demo exited before upgrade acceptance.' }
            if ([DateTime]::UtcNow -ge $deadline) { throw 'The installed demo did not open its window.' }
            Start-Sleep -Milliseconds 100
        } while ($preview.MainWindowHandle -eq [IntPtr]::Zero)
        $blocked = Start-Process -FilePath $Installer -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/DIR=`"$app`"" -Wait -PassThru
        if ($blocked.ExitCode -ne 7) { throw "Upgrade did not refuse the running app before installation (exit $($blocked.ExitCode))." }
        $blocked = Start-Process -FilePath (Join-Path $app 'unins000.exe') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
        if ($blocked.ExitCode -eq 0) { throw 'Uninstall accepted a running app.' }
        foreach ($file in 'unins000.exe', 'speakeasy-cli.exe') {
            if (!(Test-Path (Join-Path $app $file))) { throw "Blocked uninstall removed $file." }
        }
        $preview.Refresh()
        if ($preview.HasExited -or (Get-FileHash -LiteralPath $image).Hash -ne $imageHash) { throw 'Blocked maintenance disturbed the running app.' }
        if ([IO.File]::ReadAllText($previewConfig) -ne 'demo preserves this file') { throw 'Demo changed its config.' }
        $checked = $true
    } finally {
        # The owned demo opens no devices and has no saves; end only its main message loop.
        try {
            if (!$preview.HasExited) {
                [uint32]$owner = 0
                $thread = [InstallerPreview]::GetWindowThreadProcessId($preview.MainWindowHandle, [ref]$owner)
                if ($owner -ne $preview.Id -or ![InstallerPreview]::PostThreadMessage($thread, 0x0012, [IntPtr]::Zero, [IntPtr]::Zero)) {
                    $preview.Kill()
                    [void]$preview.WaitForExit(10000)
                    throw 'Could not close the owned preview.'
                }
                if (!$preview.WaitForExit(10000)) {
                    $preview.Kill()
                    [void]$preview.WaitForExit(10000)
                    throw 'The owned preview did not exit.'
                }
            }
        } catch {
            if ($checked) { throw }
            Write-Warning "Preview cleanup after an earlier failure: $_"
        } finally {
            $preview.Dispose()
        }
    }
}
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class InstallerPreview {
    [DllImport("user32")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32")] public static extern bool PostThreadMessage(uint thread, uint message, IntPtr wparam, IntPtr lparam);
}
'@
try {
    Invoke-Setup
    $version = & (Join-Path $app 'speakeasy-cli.exe') --version
    if ($LASTEXITCODE -ne 0 -or $version -notmatch '^speakeasy \d+\.\d+\.\d+$') { throw 'Installed CLI failed.' }
    Check-RunningAppProtection
    Invoke-Setup
    if ((Get-FileHash -LiteralPath $fixture).Hash -ne $before) { throw 'Upgrade changed user data.' }
    if (!(Test-Path $runKey)) { New-Item -Path $runKey -Force | Out-Null }
    New-ItemProperty -Path $runKey -Name 'Speakeasy' -Value ('"' + (Join-Path $app 'speakeasy.exe') + '"') -PropertyType String -Force | Out-Null
    Invoke-Uninstall
    if ($null -ne (Get-SignInCommand)) { throw 'Uninstall left its sign-in command.' }
    Invoke-Setup
    $portable = '"C:\portable\speakeasy.exe"'
    New-ItemProperty -Path $runKey -Name 'Speakeasy' -Value $portable -PropertyType String -Force | Out-Null
    Invoke-Uninstall
    if ((Get-SignInCommand) -ne $portable) { throw 'Uninstall removed another copy''s sign-in command.' }
    Write-Host 'Per-user install, running-app protection, upgrade, CLI launch, and uninstall passed; user data and other copies are preserved.'
} finally {
    if (Test-Path (Join-Path $app 'unins000.exe')) { Invoke-Uninstall }
    Remove-ItemProperty -Path $runKey -Name 'Speakeasy' -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $fixture -Force
    # Remove only our empty data directory, never a recursively populated user directory.
    [IO.Directory]::Delete($data, $false)
    Remove-Item -LiteralPath $work -Force -Recurse
}
