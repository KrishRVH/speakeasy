//! Keeping Speakeasy's own windows out of screen capture.
//!
//! A capture leaves out every window of this process while it runs and shows them to capture again
//! when it ends, except the overlay, which `configure_overlay` leaves out for good.

use anyhow::{Context as _, ensure};
use windows_sys::{
    Wdk::System::SystemServices::RtlGetVersion,
    Win32::{
        Foundation::{FALSE, HWND, LPARAM, LRESULT, TRUE, WPARAM},
        System::{SystemInformation::OSVERSIONINFOW, Threading::GetCurrentProcessId},
        UI::{
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{
                EnumWindows, GetPropW, GetWindowDisplayAffinity, GetWindowThreadProcessId,
                IsWindowVisible, RemovePropW, SetPropW, SetWindowDisplayAffinity,
                WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WM_NCDESTROY,
            },
        },
    },
    core::{BOOL, PCWSTR},
    w,
};

use super::Failure;
use crate::windows::ok_or_last_error;

/// The first build that leaves excluded windows out of capture; earlier builds black them out.
const EXCLUSION_BUILD: u32 = 19_041;
/// Marks a window `configure_overlay` left out of all screen capture, so that a capture ending
/// never shows it to capture again.
pub(super) const ALWAYS_EXCLUDED: PCWSTR = w!("SpeakeasyAlwaysExcluded");

/// The Speakeasy windows one capture has left out, shown to capture again when it ends.
#[derive(Default)]
pub(super) struct Exclusions {
    changed: Vec<HWND>,
}

impl Exclusions {
    /// Leaves out every window of this process that capture would still show, hidden ones too so
    /// they stay out once shown. Reports whether any was newly left out: even one hidden now may
    /// have been shown in frames composed since the last check.
    pub(super) fn refresh(&mut self) -> Result<bool, Failure> {
        let mut newcomers = false;
        let own = top_level_windows()?
            .into_iter()
            .filter(|&window| owned(window));
        for window in own.filter(|&window| affinity(window) == Some(WDA_NONE)) {
            // SAFETY: both calls take a window of this process by value.
            let (excluded, visible) = unsafe {
                (
                    SetWindowDisplayAffinity(window, WDA_EXCLUDEFROMCAPTURE) != FALSE,
                    IsWindowVisible(window) != FALSE,
                )
            };
            if excluded {
                self.changed.push(window);
                newcomers = true;
            } else if visible {
                return Err(Failure::Exposed);
            }
        }
        Ok(newcomers)
    }
}

impl Drop for Exclusions {
    fn drop(&mut self) {
        let restorable = |window: HWND| {
            // SAFETY: reads a property by handle; a destroyed window has none.
            let marked = !unsafe { GetPropW(window, ALWAYS_EXCLUDED) }.is_null();
            !marked && affinity(window) == Some(WDA_EXCLUDEFROMCAPTURE)
        };
        for &window in self.changed.iter().filter(|&&window| restorable(window)) {
            // SAFETY: takes values only; a window since destroyed, or reused by another process,
            // fails the call.
            unsafe { SetWindowDisplayAffinity(window, WDA_NONE) };
        }
    }
}

/// Every top-level window on the desktop, front to back.
pub(super) fn top_level_windows() -> windows::core::Result<Vec<HWND>> {
    unsafe extern "system" fn collect(window: HWND, windows: LPARAM) -> BOOL {
        // SAFETY: `top_level_windows` passes its live vector for the length of the enumeration.
        unsafe { (*(windows as *mut Vec<HWND>)).push(window) };
        TRUE
    }
    let mut windows = Vec::new();
    // SAFETY: the callback only pushes into the vector, which outlives the enumeration.
    let enumerated = unsafe { EnumWindows(Some(collect), (&raw mut windows) as LPARAM) };
    if enumerated == FALSE {
        return Err(windows::core::Error::from_thread());
    }
    Ok(windows)
}

pub(super) fn owned(window: HWND) -> bool {
    let mut process = 0;
    // SAFETY: writes only the live local; neither call retains anything.
    unsafe {
        GetWindowThreadProcessId(window, &raw mut process);
        process == GetCurrentProcessId()
    }
}

pub(super) fn affinity(window: HWND) -> Option<u32> {
    let mut affinity = 0;
    // SAFETY: writes only the live local.
    (unsafe { GetWindowDisplayAffinity(window, &raw mut affinity) } != FALSE).then_some(affinity)
}

/// Leaves `hwnd` out of all screen capture for good, marked so no capture ending restores it.
pub(super) fn exclude_always(hwnd: HWND) -> anyhow::Result<()> {
    ensure!(exclusion_supported(), Failure::OldWindows);
    // SAFETY: each call takes the handle by value and fails for one that names no window. Installing
    // the subclass, which carries no reference data, also fails for a window another thread owns,
    // so the mark is set only once the subclass is in place to remove it before the window is
    // destroyed, as properties require.
    unsafe {
        ok_or_last_error(SetWindowSubclass(hwnd, Some(always_excluded), 1, 0))?;
        ok_or_last_error(SetPropW(hwnd, ALWAYS_EXCLUDED, hwnd.cast()))?;
        ok_or_last_error(SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE))
            .context("Cannot keep the Show overlay out of screen capture")
    }
}

unsafe extern "system" fn always_excluded(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    // SAFETY: this window-owned subclass runs on the window's thread and is removed, with the
    // mark, at destruction.
    unsafe {
        if message == WM_NCDESTROY {
            RemovePropW(hwnd, ALWAYS_EXCLUDED);
            RemoveWindowSubclass(hwnd, Some(always_excluded), id);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

/// Whether this Windows build leaves excluded windows out of capture rather than blacking them out.
pub(super) fn exclusion_supported() -> bool {
    windows_build().is_some_and(|build| build >= EXCLUSION_BUILD)
}

/// The running Windows build, read from the kernel: the documented version APIs report Windows 8
/// to executables whose manifests list no newer version.
fn windows_build() -> Option<u32> {
    let mut version = OSVERSIONINFOW {
        dwOSVersionInfoSize: u32::try_from(size_of::<OSVERSIONINFOW>()).ok()?,
        ..Default::default()
    };
    // SAFETY: RtlGetVersion fills this live structure, whose size field it checks.
    let status = unsafe { RtlGetVersion(&raw mut version) };
    (status >= 0).then_some(version.dwBuildNumber)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windows::with_foreign_window;

    #[test]
    #[ignore = "Creates an owned native window; no microphone, hook, clipboard, or injected input"]
    fn an_exclusion_without_its_subclass_leaves_no_mark() -> anyhow::Result<()> {
        // Subclasses work only on the thread that owns the window, so exclusion from another
        // thread fails to install one.
        let exclude = |hwnd| {
            let refused = exclude_always(hwnd).is_err();
            // SAFETY: reads a property of a window `with_foreign_window` keeps live.
            let marked = !unsafe { GetPropW(hwnd, ALWAYS_EXCLUDED) }.is_null();
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
