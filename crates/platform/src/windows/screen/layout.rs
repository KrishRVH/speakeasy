use windows_sys::Win32::{
    Foundation::{FALSE, HWND},
    UI::WindowsAndMessaging::{
        GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GetWindow, GetWindowLongW, IsIconic, IsWindowVisible,
        WS_CAPTION, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    },
};

use super::{Display, class_name, exclusion::top_level_windows, frame_bounds, is_cloaked};
use crate::{
    screen::{Listed, WindowLayer, geometry::Bounds},
    windows::window_process,
};

const DESKTOP_CLASSES: [&str; 2] = ["Progman", "WorkerW"];

const BAR_CLASSES: [&str; 2] = ["Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

const MENU_CLASS: &str = "#32768";

pub(crate) fn stack(display: Display) -> Option<Vec<Listed>> {
    let own = std::process::id();
    let windows = top_level_windows().ok()?;
    Some(
        windows
            .into_iter()
            .filter_map(|window| listed(display, window, own))
            .collect(),
    )
}

fn listed(display: Display, window: HWND, own: u32) -> Option<Listed> {
    let process = window_process(window).filter(|&process| process != own)?;
    let layer = classify(window)?;
    let part = display
        .bounds()
        .part_covered_by(Bounds::from(frame_bounds(window)?))?;
    Some(Listed {
        window: u64::try_from(window.addr()).ok()?,
        process: u64::from(process),
        layer,
        part,
    })
}

pub(super) fn classify(window: HWND) -> Option<WindowLayer> {
    let (shown, style, extended) = unsafe {
        (
            IsWindowVisible(window) != FALSE && IsIconic(window) == FALSE,
            GetWindowLongW(window, GWL_STYLE).cast_unsigned(),
            GetWindowLongW(window, GWL_EXSTYLE).cast_unsigned(),
        )
    };
    if !shown || extended & WS_EX_TRANSPARENT != 0 || is_cloaked(window) {
        return None;
    }
    let class = class_name(window);
    if DESKTOP_CLASSES.contains(&class.as_str()) {
        None
    } else if BAR_CLASSES.contains(&class.as_str()) {
        Some(WindowLayer::Bar)
    } else if class == MENU_CLASS || is_transient(window, style, extended) {
        Some(WindowLayer::Menu)
    } else if extended & WS_EX_TOPMOST != 0 {
        Some(WindowLayer::Floating)
    } else {
        Some(WindowLayer::Normal)
    }
}

fn is_transient(window: HWND, style: u32, extended: u32) -> bool {
    let popup = style & WS_POPUP != 0 && style & WS_CAPTION != WS_CAPTION;
    popup && (extended & WS_EX_TOOLWINDOW != 0 || !unsafe { GetWindow(window, GW_OWNER) }.is_null())
}
