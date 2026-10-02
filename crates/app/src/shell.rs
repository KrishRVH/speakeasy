//! The Settings window and the app-wide services behind it. Services owns every native service on
//! the UI thread; Settings edits a draft that applies only once saved.

mod demo;
mod services;
mod settings;
mod shutdown;
mod window;

pub(crate) use self::{
    services::{LaunchMode, Services, send, toggle_enabled},
    shutdown::request_quit,
    window::reveal,
};
