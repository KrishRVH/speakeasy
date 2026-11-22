use std::{
    num::NonZeroIsize,
    ptr::{null, null_mut},
};

use anyhow::{Context as _, ensure};
use raw_window_handle::{RawWindowHandle, Win32WindowHandle, WindowHandle};
use windows_sys::{
    Win32::{
        Foundation::HWND,
        Graphics::Gdi::HBRUSH,
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, RegisterClassW, SW_SHOWNOACTIVATE, ShowWindow,
            UnregisterClassW, WNDCLASSW, WNDPROC, WS_POPUP,
        },
    },
    core::PCWSTR,
};

use crate::NativeWindow;

pub(super) unsafe fn native_window(hwnd: HWND) -> anyhow::Result<NativeWindow> {
    let raw = NonZeroIsize::new(hwnd as isize).context("Missing window")?;
    let raw = RawWindowHandle::Win32(Win32WindowHandle::new(raw));

    NativeWindow::new(unsafe { WindowHandle::borrow_raw(raw) })
}

pub(super) struct Class(PCWSTR);

impl Class {
    pub(super) fn register(
        name: PCWSTR,
        procedure: WNDPROC,
        background: HBRUSH,
    ) -> anyhow::Result<Self> {
        let class = WNDCLASSW {
            lpfnWndProc: procedure,
            hInstance: unsafe { GetModuleHandleW(null()) },
            hbrBackground: background,
            lpszClassName: name,
            ..Default::default()
        };

        let registered = unsafe { RegisterClassW(&raw const class) } != 0;
        ensure!(registered, "Cannot register a test window class");
        Ok(Self(name))
    }
}

impl Drop for Class {
    fn drop(&mut self) {
        unsafe { UnregisterClassW(self.0, GetModuleHandleW(null())) };
    }
}

pub(super) struct Popup(pub(super) HWND);

impl Popup {
    pub(super) fn create(class: PCWSTR, extended: u32, bounds: [i32; 4]) -> anyhow::Result<Self> {
        Self::styled(class, [WS_POPUP, extended], bounds)
    }

    pub(super) fn styled(
        class: PCWSTR,
        [style, extended]: [u32; 2],
        [left, top, right, bottom]: [i32; 4],
    ) -> anyhow::Result<Self> {
        let [width, height] = [right.saturating_sub(left), bottom.saturating_sub(top)];

        let hwnd = unsafe {
            CreateWindowExW(
                extended,
                class,
                class,
                style,
                left,
                top,
                width,
                height,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            )
        };
        ensure!(!hwnd.is_null(), "Cannot create a test window");
        Ok(Self(hwnd))
    }

    pub(super) fn show(&self) {
        unsafe { ShowWindow(self.0, SW_SHOWNOACTIVATE) };
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.0) };
    }
}
