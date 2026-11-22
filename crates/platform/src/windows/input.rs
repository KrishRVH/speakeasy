use std::{
    cell::RefCell,
    io,
    ops::ControlFlow,
    os::windows::io::{AsRawHandle, OwnedHandle},
    panic::{self, AssertUnwindSafe},
    ptr::{null, null_mut},
    sync::Arc,
    time::Instant,
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

use super::{OWN_INPUT, insertion::Insert, is_key_down, press, release, send_input, take_handle};
use crate::{
    CANCEL_SHORTCUT, DesktopOptions, Input, InputSender, Inserter, OwnFocus, OwnedThread,
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
    options: DesktopOptions,
    own_focus: OwnFocus,
) -> anyhow::Result<(OwnedThread, Monitor, Inserter)> {
    let shortcut = options.shortcut_key.windows;
    let typed = insertion::KeyCount::default();
    let (insertion, inserter) = insertion::Worker::start(input.clone(), own_focus, {
        let typed = typed.clone();
        let mut continuation = insertion::Continuation::default();
        move |request| {
            let native = Insert {
                shortcut,
                typed: typed.clone(),
            };
            insertion::settle(native, request, &mut continuation)
        }
    })?;
    let stop = Arc::new(StopEvent::new()?);
    let thread = OwnedThread::spawn("input-monitor", {
        let stop = Arc::clone(&stop);
        move || monitor(&input, &stop, shortcut, typed)
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
    shortcut: crate::WindowsShortcut,
    policy: keyboard::Windows,
    typed: insertion::KeyCount,
}

impl HookState {
    fn observe(&mut self, key: u32, down: bool) -> bool {
        let physical = keyboard::Physical::read(self.shortcut, is_key_down);
        let decision = self.policy.observe(key, down, physical, Instant::now());
        decision.deliver(&self.input);
        if decision.starts() && self.shortcut.masks_start() {
            unsafe {
                PostThreadMessageW(GetCurrentThreadId(), MASK_START, 0, 0);
            }
        }
        if keyboard::Windows::types(key, down, &decision) {
            self.typed.add();
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
    registered: bool,
}

impl<'window> SessionNotifications<'window> {
    fn register(window: &'window LifecycleWindow) -> Self {
        let registered =
            unsafe { WTSRegisterSessionNotification(window.0, NOTIFY_FOR_THIS_SESSION) != FALSE };
        Self { window, registered }
    }
}

impl Drop for SessionNotifications<'_> {
    fn drop(&mut self) {
        if self.registered {
            unsafe {
                WTSUnRegisterSessionNotification(self.window.0);
            }
        }
    }
}

fn monitor(
    input: &InputSender,
    stop: &StopEvent,
    shortcut: crate::WindowsShortcut,
    typed: insertion::KeyCount,
) {
    HOOK.set(Some(HookState {
        input: input.clone(),
        shortcut,
        policy: keyboard::Windows::new(shortcut),
        typed,
    }));
    let watched = panic::catch_unwind(AssertUnwindSafe(|| run_event_loop(input, stop, shortcut)))
        .unwrap_or_else(|_| Err(io::Error::other("it stopped unexpectedly")));
    if let Err(error) = watched
        && !stop.is_set()
        && !input.is_closed()
    {
        input.deliver(Input::Unavailable(format!(
            "Cannot watch the dictation shortcut ({error}). Resume dictation to try again."
        )));
    }
    HOOK.set(None);
    input.close();
}

fn run_event_loop(
    input: &InputSender,
    stop: &StopEvent,
    shortcut: crate::WindowsShortcut,
) -> io::Result<()> {
    if stop.is_set() {
        return Ok(());
    }

    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    }
    create_message_queue();
    let _hook = KeyboardHook::install()?;
    let window = LifecycleWindow::create()?;
    let _notifications = SessionNotifications::register(&window);

    if stop.is_set() {
        return Ok(());
    }
    input.deliver(Input::DesktopReady {
        shortcut: shortcut.label().into(),
        cancel: CANCEL_SHORTCUT.into(),
    });
    pump_messages(stop)
}

fn create_message_queue() {
    let mut message = MSG::default();

    unsafe {
        PeekMessageW(&raw mut message, null_mut(), 0, 0, PM_NOREMOVE);
    }
}

fn pump_messages(stop: &StopEvent) -> io::Result<()> {
    let handles = [stop.0.as_raw_handle()];
    while dispatch_queued_messages(stop).is_continue() && !stop.is_set() {
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
            let down = matches!(u32::try_from(wparam), Ok(WM_KEYDOWN | WM_SYSKEYDOWN));
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
    if interrupts_recording(message, wparam) {
        let interrupted = panic::catch_unwind(|| {
            HOOK.with_borrow_mut(|slot| {
                if let Some(state) = slot {
                    state.policy.interrupt(&state.input);
                }
            });
        });
        if interrupted.is_err() {
            close_input();
        }
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
        WM_POWERBROADCAST => u32::try_from(wparam) == Ok(PBT_APMSUSPEND),
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
