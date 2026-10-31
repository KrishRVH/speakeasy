//! Where Speakeasy keeps its files: roaming settings and the machine-local data root, both inside
//! the user's own folders, and what to do when a file operation fails.

use std::{
    ffi::OsString,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

use anyhow::Context as _;

/// The settings file, which roams with the user's profile on Windows.
pub(crate) fn settings_file() -> anyhow::Result<PathBuf> {
    Ok(user_folder(UserFolder::Settings, std::env::var_os)?.join("settings.json"))
}

/// Engines, models, and Shows are machine-local, so they stay out of roaming settings.
pub(crate) fn data_root() -> anyhow::Result<PathBuf> {
    user_folder(UserFolder::Data, std::env::var_os)
}

/// A durable local entry for headless commands, if available. Resolve off the UI thread: checking
/// the installed file can block. Temporary application mounts never go into saved handoffs.
pub(crate) fn command() -> Option<PathBuf> {
    command_at(
        std::env::current_exe().ok()?,
        std::env::var_os("APPIMAGE").map(PathBuf::from),
        std::env::var_os("APPDIR").map(PathBuf::from),
        Path::is_file,
    )
}

fn command_at(
    exe: PathBuf,
    appimage: Option<PathBuf>,
    appdir: Option<PathBuf>,
    is_file: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let path = if cfg!(target_os = "windows") {
        exe.with_file_name("speakeasy-cli.exe")
    } else if cfg!(target_os = "linux") {
        // AppImage variables are inherited by unrelated programs started beneath it. Only name
        // that image when this executable actually belongs to its mounted or extracted directory.
        if appdir.is_some_and(|dir| dir.is_absolute() && exe.starts_with(dir)) {
            // An extracted AppImage also disappears after exit; a missing original has no
            // durable fallback to its internal executable.
            appimage?
        } else {
            exe
        }
    } else {
        exe
    };
    // Missing or mismatched AppImage environment can still leave a temporary executable, as can
    // macOS App Translocation. Neither belongs in a saved handoff.
    let temporary = path.components().any(|part| {
        part.as_os_str().to_str().is_some_and(|part| {
            part == "AppTranslocation"
                || part.starts_with(".mount_")
                || part.starts_with("appimage_extracted_")
        })
    });
    (path.is_absolute() && path.to_str().is_some() && !temporary && is_file(&path)).then_some(path)
}

/// What to do when a file operation fails, for a message that names its path: free space on a
/// full disk; otherwise close what holds the files and check that the user can write to that path.
pub(crate) fn remedy(error: &io::Error) -> &'static str {
    match error.kind() {
        ErrorKind::StorageFull | ErrorKind::QuotaExceeded => "Free up disk space, then try again.",
        _ => {
            "Close programs using Speakeasy's files and check that you can write there, then try again."
        },
    }
}

#[derive(Clone, Copy)]
enum UserFolder {
    Settings,
    Data,
}

/// Speakeasy's folder of this kind under the user's base folder, which `var` looks up. A base that
/// is unset or relative is refused rather than resolved against the working directory, where other
/// users may read Shows and settings.
fn user_folder(
    folder: UserFolder,
    var: impl Fn(&'static str) -> Option<OsString>,
) -> anyhow::Result<PathBuf> {
    let absolute = |name: &'static str| {
        var(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    #[cfg(target_os = "windows")]
    let (base, required) = {
        let name = match folder {
            UserFolder::Settings => "APPDATA",
            UserFolder::Data => "LOCALAPPDATA",
        };
        (absolute(name), name)
    };
    #[cfg(target_os = "macos")]
    let (base, required) = (
        absolute("HOME").map(|home| home.join("Library/Application Support")),
        "HOME",
    );
    #[cfg(target_os = "linux")]
    let (base, required) = {
        let (xdg, fallback) = match folder {
            UserFolder::Settings => ("XDG_CONFIG_HOME", ".config"),
            UserFolder::Data => ("XDG_DATA_HOME", ".local/share"),
        };
        (
            absolute(xdg).or_else(|| absolute("HOME").map(|home| home.join(fallback))),
            "HOME",
        )
    };
    let kind = match folder {
        UserFolder::Settings => "settings",
        UserFolder::Data => "data",
    };
    let base = base.with_context(|| {
        format!(
            "Cannot find your {kind} folder because {required} is unset or not an absolute path"
        )
    })?;
    Ok(base.join("speakeasy"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoffs_require_a_present_durable_command() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("speakeasy");
        let expected = if cfg!(target_os = "windows") {
            exe.with_file_name("speakeasy-cli.exe")
        } else {
            exe.clone()
        };
        assert_eq!(command_at(exe.clone(), None, None, |_| false), None);
        assert_eq!(
            command_at(exe, None, None, |path| path == expected),
            Some(expected)
        );
        for folder in [
            "AppTranslocation",
            ".mount_Speakeasy",
            "appimage_extracted_Speakeasy",
        ] {
            assert_eq!(
                command_at(
                    root.path().join(folder).join("speakeasy"),
                    None,
                    None,
                    |_| true
                ),
                None
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn appimages_name_the_original_file_instead_of_the_temporary_mount() {
        let exe = PathBuf::from("/tmp/.mount_Speakeasy/usr/bin/speakeasy");
        let image = PathBuf::from("/home/user/Apps/Speakeasy.AppImage");
        assert_eq!(
            command_at(
                exe.clone(),
                Some(image.clone()),
                Some("/tmp/.mount_Speakeasy".into()),
                |_| true
            ),
            Some(image)
        );
        assert_eq!(
            command_at(
                exe.clone(),
                Some("relative.AppImage".into()),
                Some("/tmp/.mount_Speakeasy".into()),
                |_| true
            ),
            None
        );
        assert_eq!(
            command_at(
                exe,
                Some("/missing.AppImage".into()),
                Some("/tmp/.mount_Speakeasy".into()),
                |path| path.starts_with("/tmp")
            ),
            None
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inherited_appimage_variables_cannot_route_a_handoff_to_another_program() {
        let exe = PathBuf::from("/opt/speakeasy/usr/bin/speakeasy");
        for appdir in [
            None,
            Some("relative".into()),
            Some("/tmp/.mount_Other".into()),
        ] {
            assert_eq!(
                command_at(
                    exe.clone(),
                    Some("/home/user/Other.AppImage".into()),
                    appdir,
                    |_| true
                ),
                Some(exe.clone())
            );
        }
        let extracted = PathBuf::from("/tmp/appimage_extracted_X/usr/bin/speakeasy");
        let appdir = Some(PathBuf::from("/tmp/appimage_extracted_X"));
        assert_eq!(
            command_at(extracted.clone(), None, appdir.clone(), |_| true),
            None
        );
        let image = PathBuf::from("/home/user/Speakeasy.AppImage");
        assert_eq!(
            command_at(extracted, Some(image.clone()), appdir, |_| true),
            Some(image)
        );
    }

    fn lookup(vars: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        let vars = vars.to_vec();
        move |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.into())
        }
    }

    #[test]
    fn user_folders_never_fall_back_to_the_working_directory() -> anyhow::Result<()> {
        let names = ["HOME", "APPDATA", "LOCALAPPDATA"];
        let missing: Vec<_> = Vec::new();
        let empty: Vec<_> = names.iter().map(|name| (*name, "")).collect();
        let relative: Vec<_> = names.iter().map(|name| (*name, "home")).collect();
        let (settings, data) = if cfg!(windows) {
            ("APPDATA", "LOCALAPPDATA")
        } else {
            ("HOME", "HOME")
        };
        for vars in [missing, empty, relative] {
            for (folder, required) in [(UserFolder::Settings, settings), (UserFolder::Data, data)] {
                let error = user_folder(folder, lookup(&vars))
                    .err()
                    .context("A user folder resolved without an absolute base")?;
                assert!(error.to_string().contains(required));
            }
        }
        #[cfg(target_os = "linux")]
        {
            let home = ("HOME", "/home/user");
            assert_eq!(
                user_folder(
                    UserFolder::Settings,
                    lookup(&[("XDG_CONFIG_HOME", "/xdg"), home])
                )?,
                PathBuf::from("/xdg/speakeasy")
            );
            assert_eq!(
                user_folder(UserFolder::Data, lookup(&[("XDG_DATA_HOME", "data"), home]))?,
                PathBuf::from("/home/user/.local/share/speakeasy")
            );
        }
        #[cfg(target_os = "windows")]
        assert_eq!(
            user_folder(
                UserFolder::Data,
                lookup(&[("LOCALAPPDATA", r"C:\Users\user\AppData\Local")])
            )?,
            PathBuf::from(r"C:\Users\user\AppData\Local\speakeasy")
        );
        Ok(())
    }
}
