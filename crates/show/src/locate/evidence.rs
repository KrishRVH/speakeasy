use serde::Serialize;

use crate::{
    Error, Image, Moment, Point, Rect, SourceFrame, Typeface, convert,
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

impl PixelRect {
    pub(crate) fn covering(part: Rect, width: u32, height: u32) -> Self {
        let edge = |fraction: f32, extent: u32, round: fn(f32) -> f32| {
            convert::pixels(round(fraction * extent as f32)).min(extent)
        };
        let left = edge(part.left, width, f32::floor).min(width.saturating_sub(1));
        let top = edge(part.top, height, f32::floor).min(height.saturating_sub(1));
        let right = edge(part.right, width, f32::ceil);
        let bottom = edge(part.bottom, height, f32::ceil);
        Self {
            left,
            top,
            width: right.saturating_sub(left).max(1),
            height: bottom.saturating_sub(top).max(1),
        }
    }

    pub(crate) fn part(self, width: u32, height: u32) -> Rect {
        let (across, down) = (width.max(1) as f32, height.max(1) as f32);
        Rect::from_origin(
            Point::new(self.left as f32 / across, self.top as f32 / down),
            self.width as f32 / across,
            self.height as f32 / down,
        )
    }

    pub(crate) fn cut(self, image: Image) -> Result<Image, Error> {
        if self.width == image.width() && self.height == image.height() {
            Ok(image)
        } else {
            image.cropped(self.left, self.top, self.width, self.height)
        }
    }
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
                Error::invalid("Choose between 1 and 12 distinct ordered source frames").into(),
            );
        }
        let mut frames = Vec::with_capacity(moments.len());
        let mut cells = Vec::with_capacity(moments.len());
        let mut dimensions = None;
        for &at in moments {
            let SourceFrame { at: sampled, image } = frame(at)?;
            if sampled != at {
                return Err(
                    Error::invalid("Source sampling changed; refresh before exporting").into(),
                );
            }
            let size = (image.width(), image.height());
            if crop.is_some() && dimensions.is_some_and(|previous| previous != size) {
                return Err(Error::invalid(
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
                .ok_or_else(|| Error::invalid("Cannot scale the source image"))?;
            frames.push((thumbnail, Corner::Left));
            cells.push(Cell {
                sampled_ms: sampled.as_millis(),
                crop_px,
            });
        }
        let badges = moments
            .iter()
            .enumerate()
            .map(|(index, at)| Badge {
                number: index.saturating_add(1),
                time: format!("{at}.{:03}", at.as_millis() % 1_000),
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
        return Err(Error::invalid(
            "The mark has invalid bounds; export an uncropped frame instead",
        ));
    }
    let width = (rect.width() * 1.4).clamp(0.2, 1.0);
    let height = (rect.height() * 1.4).clamp(0.2, 1.0);
    let left = (rect.center().x - width / 2.0).clamp(0.0, 1.0 - width);
    let top = (rect.center().y - height / 2.0).clamp(0.0, 1.0 - height);
    let part = Rect::from_origin(Point::new(left, top), width, height);
    let pixels = PixelRect::covering(part, image.width(), image.height());
    Ok((pixels.cut(image)?, Some(pixels)))
}
