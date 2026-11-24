use std::{fs, io, path::Path};

use anyhow::Context as _;
use serde::Serialize;

use super::{
    Staged,
    checkpoint::audio::{self, KeptAudio},
};

const FILE: &str = "voice.log";

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Status {
    Available,
    Incomplete,
    Absent,
}

impl Status {
    pub(crate) const fn of(voice: Option<&KeptAudio>) -> Self {
        match voice {
            Some(audio) if audio.complete => Self::Available,
            Some(_) => Self::Incomplete,
            None => Self::Absent,
        }
    }
}

pub(super) fn retain(kept: &Path, staged: &Staged) -> anyhow::Result<()> {
    match fs::copy(kept, staged.staging.path().join(FILE)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        copied => copied
            .map(|_bytes| ())
            .context("Can't save this Show's audio"),
    }
}

pub(crate) fn read(directory: &Path) -> anyhow::Result<Option<KeptAudio>> {
    audio::read(&directory.join(FILE)).context("Can't read this Show's audio")
}
