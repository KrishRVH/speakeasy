use super::*;

#[test]
#[ignore = "Opt in: native synthetic encoder artifact for cross-platform color checks"]
fn native_movie_color_interoperability() -> anyhow::Result<()> {
    let directory = std::env::var_os("SPEAKEASY_MOVIE_PROBE_DIRECTORY")
        .ok_or_else(|| anyhow::anyhow!("Choose a synthetic probe directory"))?;
    let path = PathBuf::from(&directory).join("windows.mp4");
    let mut encoder = Encoder::open(&path, canvas::Canvas::new([320, 240])?, FILE_LIMIT)?;
    for index in 0..120_u32 {
        let at = Duration::from_nanos(u64::from(index) * 10_000_000 / 30 * 100);
        assert!(
            encoder
                .write(
                    &crate::movie::tests::color_frame(usize::try_from(index)?, [320, 240]),
                    at,
                    &mut || false,
                )?
                .is_continue()
        );
    }
    encoder.finish()?;
    for index in [119_usize, 0, 1, 45, 2] {
        let at = Duration::from_nanos(u64::try_from(index)? * 10_000_000 / 30 * 100);
        let file = std::fs::File::open(&path)?;
        let bytes = file.metadata()?.len();
        let frame = read::frame(file, bytes, [320, 240], at, &|| false)?
            .ok_or_else(|| anyhow::anyhow!("Synthetic color frame cancelled"))?;
        assert_eq!(frame.at, at);
        crate::movie::tests::check_colors(&frame, index);
    }
    let path = PathBuf::from(directory).join("windows-hd.mp4");
    let mut encoder = Encoder::open(&path, canvas::Canvas::new([1280, 720])?, FILE_LIMIT)?;
    for index in 0..2_u32 {
        assert!(
            encoder
                .write(
                    &crate::movie::tests::color_frame(usize::try_from(index)?, [1280, 720]),
                    Duration::from_nanos(u64::from(index) * 10_000_000 / 30 * 100),
                    &mut || false
                )?
                .is_continue()
        );
    }
    encoder.finish()?;
    for index in [1_usize, 0] {
        let at = Duration::from_nanos(u64::try_from(index)? * 10_000_000 / 30 * 100);
        let file = std::fs::File::open(&path)?;
        let bytes = file.metadata()?.len();
        let frame = read::frame(file, bytes, [1280, 720], at, &|| false)?
            .ok_or_else(|| anyhow::anyhow!("Synthetic HD frame cancelled"))?;
        assert_eq!(frame.at, at);
        crate::movie::tests::check_colors(&frame, index);
    }
    Ok(())
}

#[test]
#[ignore = "Opt in: decode synthetic Mac encoder artifacts on Windows"]
fn native_mac_movies_preserve_colors_and_timestamps() -> anyhow::Result<()> {
    let directory = PathBuf::from(
        std::env::var_os("SPEAKEASY_MAC_MOVIE_DIRECTORY")
            .ok_or_else(|| anyhow::anyhow!("Choose the synthetic Mac movie directory"))?,
    );
    for (name, count, size) in [
        ("moving", 120, [320, 240]),
        ("single", 1, [320, 240]),
        ("hd", 2, [1280, 720]),
    ] {
        for index in [count - 1, 0, count / 2] {
            let at = Duration::from_nanos(u64::try_from(index)? * 10_000_000 / 30 * 100);
            let file = std::fs::File::open(directory.join(format!("{name}.mp4")))?;
            let bytes = file.metadata()?.len();
            let frame = read::frame(file, bytes, size, at, &|| false)?
                .ok_or_else(|| anyhow::anyhow!("Synthetic Mac frame cancelled"))?;
            assert_eq!(frame.at, at);
            crate::movie::tests::check_colors(&frame, index);
        }
    }
    Ok(())
}
