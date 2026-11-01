use std::{
    fs::{self, File},
    io::{self, Read as _, Write as _},
    path::Path,
    time::{Duration, SystemTime},
};

use anyhow::Context as _;
use serde::Serialize;

use super::{
    KEPT_FOR, Staged,
    checkpoint::{
        self,
        audio::{self, KeptAudio},
    },
    recall,
};

const FILE: &str = "voice.log";
const EXPIRY: &str = "voice.expires";

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Status {
    Available,
    Incomplete,
    Absent,
    Expired,
}

pub(super) fn retain(
    checkpoint: &Path,
    staged: &Staged,
    recorded: SystemTime,
) -> anyhow::Result<()> {
    let expires = recorded
        .checked_add(KEPT_FOR)
        .context("Show voice expiry is out of range")?;
    if SystemTime::now() >= expires {
        return Ok(());
    }
    match recall::regular_file(checkpoint, audio::FILE, audio::FILE_LIMIT) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        result => result.context("Cannot retain Show voice; its checkpoint remains available")?,
    }
    let mut input =
        File::open(checkpoint.join(audio::FILE))?.take(audio::FILE_LIMIT.saturating_add(1));
    let mut output = File::create_new(staged.staging.path().join(FILE))?;
    let copied = io::copy(&mut input, &mut output)?;
    anyhow::ensure!(
        copied <= audio::FILE_LIMIT,
        "Show voice exceeds its storage limit"
    );
    output.sync_all()?;
    let mut expiry = File::create_new(staged.staging.path().join(EXPIRY))?;
    expiry.write_all(
        &expires
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_secs()
            .to_le_bytes(),
    )?;
    expiry.sync_all()?;
    checkpoint::sync_directory(staged.staging.path())?;
    Ok(())
}

fn deadline(directory: &Path) -> io::Result<Option<SystemTime>> {
    let bytes = match recall::read_file(directory, EXPIRY, 8) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_private_bytes| io::Error::other("Show voice expiry is damaged"))?;
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(u64::from_le_bytes(bytes)))
        .map(Some)
        .ok_or_else(|| io::Error::other("Show voice expiry is out of range"))
}

pub(crate) fn read(
    directory: &Path,
    now: SystemTime,
) -> anyhow::Result<(Status, Option<KeptAudio>)> {
    let Some(expires) = deadline(directory)? else {
        anyhow::ensure!(
            !directory.join(FILE).try_exists()?,
            "Show voice has no expiry; recover the original Show before exporting voice"
        );
        return Ok((Status::Absent, None));
    };
    if now >= expires {
        return Ok((Status::Expired, None));
    }
    recall::regular_file(directory, FILE, audio::FILE_LIMIT).context(
        "Cannot read this Show's voice; recover the original Show before exporting voice",
    )?;
    let audio = audio::read(File::open(directory.join(FILE))?)?;
    Ok((
        if audio.complete {
            Status::Available
        } else {
            Status::Incomplete
        },
        Some(audio),
    ))
}

pub(super) fn expire(directory: &Path, now: SystemTime) -> io::Result<()> {
    if deadline(directory)?.is_some_and(|expires| now >= expires) {
        match fs::remove_file(directory.join(FILE)) {
            Ok(()) => {},
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_voice_is_refused_and_erased_even_when_show_is_kept_or_renamed() -> anyhow::Result<()>
    {
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join(super::super::PIN), [])?;
        fs::write(directory.path().join(FILE), b"expired private audio")?;
        fs::write(directory.path().join(EXPIRY), 100_u64.to_le_bytes())?;
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let (status, audio) = read(directory.path(), now)?;
        assert!(matches!(status, Status::Expired));
        assert!(audio.is_none());
        expire(directory.path(), now)?;
        assert!(!directory.path().join(FILE).exists());
        assert!(directory.path().join(super::super::PIN).exists());
        Ok(())
    }
}
