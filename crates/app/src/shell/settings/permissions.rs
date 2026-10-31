//! Settings observes consent on opening and activation. The snapshot never authorizes a session
//! or replaces a device error, and demo windows never call the native queries.

use gpui::{Context, Window};
use speakeasy_platform::{Permission, Permissions};

use super::Settings;

impl Settings {
    pub(in crate::shell) fn follow_permissions(
        &mut self,
        window: &mut Window,
        cx: &Context<'_, Self>,
        mut read: impl FnMut() -> Permissions + 'static,
    ) {
        self.permissions = Some(read());
        cx.observe_window_activation(window, move |view, window, cx| {
            if window.is_window_active() {
                view.permissions = Some(read());
                cx.notify();
            }
        })
        .detach();
    }
}

pub(super) const fn description(permission: Permission) -> &'static str {
    match permission {
        Permission::Unknown => "Checked when used",
        Permission::Granted => "Allowed",
        Permission::NotGranted => "Not enabled",
        Permission::NotRequested => "Not requested yet",
        Permission::Restricted => "Restricted by system policy",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Help {
    Microphone,
    #[cfg(target_os = "macos")]
    Accessibility,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Screen,
}

impl Help {
    pub(super) const fn url(self) -> &'static str {
        #[cfg(target_os = "macos")]
        match self {
            Self::Microphone => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
            },
            Self::Accessibility => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            },
            Self::Screen => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            },
        }
        #[cfg(target_os = "windows")]
        match self {
            Self::Microphone => "ms-settings:privacy-microphone",
            Self::Screen => "ms-settings:privacy-graphicscaptureprogrammatic",
        }
        #[cfg(target_os = "linux")]
        match self {
            Self::Microphone => "https://github.com/KrishRVH/speakeasy#linux-experimental",
        }
    }
}
