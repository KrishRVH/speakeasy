//! Windows keyboard observation: the Ctrl+Win low-level hook and the lifecycle window that
//! cancels dictation when the session locks, ends, or sleeps, started beside the insertion thread.
//!
//! The hook thread creates and removes every native resource its callbacks reach.

use std::{
    cell::RefCell,
    io,
    ops::ControlFlow,
    os::windows::io::{AsRawHandle, OwnedHandle},
    panic,
    ptr::{null, null_mut},
    sync::Arc,
    time::{Duration, Instant},
};

use windows_sys::{
    Win32::{
        Foundation::{
            ERROR_CLASS_ALREADY_EXISTS, FALSE, GetLastError, HWND, LPARAM, LRESULT, TRUE,
            WAIT_FAILED, WAIT_OBJECT_0, WPARAM,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            RemoteDesktop::{
                NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
                WTSUnRegisterSessionNotification,
            },
            Threading::{
                CreateEventW, GetCurrentThread, GetCurrentThreadId, INFINITE, SetEvent,
                SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL, WaitForSingleObject,
            },
        },
        UI::{
            Input::KeyboardAndMouse::VIRTUAL_KEY,
            WindowsAndMessaging::{
                CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                HHOOK, KBDLLHOOKSTRUCT, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx,
                PBT_APMSUSPEND, PM_NOREMOVE, PM_REMOVE, PeekMessageW, PostThreadMessageW,
                QS_ALLINPUT, RegisterClassW, SetWindowsHookExW, TranslateMessage,
                UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_POWERBROADCAST,
                WM_QUERYENDSESSION, WM_QUIT, WM_SYSKEYDOWN, WM_WTSSESSION_CHANGE, WNDCLASSW,
                WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WTS_CONSOLE_DISCONNECT,
                WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_LOGOFF,
            },
        },
    },
    w,
};

use super::{OWN_INPUT, is_key_down, press, release, send_input, take_handle};
use crate::{
    CANCEL_SHORTCUT, DesktopOptions, Input, InputSender, Inserter, OwnedThread, SHORTCUT,
    insertion, keyboard,
};

/// Posted by the keyboard hook so the message loop, not the hook callback, masks Start.
const MASK_START: u32 = WM_APP + 1;
/// A virtual key Windows leaves unassigned.
const UNASSIGNED_KEY: VIRTUAL_KEY = 0xE8;

thread_local! {
    #[expect(
        clippy::disallowed_types,
        reason = "The context-free keyboard hook and the lifecycle window, which interrupts the same policy, mutate state confined to their native thread; callbacks never share the UI owner"
    )]
    static HOOK: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

/// What `InputMonitor` holds beside the hook thread: the event that stops it, and the insertion
/// thread.
pub(crate) struct Monitor {
    // Shared with the hook thread, which waits on it, so the event closes once both release it.
    stop: Arc<StopEvent>,
    insertion: insertion::Worker,
}

impl Monitor {
    pub(crate) fn request_stop(&self) {
        self.stop.set();
        self.insertion.request_stop();
    }

    pub(crate) fn stopped(&self) -> impl Future<Output = ()> + Send + use<> {
        self.insertion.stopped()
    }
}

pub(crate) fn start(
    input: InputSender,
    _: DesktopOptions,
) -> anyhow::Result<(OwnedThread, Monitor, Inserter)> {
    let (insertion, inserter) = insertion::Worker::start(input.clone())?;
    let stop = Arc::new(StopEvent::new()?);
    let thread = OwnedThread::spawn("input-monitor", {
        let stop = Arc::clone(&stop);
        move || monitor(&input, &stop)
    })?;
    Ok((thread, Monitor { stop, insertion }, inserter))
}

/// The monitor's stop, set once and for good. A manual-reset event records a stop that comes
/// before the hook thread first waits, and it ends that wait, which owner messages cannot reach.
struct StopEvent(OwnedHandle);

impl StopEvent {
    fn new() -> io::Result<Self> {
        // SAFETY: null attributes and name create an unnamed manual-reset event whose handle
        // nothing else owns.
        unsafe { take_handle(CreateEventW(null(), TRUE, FALSE, null())) }.map(Self)
    }

    fn set(&self) {
        // SAFETY: the owned event stays open for this call, which takes the handle by value.
        unsafe { SetEvent(self.0.as_raw_handle()) };
    }

    fn is_set(&self) -> bool {
        // SAFETY: the owned event stays open for this zero-timeout wait.
        unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) == WAIT_OBJECT_0 }
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
        let physical = keyboard::Physical::read(is_key_down);
        let decision = self.policy.observe(key, down, physical, Instant::now());
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
/// `WM_POWERBROADCAST` broadcasts. The shell and window utilities sometimes show hidden top-level
/// windows, so it is a zero-size tool popup with nothing to draw and no taskbar button.
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
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                class_name,
                class_name,
                WS_POPUP,
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
///
/// Registration fails early in logon, before Remote Desktop Services is ready, and wherever those
/// services are off, so the message loop retries it for a while. The hook runs either way, and
/// power and end-session messages reach the window without it.
struct SessionNotifications<'window> {
    window: &'window LifecycleWindow,
    registration: Registration,
}

impl<'window> SessionNotifications<'window> {
    fn register(window: &'window LifecycleWindow, now: Instant) -> Self {
        let mut notifications = Self {
            window,
            registration: Registration::due(now),
        };
        notifications.attempt_when_due(now);
        notifications
    }

    fn attempt_when_due(&mut self, now: Instant) {
        let window = self.window.0;
        self.registration.attempt_when_due(now, || {
            // SAFETY: the borrowed window stays live until this registration is dropped.
            unsafe { WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) != FALSE }
        });
    }
}

impl Drop for SessionNotifications<'_> {
    fn drop(&mut self) {
        if self.registration == Registration::Registered {
            // SAFETY: registration succeeded for this still-live window.
            unsafe {
                WTSUnRegisterSessionNotification(self.window.0);
            }
        }
    }
}

/// Where registering for session notifications stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Registration {
    Registered,
    /// The next attempt is due at `at`, with `retries` more after it.
    Pending {
        at: Instant,
        retries: u8,
    },
    /// Every attempt failed, so session changes go unobserved.
    Abandoned,
}

impl Registration {
    const RETRIES: u8 = 12;
    const RETRY_AFTER: Duration = Duration::from_secs(5);

    const fn due(now: Instant) -> Self {
        Self::Pending {
            at: now,
            retries: Self::RETRIES,
        }
    }

    /// Runs `register` when an attempt is due at `now`, scheduling the next if it fails.
    fn attempt_when_due(&mut self, now: Instant, register: impl FnOnce() -> bool) {
        let Self::Pending { at, retries } = *self else {
            return;
        };
        if now < at {
            return;
        }
        *self = if register() {
            Self::Registered
        } else {
            match (retries.checked_sub(1), now.checked_add(Self::RETRY_AFTER)) {
                (Some(retries), Some(at)) => Self::Pending { at, retries },
                _ => Self::Abandoned,
            }
        };
    }

    /// How long, in milliseconds, the message loop may wait before the next attempt falls due.
    /// Rounding up keeps a fraction of a millisecond from becoming a zero timeout that spins until
    /// then.
    fn timeout(self, now: Instant) -> u32 {
        match self {
            Self::Pending { at, .. } => {
                let remaining = at.saturating_duration_since(now);
                u32::try_from(remaining.as_nanos().div_ceil(1_000_000)).unwrap_or(INFINITE)
            },
            Self::Registered | Self::Abandoned => INFINITE,
        }
    }
}

fn monitor(input: &InputSender, stop: &StopEvent) {
    HOOK.set(Some(HookState {
        input: input.clone(),
        policy: keyboard::Windows::default(),
    }));
    if let Err(error) = run_event_loop(input, stop)
        && !stop.is_set()
        && !input.is_closed()
    {
        input.deliver(Input::Unavailable(format!(
            "Cannot monitor the dictation shortcut ({error}). Resume dictation to try again."
        )));
    }
    drop(HOOK.take());
    input.close();
}

fn run_event_loop(input: &InputSender, stop: &StopEvent) -> io::Result<()> {
    if stop.is_set() {
        return Ok(());
    }
    // Windows silently removes a low-level hook whose callback misses LowLevelHooksTimeout, as a
    // starved thread does on a busy system. This thread only decides key events and waits, so it
    // may preempt other work.
    // SAFETY: the pseudo-handle names this thread, and the call takes values only.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    }
    create_message_queue();
    let _hook = KeyboardHook::install()?;
    let window = LifecycleWindow::create()?;
    let mut notifications = SessionNotifications::register(&window, Instant::now());
    // A stop landing after this check may still see readiness, which is harmless: each runtime has
    // its own input lane, and the runtime is stopped together with its monitor.
    if stop.is_set() {
        return Ok(());
    }
    input.deliver(Input::DesktopReady {
        shortcut: SHORTCUT.into(),
        cancel: CANCEL_SHORTCUT.into(),
    });
    pump_messages(stop, &mut notifications)
}

/// Peeking makes Windows create this thread's queue before the hook posts to it.
fn create_message_queue() {
    let mut message = MSG::default();
    // SAFETY: PeekMessageW writes only this live local.
    unsafe {
        PeekMessageW(&raw mut message, null_mut(), 0, 0, PM_NOREMOVE);
    }
}

/// Runs the message loop until a stop or `WM_QUIT`; a stop wins even under continuous input.
/// Between messages, it retries a session registration that failed.
fn pump_messages(stop: &StopEvent, notifications: &mut SessionNotifications<'_>) -> io::Result<()> {
    let handles = [stop.0.as_raw_handle()];
    while dispatch_queued_messages(stop).is_continue() && !stop.is_set() {
        notifications.attempt_when_due(Instant::now());
        let timeout = notifications.registration.timeout(Instant::now());
        // SAFETY: the one-handle array outlives this synchronous wait.
        let woken = unsafe {
            MsgWaitForMultipleObjectsEx(
                1,
                handles.as_ptr(),
                timeout,
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

fn dispatch_queued_messages(stop: &StopEvent) -> ControlFlow<()> {
    let mut message = MSG::default();
    // SAFETY: PeekMessageW writes only this live local.
    while unsafe { PeekMessageW(&raw mut message, null_mut(), 0, 0, PM_REMOVE) } != FALSE {
        if stop.is_set() || message.message == WM_QUIT {
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
            close_input();
            false
        });
    if swallow {
        return 1;
    }
    // SAFETY: forwards this callback's own arguments unchanged.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn lifecycle_window(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if interrupts_dictation(message, wparam)
        && panic::catch_unwind(|| {
            HOOK.with_borrow_mut(|slot| {
                if let Some(state) = slot {
                    state.policy.interrupt(&state.input);
                }
            });
        })
        .is_err()
    {
        close_input();
    }
    // SAFETY: forwards this window procedure's own arguments unchanged.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Recovers from a panic inside a native callback, which must not unwind into the OS: closing the
/// input makes the owner cancel rather than continue with lost key edges.
fn close_input() {
    HOOK.with(|slot| {
        if let Ok(Some(state)) = slot.try_borrow().as_deref() {
            state.input.close();
        }
    });
}

fn interrupts_dictation(message: u32, wparam: WPARAM) -> bool {
    match message {
        WM_WTSSESSION_CHANGE => matches!(
            u32::try_from(wparam),
            Ok(WTS_CONSOLE_DISCONNECT
                | WTS_REMOTE_DISCONNECT
                | WTS_SESSION_LOGOFF
                | WTS_SESSION_LOCK)
        ),
        WM_QUERYENDSESSION => true,
        WM_POWERBROADCAST => wparam == PBT_APMSUSPEND as usize,
        _ => false,
    }
}

/// Releasing Win without another key opens Start. An unassigned key while the chord is held marks
/// Win as used, as other shortcut tools do.
fn mask_start_menu() {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Marking Win as used is a best-effort shell hint; blocked synthetic input must not stop dictation"
    )]
    let _ = send_input(&[press(UNASSIGNED_KEY), release(UNASSIGNED_KEY)]);
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::{
        Foundation::RECT,
        UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongW, GetWindowRect, IsWindowVisible, SW_SHOWNOACTIVATE,
            ShowWindow,
        },
    };

    use super::*;
    use crate::windows::ok_or_last_error;

    #[test]
    fn session_registration_retries_for_a_while_without_spinning() {
        let start = Instant::now();
        let mut registration = Registration::due(start);
        registration.attempt_when_due(start, || false);
        assert_eq!(registration.timeout(start), 5_000, "A failed attempt spins");
        registration.attempt_when_due(start, || panic!("Retried before it was due"));
        let mut now = start;
        for _ in 0..Registration::RETRIES {
            now += Registration::RETRY_AFTER;
            registration.attempt_when_due(now, || false);
        }
        assert_eq!(registration, Registration::Abandoned);
        assert_eq!(
            registration.timeout(now),
            INFINITE,
            "Abandoned retries wake"
        );

        let mut registration = Registration::due(start);
        registration.attempt_when_due(start, || false);
        registration.attempt_when_due(start + Registration::RETRY_AFTER, || true);
        assert_eq!(registration, Registration::Registered);
        assert_eq!(registration.timeout(start), INFINITE);

        let almost_due = Registration::Pending {
            at: start + Duration::from_micros(1),
            retries: 0,
        };
        assert_eq!(
            almost_due.timeout(start),
            1,
            "The last fraction of a millisecond spins"
        );
    }

    #[test]
    #[ignore = "Creates an owned native window; no microphone, hook, clipboard, or injected input"]
    fn a_shown_lifecycle_window_has_no_frame_or_taskbar_button() -> anyhow::Result<()> {
        let window = LifecycleWindow::create()?;
        let mut frame = RECT::default();
        // SAFETY: this thread owns the live window, and showing it activates nothing.
        let (visible, extended) = unsafe {
            ShowWindow(window.0, SW_SHOWNOACTIVATE);
            ok_or_last_error(GetWindowRect(window.0, &raw mut frame))?;
            (
                IsWindowVisible(window.0) != FALSE,
                GetWindowLongW(window.0, GWL_EXSTYLE).cast_unsigned(),
            )
        };
        assert!(visible, "The lifecycle window was never shown");
        assert_eq!(
            (frame.right - frame.left, frame.bottom - frame.top),
            (0, 0),
            "A shown lifecycle window draws a frame"
        );
        assert_ne!(
            extended & WS_EX_TOOLWINDOW,
            0,
            "A shown lifecycle window gets a taskbar button"
        );
        Ok(())
    }
}
