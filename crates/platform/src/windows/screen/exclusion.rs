use anyhow::Context as _;
use windows_sys::{
    Win32::{
        Foundation::{FALSE, HWND, LPARAM, LRESULT, TRUE, WPARAM},
        UI::{
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{
                EnumWindows, GetPropW, GetWindowDisplayAffinity, IsWindowVisible, RemovePropW,
                SetPropW, SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WM_NCDESTROY,
            },
        },
    },
    core::{BOOL, PCWSTR},
    w,
};

use super::Failure;
use crate::windows::{is_own_window, ok_or_last_error};

const ALWAYS_EXCLUDED: PCWSTR = w!("SpeakeasyAlwaysExcluded");

#[derive(Default)]
pub(super) struct Exclusions {
    left_out: Vec<HWND>,
}

impl Exclusions {
    pub(super) fn refresh(&mut self) -> Result<bool, Failure> {
        let before = self.left_out.len();
        let capturable = top_level_windows()?
            .into_iter()
            .filter(|&window| is_own_window(window) && affinity(window) == Some(WDA_NONE));
        for window in capturable {
            let (excluded, visible) = unsafe {
                (
                    SetWindowDisplayAffinity(window, WDA_EXCLUDEFROMCAPTURE) != FALSE,
                    IsWindowVisible(window) != FALSE,
                )
            };
            if excluded {
                self.left_out.push(window);
            } else if visible {
                return Err(Failure::Exposed);
            }
        }
        Ok(self.left_out.len() > before)
    }
}

impl Drop for Exclusions {
    fn drop(&mut self) {
        let restorable = self.left_out.iter().copied().filter(|&window| {
            !excluded_for_good(window) && affinity(window) == Some(WDA_EXCLUDEFROMCAPTURE)
        });
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

pub(super) fn exclude_always(hwnd: HWND) -> anyhow::Result<()> {
    unsafe {
        ok_or_last_error(SetWindowSubclass(hwnd, Some(always_excluded), 1, 0))?;
        ok_or_last_error(SetPropW(hwnd, ALWAYS_EXCLUDED, hwnd.cast()))?;
        ok_or_last_error(SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE))
            .context("Cannot keep the Show overlay out of screen capture")
    }
}

pub(super) fn excluded_for_good(window: HWND) -> bool {
    !unsafe { GetPropW(window, ALWAYS_EXCLUDED) }.is_null()
}

unsafe extern "system" fn always_excluded(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        if message == WM_NCDESTROY {
            RemovePropW(hwnd, ALWAYS_EXCLUDED);
            RemoveWindowSubclass(hwnd, Some(always_excluded), id);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windows::with_foreign_window;

    #[test]
    #[ignore = "Creates an owned native window; no microphone, hook, clipboard, or injected input"]
    fn an_exclusion_without_its_subclass_leaves_no_mark() -> anyhow::Result<()> {
        let exclude = |hwnd| {
            let refused = exclude_always(hwnd).is_err();
            let marked = excluded_for_good(hwnd);
            (refused, marked)
        };
        let ((), (refused, marked)) = with_foreign_window(|_| (), exclude)?;
        assert!(refused, "A window another thread owns was subclassed");
        assert!(
            !marked,
            "The mark outlives the window, with no subclass to remove it"
        );
        Ok(())
    }
}
