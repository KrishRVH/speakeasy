use std::{
    cell::Cell,
    io::{Read as _, Seek as _},
    mem::MaybeUninit,
    os::{fd::AsRawFd as _, unix::fs::MetadataExt as _},
    path::Path,
    time::Instant,
};

use speakeasy_show::MoviePrefix;

use super::*;
use crate::movie::tests::check_colors;

fn validated(path: &Path) -> anyhow::Result<(File, MoviePrefix)> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let prefix = MoviePrefix::read::<anyhow::Error>(length, |range| {
        file.seek(std::io::SeekFrom::Start(range.start))?;
        let mut bytes = vec![0; usize::try_from(range.end.checked_sub(range.start).unwrap())?];
        file.read_exact(&mut bytes)?;
        Ok(bytes)
    })?;
    Ok((file, prefix))
}

fn read(
    path: &Path,
    index: usize,
    cancelled: impl Fn() -> bool,
) -> anyhow::Result<Option<MovieFrame>> {
    let (file, prefix) = validated(path)?;
    let metadata = file.metadata()?;
    assert_eq!(prefix.bytes, metadata.len());
    assert!(!prefix.truncated);
    let sample = prefix.samples()[index];
    let descriptor = file.as_raw_fd();
    let frame = crate::read_movie_frame(file, &prefix, sample.at, cancelled)?;

    let mut current = MaybeUninit::<libc::stat>::uninit();
    let status = unsafe { libc::fstat(descriptor, current.as_mut_ptr()) };
    if status == 0 {
        let current = unsafe { current.assume_init() };
        assert!(
            u64::try_from(current.st_dev).ok() != Some(metadata.dev())
                || current.st_ino != metadata.ino(),
            "Movie retained its input"
        );
    }
    Ok(frame)
}

#[test]
#[ignore = "Opt in: native file-fed codec; run scripts/check-movie-macos.sh with FFmpeg installed"]
fn native_movie_preserves_exact_samples_pixels_and_input_ownership() -> anyhow::Result<()> {
    let directory =
        std::env::var_os("SPEAKEASY_MOVIE_FIXTURES").context("Set SPEAKEASY_MOVIE_FIXTURES")?;
    let directory = Path::new(&directory);
    let path = directory.join("moving.mp4");
    let (_, prefix) = validated(&path)?;
    assert_eq!(prefix.frames, 120);
    assert_eq!(prefix.timescale, 30_000);
    assert_eq!(prefix.samples()[1].ticks, 1001);
    for index in [119, 0, 1, 45, 2] {
        let frame = read(&path, index, || false)?.context("Synthetic frame was cancelled")?;
        assert_eq!(frame.at, prefix.samples()[index].at);
        check_colors(&frame, index);
    }
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

    let private = tempfile::tempdir()?;
    let original = private.path().join("movie.mp4");
    std::fs::copy(&path, &original)?;
    let (file, prefix) = validated(&original)?;
    std::fs::rename(&original, private.path().join("retained.mp4"))?;
    std::fs::copy(directory.join("other.mp4"), &original)?;
    let sample = prefix.samples()[1];
    let frame = crate::read_movie_frame(file, &prefix, sample.at, || false)?
        .context("Synthetic replaced-path read cancelled")?;
    check_colors(&frame, 1);

    let mut truncated = std::fs::read(&path)?;
    truncated.truncate(truncated.len().checked_sub(8).unwrap());
    let recovered = private.path().join("recovered.mp4");
    std::fs::write(&recovered, &truncated)?;
    let (_, prefix) = validated(&recovered)?;
    assert!(prefix.truncated);
    assert!(prefix.frames < 120);
    std::fs::write(&recovered, &truncated[..usize::try_from(prefix.bytes)?])?;
    let index = prefix.samples().len().checked_sub(1).unwrap();
    let frame = read(&recovered, index, || false)?.context("Recovered frame cancelled")?;
    assert_eq!(frame.at, prefix.samples()[index].at);
    check_colors(&frame, index);

    let long = directory.join("long.mp4");
    let (_, prefix) = validated(&long)?;
    assert!(prefix.duration >= Duration::from_secs(299));
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
#[ignore = "Opt in: read the synthetic Windows encoder artifact on macOS"]
fn native_windows_movie_preserves_colors_on_macos() -> anyhow::Result<()> {
    let path = std::env::var_os("SPEAKEASY_WINDOWS_MOVIE")
        .context("Set SPEAKEASY_WINDOWS_MOVIE to the synthetic encoder artifact")?;
    let path = Path::new(&path);
    retained_colors(path, [320, 240], &[119, 0, 1, 45, 2])?;
    retained_colors(&path.with_file_name("windows-hd.mp4"), [1280, 720], &[1, 0])
}

fn retained_colors(path: &Path, size: [u32; 2], indices: &[usize]) -> anyhow::Result<()> {
    let (mut input, prefix) = validated(path)?;
    assert!(!prefix.truncated);
    assert_eq!(prefix.size, size);
    input.rewind()?;
    let private = tempfile::tempdir()?;
    let path = private.path().join("retained.mp4");
    let mut retained = File::create_new(&path)?;
    assert_eq!(
        std::io::copy(&mut input.take(prefix.bytes), &mut retained)?,
        prefix.bytes
    );
    drop(retained);
    for normalized in [false, true] {
        if normalized {
            let mut bytes = std::fs::read(&path)?;
            prefix.compatibility().apply(0, &mut bytes)?;
            std::fs::write(&path, bytes)?;
        }
        for &index in indices {
            let frame =
                read(&path, index, || false)?.context("Synthetic Windows frame cancelled")?;
            check_colors(&frame, index);
        }
    }
    Ok(())
}
