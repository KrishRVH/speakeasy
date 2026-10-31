//! Bounded preview decoding keeps private pixels out of GPUI's persistent asset cache.

use std::sync::Arc;

use gpui::RenderImage;

use super::{Content, Decoded, Request};
use crate::show::store::{movie, recall, source};

const LONGEST_EDGE: u32 = 4_096;

pub(super) fn read(request: Request, cancelled: &dyn Fn() -> bool) -> Result<Decoded, String> {
    let Request {
        directory,
        content,
        at,
        annotated,
        fixture: _fixture,
    } = request;
    if cancelled() {
        return Err("Preview cancelled".into());
    }
    let bytes = match content {
        Content::Png(name) => {
            let bytes = recall::read_file(&directory, &name, recall::IMAGE_LIMIT)
                .map_err(|error| format!("Cannot open {name}: {error}"))?;
            check_header(&bytes).map_err(str::to_owned)?;
            bytes
        },
        Content::Source { index, at } => source::frame(&directory, &index, at)
            .and_then(|frame| frame.image.encode_png().map_err(anyhow::Error::from))
            .map_err(|error| format!("Cannot read source frame: {error:#}"))?,
        Content::Video {
            span,
            annotations,
            scene,
        } => {
            let decode = || -> anyhow::Result<Decoded> {
                use anyhow::Context as _;
                let reading = movie::Reading::open(&directory, annotations)?;
                let frame = reading
                    .sample(span.start, cancelled)?
                    .context("Preview cancelled")?;
                let at = frame.at;
                let size = frame.size;
                let (pixels, annotations) = if annotated {
                    match scene {
                        Some(scene) => {
                            let rendered = frame
                                .render(Some(&scene), None, cancelled)?
                                .context("Preview cancelled")?;
                            (rendered.image.into_bgra(), rendered.annotations)
                        },
                        None => (frame.bgra, movie::AnnotationState::Unavailable),
                    }
                } else {
                    (frame.bgra, movie::AnnotationState::Off)
                };
                if cancelled() {
                    anyhow::bail!("Preview cancelled");
                }
                let pixels = image::RgbaImage::from_raw(size[0], size[1], pixels)
                    .context("Video frame dimensions are invalid")?;
                // GPUI's RenderImage stores BGRA in image::Frame's byte buffer.
                let image = Arc::new(RenderImage::new([image::Frame::new(pixels)]));
                Ok(Decoded {
                    image,
                    at,
                    annotations,
                })
            };
            return decode().map_err(|error| format!("Cannot read video frame: {error:#}"));
        },
    };
    if cancelled() {
        return Err("Preview cancelled".into());
    }
    let mut pixels = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|_private_error| "This frame could not be decoded as a PNG image.".to_owned())?
        .into_rgba8();
    for [red, _, blue, _] in pixels.as_flat_samples_mut().samples.as_chunks_mut::<4>().0 {
        std::mem::swap(red, blue);
    }
    let image = Arc::new(RenderImage::new([image::Frame::new(pixels)]));
    Ok(Decoded {
        image,
        at: at.since(speakeasy_show::Moment::START),
        annotations: movie::AnnotationState::Off,
    })
}

fn check_header(bytes: &[u8]) -> Result<(), &'static str> {
    // PNG requires a 13-byte IHDR first. Bound its dimensions before the decoder can allocate
    // pixels; the decoder still verifies the complete file, including the header's CRC.
    if bytes.get(..16) != Some(&b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"[..]) || bytes.len() < 33 {
        return Err("This frame has an invalid or truncated PNG header.");
    }
    let dimension = |range| {
        bytes
            .get(range)
            .and_then(|bytes: &[u8]| bytes.try_into().ok())
            .map(u32::from_be_bytes)
    };
    match (dimension(16..20), dimension(20..24)) {
        (Some(width), Some(height))
            if (1..=LONGEST_EDGE).contains(&width) && (1..=LONGEST_EDGE).contains(&height) =>
        {
            Ok(())
        },
        _ => Err("This frame is too large to display (maximum 4096 pixels per side)."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        header[16..20].copy_from_slice(&4_097_u32.to_be_bytes());
        assert!(check_header(&header).is_err());
        assert!(
            check_header(b"PRIVATE_SCREEN_CONTENT")
                .unwrap_err()
                .contains("PNG header")
        );
    }
}
