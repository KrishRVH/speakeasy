use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::Context as _;

use super::{PIN, address, age, remove_if_present};

pub(crate) struct Deleted {
    pub checkpoint: Option<PathBuf>,
    pub cleanup: Option<io::Error>,
}

pub(crate) fn delete(parent: &Path, directory: &Path) -> anyhow::Result<Deleted> {
    address::managed_id(parent, directory)
        .context("Only Shows in the managed Shows folder can be deleted here")?;
    anyhow::ensure!(
        directory.is_dir(),
        "This Show is already gone. Refresh Shows."
    );
    Ok(Deleted {
        checkpoint: super::checkpoint::discard_published(parent, directory)?,
        cleanup: fs::remove_dir_all(directory).err(),
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
        let [year, month, day, hour, minute, second] = super::utc_fields(at);
        Some(format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
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

pub(crate) fn set_kept(parent: &Path, directory: &Path, kept: bool) -> io::Result<Retention> {
    let id = address::managed_id(parent, directory).ok_or_else(|| {
        io::Error::other(format!(
            "Only Shows directly in {} expire; this folder is not managed",
            parent.display()
        ))
    })?;
    let pin = directory.join(PIN);
    if kept {
        fs::write(pin, [])?;
    } else {
        remove_if_present(&pin)?;
    }
    Ok(Retention::of(id.as_str(), kept))
}

#[cfg(test)]
mod tests;
