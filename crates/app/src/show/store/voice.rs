use std::{fs::File, io, path::Path, time::SystemTime};

use anyhow::Context as _;
use serde::Serialize;

use super::{
    KEPT_FOR, Staged,
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

pub(super) fn retain(
    checkpoint: &Path,
    staged: &Staged,
    recorded: SystemTime,
) -> anyhow::Result<bool> {
    if recorded
        .checked_add(KEPT_FOR)
        .is_none_or(|expires| SystemTime::now() >= expires)
    {
        return Ok(false);
    }
    let mut input = match audio::available(checkpoint, audio::FILE) {
        Ok(Some(input)) => input,
        Ok(None) => return Ok(false),
        Err(damaged) if damaged.is::<audio::Damaged>() => return Ok(false),
        Err(error) => return Err(error),
    };
    let cut = !audio::ended(&mut input)?;
    io::copy(
        &mut input,
        &mut File::create_new(staged.staging.path().join(FILE))?,
    )?;
    Ok(cut)
}

pub(crate) fn read(directory: &Path) -> anyhow::Result<Option<KeptAudio>> {
    audio::available(directory, FILE)
        .context("Cannot read this Show's voice")?
        .map(audio::read)
        .transpose()
}
