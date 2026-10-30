//! Shows: screen recordings with narration and ink, published as private bundles and inspected
//! through the command line or recalled from the tray.

mod command;
pub(crate) mod edits;
mod overlay;
mod publish;
mod recorder;
pub(crate) mod review;
mod store;
pub(crate) use store::{
    checkpoint,
    checkpoint::{AudioLog, Checkpoint, Checkpointing, FrameLog, Interrupted, Keeper, Kept},
};

pub(crate) use self::{
    command::Command,
    overlay::{Canvas, attend as attend_overlay},
    publish::{Origin, Publication, Reopened, publish, reopen},
    recorder::{ScreenRecorder, Unwound},
    store::{SavedShow, attend as attend_retention},
};

/// Begins private checkpointing only for an explicitly requested Show.
pub(crate) fn keep(recorded: std::time::SystemTime) -> anyhow::Result<Kept> {
    Keeper::start(store::private_directory, recorded)
}
