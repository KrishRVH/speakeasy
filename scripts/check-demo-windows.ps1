param([Parameter(Mandatory=$true)][string]$Executable, [string]$WorkingDirectory = (Get-Location).Path, [switch]$StaticAccessibility)
$ErrorActionPreference = 'Stop'
# A visible, unattended demo check. No hook, microphone, clipboard, or input injection.
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
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
    [DllImport("user32")] public static extern uint GetDpiForWindow(IntPtr window);
    [DllImport("user32")] public static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32")] public static extern IntPtr WindowFromPhysicalPoint(Point point);
    [DllImport("user32")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    [DllImport("user32")] public static extern bool GetClientRect(IntPtr window, out Rect rect);
    [DllImport("user32")] public static extern bool ClientToScreen(IntPtr window, ref Point point);
    [DllImport("user32")] public static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);
    [DllImport("user32")] public static extern bool ShowWindow(IntPtr window, int command);
    public struct Rect { public int Left, Top, Right, Bottom; }
}
'@ -ReferencedAssemblies System.Drawing
function Test-DemoAccessibility([IntPtr]$Window, [int]$Owner) {
    [uint32]$actualOwner = 0
    $null = [DemoWindows]::GetWindowThreadProcessId($Window, [ref]$actualOwner)
    if ($actualOwner -ne $Owner) { throw 'Accessibility fixture has the wrong owner' }
    # Query only the HWND already identified as belonging to this simulated process.
    $root = [Windows.Automation.AutomationElement]::FromHandle($Window)
    function Find-Control([string]$Name) {
        $condition = New-Object Windows.Automation.PropertyCondition ([Windows.Automation.AutomationElement]::NameProperty), $Name
        return $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
    }
    function Wait-Control([string]$Name) {
        $deadline = [DateTime]::UtcNow.AddSeconds(5)
        do {
            $found = Find-Control $Name
            if ($null -ne $found) { return $found }
            Start-Sleep -Milliseconds 50
        } while ([DateTime]::UtcNow -lt $deadline)
        throw 'Accessible control did not appear'
    }
    function Invoke-Control($Control) {
        $pattern = $null
        if (!$Control.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
            throw 'Accessible control has no invocation pattern'
        }
        $pattern.Invoke()
    }
    function Assert-Removed($Control) {
        try {
            $name = $Control.Current.Name
            $patterns = $Control.GetSupportedPatterns()
            if ([string]::IsNullOrEmpty($name) -and $patterns.Count -eq 0) { return }
        }
        catch [Windows.Automation.ElementNotAvailableException] { return }
        throw 'Removed accessible control retained its content'
    }
    $dictation = Wait-Control 'Dictation'
    if ($dictation.Current.ControlType -ne [Windows.Automation.ControlType]::TabItem) {
        throw 'Settings page does not expose a tab role'
    }
    $selected = $null
    if (!$dictation.TryGetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern, [ref]$selected)) {
        throw 'Settings tab has no selection pattern'
    }
    if (!$selected.Current.IsSelected) { throw 'Current page is not exposed as selected' }
    $motion = Find-Control 'Reduce motion: Off'
    if ($null -eq $motion) { $motion = Find-Control 'Reduce motion: On' }
    if ($null -eq $motion) { throw 'Motion switch is inaccessible' }
    $toggle = $motion.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern)
    $previousToggle = $toggle.Current.ToggleState
    $toggle.Toggle()
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while ($toggle.Current.ToggleState -eq $previousToggle -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
    if ($toggle.Current.ToggleState -eq $previousToggle) { throw 'Accessible switch did not toggle' }
    $buttons = $root.FindAll([Windows.Automation.TreeScope]::Descendants,
        (New-Object Windows.Automation.PropertyCondition ([Windows.Automation.AutomationElement]::ControlTypeProperty), ([Windows.Automation.ControlType]::Button)))
    $theme = $buttons | Where-Object { $_.Current.Name.StartsWith('Theme: ') } | Select-Object -First 1
    if ($null -eq $theme) { throw 'Theme button is inaccessible' }
    $previous = $theme.Current.Name
    Invoke-Control $theme
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while ($theme.Current.Name -eq $previous -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
    if ($theme.Current.Name -eq $previous) { throw 'Accessible invocation did not change theme' }
    $words = Wait-Control 'Words'
    $selectWords = $words.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern)
    $selectWords.Select()
    $heard = Wait-Control 'When I say'
    Assert-Removed $theme
    if ($heard.Current.ControlType -ne [Windows.Automation.ControlType]::Edit) { throw 'Field has no edit role' }
    $heard.SetFocus()
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while (!$heard.Current.HasKeyboardFocus -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
    if (!$heard.Current.HasKeyboardFocus) { throw 'Accessible focus did not reach the field' }
    $value = $null
    if (!$heard.TryGetCurrentPattern([Windows.Automation.ValuePattern]::Pattern, [ref]$value)) {
        throw 'Editable field has no value pattern'
    }
    $synthetic = 'Synthetic ' + [char]0x00E9 + [char]0xD83D + [char]0xDE80
    $value.SetValue($synthetic)
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while ($value.Current.Value -ne $synthetic -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
    if ($value.Current.Value -ne $synthetic) { throw 'Accessible Unicode editing did not reach the field' }
    $value.SetValue(('x' * 2049))
    Start-Sleep -Milliseconds 200
    if ($value.Current.Value -ne $synthetic) { throw 'Accessible input bypassed the field size limit' }
    $value.SetValue("line`nfeed")
    Start-Sleep -Milliseconds 200
    if ($value.Current.Value -ne $synthetic) { throw 'Accessible input bypassed the single-line limit' }
    $null = [DemoWindows]::SetWindowPos($Window, [IntPtr]::Zero, 180, 120, 0, 0, 0x15)
    Start-Sleep -Milliseconds 200
    $client = New-Object DemoWindows+Rect
    $origin = New-Object Drawing.Point 0, 0
    if (![DemoWindows]::GetClientRect($Window, [ref]$client) -or ![DemoWindows]::ClientToScreen($Window, [ref]$origin)) {
        throw 'Could not read owned client geometry'
    }
    $bounds = $root.Current.BoundingRectangle
    if ([Math]::Abs($bounds.X - $origin.X) -gt 1 -or [Math]::Abs($bounds.Y - $origin.Y) -gt 1 -or
        [Math]::Abs($bounds.Width - ($client.Right - $client.Left)) -gt 1 -or
        [Math]::Abs($bounds.Height - ($client.Bottom - $client.Top)) -gt 1) {
        throw 'Accessibility bounds disagree with the moved native client'
    }
    $null = [DemoWindows]::ShowWindow($Window, 0)
    Start-Sleep -Milliseconds 200
    Assert-Removed $heard
    if ($null -ne (Find-Control 'When I say')) { throw 'Hidden window exposes its field' }
    $oldHeard = $heard
    $null = [DemoWindows]::ShowWindow($Window, 4)
    $heard = Wait-Control 'When I say'
    Assert-Removed $oldHeard
    $value = $heard.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern)
    if ($value.Current.Value -ne $synthetic) { throw 'Reopening changed the private draft' }
    $null = [DemoWindows]::ShowWindow($Window, 6)
    Start-Sleep -Milliseconds 200
    Assert-Removed $heard
    if ($null -ne (Find-Control 'When I say')) { throw 'Minimized window exposes its field' }
    $null = [DemoWindows]::ShowWindow($Window, 9)
    $restored = Wait-Control 'When I say'
    Assert-Removed $heard
    $restoredValue = $restored.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern)
    if ($restoredValue.Current.Value -ne $synthetic) { throw 'Restoring changed the private draft' }
    Write-Host 'PASS: owned UI Automation roles, selection, toggle, invocation, focus, bounded Unicode editing, moved bounds, hide/minimize redaction, and retired providers.'
}
function Test-TextAccessibility($Field, [IntPtr]$Window) {
    Add-Type -TypeDefinition @'
using System;
using System.Threading;
using System.Windows.Automation;
public sealed class TextEventCounts : IDisposable {
    readonly AutomationElement element;
    readonly AutomationEventHandler changed;
    readonly AutomationEventHandler selected;
    public int Changed;
    public int Selected;
    public TextEventCounts(AutomationElement element) {
        this.element = element;
        changed = (sender, args) => Interlocked.Increment(ref Changed);
        selected = (sender, args) => Interlocked.Increment(ref Selected);
        Automation.AddAutomationEventHandler(TextPattern.TextChangedEvent, element, TreeScope.Element, changed);
        Automation.AddAutomationEventHandler(TextPattern.TextSelectionChangedEvent, element, TreeScope.Element, selected);
    }
    public void Dispose() {
        Automation.RemoveAutomationEventHandler(TextPattern.TextChangedEvent, element, changed);
        Automation.RemoveAutomationEventHandler(TextPattern.TextSelectionChangedEvent, element, selected);
    }
}
'@ -ReferencedAssemblies ([Windows.Automation.AutomationElement].Assembly.Location), ([Windows.Automation.AutomationEvent].Assembly.Location), ([Windows.Point].Assembly.Location)
    $value = $Field.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern)
    $text = $Field.GetCurrentPattern([Windows.Automation.TextPattern]::Pattern)
    function Set-Text([string]$Content) {
        $value.SetValue($Content)
        $deadline = [DateTime]::UtcNow.AddSeconds(5)
        while ($text.DocumentRange.GetText(-1) -ne $Content -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 30 }
        if ($text.DocumentRange.GetText(-1) -ne $Content) { throw 'DocumentRange did not track the bounded field value' }
    }
    $Field.SetFocus()
    $events = New-Object TextEventCounts $Field
    try {
        $rocket = [string][char]0xD83D + [char]0xDE80
        $unicode = 'a' + $rocket + 'e' + [char]0x0301 + 'b'
        Set-Text $unicode
        $deadline = [DateTime]::UtcNow.AddSeconds(5)
        while ($events.Changed -eq 0 -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 30 }
        $beforeReplacement = $events.Changed
        Set-Text $unicode
        $deadline = [DateTime]::UtcNow.AddSeconds(5)
        while ($events.Changed -le $beforeReplacement -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 30 }
        if ($events.Changed -le $beforeReplacement) { throw 'Replacing identical text did not raise TextChanged' }
        $document = $text.DocumentRange
        $range = $document.Clone()
        $range.MoveEndpointByRange([Windows.Automation.Text.TextPatternRangeEndpoint]::End, $document, [Windows.Automation.Text.TextPatternRangeEndpoint]::Start)
        $null = $range.MoveEndpointByUnit([Windows.Automation.Text.TextPatternRangeEndpoint]::Start, [Windows.Automation.Text.TextUnit]::Character, 1)
        $null = $range.MoveEndpointByUnit([Windows.Automation.Text.TextPatternRangeEndpoint]::End, [Windows.Automation.Text.TextUnit]::Character, 1)
        if ($range.GetText(-1) -ne $rocket) { throw 'TextPattern character units split a Unicode scalar' }
        $beforeSelection = $events.Selected
        $range.Select()
        $deadline = [DateTime]::UtcNow.AddSeconds(5)
        while (($text.GetSelection()[0].GetText(-1) -ne $rocket -or $events.Selected -le $beforeSelection) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 30 }
        if ($text.GetSelection()[0].GetText(-1) -ne $rocket) { throw 'Native text selection did not reach the editor' }
        if ($events.Selected -le $beforeSelection) { throw 'Native text selection event was not raised' }

        $long = ('a' * 260) + ' ' + $rocket + ' end'
        Set-Text $long
        try {
            if (![string]::IsNullOrEmpty($document.GetText(-1))) { throw 'Retired range exposed an older text snapshot' }
        } catch [Windows.Automation.ElementNotAvailableException] {}
        catch [InvalidOperationException] {}
        $document = $text.DocumentRange
        $word = $document.Clone()
        $word.ExpandToEnclosingUnit([Windows.Automation.Text.TextUnit]::Word)
        if ($word.GetText(-1).TrimEnd() -ne ('a' * 260)) { throw 'Word navigation split a word at a text-run boundary' }
        $visible = $text.GetVisibleRanges()
        if ($visible.Count -ne 1 -or [string]::IsNullOrEmpty($visible[0].GetText(-1))) { throw 'Scrolled field has no single continuous visible text range' }
        if ($visible[0].GetText(-1).Length -ge $long.Length -or !$visible[0].GetText(-1).EndsWith(' end')) { throw 'Visible range does not match the scrolled suffix' }
        $rectangles = $document.GetBoundingRectangles()
        if ($rectangles.Count -ne 1) { throw 'One scrolled line must have one visible rectangle' }
        $scale = [DemoWindows]::GetDpiForWindow($Window) / 96.0
        $control = $Field.Current.BoundingRectangle
        $viewport = New-Object Windows.Rect ($control.Left + 9 * $scale), ($control.Top + 6 * $scale), ($control.Width - 18 * $scale), (20 * $scale)
        $rect = $rectangles[0]
        if ([Math]::Abs($rect.Left - $viewport.Left) -gt 1 -or $rect.Right -gt $viewport.Right + 1 -or $rect.Top -lt $viewport.Top - 1 -or $rect.Bottom -gt $viewport.Bottom + 1) { throw 'Text geometry escaped or did not reach the actual paint clip' }
        $hidden = $document.Clone()
        $hidden.MoveEndpointByRange([Windows.Automation.Text.TextPatternRangeEndpoint]::End, $document, [Windows.Automation.Text.TextPatternRangeEndpoint]::Start)
        $null = $hidden.MoveEndpointByUnit([Windows.Automation.Text.TextPatternRangeEndpoint]::End, [Windows.Automation.Text.TextUnit]::Character, 2)
        if ($hidden.GetBoundingRectangles().Count -ne 0) { throw 'Fully scrolled-out text retained rectangles' }
        $caret = $document.Clone()
        $caret.MoveEndpointByRange([Windows.Automation.Text.TextPatternRangeEndpoint]::Start, $document, [Windows.Automation.Text.TextPatternRangeEndpoint]::End)
        if ($caret.GetBoundingRectangles().Count -ne 0) { throw 'Degenerate caret retained rectangles' }
        $hit = $text.RangeFromPoint((New-Object Windows.Point ($rect.Right - 3), (($rect.Top + $rect.Bottom) / 2)))
        $hit.ExpandToEnclosingUnit([Windows.Automation.Text.TextUnit]::Character)
        if ([string]::IsNullOrEmpty($hit.GetText(-1))) { throw 'Text hit testing failed in the visible line' }
        if ($events.Changed -eq 0) { throw 'Native text change event was not raised' }
        Set-Text ''
        if ($text.GetVisibleRanges().Count -ne 1 -or $text.GetSelection()[0].GetText(-1) -ne '') { throw 'Empty field lost its caret range' }
        Set-Text 'Synthetic private draft'
        Write-Host 'PASS: native TextPattern Unicode selection, events, retired ranges, empty carets, cross-run visibility, and clipped scrolled geometry.'
    } finally {
        $events.Dispose()
    }
}

function Test-StaticAccessibility([IntPtr]$Window) {
    $root = [Windows.Automation.AutomationElement]::FromHandle($Window)
    $condition = New-Object Windows.Automation.PropertyCondition ([Windows.Automation.AutomationElement]::NameProperty), 'Synthetic field'
    function Wait-Field([string]$Phase) {
        $deadline = [DateTime]::UtcNow.AddSeconds(5)
        do {
            $field = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
            if ($null -ne $field) { return $field }
            Start-Sleep -Milliseconds 50
        } while ([DateTime]::UtcNow -lt $deadline)
        throw "Static accessible field did not appear during $Phase"
    }
    Start-Sleep -Milliseconds 200
    if ($null -ne $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)) {
        throw 'Initially hidden fixture exposes its field'
    }
    $null = [DemoWindows]::ShowWindow($Window, 4)
    $field = Wait-Field 'first nonactivating show'
    Test-TextAccessibility $field $Window
    $oldValue = $field.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern)
    $oldText = $field.GetCurrentPattern([Windows.Automation.TextPattern]::Pattern).DocumentRange
    foreach ($transition in @(@{Hide=0; Show=4}, @{Hide=6; Show=9}, @{Hide=0; Show=1})) {
        $hide = $transition.Hide
        $null = [DemoWindows]::ShowWindow($Window, $hide)
        Start-Sleep -Milliseconds 200
        if ($null -ne $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)) {
            throw 'Static hidden or minimized tree retained its field'
        }
        try {
            if (![string]::IsNullOrEmpty($oldValue.Current.Value)) { throw 'Hidden provider retained its field value' }
        } catch [Windows.Automation.ElementNotAvailableException] {}
        catch [InvalidOperationException] {}
        try {
            if (![string]::IsNullOrEmpty($oldText.GetText(-1))) { throw 'Hidden text range retained its text' }
        } catch [Windows.Automation.ElementNotAvailableException] {}
        catch [InvalidOperationException] {}
        # This fixture has no presentation updates, timers that redraw, or animation.
        $show = $transition.Show
        $null = [DemoWindows]::ShowWindow($Window, $show)
        $field = Wait-Field "reopen with command $show"
        try { $oldValue.SetValue('Stale synthetic edit') }
        catch [Windows.Automation.ElementNotAvailableException] {}
        catch [InvalidOperationException] {}
        Start-Sleep -Milliseconds 200
        $value = $field.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern)
        if ($value.Current.Value -ne 'Synthetic private draft') { throw 'Retired provider changed the current draft' }
        $oldValue = $value
        $oldText = $field.GetCurrentPattern([Windows.Automation.TextPattern]::Pattern).DocumentRange
    }
    Write-Host 'PASS: static owned UIA hide/minimize, nonactivating and normal reopen, and stale-provider rejection without unrelated redraws.'
    return $root
}
# -4 is per-monitor DPI awareness (v2), so window rectangles are in the physical pixels that
# WindowFromPhysicalPoint takes.
$previousDpi = [DemoWindows]::SetThreadDpiAwarenessContext([IntPtr](-4))
$process = New-Object Diagnostics.Process
$process.StartInfo.FileName = $Executable
$process.StartInfo.Arguments = if ($StaticAccessibility) { 'native_accessibility_static_fixture --ignored --nocapture --test-threads=1' } else { '--demo' }
$process.StartInfo.WorkingDirectory = $WorkingDirectory
$process.StartInfo.UseShellExecute = $false
$started = $false
try {
    $started = $process.Start()
    $script:demoProcess = $process.Id
    $script:pill = [IntPtr]::Zero
    $script:settings = [IntPtr]::Zero
    $script:shows = [IntPtr]::Zero
    $callback = [DemoWindows+Callback]{ param($window, $unused)
        [uint32]$owner = 0
        $null = [DemoWindows]::GetWindowThreadProcessId($window, [ref]$owner)
        if ($owner -eq $script:demoProcess) {
            $title = New-Object Text.StringBuilder 256
            $null = [DemoWindows]::GetWindowText($window, $title, 256)
            if ($title.ToString() -eq 'Speakeasy pill') { $script:pill = $window }
            if ($title.ToString() -eq 'Speakeasy Shows') { $script:shows = $window }
            if ($title.ToString() -eq 'Speakeasy' -or $title.ToString() -eq 'Speakeasy accessibility fixture') { $script:settings = $window }
        }
        return $true
    }
    if ($StaticAccessibility) {
        $deadline = [DateTime]::UtcNow.AddSeconds(20)
        while ($script:settings -eq [IntPtr]::Zero -and [DateTime]::UtcNow -lt $deadline) {
            if ($process.HasExited) { throw 'Static accessibility fixture exited before opening' }
            $null = [DemoWindows]::EnumWindows($callback, [IntPtr]::Zero)
            Start-Sleep -Milliseconds 50
        }
        if ($script:settings -eq [IntPtr]::Zero) { throw 'Static accessibility fixture did not open' }
        $script:staticProvider = Test-StaticAccessibility $script:settings
        return
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
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while (($script:shows -eq [IntPtr]::Zero -or ![DemoWindows]::IsWindowVisible($script:shows)) -and [DateTime]::UtcNow -lt $deadline) {
        $null = [DemoWindows]::EnumWindows($callback, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 100
    }
    if ($script:shows -eq [IntPtr]::Zero -or ![DemoWindows]::IsWindowVisible($script:shows)) { throw 'Shows window did not appear' }
    $null = [DemoWindows]::PostMessage($script:shows, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while ([DemoWindows]::IsWindowVisible($script:shows) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
    if ([DemoWindows]::IsWindowVisible($script:shows)) { throw 'Shows did not close' }
    if ($process.HasExited -or ![DemoWindows]::IsWindowVisible($script:settings)) { throw 'Closing Shows also closed Settings' }
    Test-DemoAccessibility $script:settings $script:demoProcess
    Write-Host 'PASS: demo pill stays passive; Shows opens and closes without quitting Settings.'
} finally {
    if ($started -and !$process.HasExited) {
        # A user ends the demo by closing Settings; the pill refuses to close on its own. Settings
        # may open after the pill, so look for it again.
        $null = [DemoWindows]::EnumWindows($callback, [IntPtr]::Zero)
        $null = [DemoWindows]::PostMessage($script:settings, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
        if (!$process.WaitForExit(4000)) {
            $process.Kill(); $process.WaitForExit()
            throw 'Demo did not quit after Settings closed'
        }
    }
    if ($started -and $process.ExitCode -ne 0) { throw 'Owned accessibility process failed' }
    if ($StaticAccessibility -and $null -ne $script:staticProvider) {
        try {
            $bounds = $script:staticProvider.Current.BoundingRectangle
            if (!$bounds.IsEmpty -and $bounds.Width -gt 0 -and $bounds.Height -gt 0) { throw 'Closed provider retained native geometry' }
        } catch [Windows.Automation.ElementNotAvailableException] {}
    }
    $process.Dispose()
    $null = [DemoWindows]::SetThreadDpiAwarenessContext($previousDpi)
}
