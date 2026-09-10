using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using Speakeasy.Core;
using static Speakeasy.App.Platform.KeyboardInput;

namespace Speakeasy.App.Platform;

/// <summary>
/// Invoke on the application's STA UI thread. This never changes the foreground window.
/// Successful insertion means input was delivered; Windows cannot prove the target accepted it.
/// </summary>
public sealed class ClipboardInserter : ITextInserter, IDisposable
{
    private readonly ClipboardOwner _owner;
    private readonly SemaphoreSlim _insertionGate = new(1, 1);
    private bool _disposed;

    public ClipboardInserter()
    {
        EnsureUiThread();
        _owner = new ClipboardOwner();
    }

    public async Task<InsertionResult> InsertAsync(string text, bool restoreClipboard, int restoreDelayMs, CancellationToken cancellationToken)
    {
        EnsureUiThread();
        ObjectDisposedException.ThrowIf(_disposed, this);
        ArgumentNullException.ThrowIfNull(text);
        if (text.Length == 0) return new InsertionResult(false, "There was no text to insert.");
        await _insertionGate.WaitAsync(cancellationToken);
        ClipboardSnapshot? snapshot = null;
        uint transcriptSequence = 0;
        var pasted = false;
        try
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            cancellationToken.ThrowIfCancellationRequested();
            var modifiersReleased = await WaitForModifiersAsync(cancellationToken);
            cancellationToken.ThrowIfCancellationRequested();

            // Materialize every IDataObject format before replacing anything, so a lazy
            // source object cannot disappear when Windows notifies its old clipboard owner.
            uint? previousSequence = null;
            if (restoreClipboard)
            {
                for (var attempt = 0; attempt < 3; attempt++)
                {
                    var before = ClipboardNative.GetClipboardSequenceNumber();
                    try { snapshot = ClipboardSnapshot.Capture(); }
                    catch (Exception ex) when (ex is ExternalException or NotSupportedException or InvalidOperationException)
                    {
                        // Some Windows metadata cannot be materialized. Keep the entire
                        // clipboard intact and use direct text input for this insertion.
                        return await InsertDirectAsync(text, modifiersReleased, cancellationToken);
                    }
                    var after = ClipboardNative.GetClipboardSequenceNumber();
                    if (before == after)
                    {
                        previousSequence = after;
                        break;
                    }
                    snapshot.Dispose();
                    snapshot = null;
                    await Task.Delay(30, cancellationToken);
                }
                if (snapshot == null)
                    return await InsertDirectAsync(text, modifiersReleased, cancellationToken);
            }

            // CF_UNICODETEXT requires CRLF even when the model returns Unix line endings.
            using var payload = ClipboardFormatHandle.FromText(text.ReplaceLineEndings("\r\n"));
            if (!await OpenClipboardAsync(cancellationToken))
                return new InsertionResult(false, "Another application is holding the clipboard open. Release it and try again.");
            try
            {
                cancellationToken.ThrowIfCancellationRequested();
                if (previousSequence.HasValue && ClipboardNative.GetClipboardSequenceNumber() != previousSequence.Value)
                    return new InsertionResult(false, "The clipboard changed before insertion. Its new contents were preserved.");
                if (!ClipboardNative.EmptyClipboard())
                    return new InsertionResult(false, "Windows could not update the clipboard.");
                if (!payload.TransferToClipboard())
                    return new InsertionResult(false, "Windows could not place the transcript on the clipboard.");
            }
            finally { ClipboardNative.CloseClipboard(); }

            // Closing completes the clipboard write transaction, including Windows' added
            // formats. Sample afterward, and verify ownership so a concurrent copy cannot
            // become the transcript's baseline sequence.
            transcriptSequence = ClipboardNative.GetClipboardSequenceNumber();
            if (!OwnsClipboard(transcriptSequence))
                return new InsertionResult(false, "The clipboard changed before paste. Speakeasy did not paste the replacement contents.");
            cancellationToken.ThrowIfCancellationRequested();
            if (!modifiersReleased || AreModifiersDown())
                return new InsertionResult(false, "Transcript copied. Release the modifier keys, then paste with Ctrl+V.");

            var foreground = GetForegroundWindow();
            if (foreground == 0)
                return new InsertionResult(false, "Transcript copied. Focus an editable field and paste with Ctrl+V.");
            if (IsElevatedTarget(foreground))
                return new InsertionResult(false, "Transcript copied. Windows blocks automatic paste into this elevated application; press Ctrl+V there.");

            cancellationToken.ThrowIfCancellationRequested();
            if (!OwnsClipboard(transcriptSequence))
                return new InsertionResult(false, "The clipboard changed before paste. Speakeasy did not paste the replacement contents.");
            Input[] inputs = [Key(0xA2), Key(0x56), Key(0x56, keyUp: true), Key(0xA2, keyUp: true)];
            var sent = SendInput((uint)inputs.Length, inputs, Marshal.SizeOf<Input>());
            if (sent != inputs.Length)
            {
                if (sent > 0)
                {
                    // Repair only keys this insertion may have pressed, never the user's modifiers.
                    Input[] releases = [Key(0x56, keyUp: true), Key(0xA2, keyUp: true)];
                    SendInput((uint)releases.Length, releases, Marshal.SizeOf<Input>());
                }
                return new InsertionResult(false, "Transcript copied. Windows or this application blocked automatic paste; press Ctrl+V to paste it.");
            }
            pasted = true;

            if (snapshot != null)
            {
                // Once input has been sent, clipboard restoration is cleanup. Cancellation
                // must not abandon the saved clipboard after a successful paste.
                await Task.Delay(Math.Clamp(restoreDelayMs, 200, 10_000));
                var restored = await RestoreIfUnchangedAsync(snapshot, transcriptSequence);
                if (!restored)
                    return new InsertionResult(true, "Paste sent. Clipboard restoration was skipped because its contents changed or another application is using it.");
            }
            return new InsertionResult(true);
        }
        catch (OperationCanceledException)
        {
            if (!pasted && snapshot != null && transcriptSequence != 0)
                await RestoreIfUnchangedAsync(snapshot, transcriptSequence);
            throw;
        }
        catch (Exception ex) when (ex is ExternalException or NotSupportedException or InvalidOperationException)
        {
            return new InsertionResult(pasted, transcriptSequence == 0
                ? $"The clipboard could not be prepared safely: {ex.Message}"
                : "The transcript was copied, but automatic insertion or clipboard restoration failed. Try Ctrl+V.");
        }
        finally
        {
            snapshot?.Dispose();
            _insertionGate.Release();
        }
    }

    private async Task<InsertionResult> InsertDirectAsync(string text, bool modifiersReleased, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        if (!modifiersReleased || AreModifiersDown())
            return new InsertionResult(false, "Release the modifier keys and try dictating again. Your previous clipboard was preserved.");
        var foreground = GetForegroundWindow();
        if (foreground == 0)
            return new InsertionResult(false, "Focus an editable field and try dictating again. Your previous clipboard was preserved.");
        if (IsElevatedTarget(foreground))
            return new InsertionResult(false, "Windows blocks direct text input into this elevated application. Your previous clipboard was preserved.");

        var nativeInsertion = TryInsertNativeEdit(text, foreground, cancellationToken);
        if (nativeInsertion != null) return nativeInsertion;

        // WM_CHAR uses one carriage return per line break. Unicode packets do not
        // synthesize VK_RETURN, so newlines are never sent as an Enter key shortcut.
        var normalized = text.ReplaceLineEndings("\r");
        var insertedAny = false;
        InsertionResult Stopped(string reason) => new(false, insertedAny
            ? $"{reason} Part of the transcript may have been inserted. Your previous clipboard was preserved."
            : $"{reason} Your previous clipboard was preserved.");
        for (var offset = 0; offset < normalized.Length;)
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (_disposed) return Stopped("Speakeasy stopped before insertion finished.");
            if (GetForegroundWindow() != foreground) return Stopped("Focus changed before insertion finished.");
            if (AreModifiersDown()) return Stopped("A modifier key was pressed before insertion finished.");

            // Text services can decode VK_PACKET from keyboard state later. Pace one
            // Unicode scalar at a time so the next packet does not replace that state.
            var inputs = UnicodeScalar(normalized, offset);
            var sent = SendInput((uint)inputs.Length, inputs, Marshal.SizeOf<Input>());
            insertedAny |= sent > 0;
            if (sent != inputs.Length)
            {
                if ((sent & 1) != 0)
                {
                    // Complete only the Unicode key pair partially sent by this batch.
                    Input[] release = [Unicode((char)inputs[sent - 1].Data.Keyboard.ScanCode, keyUp: true)];
                    SendInput(1, release, Marshal.SizeOf<Input>());
                }
                return Stopped("Windows or this application blocked direct text input.");
            }
            offset += inputs.Length / 2;
            if (offset < normalized.Length) await Task.Delay(5, cancellationToken);
        }
        return new InsertionResult(true, "Inserted directly. Your previous clipboard was preserved.");
    }

    private InsertionResult? TryInsertNativeEdit(string text, nint foreground, CancellationToken cancellationToken)
    {
        var thread = GetWindowThreadProcessId(foreground, out _);
        var info = new GuiThreadInfo { Size = (uint)Marshal.SizeOf<GuiThreadInfo>() };
        if (thread == 0 || !GetGUIThreadInfo(thread, ref info) || info.Focus == 0) return null;
        var target = info.Focus;
        var className = new StringBuilder(256);
        if (GetClassName(target, className, className.Capacity) == 0 || !IsWindowUnicode(target)) return null;
        var name = className.ToString();
        if (!name.Equals("Edit", StringComparison.OrdinalIgnoreCase) &&
            !name.StartsWith("RichEdit", StringComparison.OrdinalIgnoreCase) &&
            !name.StartsWith("WindowsForms10.EDIT.", StringComparison.OrdinalIgnoreCase) &&
            !name.StartsWith("WindowsForms10.RichEdit", StringComparison.OrdinalIgnoreCase)) return null;

        cancellationToken.ThrowIfCancellationRequested();
        if (_disposed || GetForegroundWindow() != foreground || !GetGUIThreadInfo(thread, ref info) ||
            info.Focus != target || GetAncestor(target, 2) != foreground || (info.Flags & 0x1C) != 0 || AreModifiersDown())
            return new InsertionResult(false, "Focus or keyboard state changed before insertion. Your previous clipboard was preserved.");
        if (!IsWindowEnabled(target) || (GetWindowLong(target, -16) & 0x800) != 0)
            return new InsertionResult(false, "The focused text field is disabled or read-only. Your previous clipboard was preserved.");

        // EM_REPLACESEL inserts at the caret (or replaces the selection) with undo.
        // It is a system message, so Windows marshals the UTF-16 text across processes.
        cancellationToken.ThrowIfCancellationRequested();
        var delivered = SendMessageTimeout(target, 0x00C2, 1, text.ReplaceLineEndings("\r\n"),
            0x0001 | 0x0002 | 0x0020, 250, out _);
        // A timeout can occur after insertion began. Never retry with another method.
        return delivered != 0
            ? new InsertionResult(true, "Inserted directly. Your previous clipboard was preserved.")
            : new InsertionResult(false, "Windows could not confirm text insertion. Check the field before trying again; your previous clipboard was preserved.");
    }

    private async Task<bool> RestoreIfUnchangedAsync(ClipboardSnapshot snapshot, uint sequence)
    {
        if (!OwnsClipboard(sequence)) return false;
        if (!await OpenClipboardAsync(CancellationToken.None)) return false;
        try
        {
            // Check under the clipboard lock: nobody can race a new copy between this
            // comparison and replacement, as could happen with Clipboard.SetDataObject.
            if (!OwnsClipboard(sequence)) return false;
            return snapshot.RestoreWhileOpen();
        }
        finally { ClipboardNative.CloseClipboard(); }
    }

    private bool OwnsClipboard(uint sequence) => !_disposed &&
        ClipboardNative.GetClipboardSequenceNumber() == sequence &&
        ClipboardNative.GetClipboardOwner() == _owner.Handle;

    private async Task<bool> OpenClipboardAsync(CancellationToken cancellationToken)
    {
        for (var attempt = 0; attempt < 12; attempt++)
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (_disposed) return false;
            if (ClipboardNative.OpenClipboard(_owner.Handle)) return true;
            await Task.Delay(25, cancellationToken);
        }
        return false;
    }

    private static async Task<bool> WaitForModifiersAsync(CancellationToken cancellationToken)
    {
        var timer = Stopwatch.StartNew();
        while (AreModifiersDown())
        {
            if (timer.Elapsed > TimeSpan.FromSeconds(3)) return false;
            await Task.Delay(15, cancellationToken);
        }
        return true;
    }

    private static bool AreModifiersDown() =>
        IsDown(0xA0) || IsDown(0xA1) || IsDown(0xA2) || IsDown(0xA3) ||
        IsDown(0xA4) || IsDown(0xA5) || IsDown(0x5B) || IsDown(0x5C);

    private static bool IsDown(int key) => (GetAsyncKeyState(key) & 0x8000) != 0;

    private static bool IsElevatedTarget(nint window)
    {
        GetWindowThreadProcessId(window, out var pid);
        var process = OpenProcess(0x1000, false, pid);
        if (process == 0) return false;
        try { return IsElevated(process) && !IsElevated(GetCurrentProcess()); }
        finally { CloseHandle(process); }
    }

    private static bool IsElevated(nint process)
    {
        if (!OpenProcessToken(process, 0x0008, out var token)) return false;
        try { return GetTokenInformation(token, 20, out var elevated, sizeof(int), out _) && elevated != 0; }
        finally { CloseHandle(token); }
    }

    private static void EnsureUiThread()
    {
        if (Thread.CurrentThread.GetApartmentState() != ApartmentState.STA || SynchronizationContext.Current == null)
            throw new InvalidOperationException("Clipboard insertion must run on the application's STA UI thread with a synchronization context.");
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _owner.DestroyHandle();
    }

    private sealed class ClipboardOwner : NativeWindow
    {
        internal ClipboardOwner() => CreateHandle(new CreateParams { Caption = "Speakeasy clipboard", Parent = new nint(-3) });
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct GuiThreadInfo
    {
        internal uint Size, Flags;
        internal nint Active, Focus, Capture, MenuOwner, MoveSize, Caret;
        internal int CaretLeft, CaretTop, CaretRight, CaretBottom;
    }

    [DllImport("user32.dll", SetLastError = true)] private static extern uint SendInput(uint count, Input[] inputs, int size);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)] private static extern nint SendMessageTimeout(nint window, uint message, nuint wParam, string text, uint flags, uint timeoutMs, out nuint result);
    [DllImport("user32.dll")][return: MarshalAs(UnmanagedType.Bool)] private static extern bool GetGUIThreadInfo(uint thread, ref GuiThreadInfo info);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetClassName(nint window, StringBuilder name, int maxCount);
    [DllImport("user32.dll")][return: MarshalAs(UnmanagedType.Bool)] private static extern bool IsWindowUnicode(nint window);
    [DllImport("user32.dll")][return: MarshalAs(UnmanagedType.Bool)] private static extern bool IsWindowEnabled(nint window);
    [DllImport("user32.dll")] private static extern nint GetAncestor(nint window, uint flags);
    [DllImport("user32.dll", EntryPoint = "GetWindowLongW")] private static extern int GetWindowLong(nint window, int index);
    [DllImport("user32.dll")] private static extern short GetAsyncKeyState(int key);
    [DllImport("user32.dll")] private static extern nint GetForegroundWindow();
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(nint window, out uint process);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern nint OpenProcess(uint access, [MarshalAs(UnmanagedType.Bool)] bool inheritHandle, uint pid);
    [DllImport("kernel32.dll")] private static extern nint GetCurrentProcess();
    [DllImport("kernel32.dll")][return: MarshalAs(UnmanagedType.Bool)] private static extern bool CloseHandle(nint handle);
    [DllImport("advapi32.dll", SetLastError = true)][return: MarshalAs(UnmanagedType.Bool)] private static extern bool OpenProcessToken(nint process, uint access, out nint token);
    [DllImport("advapi32.dll", SetLastError = true)][return: MarshalAs(UnmanagedType.Bool)] private static extern bool GetTokenInformation(nint token, int informationClass, out int information, int length, out int returned);
}
