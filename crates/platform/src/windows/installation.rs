use std::{ffi::OsString, io, path::PathBuf};

use anyhow::{Context as _, ensure};

use super::registry;

const REGISTRATION: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\io.github.krvh.speakeasy_is1";

const UNREADABLE: &str =
    "Cannot read the Windows installation. Open the release page to update manually.";

pub fn installed_directory() -> anyhow::Result<Option<PathBuf>> {
    registered(registry::text(REGISTRATION, "InstallLocation"))
}

fn registered(location: io::Result<Option<OsString>>) -> anyhow::Result<Option<PathBuf>> {
    let Some(path) = location.context(UNREADABLE)?.map(PathBuf::from) else {
        return Ok(None);
    };
    ensure!(path.is_absolute(), UNREADABLE);
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registered_absolute_folder_is_the_installation() -> anyhow::Result<()> {
        let folder = r"C:\Program Files\Speakeasy";
        assert_eq!(
            registered(Ok(Some(folder.into())))?,
            Some(PathBuf::from(folder))
        );
        assert_eq!(registered(Ok(None))?, None, "No registration is portable");
        Ok(())
    }

    #[test]
    fn an_unusable_registration_points_to_a_manual_update() {
        for location in [
            Err(io::ErrorKind::PermissionDenied.into()),
            Err(io::ErrorKind::InvalidData.into()),
            Ok(Some(r"\Program Files\Speakeasy".into())),
        ] {
            let error = registered(location).unwrap_err().to_string();
            assert!(
                error.ends_with("Open the release page to update manually."),
                "{error}"
            );
        }
    }
}
