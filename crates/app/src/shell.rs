mod choices;
mod demo;
mod lifecycle;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod recovery;
mod save;
mod services;
mod settings;
mod shutdown;
mod window;

pub(crate) use settings::onboarding::pending as onboarding_pending;
#[cfg(all(feature = "native-acceptance", target_os = "macos"))]
pub(crate) use shutdown::mac_acceptance::native_quit_check;

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) use self::recovery::LastShow;
pub(crate) use self::{
    services::{LaunchMode, Services, send, toggle_enabled},
    shutdown::request_quit,
    window::{keep_pill_presented, quit_after_settings, reveal, reveal_shows},
};
