use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read as _, Write as _},
    path::Path,
    time::{Duration, SystemTime},
};

use anyhow::Context as _;
use serde::Serialize;
use speakeasy_platform::ReadDirectory;

use super::{
    KEPT_FOR, Staged,
    checkpoint::audio::{self, KeptAudio},
    found, recall, remove_if_present, sync_directory,
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

pub(crate) enum Voice {
    Kept(KeptAudio),
    Absent,
    Expired,
}

impl Voice {
    pub(crate) const fn status(&self) -> Status {
        match self {
            Self::Kept(audio) if audio.complete => Status::Available,
            Self::Kept(_) => Status::Incomplete,
            Self::Absent => Status::Absent,
            Self::Expired => Status::Expired,
        }
    }

    pub(crate) const fn audio(&self) -> Option<&KeptAudio> {
        match self {
            Self::Kept(audio) => Some(audio),
            Self::Absent | Self::Expired => None,
        }
    }
}

pub(super) fn retain(
    checkpoint: &Path,
    staged: &Staged,
    recorded: SystemTime,
) -> anyhow::Result<bool> {
    let expires = recorded
        .checked_add(KEPT_FOR)
        .context("Show voice expiry is out of range")?;
    if SystemTime::now() >= expires {
        return Ok(false);
    }
    let mut input = match audio::available(checkpoint, audio::FILE) {
        Ok(Some(input)) => input,
        Ok(None) => return Ok(false),
        Err(damaged) if damaged.is::<audio::Damaged>() => return Ok(false),
        Err(error) => return Err(error),
    };
    let cut = !audio::ended(&mut input)?;
    let mut input = input.take(audio::FILE_LIMIT.saturating_add(1));
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
    sync_directory(staged.staging.path())?;
    Ok(cut)
}

fn deadline(directory: &ReadDirectory) -> io::Result<Option<SystemTime>> {
    let Some(bytes) = found(recall::read_from(directory, EXPIRY, 8))? else {
        return Ok(None);
    };
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_private_bytes| io::Error::other("Show voice expiry is damaged"))?;
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(u64::from_le_bytes(bytes)))
        .map(Some)
        .ok_or_else(|| io::Error::other("Show voice expiry is out of range"))
}

pub(crate) fn read(directory: &ReadDirectory, now: SystemTime) -> anyhow::Result<Voice> {
    const DAMAGED: &str =
        "Cannot read this Show's voice; recover the original Show before exporting voice";
    let Some(expires) = deadline(directory)? else {
        anyhow::ensure!(
            !directory.contains(OsStr::new(FILE))?,
            "Show voice has no expiry; recover the original Show before exporting voice"
        );
        return Ok(Voice::Absent);
    };
    if now >= expires {
        return Ok(Voice::Expired);
    }
    match audio::available_from(directory, FILE).context(DAMAGED)? {
        Some(file) => audio::read(file).map(Voice::Kept),
        None if directory.contains(OsStr::new(FILE))? => Ok(Voice::Absent),
        None => anyhow::bail!(DAMAGED),
    }
}

pub(super) fn expire(directory: &Path, now: SystemTime) -> io::Result<()> {
    let Some(folder) = found(recall::open_directory(directory))? else {
        return Ok(());
    };
    if deadline(&folder)?.is_some_and(|expires| now >= expires) {
        remove_if_present(&directory.join(FILE))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn expired_voice_is_refused_and_erased_even_when_show_is_kept_or_renamed() -> anyhow::Result<()>
    {
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join(super::super::PIN), [])?;
        fs::write(directory.path().join(FILE), b"expired private audio")?;
        fs::write(directory.path().join(EXPIRY), 100_u64.to_le_bytes())?;
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let folder = ReadDirectory::open(directory.path())?;
        assert!(matches!(read(&folder, now)?, Voice::Expired));
        expire(directory.path(), now)?;
        assert!(!directory.path().join(FILE).exists());
        assert!(directory.path().join(super::super::PIN).exists());
        Ok(())
    }

    #[test]
    fn an_existing_header_only_voice_file_exports_without_voice() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join(FILE), b"SPKAUD01")?;
        fs::write(directory.path().join(EXPIRY), 100_u64.to_le_bytes())?;
        assert!(matches!(
            read(
                &ReadDirectory::open(directory.path())?,
                SystemTime::UNIX_EPOCH
            )?,
            Voice::Absent
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn voice_without_its_expiry_is_refused_even_as_a_dangling_link() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        std::os::unix::fs::symlink(
            directory.path().join("missing"),
            directory.path().join(FILE),
        )?;
        let message = read(&ReadDirectory::open(directory.path())?, SystemTime::now())
            .err()
            .context("A dangling voice link was read as absent voice")?
            .to_string();
        assert!(message.contains("no expiry"), "{message}");
        Ok(())
    }
}
