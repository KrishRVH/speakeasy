using System.Diagnostics;
using System.Runtime.InteropServices;
using Speakeasy.Core;

namespace Speakeasy.App.Platform;

/// <summary>
/// Invoke on the application's STA UI thread. This never changes the foreground window.
/// Successful insertion means Ctrl+V was delivered; Windows cannot prove the target accepted it.
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
                    snapshot = ClipboardSnapshot.Capture();
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
                    return new InsertionResult(false, "The clipboard kept changing. Your previous clipboard was left untouched; try dictating again.");
            }

            using var payload = ClipboardFormatHandle.FromText(text);
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
                transcriptSequence = ClipboardNative.GetClipboardSequenceNumber();
            }
            finally { ClipboardNative.CloseClipboard(); }

            cancellationToken.ThrowIfCancellationRequested();
            if (!modifiersReleased || AreModifiersDown())
                return new InsertionResult(false, "Transcript copied. Release the modifier keys, then paste with Ctrl+V.");
            if (ClipboardNative.GetClipboardSequenceNumber() != transcriptSequence)
                return new InsertionResult(false, "The clipboard changed before paste. Speakeasy did not paste the replacement contents.");

            var foreground = GetForegroundWindow();
            if (foreground == 0)
                return new InsertionResult(false, "Transcript copied. Focus an editable field and paste with Ctrl+V.");
            if (IsElevatedTarget(foreground))
                return new InsertionResult(false, "Transcript copied. Windows blocks automatic paste into this elevated application; press Ctrl+V there.");

            cancellationToken.ThrowIfCancellationRequested();
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

    private async Task<bool> RestoreIfUnchangedAsync(ClipboardSnapshot snapshot, uint sequence)
    {
        if (_disposed || ClipboardNative.GetClipboardSequenceNumber() != sequence) return false;
        if (!await OpenClipboardAsync(CancellationToken.None)) return false;
        try
        {
            // Check under the clipboard lock: nobody can race a new copy between this
            // comparison and replacement, as could happen with Clipboard.SetDataObject.
            if (ClipboardNative.GetClipboardSequenceNumber() != sequence) return false;
            return snapshot.RestoreWhileOpen();
        }
        finally { ClipboardNative.CloseClipboard(); }
    }

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

    private static Input Key(ushort key, bool keyUp = false) => new()
    {
        Type = 1,
        Data = new InputUnion { Keyboard = new KeyboardInput { VirtualKey = key, Flags = keyUp ? 2u : 0u } }
    };

    [StructLayout(LayoutKind.Sequential)] private struct Input { public uint Type; public InputUnion Data; }
    [StructLayout(LayoutKind.Explicit)]
    private struct InputUnion
    {
        [FieldOffset(0)] public KeyboardInput Keyboard;
        [FieldOffset(0)] public MouseInput Mouse;
    }
    [StructLayout(LayoutKind.Sequential)] private struct KeyboardInput { public ushort VirtualKey, ScanCode; public uint Flags, Time; public nuint ExtraInfo; }
    [StructLayout(LayoutKind.Sequential)] private struct MouseInput { public int X, Y; public uint Data, Flags, Time; public nuint ExtraInfo; }
    [DllImport("user32.dll", SetLastError = true)] private static extern uint SendInput(uint count, Input[] inputs, int size);
    [DllImport("user32.dll")] private static extern short GetAsyncKeyState(int key);
    [DllImport("user32.dll")] private static extern nint GetForegroundWindow();
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(nint window, out uint process);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern nint OpenProcess(uint access, [MarshalAs(UnmanagedType.Bool)] bool inheritHandle, uint pid);
    [DllImport("kernel32.dll")] private static extern nint GetCurrentProcess();
    [DllImport("kernel32.dll")][return: MarshalAs(UnmanagedType.Bool)] private static extern bool CloseHandle(nint handle);
    [DllImport("advapi32.dll", SetLastError = true)][return: MarshalAs(UnmanagedType.Bool)] private static extern bool OpenProcessToken(nint process, uint access, out nint token);
    [DllImport("advapi32.dll", SetLastError = true)][return: MarshalAs(UnmanagedType.Bool)] private static extern bool GetTokenInformation(nint token, int informationClass, out int information, int length, out int returned);
}
