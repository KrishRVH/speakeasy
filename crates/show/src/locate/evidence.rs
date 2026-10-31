//! Bounded source contact sheets. Only thumbnails survive each source-frame read.

use serde::Serialize;

use crate::{Error, Image, Moment, Rect, SourceFrame, Typeface, convert, draw::Corner, sheet};

/// A crop in original source pixels, with an exclusive right and bottom edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PixelRect {
    /// Distance from the source's left edge.
    pub left: u32,
    /// Distance from the source's top edge.
    pub top: u32,
    /// Width before any contact-sheet scaling.
    pub width: u32,
    /// Height before any contact-sheet scaling.
    pub height: u32,
}

/// Provenance for a cell, in reading order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cell {
    /// The actual retained sample time, never the requested location.
    pub sampled_ms: u64,
    /// The source rectangle when a mark crop was requested.
    pub crop_px: Option<PixelRect>,
}

/// A focused source contact sheet and its exact provenance. Images have no ink annotations.
#[derive(Debug)]
pub struct Evidence {
    /// The sheet, badged with cell numbers and millisecond sample times.
    pub image: Image,
    /// One entry per image on the sheet, in reading order.
    pub cells: Vec<Cell>,
}

impl Evidence {
    /// Loads 1–12 distinct source frames sequentially and composes a bounded contact sheet.
    /// Each full-size source is dropped before loading the next; thumbnails are at most 800 px
    /// on their longest edge. `crop` is a mark's normalized bounds, expanded for context.
    ///
    /// # Errors
    /// Rejects empty, duplicate, unordered or oversized requests, invalid crop bounds, and crops
    /// spanning a surface resize. Propagates loading and rendering failures without adding text.
    pub fn compose<E: From<Error>>(
        moments: &[Moment],
        crop: Option<Rect>,
        mut frame: impl FnMut(Moment) -> Result<SourceFrame, E>,
        typeface: &Typeface,
    ) -> Result<Self, E> {
        if !(1..=12).contains(&moments.len())
            || moments.array_windows::<2>().any(|[one, two]| one >= two)
        {
            return Err(
                Error::evidence("Choose between 1 and 12 distinct ordered source frames").into(),
            );
        }
        let mut frames = Vec::with_capacity(moments.len());
        let mut cells = Vec::with_capacity(moments.len());
        let mut dimensions = None;
        for &at in moments {
            let SourceFrame { at: sampled, image } = frame(at)?;
            if sampled != at {
                return Err(
                    Error::evidence("Source sampling changed; refresh before exporting").into(),
                );
            }
            let size = (image.width(), image.height());
            if crop.is_some() && dimensions.is_some_and(|previous| previous != size) {
                return Err(Error::evidence(
                    "The captured surface resized; export uncropped frames instead",
                )
                .into());
            }
            dimensions = Some(size);
            let (image, crop_px) = crop_image(image, crop)?;
            let scale = (800.0 / image.width().max(image.height()) as f32).min(1.0);
            let thumbnail = image
                .resized(
                    convert::pixels(image.width() as f32 * scale).max(1),
                    convert::pixels(image.height() as f32 * scale).max(1),
                )
                .ok_or_else(|| Error::evidence("Cannot scale the source image"))?;
            frames.push((thumbnail, Corner::Left));
            cells.push(Cell {
                sampled_ms: sampled.as_millis(),
                crop_px,
            });
        }
        let labels = moments
            .iter()
            .enumerate()
            .map(|(index, at)| {
                let ms = at.as_millis();
                (
                    index.saturating_add(1),
                    format!("{}:{:02}.{:03}", ms / 60_000, ms / 1_000 % 60, ms % 1_000),
                )
            })
            .collect::<Vec<_>>();
        Ok(Self {
            image: sheet::render(&labels, &frames, typeface)?,
            cells,
        })
    }
}

/// Crops one source frame with the same geometry as a contact sheet; no crop keeps it unchanged.
///
/// # Errors
/// Rejects invalid normalized bounds or an image that cannot be cropped.
pub fn crop_image(image: Image, crop: Option<Rect>) -> Result<(Image, Option<PixelRect>), Error> {
    let Some(rect) = crop else {
        return Ok((image, None));
    };
    if ![rect.left, rect.top, rect.right, rect.bottom]
        .iter()
        .all(|edge| edge.is_finite() && (0.0..=1.0).contains(edge))
        || rect.left > rect.right
        || rect.top > rect.bottom
    {
        return Err(Error::evidence(
            "The mark has invalid bounds; export an uncropped frame instead",
        ));
    }
    let width = (rect.width() * 1.4).clamp(0.2, 1.0);
    let height = (rect.height() * 1.4).clamp(0.2, 1.0);
    let left = (rect.center().x - width / 2.0).clamp(0.0, 1.0 - width);
    let top = (rect.center().y - height / 2.0).clamp(0.0, 1.0 - height);
    let x =
        convert::pixels((left * image.width() as f32).floor()).min(image.width().saturating_sub(1));
    let y = convert::pixels((top * image.height() as f32).floor())
        .min(image.height().saturating_sub(1));
    let right = convert::pixels(((left + width) * image.width() as f32).ceil()).min(image.width());
    let bottom =
        convert::pixels(((top + height) * image.height() as f32).ceil()).min(image.height());
    let pixels = PixelRect {
        left: x,
        top: y,
        width: right.saturating_sub(x).max(1),
        height: bottom.saturating_sub(y).max(1),
    };
    let cropped = if pixels.width == image.width() && pixels.height == image.height() {
        image
    } else {
        image.cropped(pixels.left, pixels.top, pixels.width, pixels.height)?
    };
    Ok((cropped, Some(pixels)))
}
