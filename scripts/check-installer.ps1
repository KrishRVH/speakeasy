# CI runners without Speakeasy state only: install, update, and uninstall; demo opens no devices.
param(
    [Parameter(Mandatory = $true)][string]$Installer
)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or !$env:RUNNER_TEMP) {
    throw 'Installer acceptance runs only on a GitHub Actions runner.'
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
$configuration = Join-Path $work 'selected Ω config/settings.json'
$configurationHash = $null
$model = Join-Path $data 'models/synthetic.bin'
$show = Join-Path $data 'shows/synthetic'
$before = $null
function Check-UserData {
    if ($null -eq $before) { return }
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
function Invoke-Cleanup([scriptblock]$Action, [Collections.Generic.List[Management.Automation.ErrorRecord]]$Failures) {
    try { & $Action } catch { $Failures.Add($_) }
}
function Complete-Cleanup($Failure, [Collections.Generic.List[Management.Automation.ErrorRecord]]$Failures, [string]$Fixture) {
    if (!$Failures.Count) { return }
    if ($null -eq $Failure) { throw $Failures[0] }
    $Failures | ForEach-Object { Write-Warning "$Fixture cleanup failed: $_" }
}
function Invoke-Setup {
    $process = Start-Process -FilePath $Installer -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/DIR=`"$app`"" -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Installer failed with exit code $($process.ExitCode)." }
}
function Invoke-Uninstall {
    $process = Start-Process -FilePath (Join-Path $app 'unins000.exe') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Uninstall failed with exit code $($process.ExitCode)." }
    if (Test-Path (Join-Path $app 'speakeasy.exe')) { throw 'Uninstall retained the application executable.' }
    Check-UserData
}
function Open-Launched($Launched) {
    $process = Get-Process -Id $Launched.ProcessId -ErrorAction SilentlyContinue
    if (!$process) { return $null }
    $pinned = $false
    try {
        # Holding the handle pins this process, so the start time proves the PID was not reused.
        try { [void]$process.Handle } catch {
            if (Get-Process -Id $Launched.ProcessId -ErrorAction SilentlyContinue) { throw }
            return $null
        }
        $pinned = [Math]::Abs(($process.StartTime - $Launched.CreationDate).TotalMilliseconds) -lt 1
        if ($pinned) { return $process }
        return $null
    } finally {
        if (!$pinned) { $process.Dispose() }
    }
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
function Check-UpdateHandoff([bool]$ReleaseParent) {
    $staged = Join-Path $work ('staged-' + [guid]::NewGuid() + '.exe')
    Copy-Item -LiteralPath $Installer -Destination $staged
    [IO.File]::Open($staged, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete).Dispose()
    $image = Join-Path $app 'speakeasy.exe'
    $imageHash = (Get-FileHash -LiteralPath $image).Hash
    $parent = Start-Process -FilePath powershell.exe -ArgumentList '-NoProfile', '-NonInteractive', '-Command', 'Start-Sleep -Seconds 90' -PassThru
    $setup = $null
    $preview = $null
    $failure = $null
    $cleanup = [Collections.Generic.List[Management.Automation.ErrorRecord]]::new()
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
            if ($launched) { throw 'A failed update relaunched the application.' }
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
        $preview = Open-Launched $launched
        if (!$preview) { throw 'The updated preview exited before it opened its window.' }
        do {
            $preview.Refresh()
            if ($preview.HasExited -or [DateTime]::UtcNow -ge $deadline) { throw 'The updated preview did not open its window.' }
            Start-Sleep -Milliseconds 100
        } while ($preview.MainWindowHandle -eq [IntPtr]::Zero)
        $observed = & (Join-Path $app 'speakeasy-cli.exe') --version
        if ($LASTEXITCODE -ne 0 -or $observed -ne $version) { throw 'Upgrade did not install the current app version.' }
        if ((Get-FileHash -LiteralPath $image).Hash -ne $currentImageHash) { throw 'Update did not install the current packaged GUI.' }
        Check-UserData
    } catch {
        $failure = $_
        throw
    } finally {
        Invoke-Cleanup {
            if ($setup) {
                if (!$setup.HasExited) { $setup.Kill($true); [void]$setup.WaitForExit(10000) }
                $setup.Dispose()
            }
        } $cleanup
        Invoke-Cleanup {
            if (!$parent.HasExited) { $parent.Kill(); [void]$parent.WaitForExit(10000) }
            $parent.Dispose()
        } $cleanup
        Invoke-Cleanup {
            if ($preview) { Close-Preview $preview }
        } $cleanup
        $leftovers = @(Invoke-Cleanup {
            Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -eq $image }
        } $cleanup)
        foreach ($leftover in $leftovers) {
            Invoke-Cleanup {
                $process = Open-Launched $leftover
                if (!$process) { return }
                try {
                    if (!$process.HasExited) { $process.Kill() }
                    if (!$process.WaitForExit(10000)) { throw 'The relaunched fixture kept running.' }
                } finally {
                    $process.Dispose()
                }
            } $cleanup
        }
        Invoke-Cleanup { Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue } $cleanup
        Complete-Cleanup $failure $cleanup 'Update fixture'
    }
}
function Check-RunningAppProtection {
    $previewConfig = Join-Path $work 'preview.json'
    [IO.File]::WriteAllText($previewConfig, 'demo preserves this file')
    $image = Join-Path $app 'speakeasy.exe'
    $imageHash = (Get-FileHash -LiteralPath $image).Hash
    $preview = Start-Process -FilePath $image -ArgumentList '--demo-tray', "--config `"$previewConfig`"" -NoNewWindow -PassThru
    [void]$preview.Handle
    $failure = $null
    $cleanup = [Collections.Generic.List[Management.Automation.ErrorRecord]]::new()
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
    } catch {
        $failure = $_
        throw
    } finally {
        Invoke-Cleanup { Close-Preview $preview } $cleanup
        Complete-Cleanup $failure $cleanup 'Running app fixture'
    }
}
function Check-ConsoleImageProtection {
    $image = Join-Path $app 'speakeasy-cli.exe'
    $imageHash = (Get-FileHash -LiteralPath $image).Hash
    $source = [IO.File]::Open($image, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
    $mapping = [IntPtr]::Zero
    $view = [IntPtr]::Zero
    $failure = $null
    $cleanup = [Collections.Generic.List[Management.Automation.ErrorRecord]]::new()
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
    } catch {
        $failure = $_
        throw
    } finally {
        Invoke-Cleanup {
            if ($view -ne [IntPtr]::Zero -and ![InstallerPreview]::UnmapViewOfFile($view)) { throw 'Could not release the owned CLI image view.' }
        } $cleanup
        Invoke-Cleanup {
            if ($mapping -ne [IntPtr]::Zero -and ![InstallerPreview]::CloseHandle($mapping)) { throw 'Could not release the owned CLI image mapping.' }
        } $cleanup
        Invoke-Cleanup { $source.Dispose() } $cleanup
        Complete-Cleanup $failure $cleanup 'Console image fixture'
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
$failure = $null
$cleanup = [Collections.Generic.List[Management.Automation.ErrorRecord]]::new()
try {
    New-Item -ItemType Directory -Path $work, $data -Force | Out-Null
    Invoke-Setup
    $setupSignature = Get-AuthenticodeSignature -LiteralPath $Installer
    if ($setupSignature.Status -eq 'Valid') {
        $uninstallSignature = Get-AuthenticodeSignature -LiteralPath (Join-Path $app 'unins000.exe')
        if ($uninstallSignature.Status -ne 'Valid' -or
            $uninstallSignature.SignerCertificate.Thumbprint -ne $setupSignature.SignerCertificate.Thumbprint -or
            !$uninstallSignature.TimeStamperCertificate) {
            throw 'The installed uninstaller lacks the expected timestamped publisher signature.'
        }
    }
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
    $expected = @{}
    foreach ($file in Get-ChildItem -LiteralPath $data -File -Recurse) {
        $expected[$file.FullName] = (Get-FileHash -LiteralPath $file.FullName).Hash
    }
    $before = $expected
    Check-RunningAppProtection
    Check-ConsoleImageProtection
    Check-UpdateHandoff $false
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
} catch {
    $failure = $_
    throw
} finally {
    Invoke-Cleanup {
        if (Test-Path (Join-Path $app 'unins000.exe')) { Invoke-Uninstall }
    } $cleanup
    Invoke-Cleanup {
        Remove-ItemProperty -Path $runKey -Name 'Speakeasy' -ErrorAction SilentlyContinue
    } $cleanup
    # The guard admitted only a fresh data directory and no sign-in entry; every fixture is ours.
    foreach ($fixture in $data, $work) {
        Invoke-Cleanup {
            if (Test-Path -LiteralPath $fixture) { Remove-Item -LiteralPath $fixture -Force -Recurse }
        } $cleanup
    }
    Complete-Cleanup $failure $cleanup 'Installer fixture'
}
