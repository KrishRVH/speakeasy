//! Where Speakeasy keeps its files: roaming settings and the machine-local data root, both inside
//! the user's own folders, and what to do when a file operation there fails.

use std::{
    ffi::OsString,
    io::{self, ErrorKind},
    path::PathBuf,
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

/// What to do when a file operation in one of these folders fails: free space on a full disk;
/// otherwise close what holds the files and check the folder's permissions.
pub(crate) fn remedy(error: &io::Error) -> &'static str {
    match error.kind() {
        ErrorKind::StorageFull | ErrorKind::QuotaExceeded => "Free up disk space, then try again.",
        _ => "Close programs using its files and check its permissions, then try again.",
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
        for vars in [missing, empty, relative] {
            for (folder, required) in [
                (
                    UserFolder::Settings,
                    if cfg!(windows) { "APPDATA" } else { "HOME" },
                ),
                (
                    UserFolder::Data,
                    if cfg!(windows) {
                        "LOCALAPPDATA"
                    } else {
                        "HOME"
                    },
                ),
            ] {
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
