use std::path::PathBuf;

use anyhow::Context as _;

use super::registry;

const REGISTRATION: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\io.github.krvh.speakeasy_is1";

pub fn installed_directory() -> anyhow::Result<Option<PathBuf>> {
    let location = registry::text(REGISTRATION, "InstallLocation").context(
        "Can't read the Windows installation. Open the release page to update manually.",
    )?;
    Ok(location.map(PathBuf::from))
}
