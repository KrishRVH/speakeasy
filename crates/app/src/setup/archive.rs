//! Engine archives: the system tar unpacks each build into a staging directory, which is lifted
//! into one layout and renamed over the installed build.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context as _, bail};
use reqwest::Client;
use tokio::sync::watch;

use super::{
    Installation, Progress,
    download::{Download, fetch, file_name},
    system32,
};
use crate::child::hidden_command;

#[cfg(target_os = "windows")]
pub(super) const EXECUTABLE: &str = r"bin\nemo-speech.exe";
#[cfg(not(target_os = "windows"))]
pub(super) const EXECUTABLE: &str = "bin/nemo-speech";

/// Downloads and extracts an engine archive into its own directory.
pub(super) async fn unpack(
    client: &Client,
    archive: &Download<'_>,
    root: &Path,
    progress: &watch::Sender<Progress>,
    installation: &mut Installation,
) -> anyhow::Result<PathBuf> {
    let name = file_name(archive.url);
    let stem = name
        .strip_suffix(".zip")
        .or_else(|| name.strip_suffix(".tar.gz"))
        .unwrap_or(name);
    let engines = root.join("engines");
    let directory = engines.join(stem);
    if directory.join(EXECUTABLE).is_file() {
        return Ok(directory);
    }
    let file = fetch(
        client,
        archive,
        &root.join("downloads"),
        "Downloading the speech engine",
        progress,
    )
    .await?;
    progress.send_replace(Progress {
        step: Some("Unpacking the speech engine"),
        ..Progress::default()
    });
    let staging = engines.join(format!("{stem}.part"));
    remove_engine_directory(&staging)?;
    fs::create_dir_all(&staging)?;
    // The system tar reads this platform's archives; a tar earlier in PATH may not.
    let tar = if cfg!(target_os = "windows") {
        system32().join("tar.exe")
    } else {
        PathBuf::from("/usr/bin/tar")
    };
    let mut command = hidden_command(tar);
    command
        .arg("-xf")
        .arg(&file)
        .arg("-C")
        .arg(&staging)
        .stdout(Stdio::null());
    let (status, _) = installation.output(&mut command).await?;
    if !status.success() {
        bail!("Cannot unpack the speech engine. Check free disk space, then try again.");
    }
    normalize_archive(&staging)?;
    remove_engine_directory(&directory)?;
    fs::rename(&staging, &directory)?;
    #[expect(
        clippy::let_underscore_must_use,
        reason = "The archive is only a cache after the verified engine is installed; pruning it is optional"
    )]
    let _ = fs::remove_file(&file);
    Ok(directory)
}

fn remove_engine_directory(path: &Path) -> anyhow::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "Cannot replace {}. Close programs using the speech engine and check directory permissions, then try again.",
                path.display()
            )
        }),
    }
}

/// Lifts the engine out of the one directory an archive may wrap it in, so every build lays out
/// alike.
fn normalize_archive(staging: &Path) -> anyhow::Result<()> {
    if staging.join(EXECUTABLE).is_file() {
        return Ok(());
    }
    let mut directories = Vec::new();
    for entry in fs::read_dir(staging)? {
        let entry = entry?;
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            directories.push(entry.path());
        }
    }
    let directory = match directories.as_slice() {
        [directory] if directory.join(EXECUTABLE).is_file() => directory,
        _ => bail!("The engine archive has an unexpected layout."),
    };
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        fs::rename(entry.path(), staging.join(entry.file_name()))?;
    }
    fs::remove_dir(directory)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archives_preserve_the_complete_runtime_layout() -> anyhow::Result<()> {
        for wrapper in ["", "nemo-speech-0.2.0-windows-x86_64-cpu"] {
            let directory = tempfile::tempdir()?;
            let runtime = directory.path().join(wrapper);
            fs::create_dir_all(runtime.join("bin"))?;
            fs::create_dir_all(runtime.join("lib"))?;
            fs::write(runtime.join(EXECUTABLE), b"engine fixture")?;
            fs::write(runtime.join("lib/runtime.so"), b"dependency fixture")?;
            normalize_archive(directory.path())?;
            assert_eq!(
                fs::read(directory.path().join(EXECUTABLE))?,
                b"engine fixture"
            );
            assert_eq!(
                fs::read(directory.path().join("lib/runtime.so"))?,
                b"dependency fixture"
            );
            if !wrapper.is_empty() {
                assert!(!runtime.exists());
            }
        }
        Ok(())
    }

    #[test]
    fn archives_reject_an_ambiguous_runtime_layout() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        fs::create_dir_all(directory.path().join("first/bin"))?;
        fs::create_dir_all(directory.path().join("second/bin"))?;
        for wrapper in ["first", "second"] {
            fs::write(directory.path().join(wrapper).join(EXECUTABLE), b"fixture")?;
        }
        let failure = normalize_archive(directory.path())
            .err()
            .context("An ambiguous engine archive was accepted")?;
        assert!(failure.to_string().contains("unexpected layout"));
        assert!(!directory.path().join(EXECUTABLE).exists());
        Ok(())
    }

    #[tokio::test]
    async fn unpack_reports_an_unreplaceable_staging_directory() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        fs::create_dir_all(root.join("downloads"))?;
        fs::create_dir_all(root.join("engines"))?;
        fs::write(root.join("downloads/fixture.tar.gz"), b"unused archive")?;
        fs::write(root.join("engines/fixture.part"), b"not a directory")?;
        let (progress, _) = watch::channel(Progress::default());
        let mut installation = Installation {
            child: None,
            _lock: Installation::lock(root, &progress).await?,
        };
        let archive = Download {
            url: "http://127.0.0.1:0/fixture.tar.gz",
            size: 0,
            sha256: "unused: replacement must fail before extraction",
        };
        let failure = unpack(&Client::new(), &archive, root, &progress, &mut installation)
            .await
            .unwrap_err();
        let message = failure.to_string();
        assert!(message.contains("Cannot replace"));
        assert!(message.contains("check directory permissions"));
        assert_eq!(
            fs::read(root.join("engines/fixture.part"))?,
            b"not a directory"
        );
        assert!(installation.child.is_none());
        Ok(())
    }

    /// An uncompressed ustar archive holding one file, a format every system tar reads.
    fn ustar(name: &str, contents: &[u8]) -> Vec<u8> {
        let size = format!("{:011o}\0", contents.len());
        let fields: [(usize, &[u8]); 10] = [
            (0, name.as_bytes()),
            (100, b"0000755\0"),
            (108, b"0000000\0"),
            (116, b"0000000\0"),
            (124, size.as_bytes()),
            (136, b"00000000000\0"),
            // The checksum counts its own field as spaces.
            (148, b"        "),
            (156, b"0"),
            (257, b"ustar\0"),
            (263, b"00"),
        ];
        let mut header = [0; 512];
        for (offset, field) in fields {
            header[offset..][..field.len()].copy_from_slice(field);
        }
        let checksum: u32 = header.iter().copied().map(u32::from).sum();
        header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        let mut archive = header.to_vec();
        archive.extend_from_slice(contents);
        // Pad the file to whole blocks, then end the archive with two empty blocks.
        archive.resize(archive.len().next_multiple_of(512), 0);
        archive.extend_from_slice(&[0; 1024]);
        archive
    }

    #[tokio::test]
    async fn unpack_extracts_an_engine_with_the_system_tar() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        fs::create_dir_all(root.join("downloads"))?;
        let entry = format!("nemo-speech-fixture/{}", EXECUTABLE.replace('\\', "/"));
        fs::write(
            root.join("downloads/fixture.tar"),
            ustar(&entry, b"engine fixture"),
        )?;
        let (progress, _) = watch::channel(Progress::default());
        let mut installation = Installation {
            child: None,
            _lock: Installation::lock(root, &progress).await?,
        };
        let archive = Download {
            url: "http://127.0.0.1:0/fixture.tar",
            size: 0,
            sha256: "unused: the archive is already downloaded",
        };
        let engine = unpack(&Client::new(), &archive, root, &progress, &mut installation).await?;
        assert_eq!(engine, root.join("engines").join("fixture.tar"));
        assert_eq!(fs::read(engine.join(EXECUTABLE))?, b"engine fixture");
        assert!(!root.join("engines/fixture.tar.part").exists());
        assert!(!root.join("downloads/fixture.tar").exists());
        Ok(())
    }
}
