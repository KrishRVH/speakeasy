use std::{
    ffi::OsString,
    fs,
    io::{self, ErrorKind, Write as _},
    path::{Path, PathBuf},
};

use anyhow::Context as _;

pub(crate) fn settings_file(chosen: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let file = match chosen {
        Some(file) => file,
        None => user_folder(UserFolder::Settings)?.join("settings.json"),
    };
    std::path::absolute(file).context("Can't resolve the settings file from the current folder")
}

pub(crate) fn settings_argument(argument: Option<OsString>) -> anyhow::Result<PathBuf> {
    argument
        .map(PathBuf::from)
        .filter(|file| file.file_name().is_some())
        .context("--config needs the path of a settings file")
}

pub(crate) fn data_root() -> anyhow::Result<PathBuf> {
    user_folder(UserFolder::Data)
}

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
        if appdir.is_some_and(|dir| dir.is_absolute() && exe.starts_with(dir)) {
            appimage?
        } else {
            exe
        }
    } else {
        exe
    };

    let temporary = path.components().any(|part| {
        part.as_os_str().to_str().is_some_and(|part| {
            part == "AppTranslocation"
                || part.starts_with(".mount_")
                || part.starts_with("appimage_extracted_")
        })
    });
    (path.is_absolute() && path.to_str().is_some() && !temporary && is_file(&path)).then_some(path)
}

/// Renames a temporary file beside `target` over it, on the same filesystem, so a failure never
/// leaves half a file.
pub(crate) fn replace(target: &Path, contents: &[u8]) -> io::Result<()> {
    let directory = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(contents)?;
    file.persist(target)?;
    Ok(())
}

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

fn user_folder(folder: UserFolder) -> anyhow::Result<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
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
        format!("Can't find your {kind} folder because {required} is unset or not an absolute path")
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
                exe,
                Some(image.clone()),
                Some("/tmp/.mount_Speakeasy".into()),
                |_| true
            ),
            Some(image)
        );
    }
}
