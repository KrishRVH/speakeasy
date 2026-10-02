//! The dictation service without a user interface: one session owner for capture, local
//! recognition, and insertion, plus the settings, setup, and instance ownership around it.
//!
//! A shell renders [`runtime::Snapshot`]s and forwards user intent; it never reads audio or owns
//! session state.

#![forbid(unsafe_code)]

pub mod audio;
mod child;
pub mod config;
pub mod instance;
pub mod lifecycle;
mod local_speech;
mod ports;
pub mod runtime;
pub mod save;
pub mod setup;
pub mod status;
pub mod theme;
mod transcript;
