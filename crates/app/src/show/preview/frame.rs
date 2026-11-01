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
    let pixels = match content {
        Content::Png(name) => {
            let bytes = recall::read_file(&directory, &name, recall::IMAGE_LIMIT)
                .map_err(|error| format!("Cannot open {name}: {error}"))?;
            check_header(&bytes).map_err(str::to_owned)?;
            if cancelled() {
                return Err("Preview cancelled".into());
            }
            let mut pixels = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
                .map_err(|_private_error| {
                    "This frame could not be decoded as a PNG image.".to_owned()
                })?
                .into_rgba8();
            for [red, _, blue, _] in pixels.as_flat_samples_mut().samples.as_chunks_mut::<4>().0 {
                std::mem::swap(red, blue);
            }
            pixels
        },
        Content::Source { index, at } => {
            let image = source::frame(&directory, &index, at)
                .map_err(|error| format!("Cannot read source frame: {error:#}"))?
                .image;
            image::RgbaImage::from_raw(image.width(), image.height(), image.into_bgra())
                .ok_or_else(|| "Source frame dimensions are invalid".to_owned())?
        },
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
    let image = Arc::new(RenderImage::new([image::Frame::new(pixels)]));
    Ok(Decoded {
        image,
        at: at.since(speakeasy_show::Moment::START),
        annotations: movie::AnnotationState::Off,
    })
}

fn check_header(bytes: &[u8]) -> Result<(), &'static str> {
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
        for content in [
            Content::Source { index, at },
            Content::Png("frame.png".into()),
        ] {
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
        header[16..20].copy_from_slice(&4_097_u32.to_be_bytes());
        assert!(check_header(&header).is_err());
        assert!(
            check_header(b"PRIVATE_SCREEN_CONTENT")
                .unwrap_err()
                .contains("PNG header")
        );
    }
}
