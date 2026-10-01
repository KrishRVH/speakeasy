use super::{
    Arc, Input, InputSender, InsertPermit, Inserted, MonitorControl, RawWindowHandle, SHORTCUT,
    Sender, deliver, insertion, keyboard,
};
use anyhow::{Context as _, bail};
use std::{
    ffi::OsString,
    os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::PathBuf,
    ptr::{null, null_mut},
    thread,
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WAIT_FAILED,
        WAIT_OBJECT_0, WPARAM,
    },
    Graphics::{
        Dwm::{
            DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE,
            DWMWCP_DONOTROUND, DwmSetWindowAttribute,
        },
        Gdi::{
            BeginPaint, EndPaint, GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
            MonitorFromWindow,
        },
    },
    System::{
        Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize},
        LibraryLoader::GetModuleHandleW,
        RemoteDesktop::{
            NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
            WTSUnRegisterSessionNotification,
        },
        Threading::{CreateEventW, GetCurrentProcessId, GetCurrentThreadId, INFINITE, SetEvent},
    },
    UI::{
        Controls::Dialogs::{
            CommDlgExtendedError, GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST,
            OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
        },
        HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
            KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LCONTROL, VK_LWIN, VK_MENU,
            VK_RCONTROL, VK_RWIN, VK_SHIFT,
        },
        WindowsAndMessaging::{
            CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
            GWL_EXSTYLE, GWL_STYLE, GetForegroundWindow, GetWindowLongPtrW,
            GetWindowThreadProcessId, HWND_TOPMOST, IsWindowVisible, KBDLLHOOKSTRUCT, LWA_ALPHA,
            MB_ICONERROR, MB_OK, MWMO_INPUTAVAILABLE, MessageBoxW, MsgWaitForMultipleObjectsEx,
            PBT_APMSUSPEND, PM_NOREMOVE, PM_REMOVE, PeekMessageW, PostThreadMessageW, QS_ALLINPUT,
            RegisterClassW, SC_MINIMIZE, SIZE_MINIMIZED, SPI_GETCLIENTAREAANIMATION, SW_HIDE,
            SW_RESTORE, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
            SWP_NOSIZE, SetForegroundWindow, SetLayeredWindowAttributes, SetWindowLongPtrW,
            SetWindowPos, SetWindowsHookExW, ShowWindow, SystemParametersInfoW, TranslateMessage,
            UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_NCDESTROY, WM_PAINT,
            WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_QUIT, WM_SIZE, WM_SYSCOMMAND, WM_SYSKEYDOWN,
            WM_WTSSESSION_CHANGE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
            WS_EX_TRANSPARENT, WS_OVERLAPPEDWINDOW, WS_POPUP,
        },
    },
};

const OWN_INPUT: usize = 0x5350_4541;
const MASK_START: u32 = WM_APP + 1;
// Either Ctrl with either Windows key, as left and right keys report separately.
const CHORD: [VIRTUAL_KEY; 4] = [VK_LCONTROL, VK_RCONTROL, VK_LWIN, VK_RWIN];

unsafe extern "system" fn lifecycle(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let cancel = (message == WM_WTSSESSION_CHANGE && matches!(wparam, 2 | 4 | 6 | 7))
        || message == WM_QUERYENDSESSION
        || (message == WM_POWERBROADCAST && wparam == PBT_APMSUSPEND as usize);
    if cancel {
        HOOK.with(|slot| {
            if let Ok(mut state) = slot.try_borrow_mut()
                && let Some(state) = state.as_mut()
            {
                deliver(&state.tx, Input::Cancel);
                deliver(&state.tx, Input::Release);
                state.policy.interrupt();
            }
        });
    }
    // SAFETY: forward unchanged arguments for this registered native window.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}
struct HookState {
    tx: InputSender,
    policy: keyboard::Windows,
}
impl HookState {
    fn observe(&mut self, key: u32, down: bool) -> bool {
        // The callback's own key has not yet updated GetAsyncKeyState. Policy
        // overrides it with this event and uses this snapshot for missed releases.
        let physical = CHORD.map(|vk| {
            // SAFETY: GetAsyncKeyState accepts a value, with no pointer lifetime.
            unsafe { GetAsyncKeyState(i32::from(vk)) < 0 }
        });
        let decision = self.policy.observe(key, down, physical);
        decision.deliver(&self.tx);
        if decision.starts() {
            // SAFETY: posts the Start-menu mask to this hook's own thread.
            unsafe {
                PostThreadMessageW(GetCurrentThreadId(), MASK_START, 0, 0);
            }
        }
        decision.swallow
    }
}

/// Releasing Win without another key opens Start. An unassigned key while the
/// chord is held marks Win as used, as other shortcut tools do.
fn mask_start_menu() {
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0xe8,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: OWN_INPUT,
            },
        },
    };
    let input = [key(0), key(KEYEVENTF_KEYUP)];
    // Our marker keeps these records out of the shortcut hook.
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Marking Win as used is a best-effort shell hint; blocked synthetic input must not stop dictation"
    )]
    let _ = send_input(&input);
}

thread_local! {
    // WH_KEYBOARD_LL has no context argument. This storage belongs to the
    // monitor's native thread and is cleared before that thread exits.
    #[expect(
        clippy::disallowed_types,
        reason = "The context-free Windows hook mutates state confined to its own native thread; callbacks never share the UI owner"
    )]
    static HOOK: std::cell::RefCell<Option<HookState>> = const { std::cell::RefCell::new(None) };
}

/// Owns the desktop observation thread until explicit stop and acknowledged cleanup.
pub struct InputMonitor {
    control: Arc<MonitorControl<()>>,
    wake: Arc<OwnedHandle>,
    finished: async_channel::Receiver<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl InputMonitor {
    pub(super) fn start(tx: InputSender) -> anyhow::Result<Self> {
        let control = Arc::new(MonitorControl::default());
        let native_control = control.clone();
        // SAFETY: a successful CreateEventW transfers the sole owning handle.
        // OwnedHandle closes it only after both monitor owners have released it.
        let wake = unsafe {
            let event = CreateEventW(null(), 1, 0, null());
            if event.is_null() {
                return Err(std::io::Error::last_os_error().into());
            }
            Arc::new(OwnedHandle::from_raw_handle(event.cast()))
        };
        let native_wake = wake.clone();
        let (complete, finished) = async_channel::bounded(1);
        let thread = thread::Builder::new()
            .name("shortcut".into())
            .spawn(move || {
                HOOK.with(|slot| {
                    *slot.borrow_mut() = Some(HookState {
                        tx: tx.clone(),
                        policy: keyboard::Windows::default(),
                    });
                });
                if let Err(error) = run_monitor(&tx, &native_control, &native_wake)
                    && !native_control.stopping()
                    && !tx.is_closed()
                {
                    deliver(
                        &tx,
                        Input::Unavailable(format!(
                            "Cannot monitor the dictation shortcut: {error}. Pause and resume dictation to try again."
                        )),
                    );
                }
                native_control.clear();
                drop(HOOK.with(|slot| slot.borrow_mut().take()));
                tx.close();
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "The one-shot receiver can close only when the owner no longer waits for monitor retirement"
                )]
                let _ = complete.try_send(());
            })?;
        Ok(Self {
            control,
            wake,
            finished,
            thread: Some(thread),
        })
    }

    /// Request native observation shutdown without joining its thread.
    pub fn request_stop(&self) {
        self.control.request_stop(|()| {
            // SAFETY: the shared owning handle remains live through native exit;
            // setting a manual-reset event wakes a running or future wait.
            unsafe {
                SetEvent(self.wake.as_raw_handle().cast());
            }
        });
    }

    /// Wait for native resources to retire before replacing or dropping the monitor.
    pub fn stopped(&self) -> impl std::future::Future<Output = ()> + use<> {
        let finished = self.finished.clone();
        async move {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Completion and sender closure both acknowledge that the native monitor thread has exited"
            )]
            let _ = finished.recv().await;
        }
    }
}

fn run_monitor(
    tx: &InputSender,
    control: &MonitorControl<()>,
    wake: &OwnedHandle,
) -> anyhow::Result<()> {
    if control.stopping() {
        return Ok(());
    }
    // SAFETY: this thread owns the hook and window, pumps their messages, and
    // removes them before the thread-local callback state is destroyed.
    unsafe {
        let mut msg = std::mem::zeroed();
        PeekMessageW(&raw mut msg, null_mut(), 0, 0, PM_NOREMOVE);
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), GetModuleHandleW(null()), 0);
        if hook.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let class_name: Vec<u16> = "SpeakeasyEvents\0".encode_utf16().collect();
        let class = WNDCLASSW {
            lpfnWndProc: Some(lifecycle),
            hInstance: GetModuleHandleW(null()),
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        if RegisterClassW(&raw const class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            let error = std::io::Error::last_os_error();
            UnhookWindowsHookEx(hook);
            return Err(error.into());
        }
        let window = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            class.hInstance,
            null(),
        );
        if window.is_null() || WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) == 0
        {
            let error = std::io::Error::last_os_error();
            if !window.is_null() {
                DestroyWindow(window);
            }
            UnhookWindowsHookEx(hook);
            return Err(error.into());
        }
        let mut result = Ok(());
        if control.start((), || {
            deliver(
                tx,
                Input::DesktopReady {
                    shortcut: SHORTCUT.into(),
                    cancel: "Escape".into(),
                },
            );
        }) {
            'pump: loop {
                // Drain queued messages before waiting, including thread-only
                // Start-menu masks. Stop wins even under continuous input.
                while PeekMessageW(&raw mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if control.stopping() || msg.message == WM_QUIT {
                        break 'pump;
                    }
                    if msg.message == MASK_START {
                        mask_start_menu();
                        continue;
                    }
                    TranslateMessage(&raw const msg);
                    DispatchMessageW(&raw const msg);
                }
                if control.stopping() {
                    break;
                }
                let handles = [wake.as_raw_handle().cast()];
                match MsgWaitForMultipleObjectsEx(
                    1,
                    handles.as_ptr(),
                    INFINITE,
                    QS_ALLINPUT,
                    MWMO_INPUTAVAILABLE,
                ) {
                    WAIT_OBJECT_0 => break,
                    WAIT_FAILED => {
                        result = Err(std::io::Error::last_os_error().into());
                        break;
                    },
                    _ => {},
                }
            }
        }
        control.clear();
        WTSUnRegisterSessionNotification(window);
        DestroyWindow(window);
        UnhookWindowsHookEx(hook);
        result
    }
}

impl Drop for InputMonitor {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Joining reaps the monitor even after a callback panic; the input lane already reports unavailability"
            )]
            let _ = thread.join();
        }
    }
}

unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Never unwind through the OS callback. A poisoned input path closes its
    // channel, causing the owner to cancel rather than continuing with lost edges.
    let swallow = code >= 0
        && std::panic::catch_unwind(|| {
            // SAFETY: for nonnegative WH_KEYBOARD_LL callbacks lparam points to a
            // KBDLLHOOKSTRUCT valid for this invocation.
            let key = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            if key.dwExtraInfo == OWN_INPUT {
                return false;
            }
            let down = wparam == WM_KEYDOWN as usize || wparam == WM_SYSKEYDOWN as usize;
            HOOK.with(|slot| {
                slot.borrow_mut()
                    .as_mut()
                    .is_some_and(|state| state.observe(key.vkCode, down))
            })
        })
        .unwrap_or_else(|_| {
            HOOK.with(|slot| {
                if let Ok(state) = slot.try_borrow()
                    && let Some(state) = state.as_ref()
                {
                    state.tx.close();
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

/// Configure an owned UI-thread window for nonactivating, click-through presentation.
///
/// # Errors
/// Returns an error for the wrong window kind or failed native configuration.
pub fn configure_pill(handle: RawWindowHandle) -> anyhow::Result<()> {
    use windows_sys::Win32::UI::Shell::SetWindowSubclass;
    let RawWindowHandle::Win32(raw) = handle else {
        bail!("Expected a Windows window");
    };
    let hwnd = raw.hwnd.get() as HWND;
    // SAFETY: the caller holds a live GPUI window on its owning UI thread, and
    // both DWM values outlive their synchronous calls. The window owns its
    // subclass until WM_NCDESTROY on this same thread.
    unsafe {
        // GPUI creates an overlapped window, whose frame Windows 11 outlines
        // and shadows around the whole transparent surface. The pill is a
        // plain popup with no frame, border, or rounded corners.
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            (style & !(WS_OVERLAPPEDWINDOW.cast_signed() as isize))
                | WS_POPUP.cast_signed() as isize,
        );
        let border = DWMWA_COLOR_NONE;
        let corners = DWMWCP_DONOTROUND;
        for (attribute, value) in [
            (DWMWA_BORDER_COLOR, (&raw const border).cast()),
            (DWMWA_WINDOW_CORNER_PREFERENCE, (&raw const corners).cast()),
        ] {
            // Decoration hints are best effort.
            DwmSetWindowAttribute(hwnd, attribute.cast_unsigned(), value, 4);
        }
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            style
                | (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_LAYERED)
                    .cast_signed() as isize,
        );
        // WS_EX_TRANSPARENT only affects paint ordering on an ordinary window.
        // Layering makes the entire noninteractive pill pass mouse hit testing.
        if SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        if SetWindowSubclass(hwnd, Some(pill_window), 1, 0) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
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
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
    // SAFETY: this window-owned subclass runs on the pill's UI thread and
    // is removed at destruction. PAINTSTRUCT lives through its paint cycle.
    unsafe {
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(pill_window), id);
        } else if message == WM_PAINT && IsWindowVisible(hwnd) == 0 {
            // ValidateRect alone leaves this hidden window's paint pending.
            // Complete the native paint cycle without drawing or calling GPUI.
            let mut paint = std::mem::zeroed();
            BeginPaint(hwnd, &raw mut paint);
            EndPaint(hwnd, &raw const paint);
            return 0;
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

pub(super) fn modifiers_down() -> bool {
    // SAFETY: querying virtual key state requires no owned resources.
    unsafe {
        [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
            .iter()
            .any(|key| GetAsyncKeyState(i32::from(*key)) < 0)
    }
}

/// Install once per Settings window. The window owns the callback context until
/// `WM_NCDESTROY`; callbacks only enqueue work, avoiding reentrant GPUI updates.
/// # Errors
/// Returns an error if the window is invalid or callback installation fails.
pub fn minimize_to_tray(handle: RawWindowHandle, hide: Sender<()>) -> anyhow::Result<()> {
    use windows_sys::Win32::UI::Shell::SetWindowSubclass;
    let RawWindowHandle::Win32(raw) = handle else {
        bail!("Expected a Windows window");
    };
    let context = Box::into_raw(Box::new(hide));
    // SAFETY: the live window belongs to this UI thread. Ownership transfers to
    // settings_window on success, or is reclaimed immediately on failure.
    unsafe {
        if SetWindowSubclass(
            raw.hwnd.get() as HWND,
            Some(settings_window),
            1,
            context as usize,
        ) == 0
        {
            drop(Box::from_raw(context));
            bail!("Cannot enable minimize to tray. Restart Speakeasy.");
        }
    }
    Ok(())
}

#[expect(
    clippy::let_underscore_must_use,
    reason = "Minimize notifications coalesce in a one-slot lane; a closed lane belongs to a destroyed settings owner"
)]
unsafe extern "system" fn settings_window(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    context: usize,
) -> LRESULT {
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
    // SAFETY: SetWindowSubclass stores this boxed sender for this window only.
    // Remove the callback before freeing its context at final destruction.
    unsafe {
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(settings_window), id);
            drop(Box::from_raw(context as *mut Sender<()>));
        } else if message == WM_SYSCOMMAND && wparam & 0xfff0 == SC_MINIMIZE as usize {
            let _ = (*(context as *const Sender<()>)).try_send(());
            return 0;
        } else if message == WM_SIZE && wparam == SIZE_MINIMIZED as usize {
            // ShowWindow(SW_MINIMIZE), including GPUI's own minimize path, can
            // bypass WM_SYSCOMMAND. Still forward size so GPUI tracks restore.
            let _ = (*(context as *const Sender<()>)).try_send(());
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

/// Show or hide the owned Settings window; call on its UI thread.
pub fn set_settings_visible(handle: RawWindowHandle, visible: bool) {
    if let RawWindowHandle::Win32(raw) = handle {
        // SAFETY: caller resolves a live Settings window on its owning UI thread,
        // after releasing GPUI's borrow because ShowWindow can send messages.
        unsafe {
            let hwnd = raw.hwnd.get() as HWND;
            ShowWindow(hwnd, if visible { SW_RESTORE } else { SW_HIDE });
            if visible {
                SetForegroundWindow(hwnd);
            }
        }
    }
}

/// Asks for one existing file, offering `filter` (a name and pattern) before
/// all files, without blocking the UI owner.
///
/// The dialog runs its own message loop on a dedicated thread; shown from
/// GPUI's UI thread, it stays unpainted while GPUI is idle.
///
/// # Errors
/// Returns native dialog or worker failures; cancellation returns no path.
pub fn choose_file(
    owner: RawWindowHandle,
    title: &str,
    filter: [&str; 2],
) -> impl std::future::Future<Output = anyhow::Result<Option<PathBuf>>> + Send + use<> {
    // Resolve the UI-only handle before constructing the Send completion future.
    let owner = match owner {
        RawWindowHandle::Win32(raw) => Ok(raw.hwnd.get()),
        _ => Err(anyhow::anyhow!("Expected a Windows window")),
    };
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let filter: Vec<u16> = [filter[0], filter[1], "All files", "*.*", ""]
        .join("\0")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    async move {
        let owner = owner?;
        let (tx, rx) = async_channel::bounded(1);
        thread::Builder::new()
            .name("file dialog".into())
            .spawn(move || {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A closed completion lane means the settings owner no longer needs the dialog result"
                )]
                let _ = tx.send_blocking(open_file(owner as HWND, &title, &filter));
            })?;
        rx.recv().await?
    }
}

fn open_file(owner: HWND, title: &[u16], filter: &[u16]) -> anyhow::Result<Option<PathBuf>> {
    let mut file = vec![0u16; 32_768];
    let mut dialog = OPENFILENAMEW {
        lStructSize: u32::try_from(std::mem::size_of::<OPENFILENAMEW>())?,
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
    // SAFETY: this thread owns the dialog's apartment and message loop, and
    // every buffer in `dialog` outlives the synchronous call.
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
    let length = file.iter().position(|&unit| unit == 0).unwrap_or(0);
    Ok(file
        .get(..length)
        .filter(|path| !path.is_empty())
        .map(|path| OsString::from_wide(path).into()))
}

/// Show or hide the owned pill without activating it; call on its UI thread.
pub fn set_pill_visible(handle: RawWindowHandle, visible: bool) {
    let Ok(monitor_bytes) = u32::try_from(std::mem::size_of::<MONITORINFO>()) else {
        return;
    };
    if let RawWindowHandle::Win32(raw) = handle {
        // SAFETY: called with a live window handle on its UI thread.
        unsafe {
            if visible {
                let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
                let mut info: MONITORINFO = std::mem::zeroed();
                info.cbSize = monitor_bytes;
                let mut dpi_x = 96;
                let mut dpi_y = 96;
                GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi_x, &raw mut dpi_y);
                if GetMonitorInfoW(monitor, &raw mut info) != 0 {
                    let scaled = |logical: u32| {
                        i32::try_from(u64::from(logical).saturating_mul(u64::from(dpi_x)) / 96)
                            .unwrap_or(i32::MAX)
                    };
                    let width = scaled(400);
                    let height = scaled(100);
                    // Widen before centering so monitor coordinates may span
                    // both ends of the signed desktop coordinate range.
                    let center = i64::from(info.rcWork.left)
                        .saturating_add(i64::from(info.rcWork.right))
                        .saturating_sub(i64::from(width))
                        / 2;
                    let left =
                        i32::try_from(center.clamp(i64::from(i32::MIN), i64::from(i32::MAX)))
                            .unwrap_or(info.rcWork.left);
                    SetWindowPos(
                        raw.hwnd.get() as HWND,
                        HWND_TOPMOST,
                        left,
                        info.rcWork
                            .bottom
                            .saturating_sub(height)
                            .saturating_sub(scaled(2)),
                        width,
                        height,
                        SWP_NOACTIVATE,
                    );
                }
            }
            ShowWindow(
                raw.hwnd.get() as HWND,
                if visible { SW_SHOWNOACTIVATE } else { SW_HIDE },
            );
        }
    }
}

// Clipboard listeners can briefly hold it immediately after a change. Retry
// with a short wall-clock budget on the insertion worker, never the UI thread.
fn open_clipboard() -> anyhow::Result<clipboard_win::Clipboard> {
    let started = std::time::Instant::now();
    let until = started
        .checked_add(std::time::Duration::from_millis(50))
        .unwrap_or(started);
    loop {
        match clipboard_win::Clipboard::new() {
            Ok(clipboard) => return Ok(clipboard),
            Err(error) if std::time::Instant::now() >= until => return Err(error.into()),
            Err(_) => thread::sleep(std::time::Duration::from_millis(2)),
        }
    }
}

pub(super) fn insert(
    text: &str,
    gate: &InsertPermit,
    preserve_clipboard: bool,
) -> anyhow::Result<Inserted> {
    if !gate.active() {
        return Ok(Inserted::Cancelled);
    }
    if preserve_clipboard {
        return insert_direct(text, gate);
    }
    let normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
    {
        let _clipboard = open_clipboard()?;
        if !gate.active() {
            return Ok(Inserted::Cancelled);
        }
        clipboard_win::raw::set_string(&normalized)?;
    }
    let sequence = clipboard_win::raw::seq_num();
    {
        let _clipboard = open_clipboard()?;
        let mut current = Vec::new();
        clipboard_win::raw::get_string(&mut current)?;
        if current != normalized.as_bytes() || sequence != clipboard_win::raw::seq_num() {
            return Ok(Inserted::Unavailable(
                "Clipboard changed. New contents were preserved; paste was not sent.",
            ));
        }
    }
    if let Some(outcome) = insertion::preflight(
        has_external_target(),
        !modifiers_down(),
        insertion::Mode::Paste,
    ) {
        return Ok(outcome);
    }
    if let Some(outcome) = insertion::preflight(has_external_target(), true, insertion::Mode::Paste)
    {
        return Ok(outcome);
    }
    let key = |vk, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: OWN_INPUT,
            },
        },
    };
    let input = [
        key(VK_CONTROL, 0),
        key(0x56, 0),
        key(0x56, KEYEVENTF_KEYUP),
        key(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    if sequence != clipboard_win::raw::seq_num() {
        return Ok(Inserted::Unavailable(
            "Clipboard changed before paste. New contents were preserved.",
        ));
    }
    if !gate.commit() {
        return Ok(Inserted::Cancelled);
    }
    let sent = send_input(&input)?;
    if sent != input.len() {
        let release = [key(0x56, KEYEVENTF_KEYUP), key(VK_CONTROL, KEYEVENTF_KEYUP)];
        if sent > 0 {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Release stuck synthetic modifiers after partial input even if the target also blocks cleanup"
            )]
            let _ = send_input(&release);
        }
        return Ok(Inserted::Copied(
            "Copied. This app blocked paste; press Ctrl+V.",
        ));
    }
    Ok(Inserted::Sent)
}

/// Query the native reduced-motion preference; Linux uses the app setting.
#[must_use]
pub fn reduced_motion() -> bool {
    let mut enabled: i32 = 1;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes a BOOL into this live variable.
    unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0);
    }
    enabled == 0
}

fn insert_direct(text: &str, gate: &InsertPermit) -> anyhow::Result<Inserted> {
    if let Some(outcome) = insertion::preflight(
        has_external_target(),
        !modifiers_down(),
        insertion::Mode::Direct,
    ) {
        return Ok(outcome);
    }
    let mut input = Vec::with_capacity(text.len().saturating_mul(2));
    for unit in text.replace("\r\n", "\n").encode_utf16() {
        for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
            input.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0,
                        wScan: unit,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: OWN_INPUT,
                    },
                },
            });
        }
    }
    if let Some(outcome) =
        insertion::preflight(has_external_target(), true, insertion::Mode::Direct)
    {
        return Ok(outcome);
    }
    if !gate.commit() {
        return Ok(Inserted::Cancelled);
    }
    // Direct input leaves every clipboard format untouched.
    let sent = send_input(&input)?;
    if sent != input.len() {
        return Ok(Inserted::Unavailable(
            "Direct input was blocked or only partly sent. Clipboard preserved.",
        ));
    }
    Ok(Inserted::Sent)
}

fn send_input(input: &[INPUT]) -> anyhow::Result<usize> {
    let count = u32::try_from(input.len()).context("Text is too long for native keyboard input")?;
    let size = i32::try_from(std::mem::size_of::<INPUT>())?;
    // SAFETY: initialized keyboard records and their buffer outlive this
    // synchronous submission; both lengths are checked against the native ABI.
    let sent = unsafe { SendInput(count, input.as_ptr(), size) };
    Ok(usize::try_from(sent)?)
}

/// Present a local startup error; callers never include audio, transcripts, or credentials.
pub fn show_error(message: &str) {
    let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "Speakeasy".encode_utf16().chain(Some(0)).collect();
    // SAFETY: both NUL-terminated strings outlive this synchronous dialog.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use std::{cell::Cell, num::NonZeroIsize};
    use windows_sys::Win32::{
        Graphics::Gdi::{GetUpdateRect, InvalidateRect, ValidateRect},
        UI::WindowsAndMessaging::{SendMessageW, UnregisterClassW},
    };

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

    struct Window {
        hwnd: HWND,
        class: Vec<u16>,
    }

    impl Drop for Window {
        fn drop(&mut self) {
            // SAFETY: this test thread owns both the window and its class;
            // destruction removes subclasses before the class is released.
            unsafe {
                DestroyWindow(self.hwnd);
                UnregisterClassW(self.class.as_ptr(), GetModuleHandleW(null()));
            }
        }
    }

    #[test]
    #[ignore = "Creates an owned native window; no microphone, hook, clipboard, or injected input"]
    fn hidden_pill_consumes_paint_and_visible_pill_reaches_renderer() -> anyhow::Result<()> {
        let class: Vec<u16> = "SpeakeasyPaintTest\0".encode_utf16().collect();
        // SAFETY: the NUL-terminated class name lives until Window drops;
        // renderer has the native signature and all resources belong to this thread.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(null());
            let definition = WNDCLASSW {
                lpfnWndProc: Some(renderer),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            if RegisterClassW(&raw const definition) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            CreateWindowExW(
                WS_EX_NOACTIVATE,
                class.as_ptr(),
                class.as_ptr(),
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
        let window = Window { hwnd, class };
        let raw = NonZeroIsize::new(hwnd as isize).context("Cannot create paint test window")?;
        configure_pill(RawWindowHandle::Win32(
            raw_window_handle::Win32WindowHandle::new(raw),
        ))?;
        PAINTS.set(0);
        // SAFETY: synchronous messages target this thread's owned window;
        // showing uses no activation and no input or clipboard operations.
        unsafe {
            InvalidateRect(window.hwnd, null(), 0);
            SendMessageW(window.hwnd, WM_PAINT, 0, 0);
            assert_eq!(PAINTS.get(), 0, "A hidden pill must not call its renderer");
            assert_eq!(GetUpdateRect(window.hwnd, null_mut(), 0), 0);
            ShowWindow(window.hwnd, SW_SHOWNOACTIVATE);
            PAINTS.set(0);
            InvalidateRect(window.hwnd, null(), 0);
            SendMessageW(window.hwnd, WM_PAINT, 0, 0);
            assert_eq!(PAINTS.get(), 1, "Visible paint must reach the renderer");
            ShowWindow(window.hwnd, SW_HIDE);
            PAINTS.set(0);
            InvalidateRect(window.hwnd, null(), 0);
            SendMessageW(window.hwnd, WM_PAINT, 0, 0);
            assert_eq!(PAINTS.get(), 0, "Hiding must restore idle paint handling");
            assert_eq!(GetUpdateRect(window.hwnd, null_mut(), 0), 0);
        }
        Ok(())
    }
}
