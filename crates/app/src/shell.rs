//! The Settings window and the app-wide services behind it. Services owns every native service on
//! the UI thread; Settings edits a draft that applies only once saved, apart from a demo's theme
//! and motion previews.

mod demo;
mod lifecycle;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod recovery;
mod save;
mod services;
mod settings;
mod shutdown;
mod window;

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) use self::recovery::LastShow;
pub(crate) use self::{
    services::{LaunchMode, Services, send, toggle_enabled},
    shutdown::request_quit,
    window::{quit_after_settings, reveal, reveal_shows},
};
