use std::{
    cell::Cell,
    io::{Read as _, Seek as _},
    path::{Path, PathBuf},
    time::Instant,
};

use speakeasy_show::MoviePrefix;

use super::*;
use crate::movie::tests::check_colors;

fn fixtures() -> anyhow::Result<PathBuf> {
    std::env::var_os("SPEAKEASY_MOVIE_FIXTURES")
        .map(PathBuf::from)
        .context("Set SPEAKEASY_MOVIE_FIXTURES")
}

fn validated(path: &Path) -> anyhow::Result<(File, MoviePrefix)> {
    let mut file = File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
    let prefix = crate::movie::tests::prefix(&mut file)?;
    Ok((file, prefix))
}

fn read(
    path: &Path,
    index: usize,
    cancelled: impl Fn() -> bool,
) -> anyhow::Result<Option<MovieFrame>> {
    let (file, prefix) = validated(path)?;
    let metadata = file.metadata()?;
    assert_eq!(prefix.bytes(), metadata.len());
    assert!(!prefix.truncated());
    let sample = prefix.samples()[index];
    crate::read_movie_frame(file, &prefix, sample.at, Duration::MAX, cancelled)
}

#[test]
#[ignore = "Opt in: native file-fed codec; run scripts/check-movie-macos.sh with FFmpeg installed"]
fn native_movie_reads_exact_samples_and_pixels() -> anyhow::Result<()> {
    let path = fixtures()?.join("moving.mp4");
    let (_, prefix) = validated(&path)?;
    assert_eq!(prefix.frames(), 120);
    assert_eq!(prefix.timescale(), 30_000);
    assert_eq!(prefix.samples()[1].ticks, 1001);
    for index in [119, 0, 1, 45, 2] {
        let frame = read(&path, index, || false)?.context("Synthetic frame was cancelled")?;
        assert_eq!(frame.at, prefix.samples()[index].at);
        check_colors(&frame, index);
    }
    Ok(())
}

#[test]
#[ignore = "Opt in: native file-fed codec; run scripts/check-movie-macos.sh with FFmpeg installed"]
fn native_movie_cancels_before_and_after_generation_starts() -> anyhow::Result<()> {
    let path = fixtures()?.join("moving.mp4");
    assert!(read(&path, 1, || true)?.is_none());
    #[expect(
        clippy::disallowed_types,
        reason = "The injected cancellation query counts owner polls to cancel only after native generation starts"
    )]
    let checks = Cell::new(0);
    assert!(
        read(&path, 119, || {
            checks.set(checks.get() + 1);
            checks.get() >= 3
        })?
        .is_none()
    );
    assert!(checks.get() >= 3);
    Ok(())
}

#[test]
#[ignore = "Opt in: native file-fed codec; run scripts/check-movie-macos.sh with FFmpeg installed"]
fn native_movie_seeks_near_the_end_of_a_five_minute_recording() -> anyhow::Result<()> {
    let long = fixtures()?.join("long.mp4");
    let (_, prefix) = validated(&long)?;
    assert!(prefix.duration() >= Duration::from_secs(299));
    let index = prefix
        .samples()
        .partition_point(|sample| sample.at <= Duration::from_secs(290))
        .checked_sub(1)
        .unwrap();
    let began = Instant::now();
    let frame = read(&long, index, || false)?.context("Synthetic near-end read cancelled")?;
    assert_eq!(frame.at, prefix.samples()[index].at);
    println!("Native 290-second seek completed in {:?}", began.elapsed());
    Ok(())
}

#[test]
#[ignore = "Opt in: read the synthetic Windows encoder artifacts on macOS"]
fn native_windows_movie_preserves_colors_on_macos() -> anyhow::Result<()> {
    let directory = std::env::var_os("SPEAKEASY_WINDOWS_MOVIE_DIRECTORY")
        .map(PathBuf::from)
        .context("Set SPEAKEASY_WINDOWS_MOVIE_DIRECTORY to the synthetic encoder artifacts")?;
    for (name, size, indices) in [
        ("windows.mp4", [320, 240], &[119, 0, 1, 45, 2][..]),
        ("windows-hd.mp4", [1280, 720], &[1, 0]),
    ] {
        check_retained_colors(&directory.join(name), size, indices)?;
    }
    Ok(())
}

fn check_retained_colors(path: &Path, size: [u32; 2], indices: &[usize]) -> anyhow::Result<()> {
    let (mut input, prefix) = validated(path)?;
    assert!(!prefix.truncated());
    assert_eq!(prefix.size(), size);
    input.rewind()?;
    let mut bytes = Vec::new();
    input.take(prefix.bytes()).read_to_end(&mut bytes)?;
    assert_eq!(u64::try_from(bytes.len())?, prefix.bytes());
    prefix.normalization().apply(0, &mut bytes)?;
    let private = tempfile::tempdir()?;
    let retained = private.path().join("retained.mp4");
    std::fs::write(&retained, bytes)?;
    for &index in indices {
        let frame =
            read(&retained, index, || false)?.context("Synthetic Windows frame cancelled")?;
        check_colors(&frame, index);
    }
    Ok(())
}
