use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::Context as _;

use super::{PIN, age, found, pinned, remove_if_present};

pub(crate) struct Deleted {
    pub checkpoint: Option<PathBuf>,
    pub cleanup: Option<io::Error>,
}

pub(crate) fn delete(parent: &Path, directory: &Path) -> anyhow::Result<Deleted> {
    let (retired, checkpoint) = {
        let _lock = lock(parent)?;
        let id = super::address::managed_id(parent, directory)?
            .context("Only Shows in the managed Shows folder can be deleted here")?;
        let checkpoint = super::checkpoint::discard_published(parent, directory)?;
        let retired = super::retire(parent, directory, id.as_str())
            .context("Cannot delete this Show. Close its open files, refresh, and try again")?;
        (retired, checkpoint)
    };
    let cleanup = super::sync_directory(parent)
        .and_then(|()| found(fs::remove_dir_all(&retired)))
        .and_then(|_removed| super::sync_directory(parent))
        .err();
    Ok(Deleted {
        checkpoint,
        cleanup,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Retention {
    Kept,
    Expires(SystemTime),
    Unknown,
}

impl Retention {
    pub(crate) fn of(name: &str, kept: bool) -> Self {
        if kept {
            Self::Kept
        } else {
            age::expires_at(name).map_or(Self::Unknown, Self::Expires)
        }
    }

    pub(crate) fn label(self, now: SystemTime) -> String {
        match self {
            Self::Kept => "Kept".into(),
            Self::Unknown => "Retention status unavailable".into(),
            Self::Expires(at) => match at.duration_since(now) {
                Ok(left) if !left.is_zero() => format!("Expires in {}", interval(left)),
                _ => "Expires at the next cleanup".into(),
            },
        }
    }

    pub(crate) fn expires_at(self) -> Option<String> {
        let Self::Expires(at) = self else {
            return None;
        };
        let stamp = super::utc_stamp(at);
        Some(format!(
            "{}-{}-{}T{}:{}:{}Z",
            stamp.get(..4)?,
            stamp.get(4..6)?,
            stamp.get(6..8)?,
            stamp.get(9..11)?,
            stamp.get(11..13)?,
            stamp.get(13..15)?,
        ))
    }
}

pub(crate) fn interval(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (count, unit) = if seconds >= 86_400 {
        (seconds / 86_400, "day")
    } else if seconds >= 3_600 {
        (seconds / 3_600, "hour")
    } else {
        ((seconds / 60).max(1), "minute")
    };
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

pub(super) fn lock(parent: &Path) -> io::Result<File> {
    let path = parent.join(".retention.lock");
    if found(fs::symlink_metadata(&path))?.is_some_and(|metadata| !metadata.is_file()) {
        return Err(io::Error::other(format!(
            "{} must be a regular file",
            path.display()
        )));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    file.lock()?;
    Ok(file)
}

pub(crate) fn set_kept(parent: &Path, directory: &Path, kept: bool) -> io::Result<Retention> {
    let _lock = lock(parent)?;
    let id = super::address::managed_id(parent, directory)?.ok_or_else(|| {
        io::Error::other(format!(
            "Only Shows directly in {} expire; this folder is not managed",
            parent.display()
        ))
    })?;
    let pin = directory.join(PIN);
    if kept {
        match File::create_new(pin) {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {},
            Err(error) => return Err(error),
        }
    } else {
        remove_if_present(&pin)?;
    }
    super::sync_directory(directory)?;
    Ok(Retention::of(id.as_str(), pinned(directory)?))
}

#[cfg(test)]
mod tests;
