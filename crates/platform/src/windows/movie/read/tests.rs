//! Opt-in decoding checks use only the synthetic encoder fixtures, including recovered prefixes.

use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt as _, path::PathBuf};

use super::*;

#[test]
fn video_media_types_may_omit_the_default_stride() -> anyhow::Result<()> {
    let _foundation = Foundation::start()?;
    let [width, height] = [320_u32, 240_u32];
    // SAFETY: this test owns an empty media type and supplies only synthetic format attributes.
    let format = unsafe {
        let format = MFCreateMediaType()?;
        format.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        format.SetUINT64(
            &MF_MT_FRAME_SIZE,
            (u64::from(width) << 32) | u64::from(height),
        )?;
        format
    };
    assert_eq!(format_stride(&format, [320, 240])?, -1280);
    assert!(format_stride(&format, [640, 240]).is_err());
    Ok(())
}

#[test]
#[ignore = "Opt in: native decoder; set SPEAKEASY_MOVIE_READ_DIRECTORY to the validated synthetic movie prefixes"]
fn recovered_movie_seeks_preserve_pixels_time_and_file_ownership() -> anyhow::Result<()> {
    let directory = PathBuf::from(
        std::env::var_os("SPEAKEASY_MOVIE_READ_DIRECTORY")
            .context("Choose the synthetic movie prefix directory")?,
    );
    for name in ["moving.mp4", "held.mp4", "capped-closed.mp4"] {
        let path = directory.join(name);
        let decode = |ms, cancelled: &dyn Fn() -> bool| {
            let file = File::open(&path)?;
            let bytes = file.metadata()?.len();
            crate::read_movie_frame(
                file,
                bytes,
                [1280, 720],
                Duration::from_millis(ms),
                cancelled,
            )
        };
        for ms in [3500, 0, 1505, 3999] {
            let frame = decode(ms, &|| false)?.context("Synthetic read was cancelled")?;
            assert!(frame.at <= Duration::from_millis(ms));
            assert!(Duration::from_millis(ms).saturating_sub(frame.at) < Duration::from_millis(34));
            if ms == 1505 {
                assert_eq!(
                    frame.at,
                    Duration::from_millis(1500),
                    "Requested time replaced the actual decoded timestamp"
                );
            }
            assert_eq!(frame.bgra.len(), 1280 * 720 * 4);
            assert!(
                frame
                    .bgra
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| pixel[3] == 255)
            );
            if name == "moving.mp4" && ms == 0 {
                check_orientation(&frame.bgra);
            }
        }
        #[expect(
            clippy::disallowed_types,
            reason = "The injected Fn cancellation query needs a local call counter to interrupt decoding after samples have been read"
        )]
        let checks = std::cell::Cell::new(0);
        let cancel = || {
            checks.set(checks.get() + 1);
            checks.get() >= 5
        };
        assert!(decode(3500, &cancel)?.is_none());
        assert!(checks.get() >= 5);
        let exclusive = OpenOptions::new().read(true).share_mode(0).open(&path)?;
        drop(exclusive);
    }
    Ok(())
}

fn check_orientation(pixels: &[u8]) {
    let mut difference = 0_u64;
    for (index, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
        let ink = index % 1280 % 127 < 3 || index / 1280 % 41 < 2;
        let expected = if ink { [30_u8; 3] } else { [240, 245, 250] };
        difference = difference.saturating_add(
            pixel
                .iter()
                .zip(expected)
                .map(|(got, expected)| u64::from(got.abs_diff(expected)))
                .sum::<u64>(),
        );
    }
    assert!(
        difference < 1280 * 720 * 3 * 6,
        "Synthetic frame changed orientation or colors"
    );
}
