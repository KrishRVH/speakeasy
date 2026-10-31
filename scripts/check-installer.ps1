# Disposable CI runners only: install, upgrade, and uninstall; demo opens no devices.
param(
    [Parameter(Mandatory = $true)][string]$Installer,
    [string]$PreviousArchive
)
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
$configuration = Join-Path $work 'selected Ω config/settings.json'
$configurationHash = $null
$model = Join-Path $data 'models/synthetic.bin'
$show = Join-Path $data 'shows/synthetic'
$before = @{}
function Check-UserData {
    if ($configurationHash -and (Get-FileHash -LiteralPath $configuration).Hash -ne $configurationHash) {
        throw 'Maintenance changed the selected custom settings file.'
    }
    $files = @(Get-ChildItem -LiteralPath $data -File -Recurse)
    if ($files.Count -ne $before.Count) { throw 'Maintenance added or removed user data.' }
    foreach ($file in $files) {
        if ((Get-FileHash -LiteralPath $file.FullName).Hash -ne $before[$file.FullName]) {
            throw 'Maintenance changed settings, models, or an edited Show.'
        }
    }
}
function Invoke-Setup([string]$Source = $Installer) {
    $process = Start-Process -FilePath $Source -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/DIR=`"$app`"" -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Installer failed with exit code $($process.ExitCode)." }
}
function Invoke-Uninstall {
    $process = Start-Process -FilePath (Join-Path $app 'unins000.exe') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Uninstall failed with exit code $($process.ExitCode)." }
    if (Test-Path (Join-Path $app 'speakeasy.exe')) { throw 'Uninstall retained the application executable.' }
    Check-UserData
}
function Close-Preview([Diagnostics.Process]$Preview) {
    try {
        $Preview.Refresh()
        if ($Preview.HasExited) { return }
        [uint32]$owner = 0
        $thread = [InstallerPreview]::GetWindowThreadProcessId($Preview.MainWindowHandle, [ref]$owner)
        if ($owner -ne $Preview.Id -or ![InstallerPreview]::PostThreadMessage($thread, 0x0012, [IntPtr]::Zero, [IntPtr]::Zero)) {
            throw 'Could not close the owned preview.'
        }
        if (!$Preview.WaitForExit(10000)) { throw 'The owned preview did not exit.' }
    } finally {
        if (!$Preview.HasExited) { $Preview.Kill(); [void]$Preview.WaitForExit(10000) }
        $Preview.Dispose()
    }
}
function Prepare-PreviousVersion {
    if (!$PreviousArchive) { return }
    $expectedArchive = '446a959ee8771e36b2d57db4b93af597b3b3f4c2a62323cc366f7a4f16e5c2d3'
    if ((Get-FileHash -LiteralPath $PreviousArchive -Algorithm SHA256).Hash -ne $expectedArchive) {
        throw 'The previous fixture must be the published v0.6.0 Windows ZIP.'
    }
    $previous = Join-Path $work 'previous'
    Expand-Archive -LiteralPath $PreviousArchive -DestinationPath $previous
    $oldNumber = '0.6.0'
    $newNumber = $version.Trim().Substring('speakeasy '.Length)
    if ([version]$oldNumber -ge [version]$newNumber) { throw 'Upgrade acceptance requires an older released app.' }
    # v0.6.0 has no installer, CLI, or version query. Package its pinned GUI with the current installer and CLI.
    $package = Join-Path $work 'previous-package'
    New-Item -ItemType Directory -Path $package | Out-Null
    Copy-Item (Join-Path $app '*') $package
    Copy-Item (Join-Path $previous 'speakeasy.exe') $package -Force
    $iscc = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
    if (!$iscc) { $iscc = "${env:ProgramFiles(x86)}/Inno Setup 6/ISCC.exe" }
    & $iscc /Qp "/DAppVersion=$oldNumber" "/DPackageRoot=$package" "/O$previous" (Join-Path $PSScriptRoot '../packaging/windows/installer.iss')
    if ($LASTEXITCODE -ne 0) { throw 'Could not package the previous released app fixture.' }
    Invoke-Setup (Join-Path $previous 'speakeasy-windows-x64-setup.exe')
    if ((Get-FileHash -LiteralPath (Join-Path $app 'speakeasy.exe')).Hash -ne (Get-FileHash -LiteralPath (Join-Path $previous 'speakeasy.exe')).Hash) {
        throw 'The previous released GUI was not installed.'
    }
}
function Check-UpdateHandoff([bool]$ReleaseParent) {
    $staged = Join-Path $work ('staged-' + [guid]::NewGuid() + '.exe')
    Copy-Item -LiteralPath $Installer -Destination $staged
    [IO.File]::Open($staged, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete).Dispose()
    $image = Join-Path $app 'speakeasy.exe'
    $imageHash = (Get-FileHash -LiteralPath $image).Hash
    $parent = Start-Process -FilePath powershell.exe -ArgumentList '-NoProfile', '-NonInteractive', '-Command', 'Start-Sleep -Seconds 90' -PassThru
    $setup = $null
    $preview = $null
    try {
        $setup = Start-Process -FilePath $staged -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/DIR=`"$app`"", "/SPEAKEASYPARENT=$($parent.Id)", "/SPEAKEASYCONFIG=`"$configuration`"", '/SPEAKEASYDEMO=1' -PassThru
        if ($setup.WaitForExit(1500)) { throw 'The installer did not wait for its parent.' }
        if ((Get-FileHash -LiteralPath $image).Hash -ne $imageHash) { throw 'The installer replaced app files before its parent exited.' }
        $protected = $false
        try { [IO.File]::Delete($staged) } catch [IO.IOException] { $protected = $true } catch [UnauthorizedAccessException] { $protected = $true }
        if (!$protected) { throw 'The active installer did not retain its staged executable.' }
        if ($ReleaseParent) { $parent.Kill(); [void]$parent.WaitForExit(10000) }
        $deadline = [DateTime]::UtcNow.AddSeconds(45)
        while (!$setup.WaitForExit(20)) {
            $protected = $false
            try {
                $writable = [IO.File]::Open($staged, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
                $writable.Dispose()
            } catch [IO.IOException] { $protected = $true } catch [UnauthorizedAccessException] { $protected = $true }
            $setup.Refresh()
            if (!$protected -and !$setup.HasExited) { throw 'The installer released its staged bytes before completing.' }
            if ([DateTime]::UtcNow -ge $deadline) { throw 'The update handoff did not finish within its deadline.' }
        }
        if (!$ReleaseParent) {
            $parent.Refresh()
            if ($setup.ExitCode -eq 0 -or $parent.HasExited -or (Get-FileHash -LiteralPath $image).Hash -ne $imageHash) {
                throw 'A failed handoff changed the app or terminated its parent.'
            }
            $launched = Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -eq $image }
            if ($launched) {
                $preview = Get-Process -Id $launched.ProcessId
                throw 'A failed update relaunched the application.'
            }
            Check-UserData
            return
        }
        if ($setup.ExitCode -ne 0) { throw "The update handoff failed (exit $($setup.ExitCode))." }
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        do {
            $launched = Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -eq $image }
            if ($launched) { break }
            if ([DateTime]::UtcNow -ge $deadline) { throw 'Successful update did not relaunch Speakeasy.' }
            Start-Sleep -Milliseconds 100
        } while ($true)
        if (@($launched).Count -ne 1 -or !$launched.CommandLine.Contains('--demo-tray') -or !$launched.CommandLine.Contains($configuration)) {
            throw 'Update did not relaunch the exact app and selected configuration in demo mode.'
        }
        $preview = Get-Process -Id $launched.ProcessId
        do {
            $preview.Refresh()
            if ($preview.HasExited -or [DateTime]::UtcNow -ge $deadline) { throw 'The updated preview did not open its window.' }
            Start-Sleep -Milliseconds 100
        } while ($preview.MainWindowHandle -eq [IntPtr]::Zero)
        $observed = & (Join-Path $app 'speakeasy-cli.exe') --version
        if ($LASTEXITCODE -ne 0 -or $observed -ne $version) { throw 'Upgrade did not install the current app version.' }
        if ((Get-FileHash -LiteralPath $image).Hash -ne $currentImageHash) { throw 'Upgrade did not replace the previous GUI with the current packaged image.' }
        Check-UserData
    } finally {
        if ($preview) { Close-Preview $preview }
        if ($setup) {
            if (!$setup.HasExited) { $setup.Kill($true); [void]$setup.WaitForExit(10000) }
            $setup.Dispose()
        }
        if (!$parent.HasExited) { $parent.Kill(); [void]$parent.WaitForExit(10000) }
        $parent.Dispose()
        Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
    }
}
function Check-RunningAppProtection {
    $previewConfig = Join-Path $work 'preview.json'
    [IO.File]::WriteAllText($previewConfig, 'demo preserves this file')
    $image = Join-Path $app 'speakeasy.exe'
    $imageHash = (Get-FileHash -LiteralPath $image).Hash
    $preview = Start-Process -FilePath $image -ArgumentList '--demo-tray', "--config `"$previewConfig`"" -NoNewWindow -PassThru
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
    } finally {
        Close-Preview $preview
    }
}
function Check-ConsoleImageProtection {
    $image = Join-Path $app 'speakeasy-cli.exe'
    $imageHash = (Get-FileHash -LiteralPath $image).Hash
    $source = [IO.File]::Open($image, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
    $mapping = [IntPtr]::Zero
    $view = [IntPtr]::Zero
    try {
        $mapping = [InstallerPreview]::CreateFileMapping($source.SafeFileHandle, [IntPtr]::Zero, 0x01000002, 0, 0, $null)
        if ($mapping -eq [IntPtr]::Zero) { throw 'Could not create an owned CLI image mapping.' }
        $view = [InstallerPreview]::MapViewOfFile($mapping, 4, 0, 0, [UIntPtr]::Zero)
        if ($view -eq [IntPtr]::Zero) { throw 'Could not map the owned CLI image.' }
        $blocked = Start-Process -FilePath $Installer -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/DIR=`"$app`"" -Wait -PassThru
        if ($blocked.ExitCode -ne 7) { throw 'Upgrade did not refuse a mapped CLI image.' }
        $blocked = Start-Process -FilePath (Join-Path $app 'unins000.exe') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
        if ($blocked.ExitCode -eq 0 -or (Get-FileHash -LiteralPath $image).Hash -ne $imageHash) {
            throw 'Maintenance changed a mapped CLI image.'
        }
    } finally {
        if ($view -ne [IntPtr]::Zero -and ![InstallerPreview]::UnmapViewOfFile($view)) { throw 'Could not release the owned CLI image view.' }
        if ($mapping -ne [IntPtr]::Zero -and ![InstallerPreview]::CloseHandle($mapping)) { throw 'Could not release the owned CLI image mapping.' }
        $source.Dispose()
    }
}
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class InstallerPreview {
    [DllImport("user32")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32")] public static extern bool PostThreadMessage(uint thread, uint message, IntPtr wparam, IntPtr lparam);
    [DllImport("kernel32", EntryPoint="CreateFileMappingW", CharSet=CharSet.Unicode, SetLastError=true)] public static extern IntPtr CreateFileMapping(Microsoft.Win32.SafeHandles.SafeFileHandle file, IntPtr attributes, uint protect, uint high, uint low, string name);
    [DllImport("kernel32", SetLastError=true)] public static extern IntPtr MapViewOfFile(IntPtr mapping, uint access, uint high, uint low, UIntPtr length);
    [DllImport("kernel32", SetLastError=true)] public static extern bool UnmapViewOfFile(IntPtr view);
    [DllImport("kernel32", SetLastError=true)] public static extern bool CloseHandle(IntPtr handle);
}
'@
try {
    Invoke-Setup
    $version = & (Join-Path $app 'speakeasy-cli.exe') --version
    if ($LASTEXITCODE -ne 0 -or $version -notmatch '^speakeasy \d+\.\d+\.\d+$') { throw 'Installed CLI failed.' }
    $currentImageHash = (Get-FileHash -LiteralPath (Join-Path $app 'speakeasy.exe')).Hash
    New-Item -ItemType Directory -Path (Split-Path $configuration) -Force | Out-Null
    Copy-Item (Join-Path $app 'settings.example.json') $configuration
    Copy-Item (Join-Path $app 'settings.example.json') (Join-Path $data 'settings.json')
    $configurationHash = (Get-FileHash -LiteralPath $configuration).Hash
    New-Item -ItemType Directory -Path (Split-Path $model), (Split-Path $show) -Force | Out-Null
    [IO.File]::WriteAllText($model, 'Synthetic model fixture; never loaded.')
    & (Join-Path $app 'speakeasy-cli.exe') show demo --out $show | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Could not prepare a synthetic Show.' }
    [IO.File]::WriteAllText((Join-Path $show 'user-notes.txt'), 'Keep these edits through every upgrade.')
    foreach ($file in Get-ChildItem -LiteralPath $data -File -Recurse) {
        $before[$file.FullName] = (Get-FileHash -LiteralPath $file.FullName).Hash
    }
    Check-RunningAppProtection
    Check-ConsoleImageProtection
    Check-UpdateHandoff $false
    Prepare-PreviousVersion
    Check-UpdateHandoff $true
    if (!(Test-Path $runKey)) { New-Item -Path $runKey -Force | Out-Null }
    New-ItemProperty -Path $runKey -Name 'Speakeasy' -Value ('"' + (Join-Path $app 'speakeasy.exe') + '"') -PropertyType String -Force | Out-Null
    Invoke-Uninstall
    if ($null -ne (Get-SignInCommand)) { throw 'Uninstall left its sign-in command.' }
    Invoke-Setup
    $portable = '"C:\portable\speakeasy.exe"'
    New-ItemProperty -Path $runKey -Name 'Speakeasy' -Value $portable -PropertyType String -Force | Out-Null
    Invoke-Uninstall
    if ((Get-SignInCommand) -ne $portable) { throw 'Uninstall removed another copy''s sign-in command.' }
    Write-Host 'Per-user install, both running images, parent wait and timeout, staged-file retention, upgrade/relaunch, and uninstall passed; settings, models, edited Shows, and other copies are preserved.'
} finally {
    if (Test-Path (Join-Path $app 'unins000.exe')) { Invoke-Uninstall }
    Remove-ItemProperty -Path $runKey -Name 'Speakeasy' -ErrorAction SilentlyContinue
    # The guard admitted only a fresh data directory on a disposable runner; every fixture is ours.
    Remove-Item -LiteralPath $data -Force -Recurse
    Remove-Item -LiteralPath $work -Force -Recurse
}
