using System.ComponentModel;
using System.Runtime.InteropServices;

namespace Speakeasy.App.Platform;

/// <summary>
/// Installs a physical keyboard hook on a dedicated message-loop thread. Construct on
/// the WinForms UI thread after its synchronization context exists, or supply it explicitly.
/// Down, Up and Escape are posted to that context; no subscriber runs inside the hook.
/// </summary>
public sealed class GlobalHotkey : IDisposable
{
    private readonly SynchronizationContext _context;
    private readonly HotkeyTracker _tracker;
    private readonly Thread _thread;
    private readonly HookProc _callback;
    private readonly TaskCompletionSource _ready = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private nint _hook;
    private uint _threadId;
    private long _enabledState = 1;
    private int _resetPending;
    private volatile bool _disposed;
    private const uint ResetPressedMessage = 0x8001;

    public GlobalHotkey(string shortcut, SynchronizationContext? context = null)
    {
        _tracker = new HotkeyTracker(HotkeyGesture.Parse(shortcut));
        _context = context ?? SynchronizationContext.Current
            ?? throw new InvalidOperationException("Create GlobalHotkey on the running UI thread or supply its SynchronizationContext.");
        _callback = HookCallback;
        _thread = new Thread(RunMessageLoop) { IsBackground = true, Name = "Speakeasy keyboard hook" };
        _thread.Start();
        _ready.Task.GetAwaiter().GetResult();
    }

    public event Action? Down;
    public event Action? Up;
    public event Action? Escape;

    public bool Enabled
    {
        get => (Interlocked.Read(ref _enabledState) & 1) != 0;
        set
        {
            long oldValue, newValue;
            do
            {
                oldValue = Interlocked.Read(ref _enabledState);
                if (((oldValue & 1) != 0) == value) return;
                newValue = ((oldValue + 2) & ~1L) | (value ? 1L : 0L);
            } while (Interlocked.CompareExchange(ref _enabledState, newValue, oldValue) != oldValue);
        }
    }

    /// <summary>
    /// Reconciles keys after a desktop switch or suspend, where releases may be lost.
    /// Cancel the current operation before calling. A cleanup Up is posted to the UI;
    /// physically held keys cannot reactivate until released and pressed again.
    /// </summary>
    public void ResetPressedState()
    {
        if (_disposed) return;
        var alreadyPending = Interlocked.Exchange(ref _resetPending, 1) != 0;
        Interlocked.Add(ref _enabledState, 2); // Invalidate queued activations from the old desktop.
        if (alreadyPending) return;
        if (!PostThreadMessage(_threadId, ResetPressedMessage, 0, 0))
            Interlocked.Exchange(ref _resetPending, 0);
    }

    private void RunMessageLoop()
    {
        try
        {
            _threadId = GetCurrentThreadId();
            // PeekMessage creates the queue before Dispose can post WM_QUIT.
            PeekMessage(out _, 0, 0, 0, 0);
            _tracker.ReconcilePressedKeys(IsPhysicallyPressed);
            _hook = SetWindowsHookEx(13, _callback, GetModuleHandle(null), 0);
            if (_hook == 0) throw new Win32Exception(Marshal.GetLastWin32Error(), "Could not install the dictation keyboard hook.");
            _ready.TrySetResult();
            while (GetMessage(out var message, 0, 0, 0) > 0)
            {
                if (message.Id == ResetPressedMessage)
                {
                    // GetAsyncKeyState is valid here, outside LowLevelKeyboardProc.
                    _tracker.ReconcilePressedKeys(IsPhysicallyPressed);
                    PostNotification(HotkeySignal.Up, Interlocked.Read(ref _enabledState));
                    Interlocked.Exchange(ref _resetPending, 0);
                    continue;
                }
                TranslateMessage(ref message);
                DispatchMessage(ref message);
            }
        }
        catch (Exception ex)
        {
            _ready.TrySetException(ex);
        }
        finally
        {
            if (_hook != 0) UnhookWindowsHookEx(_hook);
            _hook = 0;
        }
    }

    private nint HookCallback(int code, nint wParam, nint lParam)
    {
        if (code >= 0 && !_disposed)
        {
            var message = unchecked((uint)wParam);
            if (message is 0x100 or 0x101 or 0x104 or 0x105)
            {
                var keyboard = Marshal.PtrToStructure<KeyboardHookData>(lParam);
                var enabledState = Interlocked.Read(ref _enabledState);
                var key = keyboard.Key switch
                {
                    0x10 => keyboard.ScanCode == 0x36 ? 0xA1 : 0xA0,
                    0x11 => (keyboard.Flags & 1) != 0 ? 0xA3 : 0xA2,
                    0x12 => (keyboard.Flags & 1) != 0 ? 0xA5 : 0xA4,
                    _ => (int)keyboard.Key
                };
                var signal = _tracker.Process(key, message is 0x100 or 0x104,
                    (keyboard.Flags & 0x12) != 0, (enabledState & 1) != 0 && Volatile.Read(ref _resetPending) == 0);
                var notification = signal & ~HotkeySignal.Suppress;
                if (notification != HotkeySignal.None)
                    PostNotification(notification, enabledState);
                if ((signal & HotkeySignal.Suppress) != 0) return 1;
            }
        }
        // Escape is always forwarded, even when it cancels a live operation.
        return CallNextHookEx(_hook, code, wParam, lParam);
    }

    private void Deliver(object? state)
    {
        var notification = (Notification)state!;
        if (_disposed) return;
        var signal = notification.Signal;
        // Release cleanup must survive a pause or preferences dialog. Posts from the
        // single hook thread stay ordered, so an old Up precedes any subsequent Down.
        if ((signal & HotkeySignal.Up) != 0) Up?.Invoke();
        if (!Enabled || notification.EnabledState != Interlocked.Read(ref _enabledState)) return;
        if ((signal & HotkeySignal.Down) != 0) Down?.Invoke();
        if ((signal & HotkeySignal.Escape) != 0) Escape?.Invoke();
    }

    private void PostNotification(HotkeySignal signal, long enabledState)
    {
        try { _context.Post(Deliver, new Notification(signal, enabledState)); }
        catch (InvalidAsynchronousStateException) { }
        catch (ObjectDisposedException) { }
    }

    private static bool IsPhysicallyPressed(int key) => (GetAsyncKeyState(key) & 0x8000) != 0;

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        Enabled = false;
        if (_threadId != 0) PostThreadMessage(_threadId, 0x12, 0, 0);
        if (_thread != Thread.CurrentThread) _thread.Join(TimeSpan.FromSeconds(2));
        GC.KeepAlive(_callback);
    }

    private delegate nint HookProc(int code, nint wParam, nint lParam);
    private sealed record Notification(HotkeySignal Signal, long EnabledState);
    [StructLayout(LayoutKind.Sequential)]
    private struct KeyboardHookData { public uint Key, ScanCode, Flags, Time; public nuint ExtraInfo; }
    [StructLayout(LayoutKind.Sequential)]
    private struct Message { public nint Window; public uint Id; public nuint WParam; public nint LParam; public uint Time; public int X, Y; public uint Private; }

    [DllImport("user32.dll", SetLastError = true)] private static extern nint SetWindowsHookEx(int id, HookProc callback, nint module, uint threadId);
    [DllImport("user32.dll")][return: MarshalAs(UnmanagedType.Bool)] private static extern bool UnhookWindowsHookEx(nint hook);
    [DllImport("user32.dll")] private static extern nint CallNextHookEx(nint hook, int code, nint wParam, nint lParam);
    [DllImport("user32.dll")] private static extern int GetMessage(out Message message, nint window, uint min, uint max);
    [DllImport("user32.dll")][return: MarshalAs(UnmanagedType.Bool)] private static extern bool PeekMessage(out Message message, nint window, uint min, uint max, uint remove);
    [DllImport("user32.dll")][return: MarshalAs(UnmanagedType.Bool)] private static extern bool TranslateMessage(ref Message message);
    [DllImport("user32.dll")] private static extern nint DispatchMessage(ref Message message);
    [DllImport("user32.dll", SetLastError = true)][return: MarshalAs(UnmanagedType.Bool)] private static extern bool PostThreadMessage(uint threadId, uint message, nuint wParam, nint lParam);
    [DllImport("user32.dll")] private static extern short GetAsyncKeyState(int key);
    [DllImport("kernel32.dll")] private static extern uint GetCurrentThreadId();
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] private static extern nint GetModuleHandle(string? module);
}
