use std::{io, ptr::null_mut};

use anyhow::bail;
use async_channel::Sender;
use windows_sys::{
    Win32::{
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
            HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI},
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{
                GWL_EXSTYLE, GWL_STYLE, GetForegroundWindow, GetPropW, HWND_TOPMOST,
                IsWindowVisible, LWA_ALPHA, RemovePropW, SC_MINIMIZE, SIZE_MINIMIZED, SW_HIDE,
                SW_RESTORE, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
                SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow, SetLayeredWindowAttributes,
                SetPropW, SetWindowPos, ShowWindow, WM_NCDESTROY, WM_PAINT, WM_SIZE, WM_SYSCOMMAND,
                WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT, WS_OVERLAPPEDWINDOW, WS_POPUP,
            },
        },
    },
    core::PCWSTR,
    w,
};

use super::{BASE_DPI, ok_or_last_error};
use crate::{NativeWindow, PILL_HEIGHT, PILL_MARGIN, PILL_WIDTH};

const DWM_VALUE_SIZE: u32 = 4;

const HIDE_LANE: PCWSTR = w!("SpeakeasyHideLane");

impl NativeWindow {
    pub fn configure_pill(&self) -> anyhow::Result<()> {
        let window = &self.native;
        let hwnd = window.hwnd;
        let border = DWMWA_COLOR_NONE;
        let corners = DWMWCP_DONOTROUND;

        // A caption style leaves a frame around the transparent pill; a plain popup has none.
        // The DWM border and corner hints below are best effort.
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
            // WS_EX_TRANSPARENT alone only changes paint order; layering lets clicks pass through.
            ok_or_last_error(SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA))?;
            // Style changes take effect only once SWP_FRAMECHANGED applies them.
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
    // SAFETY: the subclass runs on the pill's thread and removes itself at WM_NCDESTROY;
    // `paint` lives through its BeginPaint/EndPaint cycle.
    unsafe {
        match message {
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(pill_window), id);
                DefSubclassProc(hwnd, message, wparam, lparam)
            },
            // A hidden pill finishes its paint cycle without GPUI: no render, no pending paint.
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

    #[expect(
        clippy::let_underscore_must_use,
        reason = "The status pill can still be shown in its last valid position if repositioning fails"
    )]
    let _ = place_across_dpi(
        || unsafe { GetDpiForWindow(hwnd) },
        || {
            ok_or_last_error(unsafe {
                SetWindowPos(hwnd, HWND_TOPMOST, left, top, width, height, SWP_NOACTIVATE)
            })
        },
    );
}

/// Places a window, then again if that moved it onto a display of another DPI.
///
/// Arriving at a new DPI makes GPUI resize the window synchronously to the size Windows
/// suggests; the second placement restores the exact bounds.
pub(super) fn place_across_dpi(
    mut dpi: impl FnMut() -> u32,
    mut place: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let before = dpi();
    place()?;
    if dpi() != before {
        place()?;
    }
    Ok(())
}

fn centered_left(area: RECT, width: i32) -> i32 {
    let left = i64::from(area.left)
        .saturating_add(i64::from(area.right))
        .saturating_sub(i64::from(width))
        / 2;
    i32::try_from(left).unwrap_or(if left < 0 { i32::MIN } else { i32::MAX })
}

impl NativeWindow {
    /// Hides the window to the tray on minimize by sending on `hide`; install once per window.
    ///
    /// The callback only enqueues, since it runs inside GPUI's message dispatch. A second call
    /// replaces the `HIDE_LANE` property and leaks the first sender.
    pub fn minimize_to_tray(&self, hide: Sender<()>) -> anyhow::Result<()> {
        let hwnd = self.native.hwnd;
        let lane = Box::into_raw(Box::new(hide));

        // SAFETY: on success the window owns the boxed lane until `settings_window` frees it at
        // WM_NCDESTROY; on failure it is reclaimed here before any message can read it.
        unsafe {
            if SetPropW(hwnd, HIDE_LANE, lane.cast()) == FALSE
                || SetWindowSubclass(hwnd, Some(settings_window), 1, 0) == FALSE
            {
                RemovePropW(hwnd, HIDE_LANE);
                drop(Box::from_raw(lane));
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
    _: usize,
) -> LRESULT {
    // SAFETY: runs on the window's thread. Final destruction removes the subclass before freeing
    // the lane, so no later message reaches a freed sender.
    unsafe {
        match message {
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(settings_window), id);
                let lane = RemovePropW(hwnd, HIDE_LANE).cast::<Sender<()>>();
                if !lane.is_null() {
                    drop(Box::from_raw(lane));
                }
                DefSubclassProc(hwnd, message, wparam, lparam)
            },
            // WM_SYSCOMMAND reserves wParam's low four bits for the system.
            WM_SYSCOMMAND if wparam & 0xFFF0 == SC_MINIMIZE as usize => {
                request_hide(hwnd);
                0
            },
            // ShowWindow(SW_MINIMIZE), including GPUI's own minimize, bypasses WM_SYSCOMMAND.
            // Still forward the size so GPUI tracks the restore.
            WM_SIZE if wparam == SIZE_MINIMIZED as usize => {
                request_hide(hwnd);
                DefSubclassProc(hwnd, message, wparam, lparam)
            },
            _ => DefSubclassProc(hwnd, message, wparam, lparam),
        }
    }
}

unsafe fn request_hide(hwnd: HWND) {
    // SAFETY: `HIDE_LANE` holds the live boxed sender until WM_NCDESTROY removes it.
    let Some(hide) = (unsafe { GetPropW(hwnd, HIDE_LANE).cast::<Sender<()>>().as_ref() }) else {
        return;
    };
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
        ptr::{null, null_mut},
    };

    use windows_sys::Win32::{
        Graphics::Gdi::{GetUpdateRect, InvalidateRect, ValidateRect},
        UI::WindowsAndMessaging::{DefWindowProcW, SendMessageW},
    };

    use super::{
        super::fixture::{Class, Popup, native_window},
        *,
    };

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

    #[test]
    #[ignore = "Creates an owned native window; no microphone, hook, clipboard, or injected input"]
    fn hidden_pill_consumes_paint_and_visible_pill_reaches_renderer() -> anyhow::Result<()> {
        let _class = Class::register(PAINT_TEST_CLASS, Some(renderer), null_mut())?;
        let pill = Popup::styled(PAINT_TEST_CLASS, [WS_POPUP, WS_EX_NOACTIVATE], [0, 0, 1, 1])?;
        let hwnd = pill.0;

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

        pill.show();
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
    #[ignore = "Creates an owned native window; no microphone, hook, clipboard, or injected input"]
    fn a_window_minimizes_to_its_tray_lane_until_it_is_destroyed() -> anyhow::Result<()> {
        let settings = Popup::create(w!("STATIC"), 0, [0; 4])?;
        let (lane, hides) = async_channel::bounded(1);
        unsafe { native_window(settings.0) }?.minimize_to_tray(lane)?;

        unsafe { SendMessageW(settings.0, WM_SYSCOMMAND, SC_MINIMIZE as usize, 0) };
        drop(settings);
        assert_eq!(
            hides.try_recv(),
            Ok(()),
            "Minimize missed the installed lane"
        );
        assert!(hides.is_closed(), "A tray lane outlived its window");
        Ok(())
    }

    #[test]
    fn placement_reasserts_the_requested_bounds_after_dpi_changes() -> anyhow::Result<()> {
        for (dpi_after, expected) in [(96, 1), (144, 2)] {
            let mut dpis = [96, dpi_after].into_iter();
            let mut placements = 0;
            place_across_dpi(
                || dpis.next().expect("Sample DPI before and after placement"),
                || {
                    placements += 1;
                    Ok(())
                },
            )?;
            assert_eq!(placements, expected);
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
