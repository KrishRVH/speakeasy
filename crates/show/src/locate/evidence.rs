use serde::Serialize;

use crate::{
    Error, Image, Moment, Rect, SourceFrame, Typeface, convert,
    draw::{Badge, Corner},
    sheet,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PixelRect {
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cell {
    pub sampled_ms: u64,
    pub crop_px: Option<PixelRect>,
}

#[derive(Debug)]
pub struct Evidence {
    pub image: Image,
    pub cells: Vec<Cell>,
}

impl Evidence {
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
        let badges = moments
            .iter()
            .enumerate()
            .map(|(index, at)| {
                let ms = at.as_millis();
                Badge {
                    number: index.saturating_add(1),
                    time: format!("{}:{:02}.{:03}", ms / 60_000, ms / 1_000 % 60, ms % 1_000),
                }
            })
            .collect::<Vec<_>>();
        Ok(Self {
            image: sheet::render(&badges, &frames, typeface)?,
            cells,
        })
    }
}

pub fn crop_image(image: Image, crop: Option<Rect>) -> Result<(Image, Option<PixelRect>), Error> {
    let Some(rect) = crop else {
        return Ok((image, None));
    };
    if ![rect.left, rect.top, rect.right, rect.bottom]
        .iter()
        .all(|edge| (0.0..=1.0).contains(edge))
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
