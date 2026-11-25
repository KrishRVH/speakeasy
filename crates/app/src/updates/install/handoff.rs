use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use anyhow::{Context as _, ensure};
use tempfile::TempDir;

use super::{Version, manifest};

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
        Ok(Self {
            directory: registered,
            configuration,
        })
    }
}

pub(crate) struct Installer {
    staged: TempDir,
    version: Version,
    target: Target,
}

impl Installer {
    #[cfg(test)]
    pub(crate) fn fixture(
        root: &std::path::Path,
        configuration: PathBuf,
    ) -> anyhow::Result<(Self, PathBuf)> {
        let cache = root.join("updates");
        std::fs::create_dir_all(&cache)?;
        let staged = tempfile::Builder::new()
            .prefix("release-")
            .tempdir_in(&cache)?;
        let path = staged.path().join(manifest::NAME);
        std::fs::write(&path, b"Synthetic installer; never executed.")?;
        let target = Target {
            directory: root.join("app"),
            configuration,
        };
        let version = Version::parse("1.2.3").expect("fixture version");
        Ok((Self::new(staged, version, target), path))
    }

    pub(super) const fn new(staged: TempDir, version: Version, target: Target) -> Self {
        Self {
            staged,
            version,
            target,
        }
    }

    pub(crate) const fn version(&self) -> Version {
        self.version
    }

    pub(crate) fn launch(self) -> anyhow::Result<()> {
        let mut command = Command::new(self.staged.path().join(manifest::NAME));
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
            .context("Can't start the verified installer. Reopen Speakeasy and try again.")?;
        // Agents' Show MCP servers map both images; ending them while the installer runs lets it
        // replace them. It checks its files only after this process exits.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Speakeasy is quitting for the installer, which reports a server still holding its files"
        )]
        let _ = speakeasy_platform::announce_update(&child);
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The installer owns the staged bytes after handoff; the next update clears the cache"
        )]
        let _ = self.staged.keep();
        drop(child);
        Ok(())
    }
}
