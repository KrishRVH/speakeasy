mod timing;

use std::{fs::File, mem::MaybeUninit, os::unix::fs::MetadataExt as _, time::Duration};

use speakeasy_show::MoviePrefix;

use super::*;
use crate::movie::encode::Codec as _;

#[test]
#[ignore = "Opt in: native file-fed encoder and decoder; synthetic pixels only"]
fn native_mac_movie_encoding() -> anyhow::Result<()> {
    let directory = std::path::PathBuf::from(
        std::env::var_os("SPEAKEASY_MAC_MOVIE_DIRECTORY")
            .context("Choose the synthetic Mac encoder artifact directory")?,
    );
    std::fs::create_dir_all(&directory)?;
    for (name, count, size) in [
        ("moving", 120, [320, 240]),
        ("single", 1, [320, 240]),
        ("hd", 2, [1280, 720]),
        ("held", 180, [320, 240]),
        ("large", 90, [2560, 1440]),
    ] {
        let path = directory.join(format!("{name}.mp4"));
        let mut encoder = Codec::open(&path, size, 384 << 20)?;
        let descriptor = encoder.output.file_identity();
        let patterns = [0, 1].map(|index| crate::movie::tests::color_frame(index, size));
        let started = Instant::now();
        for index in 0..count {
            if name == "large" {
                let due = Duration::from_nanos(u64::try_from(index)? * 1_000_000_000 / 30);
                std::thread::sleep(due.saturating_sub(started.elapsed()));
            }
            let painted = if name == "held" { 0 } else { index };
            assert!(encoder.write(
                &patterns[painted % 2].bgra,
                u32::try_from(index)?,
                &mut || false
            )?);
        }
        encoder.finish()?;
        assert_closed(descriptor);
        let prefix = inspect(&path)?;
        let length = std::fs::metadata(&path)?.len();
        assert!(!prefix.truncated);
        assert_eq!(prefix.frames, u32::try_from(count)?);
        assert_eq!(prefix.bytes, length);
        assert_eq!(
            prefix.duration,
            Duration::from_nanos(u64::try_from(count)? * 1_000_000_000 / 30)
        );
        for index in [count - 1, 0] {
            let file = File::open(&path)?;
            let frame =
                crate::read_movie_frame(file, &prefix, prefix.samples()[index].at, || false)?
                    .context("Synthetic movie was cancelled")?;
            assert_eq!(frame.at, prefix.samples()[index].at);
            crate::movie::tests::check_colors(&frame, if name == "held" { 0 } else { index });
        }
    }
    let path = directory.join("cancelled.mp4");
    let mut encoder = Codec::open(&path, [320, 240], 384 << 20)?;
    let descriptor = encoder.output.file_identity();
    let pixels = crate::movie::tests::color_frame(0, [320, 240]).bgra;
    for index in 0..90 {
        assert!(encoder.write(&pixels, index, &mut || false)?);
    }
    let waiting = Instant::now();
    loop {
        if inspect(&path).is_ok_and(|prefix| prefix.frames > 0) {
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
    assert!(!encoder.write(&pixels, submitted, &mut || true)?);
    assert_eq!(encoder.frames, submitted);
    drop(encoder);
    assert_closed(descriptor);
    let prefix = inspect(&path)?;
    assert!(prefix.frames > 0 && prefix.frames <= submitted);

    let path = directory.join("capped.mp4");
    let limit = std::fs::metadata(directory.join("moving.mp4"))?.len() / 2;
    let mut encoder = Codec::open(&path, [320, 240], limit)?;
    let descriptor = encoder.output.file_identity();
    for index in 0..120 {
        if encoder
            .write(
                &crate::movie::tests::color_frame(index, [320, 240]).bgra,
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
    assert_closed(descriptor);
    assert!(std::fs::metadata(&path)?.len() <= limit);
    let prefix = inspect(&path)?;
    assert!(prefix.frames > 0 && prefix.frames < 120);
    timing::verify(&directory)?;
    Ok(())
}

fn inspect(path: &Path) -> anyhow::Result<MoviePrefix> {
    crate::movie::tests::prefix(&mut File::open(path)?)
}

fn assert_closed((descriptor, previous): (std::os::fd::RawFd, std::fs::Metadata)) {
    let mut current = MaybeUninit::<libc::stat>::uninit();

    let status = unsafe { libc::fstat(descriptor, current.as_mut_ptr()) };
    if status == 0 {
        let current = unsafe { current.assume_init() };
        assert!(
            u64::try_from(current.st_dev).ok() != Some(previous.dev())
                || current.st_ino != previous.ino(),
            "Encoder retained its file"
        );
    } else {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EBADF)
        );
    }
}
