use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use anyhow::{Context as _, ensure};

use super::{Payload, Version, manifest};

const MANUAL: &str = "This copy is portable or its Windows installation has moved. Open the release page to update it manually.";

pub(super) struct Target {
    directory: PathBuf,
    configuration: PathBuf,
}

impl Target {
    pub(super) fn current(configuration: PathBuf) -> anyhow::Result<Self> {
        let registered = speakeasy_platform::installed_directory()?.context(MANUAL)?;
        let directory = registered.canonicalize().context(MANUAL)?;
        let executable = std::env::current_exe()?.canonicalize().context(MANUAL)?;
        ensure!(executable.parent() == Some(directory.as_path()), MANUAL);
        ensure!(directory.join("unins000.exe").is_file(), MANUAL);
        ensure!(
            configuration.is_absolute(),
            "The settings path is not absolute. Reopen Speakeasy before updating."
        );
        Ok(Self {
            directory: registered,
            configuration,
        })
    }
}

pub(crate) struct Installer {
    payload: Payload,
    target: Target,
}

impl Installer {
    #[cfg(test)]
    pub(crate) fn fixture(
        root: &std::path::Path,
        configuration: PathBuf,
    ) -> anyhow::Result<(Self, PathBuf)> {
        let cache = root.join("updates");
        let lease = super::cache::acquire(root)?;
        let directory = tempfile::Builder::new()
            .prefix("release-")
            .tempdir_in(&cache)?;
        let path = directory.path().join(manifest::NAME);
        std::fs::write(&path, b"Synthetic authority receipt; never executed.")?;
        let file = super::open_verified(&path)?;
        Ok((
            Self::new(
                Payload {
                    file,
                    directory,
                    version: Version::parse("1.2.3").expect("fixture version"),
                    _lease: lease,
                },
                Target {
                    directory: root.join("app"),
                    configuration,
                },
            ),
            path,
        ))
    }

    pub(super) const fn new(payload: Payload, target: Target) -> Self {
        Self { payload, target }
    }

    pub(crate) const fn version(&self) -> Version {
        self.payload.version
    }

    pub(crate) fn launch(self) -> anyhow::Result<()> {
        let mut command = Command::new(self.payload.directory.path().join(manifest::NAME));
        command
            .arg("/SILENT")
            .arg("/NORESTART")
            .arg(format!("/SPEAKEASYPARENT={}", std::process::id()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut directory = std::ffi::OsString::from("/DIR=");
        directory.push(&self.target.directory);
        let mut configuration = std::ffi::OsString::from("/SPEAKEASYCONFIG=");
        configuration.push(&self.target.configuration);
        command.arg(directory).arg(configuration);
        let child = command
            .spawn()
            .context("Cannot start the verified installer. Reopen Speakeasy and try again.")?;
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The installer owns the staged bytes after handoff; a later update operation removes the cache"
        )]
        let _ = self.payload.directory.keep();
        drop(self.payload.file);
        drop(child);
        Ok(())
    }
}
