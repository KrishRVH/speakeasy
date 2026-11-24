use std::{fs, io, path::Path};

use anyhow::anyhow;

use crate::config::Config;

const DOWNLOADS: [&str; 4] = ["engines", "models", "downloads", "updates"];

pub(crate) fn purge(settings: &Path, data: &Path) -> anyhow::Result<()> {
    for folder in DOWNLOADS.map(|folder| data.join(folder)) {
        if let Err(error) = fs::remove_dir_all(&folder)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(cannot_remove(&folder, &error));
        }
    }
    Config::discard(settings).map_err(|error| cannot_remove(settings, &error))
}

fn cannot_remove(path: &Path, error: &io::Error) -> anyhow::Error {
    anyhow!(
        "Can't remove {}: {error}. {}",
        path.display(),
        crate::paths::remedy(error)
    )
}

#[cfg(test)]
mod tests;
