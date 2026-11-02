use std::ptr::null_mut;

use anyhow::bail;
use async_channel::Sender;
use windows_sys::Win32::{
    Foundation::{FALSE, HWND, LPARAM, LRESULT, RECT, WPARAM},
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
            GWL_EXSTYLE, GWL_STYLE, GetForegroundWindow, HWND_TOPMOST, IsWindowVisible, LWA_ALPHA,
            SC_MINIMIZE, SIZE_MINIMIZED, SW_HIDE, SW_RESTORE, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow,
            SetLayeredWindowAttributes, SetWindowPos, ShowWindow, WM_NCDESTROY, WM_PAINT, WM_SIZE,
            WM_SYSCOMMAND, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT, WS_OVERLAPPEDWINDOW,
            WS_POPUP,
        },
    },
};

use super::{BASE_DPI, ok_or_last_error};
use crate::{NativeWindow, PILL_HEIGHT, PILL_MARGIN, PILL_WIDTH};

const DWM_VALUE_SIZE: u32 = 4;

impl NativeWindow {
    pub fn configure_pill(&self) -> anyhow::Result<()> {
        let window = &self.native;
        let hwnd = window.hwnd;
        let border = DWMWA_COLOR_NONE;
        let corners = DWMWCP_DONOTROUND;

        window.restyle(GWL_STYLE, |style| (style & !WS_OVERLAPPEDWINDOW) | WS_POPUP);

        unsafe {
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
        }
        window.restyle(GWL_EXSTYLE, |style| {
            style | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED
        });

        unsafe {
            ok_or_last_error(SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA))?;
            ok_or_last_error(SetWindowPos(
                hwnd,
                null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            ))?;
            ok_or_last_error(SetWindowSubclass(hwnd, Some(pill_window), 1, 0))?;
        }
        Ok(())
    }

    pub fn set_pill_visible(&self, visible: bool) {
        let hwnd = self.native.hwnd;
        if visible {
            place_pill(hwnd);
        }

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
    unsafe {
        match message {
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(pill_window), id);
                DefSubclassProc(hwnd, message, wparam, lparam)
            },
            WM_PAINT if IsWindowVisible(hwnd) == FALSE => {
                let mut paint = PAINTSTRUCT::default();
                BeginPaint(hwnd, &raw mut paint);
                EndPaint(hwnd, &raw const paint);
                0
            },
            _ => DefSubclassProc(hwnd, message, wparam, lparam),
        }
    }
}

fn place_pill(hwnd: HWND) {
    let Ok(info_size) = u32::try_from(size_of::<MONITORINFO>()) else {
        return;
    };
    let mut info = MONITORINFO {
        cbSize: info_size,
        ..Default::default()
    };
    let (mut dpi, mut vertical_dpi) = (BASE_DPI, BASE_DPI);

    let found = unsafe {
        let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
        GetDpiForMonitor(
            monitor,
            MDT_EFFECTIVE_DPI,
            &raw mut dpi,
            &raw mut vertical_dpi,
        );
        GetMonitorInfoW(monitor, &raw mut info) != FALSE
    };
    if !found {
        return;
    }
    let scaled = |logical: u16| {
        u64::from(logical)
            .saturating_mul(u64::from(dpi))
            .checked_div(u64::from(BASE_DPI))
            .and_then(|physical| i32::try_from(physical).ok())
            .unwrap_or(i32::MAX)
    };
    let [width, height, margin] = [PILL_WIDTH, PILL_HEIGHT, PILL_MARGIN].map(scaled);
    let top = info
        .rcWork
        .bottom
        .saturating_sub(height)
        .saturating_sub(margin);
    let left = centered_left(info.rcWork, width);

    unsafe {
        SetWindowPos(hwnd, HWND_TOPMOST, left, top, width, height, SWP_NOACTIVATE);
    }
}

fn centered_left(area: RECT, width: i32) -> i32 {
    let left = i64::from(area.left)
        .saturating_add(i64::from(area.right))
        .saturating_sub(i64::from(width))
        / 2;
    i32::try_from(left).unwrap_or(if left < 0 { i32::MIN } else { i32::MAX })
}

impl NativeWindow {
    pub fn minimize_to_tray(&self, hide: Sender<()>) -> anyhow::Result<()> {
        let hwnd = self.native.hwnd;
        let context = Box::into_raw(Box::new(hide));

        unsafe {
            if SetWindowSubclass(hwnd, Some(settings_window), 1, context as usize) == FALSE {
                drop(Box::from_raw(context));
                bail!("Cannot enable minimize to tray. Restart Speakeasy.");
            }
        }
        Ok(())
    }

    pub fn set_window_visible(&self, visible: bool) {
        let hwnd = self.native.hwnd;

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
    unsafe {
        match message {
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(settings_window), id);
                drop(Box::from_raw(context as *mut Sender<()>));
                DefSubclassProc(hwnd, message, wparam, lparam)
            },
            WM_SYSCOMMAND if wparam & 0xFFF0 == SC_MINIMIZE as usize => {
                request_hide(context);
                0
            },
            WM_SIZE if wparam == SIZE_MINIMIZED as usize => {
                request_hide(context);
                DefSubclassProc(hwnd, message, wparam, lparam)
            },
            _ => DefSubclassProc(hwnd, message, wparam, lparam),
        }
    }
}

unsafe fn request_hide(context: usize) {
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
            unsafe {
                DestroyWindow(self.0);
                UnregisterClassW(PAINT_TEST_CLASS, GetModuleHandleW(null()));
            }
        }
    }

    #[test]
    #[ignore = "Creates an owned native window; no microphone, hook, clipboard, or injected input"]
    fn hidden_pill_consumes_paint_and_visible_pill_reaches_renderer() -> anyhow::Result<()> {
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
        let _window = Window(hwnd);
        ensure!(!hwnd.is_null(), "Cannot create paint test window");

        unsafe { native_window(hwnd) }?.configure_pill()?;
        let repaint = || {
            PAINTS.set(0);

            unsafe {
                InvalidateRect(hwnd, null(), FALSE);
                SendMessageW(hwnd, WM_PAINT, 0, 0);
            }
            PAINTS.get()
        };

        let paint_pending = || unsafe { GetUpdateRect(hwnd, null_mut(), FALSE) != FALSE };
        assert_eq!(repaint(), 0, "A hidden pill must not call its renderer");
        assert!(
            !paint_pending(),
            "A hidden pill must finish its paint cycle"
        );

        unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        assert_eq!(repaint(), 1, "Visible paint must reach the renderer");

        unsafe { ShowWindow(hwnd, SW_HIDE) };
        assert_eq!(repaint(), 0, "Hiding must restore idle paint handling");
        assert!(
            !paint_pending(),
            "A hidden pill must finish its paint cycle"
        );
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
