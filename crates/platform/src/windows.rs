//! Windows adapter: the Ctrl+Win keyboard hook, session-change cancellation, GPUI window
//! presentation, the file dialog, and clipboard or direct text insertion.
//!
//! The hook thread creates and removes every native resource its callbacks reach.

use std::{
    cell::RefCell,
    ffi::OsString,
    io,
    num::NonZeroU32,
    ops::ControlFlow,
    os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    panic,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, bail};
use async_channel::Sender;
use raw_window_handle::RawWindowHandle;
use windows_sys::{
    Win32::{
        Foundation::{
            ERROR_CLASS_ALREADY_EXISTS, FALSE, GetLastError, HANDLE, HWND, LPARAM, LRESULT, RECT,
            TRUE, WAIT_FAILED, WAIT_OBJECT_0, WPARAM,
        },
        Graphics::{
            Dwm::{
                DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE,
                DWMWCP_DONOTROUND, DwmSetWindowAttribute,
            },
            Gdi::{
                BeginPaint, EndPaint, GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
                MonitorFromWindow, PAINTSTRUCT,
            },
        },
        System::{
            Com::{
                COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
            },
            LibraryLoader::GetModuleHandleW,
            RemoteDesktop::{
                NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
                WTSUnRegisterSessionNotification,
            },
            Threading::{
                CreateEventW, GetCurrentProcessId, GetCurrentThreadId, INFINITE, SetEvent,
            },
        },
        UI::{
            Controls::Dialogs::{
                CommDlgExtendedError, GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST,
                OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
            },
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
                KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN,
                VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
            },
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{
                CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                GWL_EXSTYLE, GWL_STYLE, GetForegroundWindow, GetWindowLongPtrW,
                GetWindowThreadProcessId, HHOOK, HWND_TOPMOST, IsWindowVisible, KBDLLHOOKSTRUCT,
                LWA_ALPHA, MB_ICONERROR, MB_OK, MSG, MWMO_INPUTAVAILABLE, MessageBoxW,
                MsgWaitForMultipleObjectsEx, PBT_APMSUSPEND, PM_NOREMOVE, PM_REMOVE, PeekMessageW,
                PostThreadMessageW, QS_ALLINPUT, RegisterClassW, SC_MINIMIZE, SIZE_MINIMIZED,
                SPI_GETCLIENTAREAANIMATION, SW_HIDE, SW_RESTORE, SW_SHOWNOACTIVATE,
                SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow,
                SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, SetWindowsHookExW,
                ShowWindow, SystemParametersInfoW, TranslateMessage, UnhookWindowsHookEx,
                WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_NCDESTROY, WM_PAINT, WM_POWERBROADCAST,
                WM_QUERYENDSESSION, WM_QUIT, WM_SIZE, WM_SYSCOMMAND, WM_SYSKEYDOWN,
                WM_WTSSESSION_CHANGE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
                WS_EX_TRANSPARENT, WS_OVERLAPPEDWINDOW, WS_POPUP, WTS_CONSOLE_DISCONNECT,
                WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_LOGOFF,
            },
        },
    },
    core::BOOL,
    w,
};

use super::{
    CANCEL_SHORTCUT, Delivery, Input, InputSender, InsertPermit, Inserted, OwnedThread,
    PILL_HEIGHT, PILL_MARGIN, PILL_WIDTH, SHORTCUT, keyboard, monitor::MonitorControl,
};

/// Tags input Speakeasy synthesizes, so its own keyboard hook ignores it.
const OWN_INPUT: usize = 0x5350_4541;
/// Posted by the keyboard hook so the message loop, not the hook callback, masks Start.
const MASK_START: u32 = WM_APP + 1;
/// The DPI at which one device-independent pixel is one physical pixel.
const BASE_DPI: u32 = 96;
/// A virtual key Windows leaves unassigned.
const UNASSIGNED_KEY: VIRTUAL_KEY = 0xE8;
/// The extended-length path limit: 32,767 UTF-16 units plus NUL.
const LONGEST_PATH: usize = 32_768;
/// The byte size of the 32-bit values the pill's DWM attributes take.
const DWM_VALUE_SIZE: u32 = 4;

thread_local! {
    #[expect(
        clippy::disallowed_types,
        reason = "The context-free Windows hook mutates state confined to its own native thread; callbacks never share the UI owner"
    )]
    static HOOK: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

/// Owns the desktop observation thread until explicit stop and acknowledged cleanup.
pub struct InputMonitor {
    thread: OwnedThread,
    // Shared with the hook thread, which cannot receive owner messages while it waits natively.
    control: Arc<MonitorControl<()>>,
    // Shared so a stop can end the hook thread's wait; the event closes once both release it.
    wake: Arc<OwnedHandle>,
}

impl InputMonitor {
    pub(super) fn start(input: InputSender) -> anyhow::Result<Self> {
        let control = Arc::new(MonitorControl::default());
        let wake = Arc::new(manual_reset_event()?);
        let thread = OwnedThread::spawn("input-monitor", {
            let control = Arc::clone(&control);
            let wake = Arc::clone(&wake);
            move || monitor(&input, &control, &wake)
        })?;
        Ok(Self {
            thread,
            control,
            wake,
        })
    }

    /// Requests native observation shutdown without joining its thread.
    pub fn request_stop(&self) {
        self.control.request_stop(|()| {
            // SAFETY: the shared owning handle stays open through native exit; setting a
            // manual-reset event wakes a running or future wait.
            unsafe {
                SetEvent(self.wake.as_raw_handle());
            }
        });
    }

    /// Resolves once native resources have retired; await it before replacing or dropping the
    /// monitor.
    pub fn stopped(&self) -> impl Future<Output = ()> + use<> {
        self.thread.exited()
    }
}

impl Drop for InputMonitor {
    fn drop(&mut self) {
        self.request_stop();
    }
}

struct HookState {
    input: InputSender,
    policy: keyboard::Windows,
}

impl HookState {
    /// Delivers the policy's decision for one key event and returns whether to swallow it.
    fn observe(&mut self, key: u32, down: bool) -> bool {
        // GetAsyncKeyState does not reflect this event until the hook returns.
        let physical_before_event = keyboard::Windows::CHORD_KEYS.map(is_key_down);
        let decision = self.policy.observe(key, down, physical_before_event);
        decision.deliver(&self.input);
        if decision.starts() {
            // SAFETY: posts a value-only message to this hook's own thread.
            unsafe {
                PostThreadMessageW(GetCurrentThreadId(), MASK_START, 0, 0);
            }
        }
        decision.swallows()
    }
}

struct KeyboardHook(HHOOK);

impl KeyboardHook {
    fn install() -> io::Result<Self> {
        // SAFETY: keyboard_hook has the WH_KEYBOARD_LL signature and lives for the process.
        let hook = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_hook),
                GetModuleHandleW(null()),
                0,
            )
        };
        if hook.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(hook))
    }
}

impl Drop for KeyboardHook {
    fn drop(&mut self) {
        // SAFETY: this thread installed the hook and removes it exactly once.
        unsafe {
            UnhookWindowsHookEx(self.0);
        }
    }
}

/// A hidden window that receives session, shutdown, and power notifications.
///
/// Top-level rather than message-only: message-only windows miss the `WM_QUERYENDSESSION` and
/// `WM_POWERBROADCAST` broadcasts.
struct LifecycleWindow(HWND);

impl LifecycleWindow {
    fn create() -> io::Result<Self> {
        let class_name = w!("SpeakeasyEvents");
        // SAFETY: a null module name selects this executable.
        let instance = unsafe { GetModuleHandleW(null()) };
        let class = WNDCLASSW {
            lpfnWndProc: Some(lifecycle_window),
            hInstance: instance,
            lpszClassName: class_name,
            ..Default::default()
        };
        // SAFETY: the class names a static string and a procedure that live for the process.
        let registered = unsafe {
            RegisterClassW(&raw const class) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS
        };
        if !registered {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the class is registered, and every pointer argument is static or null.
        let window = unsafe {
            CreateWindowExW(
                0,
                class_name,
                class_name,
                0,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };
        if window.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(window))
    }
}

impl Drop for LifecycleWindow {
    fn drop(&mut self) {
        // SAFETY: this thread created the window and destroys it exactly once.
        unsafe {
            DestroyWindow(self.0);
        }
    }
}

/// Session-change notifications for a lifecycle window, unregistered before it is destroyed.
struct SessionNotifications<'window>(&'window LifecycleWindow);

impl<'window> SessionNotifications<'window> {
    fn register(window: &'window LifecycleWindow) -> io::Result<Self> {
        // SAFETY: the borrowed window stays live until this registration is dropped.
        ok_or_last_error(unsafe {
            WTSRegisterSessionNotification(window.0, NOTIFY_FOR_THIS_SESSION)
        })?;
        Ok(Self(window))
    }
}

impl Drop for SessionNotifications<'_> {
    fn drop(&mut self) {
        // SAFETY: registration succeeded for this still-live window.
        unsafe {
            WTSUnRegisterSessionNotification(self.0.0);
        }
    }
}

/// An unsignaled manual-reset event: once set, it releases the current wait and every later one.
fn manual_reset_event() -> io::Result<OwnedHandle> {
    // SAFETY: null attributes and name create an unnamed event whose handle nothing else owns.
    unsafe { take_handle(CreateEventW(null(), TRUE, FALSE, null())) }
}

fn monitor(input: &InputSender, control: &MonitorControl<()>, wake: &OwnedHandle) {
    HOOK.set(Some(HookState {
        input: input.clone(),
        policy: keyboard::Windows::default(),
    }));
    if let Err(error) = run_event_loop(input, control, wake)
        && !control.stopping()
        && !input.is_closed()
    {
        input.deliver(Input::Unavailable(format!(
            "Cannot monitor the dictation shortcut: {error}. Pause and resume dictation to try again."
        )));
    }
    control.retire();
    drop(HOOK.take());
    input.close();
}

fn run_event_loop(
    input: &InputSender,
    control: &MonitorControl<()>,
    wake: &OwnedHandle,
) -> io::Result<()> {
    if control.stopping() {
        return Ok(());
    }
    create_message_queue();
    let _hook = KeyboardHook::install()?;
    let window = LifecycleWindow::create()?;
    let _notifications = SessionNotifications::register(&window)?;
    let ready = || {
        input.deliver(Input::DesktopReady {
            shortcut: SHORTCUT.into(),
            cancel: CANCEL_SHORTCUT.into(),
        });
    };
    if control.start((), ready) {
        pump_messages(control, wake)
    } else {
        Ok(())
    }
}

/// Peeking makes Windows create this thread's queue before the hook posts to it.
fn create_message_queue() {
    let mut message = MSG::default();
    // SAFETY: PeekMessageW writes only this live local.
    unsafe {
        PeekMessageW(&raw mut message, null_mut(), 0, 0, PM_NOREMOVE);
    }
}

/// Runs the message loop until stop, `WM_QUIT`, or the wake event; stop wins even under continuous
/// input.
fn pump_messages(control: &MonitorControl<()>, wake: &OwnedHandle) -> io::Result<()> {
    let wake = [wake.as_raw_handle()];
    while dispatch_queued_messages(control).is_continue() && !control.stopping() {
        // SAFETY: the one-handle array outlives this synchronous wait.
        let woken = unsafe {
            MsgWaitForMultipleObjectsEx(
                1,
                wake.as_ptr(),
                INFINITE,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };
        match woken {
            WAIT_OBJECT_0 => break,
            WAIT_FAILED => return Err(io::Error::last_os_error()),
            _ => {},
        }
    }
    Ok(())
}

fn dispatch_queued_messages(control: &MonitorControl<()>) -> ControlFlow<()> {
    let mut message = MSG::default();
    // SAFETY: PeekMessageW writes only this live local.
    while unsafe { PeekMessageW(&raw mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
        if control.stopping() || message.message == WM_QUIT {
            return ControlFlow::Break(());
        }
        if message.message == MASK_START {
            mask_start_menu();
        } else {
            // SAFETY: the message came from this thread's queue and outlives both calls.
            unsafe {
                TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
        }
    }
    ControlFlow::Continue(())
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // A panic must not unwind into the OS. Closing the input makes the owner cancel rather than
    // continue with lost key edges.
    let swallow = code >= 0
        && panic::catch_unwind(|| {
            // SAFETY: for nonnegative WH_KEYBOARD_LL callbacks lparam points to a KBDLLHOOKSTRUCT
            // valid for this invocation.
            let event = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            let down = wparam == WM_KEYDOWN as usize || wparam == WM_SYSKEYDOWN as usize;
            event.dwExtraInfo != OWN_INPUT
                && HOOK.with_borrow_mut(|slot| {
                    slot.as_mut()
                        .is_some_and(|state| state.observe(event.vkCode, down))
                })
        })
        .unwrap_or_else(|_| {
            HOOK.with(|slot| {
                if let Ok(state) = slot.try_borrow()
                    && let Some(state) = state.as_ref()
                {
                    state.input.close();
                }
            });
            false
        });
    if swallow {
        return 1;
    }
    // SAFETY: forward the original callback arguments, including Escape.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn lifecycle_window(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if interrupts_dictation(message, wparam) {
        HOOK.with(|slot| {
            if let Ok(mut state) = slot.try_borrow_mut()
                && let Some(state) = state.as_mut()
            {
                state.policy.interrupt(&state.input);
            }
        });
    }
    // SAFETY: forward unchanged arguments for this registered native window.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn interrupts_dictation(message: u32, wparam: WPARAM) -> bool {
    match message {
        WM_WTSSESSION_CHANGE => u32::try_from(wparam).is_ok_and(|change| {
            matches!(
                change,
                WTS_CONSOLE_DISCONNECT
                    | WTS_REMOTE_DISCONNECT
                    | WTS_SESSION_LOGOFF
                    | WTS_SESSION_LOCK
            )
        }),
        WM_QUERYENDSESSION => true,
        WM_POWERBROADCAST => wparam == PBT_APMSUSPEND as usize,
        _ => false,
    }
}

fn is_key_down(key: VIRTUAL_KEY) -> bool {
    // SAFETY: GetAsyncKeyState takes a key code by value and retains nothing.
    unsafe { GetAsyncKeyState(i32::from(key)) < 0 }
}

pub(super) fn modifiers_down() -> bool {
    [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
        .into_iter()
        .any(is_key_down)
}

/// Releasing Win without another key opens Start. An unassigned key while the chord is held marks
/// Win as used, as other shortcut tools do.
fn mask_start_menu() {
    let input = [
        synthetic_key(UNASSIGNED_KEY, 0, 0),
        synthetic_key(UNASSIGNED_KEY, 0, KEYEVENTF_KEYUP),
    ];
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Marking Win as used is a best-effort shell hint; blocked synthetic input must not stop dictation"
    )]
    let _ = send_input(&input);
}

/// Configures an owned UI-thread window for nonactivating, click-through presentation.
///
/// # Errors
/// Returns an error for the wrong window kind or failed native configuration.
pub fn configure_pill(handle: RawWindowHandle) -> anyhow::Result<()> {
    let hwnd = hwnd_of(handle)?;
    let border = DWMWA_COLOR_NONE;
    let corners = DWMWCP_DONOTROUND;
    // SAFETY: the caller holds a live GPUI window on its owning UI thread, and both DWM values
    // outlive their synchronous calls. The window owns its subclass until WM_NCDESTROY on this same
    // thread.
    unsafe {
        // Windows 11 outlines and shadows GPUI's overlapped frame around the whole transparent
        // surface, so the pill becomes a plain popup; the DWM decoration hints are best effort.
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            (style & !(WS_OVERLAPPEDWINDOW.cast_signed() as isize))
                | WS_POPUP.cast_signed() as isize,
        );
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR.cast_unsigned(),
            (&raw const border).cast(),
            DWM_VALUE_SIZE,
        );
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE.cast_unsigned(),
            (&raw const corners).cast(),
            DWM_VALUE_SIZE,
        );
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            style
                | (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_LAYERED)
                    .cast_signed() as isize,
        );
        // WS_EX_TRANSPARENT alone only changes paint order; layering makes the whole pill pass
        // mouse hit testing.
        ok_or_last_error(SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA))?;
        ok_or_last_error(SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        ))?;
        ok_or_last_error(SetWindowSubclass(hwnd, Some(pill_window), 1, 0))?;
    }
    Ok(())
}

unsafe extern "system" fn pill_window(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    // SAFETY: this window-owned subclass runs on the pill's UI thread and is removed at
    // destruction. PAINTSTRUCT lives through its paint cycle.
    unsafe {
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(pill_window), id);
        } else if message == WM_PAINT && IsWindowVisible(hwnd) == 0 {
            // ValidateRect alone leaves a hidden window's paint pending; finish the cycle without
            // drawing or calling GPUI.
            let mut paint = PAINTSTRUCT::default();
            BeginPaint(hwnd, &raw mut paint);
            EndPaint(hwnd, &raw const paint);
            return 0;
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

/// Shows or hides the owned pill without activating it; call on its UI thread.
pub fn set_pill_visible(handle: RawWindowHandle, visible: bool) {
    let Ok(hwnd) = hwnd_of(handle) else {
        return;
    };
    if visible {
        place_pill(hwnd);
    }
    // SAFETY: called with a live window handle on its UI thread.
    unsafe {
        ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
    }
}

/// Bottom-centers the pill on the work area of the foreground window's monitor.
fn place_pill(hwnd: HWND) {
    let Ok(info_size) = u32::try_from(size_of::<MONITORINFO>()) else {
        return;
    };
    let mut info = MONITORINFO {
        cbSize: info_size,
        ..Default::default()
    };
    let (mut dpi, mut vertical_dpi) = (BASE_DPI, BASE_DPI);
    // SAFETY: every out-parameter is a live local that outlives these synchronous calls.
    let found = unsafe {
        let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
        GetDpiForMonitor(
            monitor,
            MDT_EFFECTIVE_DPI,
            &raw mut dpi,
            &raw mut vertical_dpi,
        );
        GetMonitorInfoW(monitor, &raw mut info) != 0
    };
    if !found {
        return;
    }
    let scaled = |logical: u32| {
        u64::from(logical)
            .saturating_mul(u64::from(dpi))
            .checked_div(u64::from(BASE_DPI))
            .and_then(|physical| i32::try_from(physical).ok())
            .unwrap_or(i32::MAX)
    };
    let width = scaled(PILL_WIDTH.into());
    let height = scaled(PILL_HEIGHT.into());
    let top = info
        .rcWork
        .bottom
        .saturating_sub(height)
        .saturating_sub(scaled(PILL_MARGIN.into()));
    let left = centered_left(info.rcWork, width);
    // SAFETY: called with a live window handle on its UI thread.
    unsafe {
        SetWindowPos(hwnd, HWND_TOPMOST, left, top, width, height, SWP_NOACTIVATE);
    }
}

/// The left edge that centers `width` in `area`, computed wide because monitors may sit at either
/// end of the signed desktop coordinate range.
fn centered_left(area: RECT, width: i32) -> i32 {
    let left = i64::from(area.left)
        .saturating_add(i64::from(area.right))
        .saturating_sub(i64::from(width))
        / 2;
    i32::try_from(left).unwrap_or(if left < 0 { i32::MIN } else { i32::MAX })
}

/// Makes minimizing the Settings window hide it to the tray; install once per window.
///
/// The window owns the callback context until `WM_NCDESTROY`; callbacks only enqueue work, avoiding
/// reentrant GPUI updates.
///
/// # Errors
/// Returns an error if the window is invalid or callback installation fails.
pub fn minimize_to_tray(handle: RawWindowHandle, hide: Sender<()>) -> anyhow::Result<()> {
    let hwnd = hwnd_of(handle)?;
    let context = Box::into_raw(Box::new(hide));
    // SAFETY: the live window belongs to this UI thread. Ownership transfers to settings_window on
    // success, or is reclaimed immediately on failure.
    unsafe {
        if SetWindowSubclass(hwnd, Some(settings_window), 1, context as usize) == 0 {
            drop(Box::from_raw(context));
            bail!("Cannot enable minimize to tray. Restart Speakeasy.");
        }
    }
    Ok(())
}

unsafe extern "system" fn settings_window(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    context: usize,
) -> LRESULT {
    // SAFETY: SetWindowSubclass stores this boxed sender for this window only. Remove the callback
    // before freeing its context at final destruction.
    unsafe {
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(settings_window), id);
            drop(Box::from_raw(context as *mut Sender<()>));
        } else if message == WM_SYSCOMMAND && wparam & 0xFFF0 == SC_MINIMIZE as usize {
            // WM_SYSCOMMAND reserves wParam's low four bits for the system.
            request_hide(context);
            return 0;
        } else if message == WM_SIZE && wparam == SIZE_MINIMIZED as usize {
            // ShowWindow(SW_MINIMIZE), including GPUI's own minimize, bypasses WM_SYSCOMMAND. Still
            // forward the size so GPUI tracks the restore.
            request_hide(context);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

/// Asks the Settings owner to hide its window to the tray.
///
/// # Safety
/// `context` is the boxed sender `minimize_to_tray` installed, not yet freed.
unsafe fn request_hide(context: usize) {
    // SAFETY: the caller guarantees the boxed sender is live.
    let hide = unsafe { &*(context as *const Sender<()>) };
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Minimize notifications coalesce in a one-slot lane; a closed lane belongs to a destroyed settings owner"
    )]
    let _ = hide.try_send(());
}

/// Shows or hides the owned Settings window; call on its UI thread with GPUI's borrows released,
/// because `ShowWindow` sends messages that reach GPUI.
pub fn set_settings_visible(handle: RawWindowHandle, visible: bool) {
    let Ok(hwnd) = hwnd_of(handle) else {
        return;
    };
    // SAFETY: the caller passes a live Settings window on its owning UI thread.
    unsafe {
        ShowWindow(hwnd, if visible { SW_RESTORE } else { SW_HIDE });
        if visible {
            SetForegroundWindow(hwnd);
        }
    }
}

/// Asks for one existing file, offering `filter` (a name and pattern) before all files, without
/// blocking the UI owner.
///
/// The dialog runs its own message loop on a dedicated thread; shown from GPUI's UI thread, it
/// stays unpainted while GPUI is idle.
///
/// # Errors
/// Returns native dialog or worker failures; cancellation returns no path.
pub fn choose_file(
    owner: RawWindowHandle,
    title: &str,
    filter: [&str; 2],
) -> impl Future<Output = anyhow::Result<Option<PathBuf>>> + Send + use<> {
    // A raw HWND is not Send, so the dialog thread receives its value.
    let owner = hwnd_of(owner).map(|owner| owner as isize);
    let title = to_wide(title);
    let [name, pattern] = filter;
    // The empty entry and to_wide's NUL end the list with the double NUL lpstrFilter requires.
    let filter = to_wide(&[name, pattern, "All files", "*.*", ""].join("\0"));
    async move {
        let owner = owner?;
        let (reply, selection) = async_channel::bounded(1);
        // Detached: the modal dialog cannot be cancelled, so dropping the future must not join it;
        // process exit reclaims the thread.
        thread::Builder::new()
            .name("file-dialog".into())
            .spawn(move || {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A closed completion lane means the settings owner no longer needs the dialog result"
                )]
                let _ = reply.send_blocking(open_file(owner as HWND, &title, &filter));
            })?;
        selection.recv().await?
    }
}

fn open_file(owner: HWND, title: &[u16], filter: &[u16]) -> anyhow::Result<Option<PathBuf>> {
    let mut file = vec![0_u16; LONGEST_PATH];
    let mut dialog = OPENFILENAMEW {
        lStructSize: u32::try_from(size_of::<OPENFILENAMEW>())?,
        hwndOwner: owner,
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrFile: file.as_mut_ptr(),
        nMaxFile: u32::try_from(file.len())?,
        lpstrTitle: title.as_ptr(),
        Flags: OFN_EXPLORER
            | OFN_FILEMUSTEXIST
            | OFN_PATHMUSTEXIST
            | OFN_HIDEREADONLY
            | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: this thread owns the dialog's apartment and message loop, and every buffer in
    // `dialog` outlives the synchronous call.
    let error = unsafe {
        let apartment = CoInitializeEx(
            null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).cast_unsigned(),
        );
        let error = if GetOpenFileNameW(&raw mut dialog) == 0 {
            CommDlgExtendedError()
        } else {
            0
        };
        if apartment >= 0 {
            CoUninitialize();
        }
        error
    };
    if error != 0 {
        bail!("The file dialog failed with code {error:#x}");
    }
    Ok(file
        .split(|&unit| unit == 0)
        .next()
        .filter(|path| !path.is_empty())
        .map(|path| OsString::from_wide(path).into()))
}

/// Returns whether the system asks apps to reduce motion; Linux uses the app setting.
#[must_use]
pub fn reduced_motion() -> bool {
    let mut animates = TRUE;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes a BOOL into this live variable.
    unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut animates).cast(), 0);
    }
    animates == FALSE
}

/// Presents a local startup error; callers never include audio, transcripts, or credentials.
pub fn show_error(message: &str) {
    let text = to_wide(message);
    // SAFETY: both NUL-terminated strings outlive this synchronous dialog.
    unsafe {
        MessageBoxW(
            null_mut(),
            text.as_ptr(),
            w!("Speakeasy"),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub(super) fn insert(
    text: &str,
    permit: &InsertPermit,
    delivery: Delivery,
) -> anyhow::Result<Inserted> {
    if !permit.active() {
        return Ok(Inserted::Cancelled);
    }
    match delivery {
        Delivery::Paste => paste(text, permit),
        Delivery::Direct => insert_direct(text, permit),
    }
}

fn paste(text: &str, permit: &InsertPermit) -> anyhow::Result<Inserted> {
    let text = text.replace("\r\n", "\n").replace('\n', "\r\n");
    {
        let _clipboard = open_clipboard()?;
        if !permit.active() {
            return Ok(Inserted::Cancelled);
        }
        clipboard_win::raw::set_string(&text)?;
    }
    let sequence = clipboard_win::raw::seq_num();
    if !clipboard_holds(&text, sequence)? {
        return Ok(Inserted::Unavailable(
            "Clipboard changed. New contents were preserved; paste was not sent.",
        ));
    }
    if let Some(outcome) = Delivery::Paste.preflight(has_external_target(), !modifiers_down()) {
        return Ok(outcome);
    }
    if sequence != clipboard_win::raw::seq_num() {
        return Ok(Inserted::Unavailable(
            "Clipboard changed before paste. New contents were preserved.",
        ));
    }
    if !permit.commit() {
        return Ok(Inserted::Cancelled);
    }
    send_paste_shortcut()
}

fn clipboard_holds(text: &str, sequence: Option<NonZeroU32>) -> anyhow::Result<bool> {
    let _clipboard = open_clipboard()?;
    let mut current = Vec::new();
    clipboard_win::raw::get_string(&mut current)?;
    Ok(current == text.as_bytes() && sequence == clipboard_win::raw::seq_num())
}

fn send_paste_shortcut() -> anyhow::Result<Inserted> {
    let press = |key| synthetic_key(key, 0, 0);
    let release = |key| synthetic_key(key, 0, KEYEVENTF_KEYUP);
    let shortcut = [
        press(VK_CONTROL),
        press(VK_V),
        release(VK_V),
        release(VK_CONTROL),
    ];
    let sent = send_input(&shortcut)?;
    if sent == shortcut.len() {
        return Ok(Inserted::Sent);
    }
    if sent > 0 {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Release stuck synthetic modifiers after partial input even if the target also blocks cleanup"
        )]
        let _ = send_input(&[release(VK_V), release(VK_CONTROL)]);
    }
    Ok(Inserted::Copied(
        "Copied. This app blocked paste; press Ctrl+V.",
    ))
}

fn insert_direct(text: &str, permit: &InsertPermit) -> anyhow::Result<Inserted> {
    let input: Vec<INPUT> = text
        .replace("\r\n", "\n")
        .encode_utf16()
        .flat_map(|unit| {
            [
                synthetic_key(0, unit, KEYEVENTF_UNICODE),
                synthetic_key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
            ]
        })
        .collect();
    if let Some(outcome) = Delivery::Direct.preflight(has_external_target(), !modifiers_down()) {
        return Ok(outcome);
    }
    if !permit.commit() {
        return Ok(Inserted::Cancelled);
    }
    let sent = send_input(&input)?;
    if sent != input.len() {
        return Ok(Inserted::Unavailable(
            "Direct input was blocked or only partly sent. Clipboard preserved.",
        ));
    }
    Ok(Inserted::Sent)
}

/// Opens the clipboard, retrying for a short wall-clock budget because clipboard listeners can hold
/// it right after a change. Runs on the insertion worker, never the UI thread.
fn open_clipboard() -> anyhow::Result<clipboard_win::Clipboard> {
    let started = Instant::now();
    let until = started
        .checked_add(Duration::from_millis(50))
        .unwrap_or(started);
    loop {
        match clipboard_win::Clipboard::new() {
            Ok(clipboard) => return Ok(clipboard),
            Err(error) if Instant::now() >= until => return Err(error.into()),
            Err(_) => thread::sleep(Duration::from_millis(2)),
        }
    }
}

fn synthetic_key(key: VIRTUAL_KEY, unit: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: OWN_INPUT,
            },
        },
    }
}

fn send_input(input: &[INPUT]) -> anyhow::Result<usize> {
    let count = u32::try_from(input.len()).context("Text is too long for native keyboard input")?;
    let size = i32::try_from(size_of::<INPUT>())?;
    // SAFETY: initialized keyboard records and their buffer outlive this synchronous submission;
    // both lengths are checked against the native ABI.
    let sent = unsafe { SendInput(count, input.as_ptr(), size) };
    Ok(usize::try_from(sent)?)
}

fn has_external_target() -> bool {
    // SAFETY: foreground HWND is queried and used immediately, with a valid PID out parameter.
    unsafe {
        let window = GetForegroundWindow();
        let mut pid = 0;
        !window.is_null()
            && GetWindowThreadProcessId(window, &raw mut pid) != 0
            && pid != GetCurrentProcessId()
    }
}

/// Takes ownership of a handle a Win32 call just returned, or reports its failure.
///
/// # Safety
/// `raw` must come straight from a Win32 call that returns null on failure and otherwise an open
/// kernel-object handle, released with `CloseHandle`, that nothing else owns.
pub(crate) unsafe fn take_handle(raw: HANDLE) -> io::Result<OwnedHandle> {
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the caller passes an open, unowned kernel-object handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// Converts a Win32 `BOOL` result, reading the thread's last error on failure.
pub(crate) fn ok_or_last_error(result: BOOL) -> io::Result<()> {
    if result == FALSE {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn hwnd_of(handle: RawWindowHandle) -> anyhow::Result<HWND> {
    match handle {
        RawWindowHandle::Win32(window) => Ok(window.hwnd.get() as HWND),
        _ => bail!("Expected a Windows window"),
    }
}

fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, num::NonZeroIsize};

    use windows_sys::{
        Win32::{
            Graphics::Gdi::{GetUpdateRect, InvalidateRect, ValidateRect},
            UI::WindowsAndMessaging::{SendMessageW, UnregisterClassW},
        },
        core::PCWSTR,
    };

    use super::*;

    const PAINT_TEST_CLASS: PCWSTR = w!("SpeakeasyPaintTest");

    thread_local! {
        #[expect(
            clippy::disallowed_types,
            reason = "The native renderer increments a test-only counter on the same thread that asserts paint delivery"
        )]
        static PAINTS: Cell<usize> = const { Cell::new(0) };
    }

    unsafe extern "system" fn renderer(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: this procedure serves only the window owned by the test below.
        unsafe {
            if message == WM_PAINT {
                PAINTS.set(PAINTS.get().saturating_add(1));
                ValidateRect(hwnd, null());
                return 0;
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
    }

    struct Window(HWND);

    impl Drop for Window {
        fn drop(&mut self) {
            // SAFETY: this test thread owns both the window and its class; destruction removes
            // subclasses before the class is released.
            unsafe {
                DestroyWindow(self.0);
                UnregisterClassW(PAINT_TEST_CLASS, GetModuleHandleW(null()));
            }
        }
    }

    #[test]
    #[ignore = "Creates an owned native window; no microphone, hook, clipboard, or injected input"]
    fn hidden_pill_consumes_paint_and_visible_pill_reaches_renderer() -> anyhow::Result<()> {
        // SAFETY: the class name is static, renderer has the native signature, and all resources
        // belong to this thread.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(null());
            let definition = WNDCLASSW {
                lpfnWndProc: Some(renderer),
                hInstance: instance,
                lpszClassName: PAINT_TEST_CLASS,
                ..Default::default()
            };
            if RegisterClassW(&raw const definition) == 0 {
                return Err(io::Error::last_os_error().into());
            }
            CreateWindowExW(
                WS_EX_NOACTIVATE,
                PAINT_TEST_CLASS,
                PAINT_TEST_CLASS,
                WS_POPUP,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };
        let window = Window(hwnd);
        let raw = NonZeroIsize::new(hwnd as isize).context("Cannot create paint test window")?;
        configure_pill(RawWindowHandle::Win32(
            raw_window_handle::Win32WindowHandle::new(raw),
        ))?;
        PAINTS.set(0);
        // SAFETY: synchronous messages target this thread's owned window; showing uses no
        // activation and no input or clipboard operations.
        unsafe {
            InvalidateRect(window.0, null(), 0);
            SendMessageW(window.0, WM_PAINT, 0, 0);
            assert_eq!(PAINTS.get(), 0, "A hidden pill must not call its renderer");
            assert_eq!(GetUpdateRect(window.0, null_mut(), 0), 0);
            ShowWindow(window.0, SW_SHOWNOACTIVATE);
            PAINTS.set(0);
            InvalidateRect(window.0, null(), 0);
            SendMessageW(window.0, WM_PAINT, 0, 0);
            assert_eq!(PAINTS.get(), 1, "Visible paint must reach the renderer");
            ShowWindow(window.0, SW_HIDE);
            PAINTS.set(0);
            InvalidateRect(window.0, null(), 0);
            SendMessageW(window.0, WM_PAINT, 0, 0);
            assert_eq!(PAINTS.get(), 0, "Hiding must restore idle paint handling");
            assert_eq!(GetUpdateRect(window.0, null_mut(), 0), 0);
        }
        Ok(())
    }

    #[test]
    fn pill_centers_across_the_whole_desktop_coordinate_range() {
        let area = |left, right| RECT {
            left,
            top: 0,
            right,
            bottom: 0,
        };
        assert_eq!(centered_left(area(0, 1920), 400), 760);
        assert_eq!(
            centered_left(area(i32::MAX - 100, i32::MAX), 400),
            i32::MAX - 250
        );
        assert_eq!(centered_left(area(i32::MIN, i32::MIN + 100), 400), i32::MIN);
    }
}
