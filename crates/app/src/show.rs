mod command;
pub(crate) mod copy;
pub(crate) mod edits;
pub(crate) mod evidence;
pub(crate) mod mcp;
mod overlay;
pub(crate) mod preview;
mod publish;
mod recorder;
pub(crate) mod review;
mod store;

pub(crate) use self::{
    command::Command,
    overlay::{Canvas, attend as attend_overlay},
    publish::{Origin, Publication, Reopened, publish, reopen},
    recorder::{ScreenRecorder, Unwound},
    store::{
        SavedShow, attend as attend_retention, checkpoint,
        checkpoint::{AudioLog, Checkpoint, Checkpointing, FrameLog, Interrupted, Keeper, Kept},
    },
};

pub(crate) fn keep(recorded: std::time::SystemTime) -> anyhow::Result<Kept> {
    Keeper::start(store::private_directory, recorded)
}
