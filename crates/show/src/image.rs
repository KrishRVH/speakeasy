//! Owned raster images, PNG, and area resampling.

use std::borrow::Cow;

use tiny_skia::{IntSize, Pixmap, PixmapMut, PixmapRef, PremultipliedColorU8};

use crate::{Error, convert};

/// An owned image with nonzero dimensions, stored as premultiplied RGBA8 rows.
///
/// Captured screens are opaque, so for them premultiplied and straight alpha coincide.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pixmap: Pixmap,
}

impl Image {
    /// Wraps straight-alpha RGBA8 rows, the layout PNG decoders produce.
    #[cfg(test)]
    pub(crate) fn from_rgba(width: u32, height: u32, mut rgba: Vec<u8>) -> Result<Self, Error> {
        let (pixels, _) = rgba.as_chunks_mut::<4>();
        for [red, green, blue, alpha] in pixels {
            if *alpha < u8::MAX {
                let color =
                    tiny_skia::ColorU8::from_rgba(*red, *green, *blue, *alpha).premultiply();
                [*red, *green, *blue] = [color.red(), color.green(), color.blue()];
            }
        }
        Self::from_premultiplied(width, height, rgba)
    }

    /// Wraps captured BGRA or BGRX rows; the screen is opaque, so their fourth byte is ignored.
    ///
    /// # Errors
    ///
    /// Fails when a dimension is zero or `bgra` does not hold exactly `width × height` pixels.
    pub fn from_bgra(width: u32, height: u32, mut bgra: Vec<u8>) -> Result<Self, Error> {
        let (pixels, _) = bgra.as_chunks_mut::<4>();
        for [blue, _, red, alpha] in pixels {
            std::mem::swap(blue, red);
            *alpha = u8::MAX;
        }
        Self::from_premultiplied(width, height, bgra)
    }

    /// An opaque image of one color.
    pub(crate) fn filled(
        width: u32,
        height: u32,
        [red, green, blue]: [u8; 3],
    ) -> Result<Self, Error> {
        let mut pixmap =
            Pixmap::new(width, height).ok_or_else(|| Error::dimensions(width, height))?;
        pixmap.fill(tiny_skia::Color::from_rgba8(red, green, blue, u8::MAX));
        Ok(Self { pixmap })
    }

    pub(crate) fn from_premultiplied(
        width: u32,
        height: u32,
        data: Vec<u8>,
    ) -> Result<Self, Error> {
        IntSize::from_wh(width, height)
            .and_then(|size| Pixmap::from_vec(data, size))
            .map(|pixmap| Self { pixmap })
            .ok_or_else(|| Error::dimensions(width, height))
    }

    /// Width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.pixmap.width()
    }

    /// Height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.pixmap.height()
    }

    /// Width divided by height.
    #[must_use]
    pub fn aspect(&self) -> f32 {
        self.width() as f32 / self.height() as f32
    }

    /// Decodes an 8-bit RGB or RGBA PNG, as [`Image::encode_png`] writes.
    #[cfg(test)]
    pub(crate) fn decode_png(bytes: &[u8]) -> Self {
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut data = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut data).unwrap();
        data.truncate(info.buffer_size());
        if info.color_type == png::ColorType::Rgb {
            data = data
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|&[red, green, blue]| [red, green, blue, u8::MAX])
                .collect();
        }
        Self::from_rgba(info.width, info.height, data).unwrap()
    }

    /// Encodes the image as an RGBA PNG.
    ///
    /// Fast compression favors latency; screen content still compresses well.
    ///
    /// # Errors
    ///
    /// Fails only if the encoder rejects the stream, which a valid image never causes.
    pub fn encode_png(&self) -> Result<Vec<u8>, Error> {
        let data = self.pixmap.data();
        let (pixels, _) = data.as_chunks::<4>();
        // Captured screens are opaque, so their premultiplied pixels are already straight.
        let rgba = if pixels.iter().all(|[.., alpha]| *alpha == u8::MAX) {
            Cow::Borrowed(data)
        } else {
            Cow::Owned(
                pixels
                    .iter()
                    .flat_map(|&[red, green, blue, alpha]| {
                        PremultipliedColorU8::from_rgba(red, green, blue, alpha).map_or(
                            [red, green, blue, alpha],
                            |color| {
                                let color = color.demultiply();
                                [color.red(), color.green(), color.blue(), alpha]
                            },
                        )
                    })
                    .collect(),
            )
        };
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, self.width(), self.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(Error::png)?;
        writer.write_image_data(&rgba).map_err(Error::png)?;
        writer.finish().map_err(Error::png)?;
        Ok(png)
    }

    /// The image resampled to `width × height`, each output pixel averaging the source area it
    /// covers, so large reductions keep text legible instead of aliasing.
    pub(crate) fn resized(&self, width: u32, height: u32) -> Option<Self> {
        let (columns, rows) = (
            footprints(self.width(), width),
            footprints(self.height(), height),
        );
        let wide = convert::count(width);
        // The horizontal pass keeps full precision so the vertical pass rounds only once.
        let mut horizontal = Vec::with_capacity(wide.saturating_mul(convert::count(self.height())));
        for row in self.rows() {
            let pixels = row.as_chunks::<4>().0;
            horizontal.extend(columns.iter().map(|footprint| footprint.average(pixels)));
        }
        let lines: Vec<&[[f32; 4]]> = horizontal.chunks_exact(wide.max(1)).collect();
        let mut data = Vec::with_capacity(
            wide.saturating_mul(convert::count(height))
                .saturating_mul(4),
        );
        let mut line = vec![[0.0; 4]; wide];
        for footprint in &rows {
            line.fill([0.0; 4]);
            for (source, weight) in lines.iter().skip(footprint.first).zip(&footprint.weights) {
                for (sum, pixel) in line.iter_mut().zip(*source) {
                    accumulate(sum, pixel, *weight);
                }
            }
            data.extend(
                line.iter()
                    .flatten()
                    .map(|&channel| convert::channel(channel)),
            );
        }
        Self::from_premultiplied(width, height, data).ok()
    }

    /// The image's rows of premultiplied RGBA8 pixels.
    pub(crate) fn rows(&self) -> impl Iterator<Item = &[u8]> {
        self.pixmap
            .data()
            .chunks_exact(convert::count(self.width()).saturating_mul(4))
    }

    pub(crate) fn as_ref(&self) -> PixmapRef<'_> {
        self.pixmap.as_ref()
    }

    pub(crate) fn as_mut(&mut self) -> PixmapMut<'_> {
        self.pixmap.as_mut()
    }
}

/// The source pixels one output pixel covers along an axis: consecutive pixels from `first`,
/// each weighted by the share of the output pixel it fills. Weights sum to one.
struct Footprint {
    first: usize,
    weights: Vec<f32>,
}

impl Footprint {
    fn average(&self, pixels: &[[u8; 4]]) -> [f32; 4] {
        pixels.iter().skip(self.first).zip(&self.weights).fold(
            [0.0; 4],
            |mut sum, (pixel, weight)| {
                accumulate(&mut sum, &pixel.map(f32::from), *weight);
                sum
            },
        )
    }
}

fn footprints(source: u32, target: u32) -> Vec<Footprint> {
    let scale = source as f32 / target.max(1) as f32;
    (0..target)
        .map(|index| {
            let low = index as f32 * scale;
            let high = (low + scale).min(source as f32);
            let first = convert::count_floor(low);
            let weights = (first..convert::count_ceil(high))
                .map(|pixel| {
                    let pixel = pixel as f32;
                    (high.min(pixel + 1.0) - low.max(pixel)).max(0.0) / scale
                })
                .collect();
            Footprint { first, weights }
        })
        .collect()
}

fn accumulate(sum: &mut [f32; 4], pixel: &[f32; 4], weight: f32) {
    for (total, channel) in sum.iter_mut().zip(pixel) {
        *total = channel.mul_add(weight, *total);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(image: &Image) -> Vec<[u8; 4]> {
        image.as_ref().data().as_chunks::<4>().0.to_vec()
    }

    #[test]
    fn captures_swap_to_rgba_and_become_opaque() {
        let image = Image::from_bgra(2, 1, vec![1, 2, 3, 0, 4, 5, 6, 9]).unwrap();
        assert_eq!(pixels(&image), [[3, 2, 1, 255], [6, 5, 4, 255]]);
        assert!(Image::from_bgra(2, 2, vec![0; 8]).is_err());
    }

    #[test]
    fn images_hold_exactly_their_pixels() {
        assert!(Image::from_rgba(2, 2, vec![0; 8]).is_err());
        assert!(Image::from_rgba(0, 1, Vec::new()).is_err());
        assert!(Image::filled(0, 1, [0; 3]).is_err());
    }

    #[test]
    fn png_round_trips_straight_alpha() {
        let image = Image::from_rgba(2, 1, vec![200, 100, 50, 255, 255, 0, 0, 128]).unwrap();
        let decoded = Image::decode_png(&image.encode_png().unwrap());
        assert_eq!(decoded, image);
        assert_eq!(pixels(&decoded)[1], [128, 0, 0, 128]);
        let opaque = Image::filled(3, 2, [9, 8, 7]).unwrap();
        assert_eq!(Image::decode_png(&opaque.encode_png().unwrap()), opaque);
    }

    #[test]
    fn reduction_averages_the_covered_area() {
        // A one-pixel checkerboard reduces to the mean gray, never to either extreme.
        let checker: Vec<u8> = (0..16_u8)
            .flat_map(|index| {
                let light = (index / 4 + index % 4) % 2 == 0;
                if light { [255; 4] } else { [0, 0, 0, 255] }
            })
            .collect();
        let image = Image::from_rgba(4, 4, checker).unwrap();
        let reduced = image.resized(1, 1).unwrap();
        assert_eq!(pixels(&reduced), [[128, 128, 128, 255]]);
        let thirds = image.resized(3, 3).unwrap();
        assert!(
            pixels(&thirds)
                .iter()
                .all(|[red, ..]| (64..=192).contains(red))
        );
        let enlarged = Image::filled(1, 1, [10, 20, 30])
            .unwrap()
            .resized(3, 2)
            .unwrap();
        assert!(
            pixels(&enlarged)
                .iter()
                .all(|pixel| *pixel == [10, 20, 30, 255])
        );
    }
}
