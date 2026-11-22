use serde::Serialize;

use crate::{
    Error, Image, Moment, PixelRect, Point, Rect, SourceFrame, Typeface, convert,
    draw::{Badge, Corner},
    sheet::{self, Tile},
};

const THUMBNAIL_EDGE: u32 = 800;

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

const _: () = assert!(
    Evidence::FRAMES == 12,
    "the frame count error names the evidence limit"
);

impl Evidence {
    pub const FRAMES: usize = 12;

    pub fn compose<E: From<Error>>(
        moments: &[Moment],
        crop: Option<Rect>,
        mut frame: impl FnMut(Moment) -> Result<SourceFrame, E>,
        typeface: &Typeface,
    ) -> Result<Self, E> {
        if !(1..=Self::FRAMES).contains(&moments.len())
            || !moments.is_sorted_by(|one, two| one < two)
        {
            return Err(
                Error::invalid("Choose between 1 and 12 distinct ordered source frames").into(),
            );
        }
        let mut thumbnails = Vec::with_capacity(moments.len());
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
            let longest = image.width().max(image.height());
            let thumbnail = if longest <= THUMBNAIL_EDGE {
                image
            } else {
                let scale = THUMBNAIL_EDGE as f32 / longest as f32;
                image.resized(
                    convert::pixels(image.width() as f32 * scale).max(1),
                    convert::pixels(image.height() as f32 * scale).max(1),
                )?
            };
            thumbnails.push(thumbnail);
            cells.push(Cell {
                sampled_ms: sampled.as_millis(),
                crop_px,
            });
        }
        let tiles: Vec<Tile<'_>> = thumbnails
            .iter()
            .zip(moments)
            .zip(1..)
            .map(|((image, at), number)| Tile {
                image,
                badge: Badge {
                    number,
                    time: format!("{at}.{:03}", at.as_millis() % 1_000),
                },
                corner: Corner::Left,
            })
            .collect();
        Ok(Self {
            image: sheet::render(&tiles, typeface)?,
            cells,
        })
    }
}

pub fn crop_image(image: Image, crop: Option<Rect>) -> Result<(Image, Option<PixelRect>), Error> {
    let Some(rect) = crop else {
        return Ok((image, None));
    };
    if !rect.is_within_unit() {
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
    Ok((image.cut(pixels)?, Some(pixels)))
}
