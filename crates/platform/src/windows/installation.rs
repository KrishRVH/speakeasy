use std::{ffi::OsString, io, path::PathBuf};

use anyhow::Context as _;

use super::registry;

const REGISTRATION: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\io.github.krvh.speakeasy_is1";

const UNREADABLE: &str =
    "Cannot read the Windows installation. Open the release page to update manually.";

pub fn installed_directory() -> anyhow::Result<Option<PathBuf>> {
    registered(registry::text(REGISTRATION, "InstallLocation"))
}

fn registered(location: io::Result<Option<OsString>>) -> anyhow::Result<Option<PathBuf>> {
    Ok(location.context(UNREADABLE)?.map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registered_folder_is_the_installation() -> anyhow::Result<()> {
        let folder = r"C:\Program Files\Speakeasy";
        assert_eq!(
            registered(Ok(Some(folder.into())))?,
            Some(PathBuf::from(folder))
        );
        assert_eq!(registered(Ok(None))?, None, "No registration is portable");
        Ok(())
    }

    #[test]
    fn an_unreadable_registration_points_to_a_manual_update() {
        let error = registered(Err(io::ErrorKind::PermissionDenied.into()))
            .unwrap_err()
            .to_string();
        assert!(
            error.ends_with("Open the release page to update manually."),
            "{error}"
        );
    }
}
