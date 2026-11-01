use std::{
    cell::RefCell,
    io,
    ops::ControlFlow,
    os::windows::io::{AsRawHandle, OwnedHandle},
    panic::{self, AssertUnwindSafe},
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

const MASK_START: u32 = WM_APP + 1;

const UNASSIGNED_KEY: VIRTUAL_KEY = 0xE8;

thread_local! {
    #[expect(
        clippy::disallowed_types,
        reason = "The context-free keyboard hook and the lifecycle window, which interrupts the recording through the same policy, mutate state confined to their native thread; callbacks never share the UI owner"
    )]
    static HOOK: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

pub(crate) struct Monitor {
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

struct StopEvent(OwnedHandle);

impl StopEvent {
    fn new() -> io::Result<Self> {
        unsafe { take_handle(CreateEventW(null(), TRUE, FALSE, null())) }.map(Self)
    }

    fn set(&self) {
        unsafe { SetEvent(self.0.as_raw_handle()) };
    }

    fn is_set(&self) -> bool {
        unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) == WAIT_OBJECT_0 }
    }
}

struct HookState {
    input: InputSender,
    policy: keyboard::Windows,
}

impl HookState {
    fn observe(&mut self, key: u32, down: bool) -> bool {
        let physical = keyboard::Physical::read(is_key_down);
        let decision = self.policy.observe(key, down, physical, Instant::now());
        decision.deliver(&self.input);
        if decision.starts() {
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
        unsafe {
            UnhookWindowsHookEx(self.0);
        }
    }
}

struct LifecycleWindow(HWND);

impl LifecycleWindow {
    fn create() -> io::Result<Self> {
        let class_name = w!("SpeakeasyEvents");

        let instance = unsafe { GetModuleHandleW(null()) };
        let class = WNDCLASSW {
            lpfnWndProc: Some(lifecycle_window),
            hInstance: instance,
            lpszClassName: class_name,
            ..Default::default()
        };

        let registered = unsafe {
            RegisterClassW(&raw const class) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS
        };
        if !registered {
            return Err(io::Error::last_os_error());
        }

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
        unsafe {
            DestroyWindow(self.0);
        }
    }
}

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
        self.registration.attempt_when_due(now, || unsafe {
            WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) != FALSE
        });
    }
}

impl Drop for SessionNotifications<'_> {
    fn drop(&mut self) {
        if self.registration == Registration::Registered {
            unsafe {
                WTSUnRegisterSessionNotification(self.window.0);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Registration {
    Registered,
    Pending { at: Instant, retries: u8 },
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
    let watched = panic::catch_unwind(AssertUnwindSafe(|| run_event_loop(input, stop)))
        .unwrap_or_else(|_| Err(io::Error::other("it stopped unexpectedly")));
    if let Err(error) = watched
        && !stop.is_set()
        && !input.is_closed()
    {
        input.deliver(Input::Unavailable(format!(
            "Cannot watch the dictation shortcut ({error}). Resume dictation to try again."
        )));
    }
    drop(HOOK.take());
    input.close();
}

fn run_event_loop(input: &InputSender, stop: &StopEvent) -> io::Result<()> {
    if stop.is_set() {
        return Ok(());
    }

    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    }
    create_message_queue();
    let _hook = KeyboardHook::install()?;
    let window = LifecycleWindow::create()?;
    let mut notifications = SessionNotifications::register(&window, Instant::now());

    if stop.is_set() {
        return Ok(());
    }
    input.deliver(Input::DesktopReady {
        shortcut: SHORTCUT.into(),
        cancel: CANCEL_SHORTCUT.into(),
    });
    pump_messages(stop, &mut notifications)
}

fn create_message_queue() {
    let mut message = MSG::default();

    unsafe {
        PeekMessageW(&raw mut message, null_mut(), 0, 0, PM_NOREMOVE);
    }
}

fn pump_messages(stop: &StopEvent, notifications: &mut SessionNotifications<'_>) -> io::Result<()> {
    let handles = [stop.0.as_raw_handle()];
    while dispatch_queued_messages(stop).is_continue() && !stop.is_set() {
        notifications.attempt_when_due(Instant::now());
        let timeout = notifications.registration.timeout(Instant::now());

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

    while unsafe { PeekMessageW(&raw mut message, null_mut(), 0, 0, PM_REMOVE) } != FALSE {
        if stop.is_set() || message.message == WM_QUIT {
            return ControlFlow::Break(());
        }
        if message.message == MASK_START {
            mask_start_menu();
        } else {
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

    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn lifecycle_window(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if interrupts_recording(message, wparam)
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

    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn close_input() {
    HOOK.with(|slot| {
        if let Ok(Some(state)) = slot.try_borrow().as_deref() {
            state.input.close();
        }
    });
}

fn interrupts_recording(message: u32, wparam: WPARAM) -> bool {
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
