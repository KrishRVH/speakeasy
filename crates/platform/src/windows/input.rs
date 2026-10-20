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
                CreateEventW, GetCurrentThreadId, INFINITE, SetEvent, WaitForSingleObject,
            },
        },
        UI::{
            Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VIRTUAL_KEY},
            WindowsAndMessaging::{
                CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                HHOOK, KBDLLHOOKSTRUCT, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx,
                PBT_APMSUSPEND, PM_NOREMOVE, PM_REMOVE, PeekMessageW, PostThreadMessageW,
                QS_ALLINPUT, RegisterClassW, SetWindowsHookExW, TranslateMessage,
                UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_POWERBROADCAST,
                WM_QUERYENDSESSION, WM_QUIT, WM_SYSKEYDOWN, WM_WTSSESSION_CHANGE, WNDCLASSW,
                WTS_CONSOLE_DISCONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK,
                WTS_SESSION_LOGOFF,
            },
        },
    },
    w,
};

use super::{OWN_INPUT, is_key_down, ok_or_last_error, send_input, synthetic_key, take_handle};
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
        reason = "The context-free Windows hook mutates state confined to its own native thread; callbacks never share the UI owner"
    )]
    static HOOK: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

/// What `InputMonitor` holds beside the hook thread: the event that stops it, and the insertion
/// thread.
pub(crate) struct Monitor {
    // Shared so a stop can end the hook thread's wait, which cannot receive owner messages; the
    // manual-reset event records a stop however early it comes, and closes once both release it.
    wake: Arc<OwnedHandle>,
    insertion: insertion::Worker,
}

impl Monitor {
    pub(crate) fn request_stop(&self) {
        // SAFETY: the shared owning handle stays open through native exit; setting a manual-reset
        // event wakes a running or future wait.
        unsafe {
            SetEvent(self.wake.as_raw_handle());
        }
        self.insertion.request_stop();
    }

    pub(crate) fn stopped(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        self.insertion.stopped()
    }
}

pub(crate) fn start(
    input: InputSender,
    _: DesktopOptions,
) -> anyhow::Result<(OwnedThread, Monitor, Inserter)> {
    let (insertion, inserter) = insertion::Worker::start(input.clone())?;
    let wake = Arc::new(manual_reset_event()?);
    let thread = OwnedThread::spawn("input-monitor", {
        let wake = Arc::clone(&wake);
        move || monitor(&input, &wake)
    })?;
    Ok((thread, Monitor { wake, insertion }, inserter))
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

fn monitor(input: &InputSender, wake: &OwnedHandle) {
    HOOK.set(Some(HookState {
        input: input.clone(),
        policy: keyboard::Windows::default(),
    }));
    if let Err(error) = run_event_loop(input, wake)
        && !stop_requested(wake)
        && !input.is_closed()
    {
        input.deliver(Input::Unavailable(format!(
            "Cannot monitor the dictation shortcut: {error}. Pause and resume dictation to try again."
        )));
    }
    drop(HOOK.take());
    input.close();
}

fn run_event_loop(input: &InputSender, wake: &OwnedHandle) -> io::Result<()> {
    // A stop that came first never installs the hook.
    if stop_requested(wake) {
        return Ok(());
    }
    create_message_queue();
    let _hook = KeyboardHook::install()?;
    let window = LifecycleWindow::create()?;
    let _notifications = SessionNotifications::register(&window)?;
    // A stop landing after this check may still see readiness, which is harmless: each runtime has
    // its own input lane, and the runtime is stopped together with its monitor.
    if stop_requested(wake) {
        return Ok(());
    }
    input.deliver(Input::DesktopReady {
        shortcut: SHORTCUT.into(),
        cancel: CANCEL_SHORTCUT.into(),
    });
    pump_messages(wake)
}

/// Whether a stop has set the monitor's manual-reset event; once set, it stays set.
fn stop_requested(wake: &OwnedHandle) -> bool {
    // SAFETY: the owning handle stays open for this zero-timeout wait.
    unsafe { WaitForSingleObject(wake.as_raw_handle(), 0) == WAIT_OBJECT_0 }
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
fn pump_messages(wake: &OwnedHandle) -> io::Result<()> {
    let handles = [wake.as_raw_handle()];
    while dispatch_queued_messages(wake).is_continue() && !stop_requested(wake) {
        // SAFETY: the one-handle array outlives this synchronous wait.
        let woken = unsafe {
            MsgWaitForMultipleObjectsEx(
                1,
                handles.as_ptr(),
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

fn dispatch_queued_messages(wake: &OwnedHandle) -> ControlFlow<()> {
    let mut message = MSG::default();
    // SAFETY: PeekMessageW writes only this live local.
    while unsafe { PeekMessageW(&raw mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
        if stop_requested(wake) || message.message == WM_QUIT {
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
