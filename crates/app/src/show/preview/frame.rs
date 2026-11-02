use std::{path::Path, sync::Arc, time::Duration};

use anyhow::Context as _;
use gpui::RenderImage;
use image::RgbaImage;
use speakeasy_show::{Annotations, Moment, Source};

use super::{Content, Decoded, Request};
use crate::show::store::{movie, recall, source};

const LONGEST_EDGE: u32 = 4_096;

const CANCELLED: &str = "Preview cancelled";

pub(super) fn read(request: Request, cancelled: &dyn Fn() -> bool) -> Result<Decoded, String> {
    let Request {
        directory,
        content,
        at,
        annotated,
        fixture: _fixture,
    } = request;
    if cancelled() {
        return Err(CANCELLED.into());
    }
    let pixels = match content {
        Content::Png(name) => png(&directory, &name, cancelled)?,
        Content::Source { index } => retained(&directory, &index, at)?,
        Content::Video {
            span,
            annotations,
            scene,
        } => {
            return video(
                &directory,
                span.start,
                annotations,
                annotated,
                scene.as_deref(),
                cancelled,
            )
            .map_err(|error| format!("Cannot read video frame: {error:#}"));
        },
    };
    if cancelled() {
        return Err(CANCELLED.into());
    }
    Ok(Decoded {
        image: render(pixels),
        at: at.since(Moment::START),
        annotations: movie::AnnotationState::Off,
    })
}

fn png(directory: &Path, name: &str, cancelled: &dyn Fn() -> bool) -> Result<RgbaImage, String> {
    let bytes = recall::read_file(directory, name, recall::IMAGE_LIMIT)
        .map_err(|error| format!("Cannot open {name}: {error}"))?;
    check_header(&bytes)?;
    if cancelled() {
        return Err(CANCELLED.into());
    }
    let mut pixels = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|_private_error| "This frame could not be decoded as a PNG image.".to_owned())?
        .into_rgba8();
    for [red, _, blue, _] in pixels.as_flat_samples_mut().samples.as_chunks_mut::<4>().0 {
        std::mem::swap(red, blue);
    }
    Ok(pixels)
}

fn retained(directory: &Path, index: &Source, at: Moment) -> Result<RgbaImage, String> {
    let image = source::frame(directory, index, at)
        .map_err(|error| format!("Cannot read source frame: {error:#}"))?
        .image;
    RgbaImage::from_raw(image.width(), image.height(), image.into_bgra())
        .ok_or_else(|| "Cannot read source frame: invalid frame dimensions".to_owned())
}

fn video(
    directory: &Path,
    at: Duration,
    through: Moment,
    annotated: bool,
    scene: Option<&Annotations>,
    cancelled: &dyn Fn() -> bool,
) -> anyhow::Result<Decoded> {
    let frame = movie::Reading::open(directory, through)?
        .sample(at, cancelled)?
        .context(CANCELLED)?;
    let (at, [width, height]) = (frame.at, frame.size);
    let (pixels, annotations) = match (annotated, scene) {
        (false, _) => (frame.bgra, movie::AnnotationState::Off),
        (true, None) => (frame.bgra, movie::AnnotationState::Unavailable),
        (true, Some(scene)) => {
            let rendered = frame
                .render(Some(scene), None, cancelled)?
                .context(CANCELLED)?;
            (rendered.image.into_bgra(), rendered.annotations)
        },
    };
    anyhow::ensure!(!cancelled(), CANCELLED);
    let pixels =
        RgbaImage::from_raw(width, height, pixels).context("Video frame dimensions are invalid")?;
    Ok(Decoded {
        image: render(pixels),
        at,
        annotations,
    })
}

fn render(pixels: RgbaImage) -> Arc<RenderImage> {
    Arc::new(RenderImage::new([image::Frame::new(pixels)]))
}

fn check_header(bytes: &[u8]) -> Result<(), String> {
    let invalid = "This frame has an invalid or truncated PNG header.";
    if bytes.get(..16) != Some(&b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"[..]) || bytes.len() < 33 {
        return Err(invalid.into());
    }
    let dimension = |range| {
        bytes
            .get(range)
            .and_then(|bytes: &[u8]| bytes.try_into().ok())
            .map(u32::from_be_bytes)
    };
    let (Some(width @ 1..), Some(height @ 1..)) = (dimension(16..20), dimension(20..24)) else {
        return Err(invalid.into());
    };
    if width > LONGEST_EDGE || height > LONGEST_EDGE {
        Err(format!(
            "This frame is too large to display (maximum {LONGEST_EDGE} pixels per side)."
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_and_png_previews_preserve_bgra_pixels_and_selected_time() -> anyhow::Result<()> {
        use speakeasy_show::{Image, Journal, Moment};

        let directory = tempfile::tempdir()?;
        let bgra = [7, 20, 190, 255].repeat(70 * 65);
        let image = Image::from_bgra(70, 65, bgra.clone())?;
        std::fs::write(directory.path().join("frame.png"), image.encode_png()?)?;
        let mut journal = Journal::new();
        journal
            .admit()
            .unwrap()
            .record(Moment::from_millis(200), image);
        let mut source_bytes = Vec::new();
        journal.write_source(Moment::from_millis(1_000), |bytes| {
            source_bytes.extend_from_slice(bytes);
            Ok::<_, anyhow::Error>(())
        })?;
        std::fs::write(directory.path().join(source::FILE), source_bytes)?;
        let index = Arc::new(source::inspect(directory.path())?);
        let at = Moment::from_millis(200);
        for content in [Content::Source { index }, Content::Png("frame.png".into())] {
            let decoded = read(
                Request {
                    directory: directory.path().to_owned(),
                    content,
                    at,
                    annotated: false,
                    fixture: None,
                },
                &|| false,
            )
            .map_err(anyhow::Error::msg)?;
            assert_eq!(decoded.image.as_bytes(0), Some(bgra.as_slice()));
            assert_eq!(decoded.at, std::time::Duration::from_millis(200));
            assert_eq!(decoded.annotations, movie::AnnotationState::Off);
        }
        Ok(())
    }

    #[test]
    fn invalid_and_oversized_png_headers_fail_before_decoding() {
        let mut header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        header.extend_from_slice(&1_u32.to_be_bytes());
        header.extend_from_slice(&1_u32.to_be_bytes());
        header.resize(33, 0);
        assert!(check_header(&header).is_ok());
        for length in 0..33 {
            assert!(check_header(&header[..length]).is_err());
        }
        for (width, height, expected) in [
            (
                0_u32,
                1_u32,
                "This frame has an invalid or truncated PNG header.",
            ),
            (1, 0, "This frame has an invalid or truncated PNG header."),
            (
                4_097,
                1,
                "This frame is too large to display (maximum 4096 pixels per side).",
            ),
            (
                1,
                4_097,
                "This frame is too large to display (maximum 4096 pixels per side).",
            ),
        ] {
            header[16..20].copy_from_slice(&width.to_be_bytes());
            header[20..24].copy_from_slice(&height.to_be_bytes());
            assert_eq!(check_header(&header).unwrap_err(), expected);
        }
        assert!(
            check_header(b"PRIVATE_SCREEN_CONTENT")
                .unwrap_err()
                .contains("PNG header")
        );
    }
}
