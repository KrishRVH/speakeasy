use std::{path::Path, sync::Arc, time::Duration};

use anyhow::Context as _;
use gpui::RenderImage;
use image::{Rgba, RgbaImage};
use speakeasy_show::{Annotations, Moment, Source, SourceFrame};

use super::{AnnotationState, Content, Decoded, Request};
use crate::show::store::{movie, source};

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
    let (pixels, at) = match content {
        Content::Png(name) => (png(&directory, &name, cancelled)?, at),
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
        annotations: AnnotationState::Off,
    })
}

fn png(directory: &Path, name: &str, cancelled: &dyn Fn() -> bool) -> Result<RgbaImage, String> {
    let bytes = std::fs::read(directory.join(name))
        .map_err(|error| format!("Cannot open {name}: {error}"))?;
    if cancelled() {
        return Err(CANCELLED.into());
    }
    let mut pixels = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|error| format!("Cannot decode {name}: {error}"))?
        .into_rgba8();
    for Rgba([red, _, blue, _]) in pixels.pixels_mut() {
        std::mem::swap(red, blue);
    }
    Ok(pixels)
}

fn retained(
    directory: &Path,
    index: &Source,
    requested: Moment,
) -> Result<(RgbaImage, Moment), String> {
    let SourceFrame { at, image } = source::frame(directory, index, requested)
        .map_err(|error| format!("Cannot read source frame: {error:#}"))?;
    let pixels = RgbaImage::from_raw(image.width(), image.height(), image.into_bgra())
        .ok_or_else(|| "Cannot read source frame: invalid frame dimensions".to_owned())?;
    Ok((pixels, at))
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
    let (at, [width, height]) = (frame.at, frame.image.size());
    let (pixels, annotations) = match (annotated, scene) {
        (false, _) => (frame.image.into_pixels(), AnnotationState::Off),
        (true, None) => (frame.image.into_pixels(), AnnotationState::Unavailable),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_and_png_previews_preserve_bgra_pixels_and_their_sample_time() -> anyhow::Result<()> {
        use speakeasy_show::{Image, Journal};

        let directory = tempfile::tempdir()?;
        let bgra = [7, 20, 190, 255].repeat(70 * 65);
        let image = Image::from_bgra(70, 65, bgra.clone())?;
        std::fs::write(directory.path().join("frame.png"), image.encode_png()?)?;
        let mut journal = Journal::default();
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
        let preview = |content, at| {
            read(
                Request {
                    directory: directory.path().to_owned(),
                    content,
                    at,
                    annotated: false,
                    fixture: None,
                },
                &|| false,
            )
            .map_err(anyhow::Error::msg)
        };
        for content in [
            Content::Source {
                index: Arc::clone(&index),
            },
            Content::Png("frame.png".into()),
        ] {
            let decoded = preview(content, Moment::from_millis(200))?;
            assert_eq!(decoded.image.as_bytes(0), Some(bgra.as_slice()));
            assert_eq!(decoded.at, Duration::from_millis(200));
            assert_eq!(decoded.annotations, AnnotationState::Off);
        }
        let held = preview(Content::Source { index }, Moment::from_millis(450))?;
        assert_eq!(
            held.at,
            Duration::from_millis(200),
            "A source preview reported the requested time instead of its sample's"
        );
        Ok(())
    }
}
