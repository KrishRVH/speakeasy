use std::{
    fs::File,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::Context as _;
use windows::{
    Win32::Media::MediaFoundation::{
        MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM,
        MFCreateSourceReaderFromURL,
    },
    core::HSTRING,
};

use super::{encoder, foundation::Foundation, read};
use crate::{
    MovieFrame, Part,
    movie::{
        canvas,
        encode::{Encoder as Timeline, End},
        recording::Movie,
        tests::frame,
    },
};

mod colors;
mod geometry;
mod timing;

type Encoder = Timeline<encoder::Codec>;
const FILE_LIMIT: u64 = 384 << 20;

#[test]
#[ignore = "Opt in: native Media Foundation encoder, synthetic pixels only; set SPEAKEASY_MOVIE_PROBE_DIRECTORY to an empty artifact directory"]
fn encode_synthetic_movie_fragments() -> anyhow::Result<()> {
    let directory = probe_directory()?;
    anyhow::ensure!(directory.is_dir(), "Movie probe directory must exist");
    let mut moving = Encoder::open(
        &directory.join("moving.mp4"),
        canvas::Canvas::new([1280, 720])?,
        FILE_LIMIT,
    )?;
    let mut image = frame(1280, 720, 240);
    let start = Instant::now();
    for number in 0..120_u32 {
        for (index, pixel) in image.bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x = u32::try_from(index % 1280)?;
            let y = u32::try_from(index / 1280)?;
            let ink = x.wrapping_add(number.wrapping_mul(11)) % 127 < 3
                || y.wrapping_add(number.wrapping_mul(7)) % 41 < 2;
            *pixel = if ink {
                [30, 30, 30, 255]
            } else {
                [240, 245, 250, 255]
            };
        }
        assert!(
            moving
                .write(&image, wait_for_sample(start, number), &mut || false)?
                .is_continue()
        );
    }
    assert!(
        moving
            .advance(End::Before(Duration::from_secs(4)), &mut || false)?
            .is_continue()
    );
    moving.finish()?;
    let times = sample_times(&directory.join("moving.mp4"))?;
    assert_eq!(times.len(), 120);
    assert_eq!(times[0].0, 0);
    let &(last, duration) = times.last().unwrap();
    assert!((last + duration - 40_000_000).abs() <= 1_000);

    let mut held = Encoder::open(
        &directory.join("held.mp4"),
        canvas::Canvas::new([1280, 720])?,
        FILE_LIMIT,
    )?;
    let start = Instant::now();
    for number in 0..180 {
        let at = wait_for_sample(start, number);
        assert!(
            match number {
                0 => held.write(&frame(1280, 720, 20), at, &mut || false),
                45 => held.write(&frame(1280, 720, 220), at, &mut || false),
                _ => held.advance(End::Through(at), &mut || false),
            }?
            .is_continue()
        );
    }
    held.finish()?;
    let times = sample_times(&directory.join("held.mp4"))?;
    let &(last, duration) = times.last().unwrap();
    assert!(
        (last + duration - 60_000_000).abs() <= 1_000,
        "A quiet screen truncated the movie before recording ended"
    );

    let path = directory.join("capped.mp4");
    let limit = 1 << 20;
    let mut capped = Encoder::open(&path, canvas::Canvas::new([1280, 720])?, limit)?;
    let mut failed = false;
    let start = Instant::now();
    for number in 0..900_u32 {
        for (index, pixel) in image.bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let shade = u8::try_from(
                (index.wrapping_add(usize::try_from(number)?.wrapping_mul(997))) % 251,
            )?;
            *pixel = [shade, shade, shade, 255];
        }
        if capped
            .write(&image, wait_for_sample(start, number), &mut || false)
            .is_err()
        {
            failed = true;
            break;
        }
    }
    let finalized = capped.finish();
    assert!(
        failed || finalized.is_err(),
        "Native encoder did not exercise its byte cap"
    );
    assert!(std::fs::metadata(&path)?.len() <= limit);

    std::fs::rename(&path, directory.join("capped-closed.mp4"))?;

    let too_small = directory.join("no-header.mp4");
    if let Ok(mut encoder) = Encoder::open(&too_small, canvas::Canvas::new([320, 240])?, 1) {
        let submitted = encoder.write(&frame(320, 240, 50), Duration::ZERO, &mut || false);
        let finished = encoder.finish();
        assert!(submitted.is_err() || finished.is_err());
    }
    assert!(std::fs::metadata(&too_small)?.len() <= 1);
    std::fs::remove_file(too_small)?;
    Ok(())
}

fn probe_directory() -> anyhow::Result<PathBuf> {
    std::env::var_os("SPEAKEASY_MOVIE_PROBE_DIRECTORY")
        .map(PathBuf::from)
        .context("Choose a synthetic movie probe directory")
}

fn decode(path: &Path, size: [u32; 2], at: Duration) -> anyhow::Result<MovieFrame> {
    let file = File::open(path)?;
    let bytes = file.metadata()?.len();
    read::frame(file, bytes, size, at, &|| false)?.context("Synthetic read was cancelled")
}

fn sample_times(path: &Path) -> anyhow::Result<Vec<(i64, i64)>> {
    let _foundation = Foundation::start()?;

    unsafe {
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None)?;
        let mut times = Vec::new();
        for _ in 0..=9_002 {
            let mut flags = 0;
            let mut sample = None;
            reader.ReadSample(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0.cast_unsigned(),
                0,
                None,
                Some(&raw mut flags),
                None,
                Some(&raw mut sample),
            )?;
            if let Some(sample) = sample {
                times.push((sample.GetSampleTime()?, sample.GetSampleDuration()?));
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 {
                return Ok(times);
            }
        }
    }
    anyhow::bail!("Synthetic movie exceeds its read bound")
}

fn wait_for_sample(start: Instant, number: u32) -> Duration {
    let at = Duration::from_nanos(u64::from(number).saturating_mul(1_000_000_000) / 30);

    std::thread::sleep(at.saturating_sub(start.elapsed()));
    at
}
