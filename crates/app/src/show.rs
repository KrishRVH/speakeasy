//! Shows: screen recordings with narration and ink, published as private bundles and inspected
//! through the command line or recalled from the tray.

mod command;
pub(crate) mod edits;
mod overlay;
mod publish;
mod recorder;
pub(crate) mod review;
mod store;

pub(crate) use store::checkpoint::{Checkpoint, Interrupted};

pub(crate) use self::{
    command::Command,
    overlay::{Canvas, attend as attend_overlay},
    publish::{publish, recover},
    recorder::ScreenRecorder,
    store::{SavedShow, attend as attend_retention},
};
