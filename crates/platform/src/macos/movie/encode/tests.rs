mod timing;

use std::fs::File;

use speakeasy_show::MoviePrefix;

use super::*;
use crate::{macos::movie::tests::assert_released, movie::encode::Codec as _};

#[test]
#[ignore = "Opt in: native file-fed encoder and decoder; synthetic pixels only"]
fn native_mac_movie_encoding() -> anyhow::Result<()> {
    let directory = std::path::PathBuf::from(
        std::env::var_os("SPEAKEASY_MAC_MOVIE_DIRECTORY")
            .context("Choose the synthetic Mac encoder artifact directory")?,
    );
    std::fs::create_dir_all(&directory)?;
    for (name, count, size, picture, pace) in [
        ("moving", 120, [320, 240], Picture::Moving, Pace::Burst),
        ("single", 1, [320, 240], Picture::Moving, Pace::Burst),
        ("hd", 2, [1280, 720], Picture::Moving, Pace::Burst),
        ("held", 180, [320, 240], Picture::Held, Pace::Burst),
        ("large", 90, [2560, 1440], Picture::Moving, Pace::RealTime),
    ] {
        let path = directory.join(format!("{name}.mp4"));
        let mut encoder = Codec::open(&path, size, 384 << 20)?;
        let (descriptor, opened) = encoder.output.file_identity();
        let patterns = [0, 1].map(|index| crate::movie::tests::color_frame(index, size));
        let started = Instant::now();
        for index in 0..count {
            if pace == Pace::RealTime {
                let due = Duration::from_nanos(u64::try_from(index)? * 1_000_000_000 / 30);
                std::thread::sleep(due.saturating_sub(started.elapsed()));
            }
            assert!(
                encoder
                    .write(
                        patterns[picture.pattern(index)].image.pixels(),
                        u32::try_from(index)?,
                        &mut || false
                    )?
                    .is_continue()
            );
        }
        encoder.finish()?;
        assert_released(descriptor, &opened);
        let prefix = inspect(&path)?;
        let length = std::fs::metadata(&path)?.len();
        assert!(!prefix.truncated());
        assert_eq!(prefix.frames(), u32::try_from(count)?);
        assert_eq!(prefix.bytes(), length);
        assert_eq!(
            prefix.duration(),
            Duration::from_nanos(u64::try_from(count)? * 1_000_000_000 / 30)
        );
        for index in [count - 1, 0] {
            let file = File::open(&path)?;
            let frame = crate::read_movie_frame(
                file,
                &prefix,
                prefix.samples()[index].at,
                Duration::MAX,
                || false,
            )?
            .context("Synthetic movie was cancelled")?;
            assert_eq!(frame.at, prefix.samples()[index].at);
            crate::movie::tests::check_colors(&frame, picture.pattern(index));
        }
    }
    let path = directory.join("cancelled.mp4");
    let mut encoder = Codec::open(&path, [320, 240], 384 << 20)?;
    let (descriptor, opened) = encoder.output.file_identity();
    let image = crate::movie::tests::color_frame(0, [320, 240]).image;
    for index in 0..90 {
        assert!(
            encoder
                .write(image.pixels(), index, &mut || false)?
                .is_continue()
        );
    }
    let waiting = Instant::now();
    loop {
        if inspect(&path).is_ok_and(|prefix| prefix.frames() > 0) {
            encoder.output.check()?;
            break;
        }
        ensure!(
            waiting.elapsed() < Duration::from_secs(5),
            "No committed media segment"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let submitted = encoder.frames;
    assert!(
        encoder
            .write(image.pixels(), submitted, &mut || true)?
            .is_break()
    );
    assert_eq!(encoder.frames, submitted);
    drop(encoder);
    assert_released(descriptor, &opened);
    let prefix = inspect(&path)?;
    assert!(prefix.frames() > 0 && prefix.frames() <= submitted);

    let path = directory.join("capped.mp4");
    let limit = std::fs::metadata(directory.join("moving.mp4"))?.len() / 2;
    let mut encoder = Codec::open(&path, [320, 240], limit)?;
    let (descriptor, opened) = encoder.output.file_identity();
    for index in 0..120 {
        if encoder
            .write(
                crate::movie::tests::color_frame(index, [320, 240])
                    .image
                    .pixels(),
                u32::try_from(index)?,
                &mut || false,
            )
            .is_err()
        {
            break;
        }
    }
    let finished = encoder.finish();
    assert!(
        finished.is_err(),
        "A failed segment must never finalize successfully"
    );
    assert_released(descriptor, &opened);
    assert!(std::fs::metadata(&path)?.len() <= limit);
    let prefix = inspect(&path)?;
    assert!(prefix.frames() > 0 && prefix.frames() < 120);
    timing::verify(&directory)?;
    Ok(())
}

#[derive(Clone, Copy)]
enum Picture {
    Moving,
    Held,
}

impl Picture {
    const fn pattern(self, frame: usize) -> usize {
        match self {
            Self::Moving => frame % 2,
            Self::Held => 0,
        }
    }
}

#[derive(PartialEq, Eq)]
enum Pace {
    Burst,
    RealTime,
}

fn inspect(path: &Path) -> anyhow::Result<MoviePrefix> {
    crate::movie::tests::prefix(&mut File::open(path)?)
}
