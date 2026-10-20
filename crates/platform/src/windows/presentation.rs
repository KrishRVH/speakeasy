//! Presenting GPUI windows on Windows: the click-through pill and the Settings window that
//! minimizes to the tray.

use anyhow::bail;
use async_channel::Sender;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
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
    UI::{
        HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            GWL_EXSTYLE, GWL_STYLE, GetForegroundWindow, GetWindowLongPtrW, HWND_TOPMOST,
            IsWindowVisible, LWA_ALPHA, SC_MINIMIZE, SIZE_MINIMIZED, SW_HIDE, SW_RESTORE,
            SW_SHOWNOACTIVATE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SetForegroundWindow, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos,
            ShowWindow, WM_NCDESTROY, WM_PAINT, WM_SIZE, WM_SYSCOMMAND, WS_EX_LAYERED,
            WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_OVERLAPPEDWINDOW, WS_POPUP,
        },
    },
};

use super::{BASE_DPI, ok_or_last_error};
use crate::{NativeWindow, PILL_HEIGHT, PILL_MARGIN, PILL_WIDTH};

/// The byte size of the 32-bit values the pill's DWM attributes take.
const DWM_VALUE_SIZE: u32 = 4;

impl NativeWindow {
    /// Configures the pill for nonactivating, click-through presentation.
    ///
    /// # Errors
    /// Returns failed native configuration.
    pub fn configure_pill(&self) -> anyhow::Result<()> {
        let hwnd = self.native.hwnd;
        let border = DWMWA_COLOR_NONE;
        let corners = DWMWCP_DONOTROUND;
        // SAFETY: this thread owns the window, and both DWM values outlive their synchronous calls.
        // The window owns its subclass until WM_NCDESTROY on this same thread.
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

    /// Shows or hides the pill without activating it.
    pub fn set_pill_visible(&self, visible: bool) {
        let hwnd = self.native.hwnd;
        if visible {
            place_pill(hwnd);
        }
        // SAFETY: this thread owns the window.
        unsafe {
            ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
    }
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

impl NativeWindow {
    /// Makes minimizing the Settings window hide it to the tray; install once per window.
    ///
    /// The window owns the callback context until `WM_NCDESTROY`; callbacks only enqueue work,
    /// avoiding reentrant GPUI updates.
    ///
    /// # Errors
    /// Returns an error if callback installation fails.
    pub fn minimize_to_tray(&self, hide: Sender<()>) -> anyhow::Result<()> {
        let hwnd = self.native.hwnd;
        let context = Box::into_raw(Box::new(hide));
        // SAFETY: this thread owns the window. Ownership of the sender transfers to settings_window
        // on success, or is reclaimed immediately on failure.
        unsafe {
            if SetWindowSubclass(hwnd, Some(settings_window), 1, context as usize) == 0 {
                drop(Box::from_raw(context));
                bail!("Cannot enable minimize to tray. Restart Speakeasy.");
            }
        }
        Ok(())
    }

    /// Shows the Settings window in front, or hides it.
    pub fn set_settings_visible(&self, visible: bool) {
        let hwnd = self.native.hwnd;
        // SAFETY: this thread owns the window.
        unsafe {
            ShowWindow(hwnd, if visible { SW_RESTORE } else { SW_HIDE });
            if visible {
                SetForegroundWindow(hwnd);
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        io,
        ptr::{null, null_mut},
    };

    use anyhow::ensure;
    use windows_sys::{
        Win32::{
            Graphics::Gdi::{GetUpdateRect, InvalidateRect, ValidateRect},
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SendMessageW,
                UnregisterClassW, WNDCLASSW,
            },
        },
        core::PCWSTR,
        w,
    };

    use super::{super::native_window, *};

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
        ensure!(!hwnd.is_null(), "Cannot create paint test window");
        // SAFETY: this thread owns the live window until `window` drops.
        unsafe { native_window(hwnd) }?.configure_pill()?;
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
