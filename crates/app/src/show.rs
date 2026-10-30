//! Shows: screen recordings with narration and ink, published as private bundles and inspected
//! through the command line or recalled from the tray.

mod command;
mod overlay;
mod publish;
mod recorder;
pub(crate) mod review;
mod store;

pub(crate) use self::{
    command::Command,
    overlay::{Canvas, attend as attend_overlay},
    publish::publish,
    recorder::ScreenRecorder,
    store::{SavedShow, attend as attend_retention},
};
