//! Explicit retention changes and the cross-process lock shared with expiry.

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::Path,
    time::{Duration, SystemTime},
};

use super::{PIN, age, pinned};

/// A saved Show's retention, independent of whether its manifest is readable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Retention {
    Kept,
    Expires(SystemTime),
    Unknown,
}

impl Retention {
    pub(crate) fn read(directory: &Path) -> io::Result<Self> {
        Ok(Self::of(
            directory
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
            pinned(directory)?,
        ))
    }

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

/// Locks only existing storage. App cleanup and headless commands are separate processes, so
/// their pin-check/rename and keep operations cannot be serialized by the session owner.
pub(super) fn lock(parent: &Path) -> io::Result<File> {
    let path = parent.join(".retention.lock");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(io::Error::other(format!(
                "{} must be a regular file",
                path.display()
            )));
        },
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {},
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

/// Changes only the pin of an existing, directly managed Show. A successful keep is ordered
/// against expiry; it cannot recreate a folder cleanup already removed. Never touches edits.
pub(crate) fn set_kept(parent: &Path, directory: &Path, kept: bool) -> io::Result<Retention> {
    let _lock = lock(parent)?;
    super::address::managed_id(parent, directory)?.ok_or_else(|| {
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
        match fs::remove_file(pin) {
            Ok(()) => {},
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }
    }
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    Retention::read(directory)
}

#[cfg(test)]
mod tests;
