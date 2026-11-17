use std::{io, path::PathBuf};

use anyhow::{Context as _, bail, ensure};

use super::registry;

pub fn installed_directory() -> anyhow::Result<Option<PathBuf>> {
    let path = match registry::text(
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\io.github.krvh.speakeasy_is1",
        "InstallLocation",
    ) {
        Ok(Some(path)) => PathBuf::from(path),
        Ok(None) => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            bail!("The Windows installation path is invalid.")
        },
        Err(error) => {
            return Err(error).context(
                "Cannot read the Windows installation. Open the release page to update manually.",
            );
        },
    };
    ensure!(
        path.is_absolute(),
        "The Windows installation path is not absolute."
    );
    Ok(Some(path))
}
