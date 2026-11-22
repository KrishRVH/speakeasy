mod command;
pub(crate) mod copy;
pub(crate) mod edits;
mod evidence;
mod mcp;
mod overlay;
pub(crate) mod preview;
mod publish;
mod recorder;
pub(crate) mod review;
mod store;
pub(crate) mod wsl;

pub(crate) use self::{
    command::Command,
    overlay::{Canvas, attend as attend_overlay},
    publish::{Origin, Publication, Reopened, publish, reopen},
    recorder::{ScreenRecorder, Unwound},
    store::{
        SavedShow, attend as attend_retention, checkpoint,
        checkpoint::{
            AudioLog, Checkpoint, Checkpointing, FrameLog, Interrupted, Keeper, Kept, Release,
        },
    },
};

pub(crate) fn keep(recorded: std::time::SystemTime) -> anyhow::Result<Kept> {
    Keeper::start(store::directory, recorded)
}
