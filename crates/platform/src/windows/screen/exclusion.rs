use windows_sys::{
    Win32::{
        Foundation::{FALSE, HWND, LPARAM, TRUE},
        UI::WindowsAndMessaging::{
            EnumWindows, GetWindowDisplayAffinity, IsWindowVisible, SetWindowDisplayAffinity,
            WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
        },
    },
    core::BOOL,
};

use super::Failure;
use crate::windows::is_own_window;

#[derive(Default)]
pub(super) struct Exclusions {
    left_out: Vec<HWND>,
}

impl Exclusions {
    pub(super) fn refresh(&mut self) -> Result<bool, Failure> {
        let mut excluded_any = false;
        for window in top_level_windows()? {
            if !is_own_window(window) || affinity(window) != Some(WDA_NONE) {
                continue;
            }
            let (excluded, visible) = unsafe {
                (
                    SetWindowDisplayAffinity(window, WDA_EXCLUDEFROMCAPTURE) != FALSE,
                    IsWindowVisible(window) != FALSE,
                )
            };
            if excluded {
                excluded_any = true;
                if !self.left_out.contains(&window) {
                    self.left_out.push(window);
                }
            } else if visible {
                return Err(Failure::Exposed);
            }
        }
        Ok(excluded_any)
    }
}

impl Drop for Exclusions {
    fn drop(&mut self) {
        let restorable = self
            .left_out
            .iter()
            .copied()
            .filter(|&window| affinity(window) == Some(WDA_EXCLUDEFROMCAPTURE));
        for window in restorable {
            unsafe { SetWindowDisplayAffinity(window, WDA_NONE) };
        }
    }
}
pub(super) fn top_level_windows() -> windows::core::Result<Vec<HWND>> {
    unsafe extern "system" fn collect(window: HWND, found: LPARAM) -> BOOL {
        unsafe { (*(found as *mut Vec<HWND>)).push(window) };
        TRUE
    }
    let mut found = Vec::new();

    let enumerated = unsafe { EnumWindows(Some(collect), (&raw mut found) as LPARAM) };
    if enumerated == FALSE {
        return Err(windows::core::Error::from_thread());
    }
    Ok(found)
}

pub(super) fn affinity(window: HWND) -> Option<u32> {
    let mut affinity = 0;

    (unsafe { GetWindowDisplayAffinity(window, &raw mut affinity) } != FALSE).then_some(affinity)
}
