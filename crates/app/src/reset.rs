use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io,
    path::Path,
};

use anyhow::{anyhow, bail};

use crate::config::Config;

const DOWNLOADS: [&str; 4] = ["engines", "models", "downloads", "updates"];

pub(crate) fn purge(settings: &Path, data: &Path) -> anyhow::Result<()> {
    purge_with(settings, data, remove)
}

fn purge_with(
    settings: &Path,
    data: &Path,
    remove: impl Fn(&Path) -> io::Result<()>,
) -> anyhow::Result<()> {
    fs::create_dir_all(data).map_err(|error| cannot("create", data, &error))?;
    let _setup = claim(
        &data.join("setup.lock"),
        "Another Speakeasy copy is setting up local speech. Let it finish, then try again.",
    )?;
    let _update = claim(
        &data.join("updates.lock"),
        "Another Speakeasy copy is preparing an update. Finish or cancel it, then try again.",
    )?;
    for folder in DOWNLOADS.map(|folder| data.join(folder)) {
        remove(&folder).map_err(|error| cannot("remove", &folder, &error))?;
    }
    Config::discard(settings).map_err(|error| cannot("remove", settings, &error))
}

fn claim(lock: &Path, busy: &str) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock)
        .map_err(|error| cannot("open", lock, &error))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => bail!("{busy}"),
        Err(TryLockError::Error(error)) => Err(cannot("lock", lock, &error)),
    }
}

fn remove(path: &Path) -> io::Result<()> {
    let removed = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => fs::remove_file(path),
        Ok(_) => fs::remove_dir_all(path),
        Err(error) => Err(error),
    };
    match removed {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        removed => removed,
    }
}

fn cannot(action: &str, path: &Path, error: &io::Error) -> anyhow::Error {
    anyhow!(
        "Cannot {action} {}: {error}. {}",
        path.display(),
        crate::paths::remedy(error)
    )
}

#[cfg(test)]
mod tests;
