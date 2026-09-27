param([Parameter(Mandatory=$true)][string]$Executable, [string]$WorkingDirectory = (Get-Location).Path)
$ErrorActionPreference = 'Stop'
# A visible, unattended demo check. No hook, microphone, clipboard, or input injection.
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Text;
public static class DemoWindows {
    public delegate bool Callback(IntPtr window, IntPtr unused);
    [DllImport("user32")] public static extern bool EnumWindows(Callback callback, IntPtr unused);
    [DllImport("user32")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr window, StringBuilder text, int capacity);
    [DllImport("user32")] public static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32")] public static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32")] public static extern IntPtr WindowFromPhysicalPoint(Point point);
    [DllImport("user32")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    public struct Rect { public int Left, Top, Right, Bottom; }
}
'@ -ReferencedAssemblies System.Drawing
$previousDpi = [DemoWindows]::SetThreadDpiAwarenessContext([IntPtr](-4))
$process = New-Object Diagnostics.Process
$process.StartInfo.FileName = $Executable
$process.StartInfo.Arguments = '--demo'
$process.StartInfo.WorkingDirectory = $WorkingDirectory
$process.StartInfo.UseShellExecute = $false
$started = $false
try {
    $started = $process.Start()
    $script:demoProcess = $process.Id
    $script:pill = [IntPtr]::Zero
    $callback = [DemoWindows+Callback]{ param($window, $unused)
        [uint32]$owner = 0
        $null = [DemoWindows]::GetWindowThreadProcessId($window, [ref]$owner)
        if ($owner -eq $script:demoProcess) {
            $title = New-Object Text.StringBuilder 256
            $null = [DemoWindows]::GetWindowText($window, $title, 256)
            if ($title.ToString() -eq 'Speakeasy pill') { $script:pill = $window }
        }
        return $true
    }
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while (($script:pill -eq [IntPtr]::Zero -or ![DemoWindows]::IsWindowVisible($script:pill)) -and [DateTime]::UtcNow -lt $deadline) {
        if ($process.HasExited) { throw "Demo exited with $($process.ExitCode)" }
        $null = [DemoWindows]::EnumWindows($callback, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 100
    }
    if ($script:pill -eq [IntPtr]::Zero -or ![DemoWindows]::IsWindowVisible($script:pill)) { throw 'Pill window did not appear' }
    Start-Sleep -Milliseconds 750
    if ($process.HasExited -or ![DemoWindows]::IsWindowVisible($script:pill)) { throw 'Pill disappeared before hit testing' }
    if ([DemoWindows]::GetForegroundWindow() -eq $script:pill) { throw 'Pill took foreground focus' }
    $rect = New-Object DemoWindows+Rect
    if (![DemoWindows]::GetWindowRect($script:pill, [ref]$rect) -or $rect.Right -le $rect.Left -or $rect.Bottom -le $rect.Top) {
        throw 'Pill has no visible bounds for hit testing'
    }
    $point = New-Object Drawing.Point (($rect.Left+$rect.Right)/2),(($rect.Top+$rect.Bottom)/2)
    $hit = [DemoWindows]::WindowFromPhysicalPoint($point)
    if ($hit -eq $script:pill) { throw 'Pill intercepted hit testing instead of passing through' }
    if ($hit -eq [IntPtr]::Zero) { throw 'No desktop window beneath the pill; run on an available Windows desktop.' }
    Write-Host 'PASS: native demo starts; visible pill stays nonactivating and passes hit testing to the desktop below.'
} finally {
    if ($started -and !$process.HasExited) {
        $null = $process.CloseMainWindow()
        if (!$process.WaitForExit(4000)) { $process.Kill(); $process.WaitForExit() }
    }
    $process.Dispose()
    $null = [DemoWindows]::SetThreadDpiAwarenessContext($previousDpi)
}
