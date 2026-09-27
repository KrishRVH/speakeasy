param([Parameter(Mandatory=$true)][string]$Executable, [switch]$MeasureIdle)
$ErrorActionPreference = 'Stop'
# Own simulated windows only. No microphone, hook, clipboard or global input.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class TrayWindows {
    public delegate bool Callback(IntPtr window, IntPtr unused);
    [DllImport("user32")] public static extern bool EnumWindows(Callback callback, IntPtr unused);
    [DllImport("user32")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr window, StringBuilder text, int capacity);
    [DllImport("user32")] public static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr wparam, IntPtr lparam);
    [DllImport("user32")] public static extern bool PostThreadMessage(uint thread, uint message, IntPtr wparam, IntPtr lparam);
    [DllImport("user32")] public static extern bool ShowWindowAsync(IntPtr window, int command);
}
'@
function Wait-For([scriptblock]$Condition, [string]$Failure) {
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while (!( & $Condition )) {
        if ($script:app.HasExited) { throw "Demo exited: $($script:app.ExitCode)" }
        if ([DateTime]::UtcNow -ge $deadline) { throw $Failure }
        Start-Sleep -Milliseconds 50
    }
}
$directory = Join-Path ([IO.Path]::GetTempPath()) ('speakeasy-tray-' + [Guid]::NewGuid())
$null = New-Item -ItemType Directory -Path $directory
$config = Join-Path $directory 'settings.json'
# Demo must neither load this deliberately invalid configuration nor change it.
[IO.File]::WriteAllText($config, 'demo preserves this file')
$script:app = New-Object Diagnostics.Process
$script:app.StartInfo.FileName = $Executable
$script:app.StartInfo.Arguments = '--demo-tray --config "' + $config + '"'
$script:app.StartInfo.WorkingDirectory = $directory
$script:app.StartInfo.UseShellExecute = $false
$started = $false
try {
    $started = $script:app.Start()
    $script:settings = [IntPtr]::Zero
    $callback = [TrayWindows+Callback]{ param($window, $unused)
        [uint32]$owner = 0
        $null = [TrayWindows]::GetWindowThreadProcessId($window, [ref]$owner)
        if ($owner -eq $script:app.Id) {
            $title = New-Object Text.StringBuilder 256
            $null = [TrayWindows]::GetWindowText($window, $title, 256)
            if ($title.ToString() -eq 'Speakeasy') { $script:settings = $window }
        }
        return $true
    }
    Wait-For {
        $null = [TrayWindows]::EnumWindows($callback, [IntPtr]::Zero)
        $script:settings -ne [IntPtr]::Zero -and [TrayWindows]::IsWindowVisible($script:settings)
    } 'Settings did not appear'
    $original = $script:settings
    foreach ($message in @('minimize', 'direct minimize', 'close')) {
        if ($message -eq 'minimize') {
            $null = [TrayWindows]::PostMessage($original, 0x0112, [IntPtr]0xF020, [IntPtr]::Zero)
        } elseif ($message -eq 'direct minimize') {
            $null = [TrayWindows]::ShowWindowAsync($original, 6)
        } else {
            $null = [TrayWindows]::PostMessage($original, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
        }
        Wait-For { ![TrayWindows]::IsWindowVisible($original) } "$message did not hide Settings"
        $relaunch = New-Object Diagnostics.Process
        try {
            $relaunch.StartInfo.FileName = $Executable
            $relaunch.StartInfo.Arguments = $script:app.StartInfo.Arguments
            $relaunch.StartInfo.WorkingDirectory = $directory
            $relaunch.StartInfo.UseShellExecute = $false
            $null = $relaunch.Start()
            if (!$relaunch.WaitForExit(5000)) { $relaunch.Kill(); throw 'Second launch did not exit' }
            if ($relaunch.ExitCode -ne 0) { throw "Second launch failed: $($relaunch.ExitCode)" }
        } finally { $relaunch.Dispose() }
        Wait-For { [TrayWindows]::IsWindowVisible($original) } 'Relaunch did not restore the original Settings window'
    }
    if ([IO.File]::ReadAllText($config) -ne 'demo preserves this file') { throw 'Demo changed configuration' }
    if ($MeasureIdle) {
        $null = [TrayWindows]::PostMessage($original, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
        Wait-For { ![TrayWindows]::IsWindowVisible($original) } 'Settings did not hide for idle measurement'
        if ($script:app.WaitForExit(25000)) { throw 'Demo exited before idle measurement' }
        $script:app.Refresh()
        $before = $script:app.TotalProcessorTime.TotalMilliseconds
        if ($script:app.WaitForExit(3000)) { throw 'Demo exited during idle measurement' }
        $script:app.Refresh()
        Write-Host ('Idle tray demo CPU over 3 seconds: {0:N2} ms; working set: {1:N1} MiB' -f ($script:app.TotalProcessorTime.TotalMilliseconds-$before), ($script:app.WorkingSet64/1MB))
    }
    # Use the same WM_QUIT signal as GPUI, targeted only at this owned UI thread.
    [uint32]$owner = 0
    $thread = [TrayWindows]::GetWindowThreadProcessId($original, [ref]$owner)
    if ($owner -ne $script:app.Id) { throw 'Settings ownership changed' }
    $null = [TrayWindows]::PostThreadMessage($thread, 0x0012, [IntPtr]::Zero, [IntPtr]::Zero)
    if (!$script:app.WaitForExit(4000)) { throw 'Tray demo did not quit cleanly' }
    if ($script:app.ExitCode -ne 0) { throw "Tray demo failed on quit: $($script:app.ExitCode)" }
    if (Test-Path (Join-Path $directory 'instance.port')) { throw 'Reopen listener did not clean up on quit' }
    Write-Host 'PASS: minimize and close hide Settings; relaunch restores the same window; configuration is preserved; quit cleans up the reopen listener.'
} finally {
    if ($started -and !$script:app.HasExited) {
        $script:app.Kill()
        $script:app.WaitForExit()
    }
    $script:app.Dispose()
    Remove-Item -LiteralPath $directory -Recurse -Force
}
