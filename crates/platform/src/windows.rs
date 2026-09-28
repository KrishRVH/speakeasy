use super::*;
use anyhow::{Context, bail};
use std::{
    cell::RefCell,
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::mpsc,
    thread,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    System::{
        Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize},
        LibraryLoader::GetModuleHandleW,
        RemoteDesktop::*,
        Threading::{GetCurrentProcessId, GetCurrentThreadId},
    },
    UI::{Controls::Dialogs::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

const OWN_INPUT: usize = 0x53504541;
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
                state.chord.interrupt();
            }
        });
    }
    // SAFETY: forward unchanged arguments for this registered native window.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}
struct HookState {
    tx: InputSender,
    chord: Chord,
    down: [bool; 4],
}

impl HookState {
    /// Returns whether to swallow the event. Only Space that locks hands-free is
    /// swallowed; the chord, Escape, and every other key reach the focused app.
    fn observe(&mut self, key: u32, down: bool) -> bool {
        if key == u32::from(VK_ESCAPE) {
            if down {
                deliver(&self.tx, Input::Cancel);
            }
            return false;
        }
        if key == u32::from(VK_SPACE) && self.chord.space(down) {
            if down {
                deliver(&self.tx, Input::Lock);
            }
            return true;
        }
        let Some(index) = CHORD.iter().position(|&vk| u32::from(vk) == key) else {
            if down && self.chord.interrupt() {
                deliver(&self.tx, Input::Cancel);
                deliver(&self.tx, Input::Release);
            }
            return false;
        };
        let fresh = down && !self.down[index];
        self.down[index] = down;
        for (other, &vk) in CHORD.iter().enumerate() {
            // A release missed during a desktop switch must not keep the chord held.
            // SAFETY: GetAsyncKeyState has no pointer or lifetime requirements.
            if other != index && self.down[other] && unsafe { GetAsyncKeyState(i32::from(vk)) } >= 0
            {
                self.down[other] = false;
            }
        }
        let held = (self.down[0] || self.down[1]) && (self.down[2] || self.down[3]);
        match self.chord.modifiers(held, fresh) {
            Some(Input::Press) => {
                deliver(&self.tx, Input::Press);
                // Send the mask after this callback returns, while Win is still down.
                // SAFETY: posts a message to this hook's own message-loop thread.
                unsafe {
                    PostThreadMessageW(GetCurrentThreadId(), MASK_START, 0, 0);
                }
            }
            Some(input) => deliver(&self.tx, input),
            None => {}
        }
        false
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
    // SAFETY: both keyboard records are initialized for this synchronous call;
    // our marker keeps them out of the shortcut hook.
    unsafe {
        SendInput(
            input.len() as u32,
            input.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}
thread_local! {
    // WH_KEYBOARD_LL has no context parameter. The hook and its state live on
    // the same message-loop thread; no state is shared with the UI.
    static HOOK: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

pub struct InputMonitor {
    thread_id: u32,
    thread: Option<thread::JoinHandle<()>>,
}

impl InputMonitor {
    pub fn start(tx: InputSender) -> anyhow::Result<Self> {
        let (ready, started) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("shortcut".into())
            .spawn(move || {
                HOOK.with(|slot| {
                    *slot.borrow_mut() = Some(HookState {
                        tx,
                        chord: Chord::default(),
                        down: [false; 4],
                    })
                });
                // SAFETY: this thread owns the hook, pumps its messages, and removes
                // it before the thread-local callback state is destroyed.
                unsafe {
                    let mut msg = std::mem::zeroed();
                    PeekMessageW(&mut msg, null_mut(), 0, 0, PM_NOREMOVE);
                    let hook = SetWindowsHookExW(
                        WH_KEYBOARD_LL,
                        Some(keyboard),
                        GetModuleHandleW(std::ptr::null()),
                        0,
                    );
                    if hook.is_null() {
                        let _ = ready.send(Err(std::io::Error::last_os_error()));
                        return;
                    }
                    let class_name: Vec<u16> = "SpeakeasyEvents\0".encode_utf16().collect();
                    let class = WNDCLASSW {
                        lpfnWndProc: Some(lifecycle),
                        hInstance: GetModuleHandleW(std::ptr::null()),
                        lpszClassName: class_name.as_ptr(),
                        ..std::mem::zeroed()
                    };
                    RegisterClassW(&class);
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
                        std::ptr::null(),
                    );
                    if window.is_null()
                        || WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) == 0
                    {
                        let error = std::io::Error::last_os_error();
                        if !window.is_null() {
                            DestroyWindow(window);
                        }
                        UnhookWindowsHookEx(hook);
                        let _ = ready.send(Err(error));
                        return;
                    }
                    let _ = ready.send(Ok(GetCurrentThreadId()));
                    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                        if msg.message == MASK_START {
                            mask_start_menu();
                            continue;
                        }
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    WTSUnRegisterSessionNotification(window);
                    DestroyWindow(window);
                    UnhookWindowsHookEx(hook);
                }
                HOOK.with(|slot| {
                    if let Some(state) = slot.borrow_mut().take() {
                        state.tx.close();
                    }
                });
            })?;
        let thread_id = started.recv().context("Shortcut thread stopped")??;
        Ok(Self {
            thread_id,
            thread: Some(thread),
        })
    }
}

impl Drop for InputMonitor {
    fn drop(&mut self) {
        // SAFETY: the ID belongs to our live message-loop thread.
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(thread) = self.thread.take() {
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
            let down = wparam as u32 == WM_KEYDOWN || wparam as u32 == WM_SYSKEYDOWN;
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

pub fn configure_pill(handle: RawWindowHandle) -> anyhow::Result<()> {
    let RawWindowHandle::Win32(raw) = handle else {
        bail!("Expected a Windows window");
    };
    let hwnd = raw.hwnd.get() as HWND;
    // SAFETY: the caller holds a live GPUI window on its owning UI thread, and
    // both DWM values are 32-bit locals that outlive their synchronous calls.
    unsafe {
        // GPUI creates an overlapped window, whose frame Windows 11 outlines
        // and shadows around the whole transparent surface. The pill is a
        // plain popup with no frame, border, or rounded corners.
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            (style & !(WS_OVERLAPPEDWINDOW as isize)) | WS_POPUP as isize,
        );
        let border = DWMWA_COLOR_NONE;
        let corners = DWMWCP_DONOTROUND;
        for (attribute, value) in [
            (DWMWA_BORDER_COLOR, (&border as *const u32).cast()),
            (
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&corners as *const i32).cast(),
            ),
        ] {
            // Windows 10 lacks these attributes; its popups draw neither anyway.
            let _ = DwmSetWindowAttribute(hwnd, attribute as u32, value, 4);
        }
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            style
                | (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_LAYERED)
                    as isize,
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
    }
    Ok(())
}

pub fn modifiers_down() -> bool {
    // SAFETY: querying virtual key state requires no owned resources.
    unsafe {
        [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
            .iter()
            .any(|key| GetAsyncKeyState(i32::from(*key)) < 0)
    }
}

/// Install once per Settings window. The window owns the callback context until
/// WM_NCDESTROY; callbacks only enqueue work, avoiding reentrant GPUI updates.
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
/// all files. The dialog runs its own message loop on a dedicated thread:
/// shown from GPUI's UI thread, it stays unpainted while GPUI is idle.
pub async fn choose_file(
    owner: RawWindowHandle,
    title: &str,
    filter: [&str; 2],
) -> anyhow::Result<Option<PathBuf>> {
    let RawWindowHandle::Win32(raw) = owner else {
        bail!("Expected a Windows window");
    };
    let owner = raw.hwnd.get();
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let filter: Vec<u16> = [filter[0], filter[1], "All files", "*.*", ""]
        .join("\0")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let (tx, rx) = async_channel::bounded(1);
    thread::Builder::new()
        .name("file dialog".into())
        .spawn(move || {
            let _ = tx.send_blocking(open_file(owner as HWND, &title, &filter));
        })?;
    rx.recv().await?
}

fn open_file(owner: HWND, title: &[u16], filter: &[u16]) -> anyhow::Result<Option<PathBuf>> {
    let mut file = vec![0u16; 32_768];
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrFile: file.as_mut_ptr(),
        nMaxFile: file.len() as u32,
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
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );
        let error = if GetOpenFileNameW(&mut dialog) == 0 {
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
    Ok((length > 0).then(|| OsString::from_wide(&file[..length]).into()))
}

pub fn set_pill_visible(handle: RawWindowHandle, visible: bool) {
    if let RawWindowHandle::Win32(raw) = handle {
        // SAFETY: called with a live window handle on its UI thread.
        unsafe {
            if visible {
                let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
                let mut info: MONITORINFO = std::mem::zeroed();
                info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                let mut dpi_x = 96;
                let mut dpi_y = 96;
                GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
                if GetMonitorInfoW(monitor, &mut info) != 0 {
                    let scale = dpi_x as f32 / 96.0;
                    let width = (400.0 * scale) as i32;
                    let height = (100.0 * scale) as i32;
                    SetWindowPos(
                        raw.hwnd.get() as HWND,
                        HWND_TOPMOST,
                        (info.rcWork.left + info.rcWork.right - width) / 2,
                        info.rcWork.bottom - height - (2.0 * scale) as i32,
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
// with a short wall-clock budget on the dictation owner, never the UI thread.
fn open_clipboard() -> anyhow::Result<clipboard_win::Clipboard> {
    let until = std::time::Instant::now() + std::time::Duration::from_millis(50);
    loop {
        match clipboard_win::Clipboard::new() {
            Ok(clipboard) => return Ok(clipboard),
            Err(error) if std::time::Instant::now() >= until => return Err(error.into()),
            Err(_) => thread::sleep(std::time::Duration::from_millis(2)),
        }
    }
}

pub fn insert(
    text: &str,
    gate: &InputSender,
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
    if modifiers_down() {
        return Ok(Inserted::Copied(
            "Copied. Release the shortcut and press Ctrl+V.",
        ));
    }
    // SAFETY: INPUT records contain initialized keyboard payloads. The OS
    // copies them synchronously; our marker excludes them from shortcut handling.
    unsafe {
        if !has_external_target() {
            return Ok(Inserted::Copied(
                "Copied. Focus an editor and press Ctrl+V.",
            ));
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
        let sent = SendInput(
            input.len() as u32,
            input.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
        if sent != input.len() as u32 {
            let release = [key(0x56, KEYEVENTF_KEYUP), key(VK_CONTROL, KEYEVENTF_KEYUP)];
            if sent > 0 {
                SendInput(2, release.as_ptr(), std::mem::size_of::<INPUT>() as i32);
            }
            return Ok(Inserted::Copied(
                "Copied. This app blocked paste; press Ctrl+V.",
            ));
        }
    }
    Ok(Inserted::Sent)
}

pub fn reduced_motion() -> bool {
    let mut enabled: i32 = 1;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes a BOOL into this live variable.
    unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut enabled as *mut i32).cast(),
            0,
        );
    }
    enabled == 0
}

fn insert_direct(text: &str, gate: &InputSender) -> anyhow::Result<Inserted> {
    if modifiers_down() {
        return Ok(Inserted::Unavailable(
            "Release the shortcut and try again. Clipboard preserved.",
        ));
    }
    let mut input = Vec::with_capacity(text.len() * 2);
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
    if !has_external_target() {
        return Ok(Inserted::Unavailable(
            "Focus an editor and try again. Clipboard preserved.",
        ));
    }
    if !gate.commit() {
        return Ok(Inserted::Cancelled);
    }
    // SAFETY: all UTF-16 keyboard records are initialized and stay alive for
    // this synchronous OS submission. No clipboard format is read or changed.
    let sent = unsafe {
        SendInput(
            input.len() as u32,
            input.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent != input.len() as u32 {
        return Ok(Inserted::Unavailable(
            "Direct input was blocked or only partly sent. Clipboard preserved.",
        ));
    }
    Ok(Inserted::Sent)
}

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
            && GetWindowThreadProcessId(window, &mut pid) != 0
            && pid != GetCurrentProcessId()
    }
}
