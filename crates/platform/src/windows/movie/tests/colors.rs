use super::*;
use crate::movie::tests::{check_colors, color_frame};

#[test]
#[ignore = "Opt in: native synthetic encoder artifact for cross-platform color checks"]
fn native_movie_color_interoperability() -> anyhow::Result<()> {
    let directory = probe_directory()?;
    for (name, size, frames, checked) in [
        ("windows.mp4", [320, 240], 120, &[119, 0, 1, 45, 2][..]),
        ("windows-hd.mp4", [1280, 720], 2, &[1, 0]),
    ] {
        let path = directory.join(name);
        let mut encoder = Encoder::open(&path, canvas::Canvas::new(size)?, FILE_LIMIT)?;
        for index in 0..frames {
            assert!(
                encoder
                    .write(&color_frame(index, size), sample_at(index), &mut || false)?
                    .is_continue()
            );
        }
        encoder.finish()?;
        for &index in checked {
            let frame = decode(&path, size, sample_at(index))?;
            assert_eq!(frame.at, sample_at(index));
            check_colors(&frame, index);
        }
    }
    Ok(())
}

#[test]
#[ignore = "Opt in: decode synthetic Mac encoder artifacts on Windows"]
fn native_mac_movies_preserve_colors_and_timestamps() -> anyhow::Result<()> {
    let directory = PathBuf::from(
        std::env::var_os("SPEAKEASY_MAC_MOVIE_DIRECTORY")
            .context("Choose the synthetic Mac movie directory")?,
    );
    for (name, count, size) in [
        ("moving", 120, [320, 240]),
        ("single", 1, [320, 240]),
        ("hd", 2, [1280, 720]),
    ] {
        for index in [count - 1, 0, count / 2] {
            let frame = decode(
                &directory.join(format!("{name}.mp4")),
                size,
                sample_at(index),
            )?;
            assert_eq!(frame.at, sample_at(index));
            check_colors(&frame, index);
        }
    }
    Ok(())
}

fn sample_at(index: usize) -> Duration {
    let ticks = u64::try_from(index).unwrap().saturating_mul(10_000_000) / 30;
    Duration::from_nanos(ticks.saturating_mul(100))
}
