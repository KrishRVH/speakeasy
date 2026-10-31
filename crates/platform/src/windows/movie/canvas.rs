//! A fixed even canvas. Every resized frame clears the margins so pixels from an earlier window
//! size cannot remain visible. Bilinear fitting never enlarges the capture's pixels.

use anyhow::{Context as _, ensure};

use crate::{Frame, Part};

const EDGE: u32 = 2_560;

pub(super) struct Canvas {
    size: [u32; 2],
    pixels: Vec<u8>,
    content: [u32; 4],
    part: Part,
}

impl Canvas {
    pub(super) fn new(size: [u32; 2]) -> anyhow::Result<Self> {
        ensure!(
            size.into_iter().all(|edge| (1..=EDGE).contains(&edge)),
            "Invalid movie canvas"
        );
        let size = size.map(|edge| edge.saturating_add(1) & !1);
        let bytes = length(size)?;
        Ok(Self {
            size,
            pixels: vec![0; bytes],
            content: [0, 0, size[0], size[1]],
            part: Part::WHOLE,
        })
    }

    pub(super) const fn size(&self) -> [u32; 2] {
        self.size
    }

    pub(super) fn held(&self) -> &[u8] {
        &self.pixels
    }

    pub(super) const fn content(&self) -> [u32; 4] {
        self.content
    }

    pub(super) const fn part(&self) -> Part {
        self.part
    }

    pub(super) fn paint(&mut self, frame: &Frame) -> anyhow::Result<&[u8]> {
        let source = [frame.width, frame.height];
        ensure!(
            source.into_iter().all(|edge| (1..=EDGE).contains(&edge))
                && length(source)? == frame.bgra.len(),
            "Invalid movie frame"
        );
        if source == self.size {
            self.pixels.copy_from_slice(&frame.bgra);
            self.content = [0, 0, source[0], source[1]];
        } else {
            self.pixels.fill(0);
            self.content = fit(&frame.bgra, source, &mut self.pixels, self.size)?;
        }
        self.part = frame.part;
        Ok(&self.pixels)
    }
}

fn length([width, height]: [u32; 2]) -> anyhow::Result<usize> {
    usize::try_from(
        u64::from(width)
            .saturating_mul(u64::from(height))
            .saturating_mul(4),
    )
    .context("Movie canvas exceeds addressable memory")
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "Both nonzero image dimensions are validated at most 2560; byte offsets fit usize on supported 64-bit hosts and interpolation totals fit u32"
)]
fn fit(
    source: &[u8],
    [sw, sh]: [u32; 2],
    destination: &mut [u8],
    [dw, dh]: [u32; 2],
) -> anyhow::Result<[u32; 4]> {
    let (mut width, mut height) = (sw.min(dw), sh.min(dh));
    if width * sh <= height * sw {
        height = (sh * width / sw).max(1);
    } else {
        width = (sw * height / sh).max(1);
    }
    let x = (dw - width) / 2;
    let y = (dh - height) / 2;
    let left = usize::try_from(x)? * 4;
    let top = usize::try_from(y)?;
    let source_stride = usize::try_from(sw)? * 4;
    let stride = usize::try_from(dw)? * 4;
    let columns: Vec<_> = (0..width).map(|x| axis(x, width, sw)).collect();
    for (y, row) in destination
        .chunks_exact_mut(stride)
        .skip(top)
        .take(usize::try_from(height)?)
        .enumerate()
    {
        let (y0, y1, fy) = axis(u32::try_from(y)?, height, sh);
        let upper = source
            .get(y0 * source_stride..(y0 + 1) * source_stride)
            .context("Missing movie row")?;
        let lower = source
            .get(y1 * source_stride..(y1 + 1) * source_stride)
            .context("Missing movie row")?;
        let output = row
            .get_mut(left..left + usize::try_from(width)? * 4)
            .context("Movie fit exceeds its canvas")?;
        for (pixel, &(x0, x1, fx)) in output.as_chunks_mut::<4>().0.iter_mut().zip(&columns) {
            for (channel, byte) in pixel.iter_mut().enumerate() {
                let get = |row: &[u8], x: usize| {
                    row.get(x * 4 + channel)
                        .copied()
                        .map(u32::from)
                        .context("Missing movie pixel")
                };
                let a = get(upper, x0)? * (256 - fx) + get(upper, x1)? * fx;
                let b = get(lower, x0)? * (256 - fx) + get(lower, x1)? * fx;
                *byte = u8::try_from((a * (256 - fy) + b * fy + 32_768) / 65_536)?;
            }
        }
    }
    Ok([x, y, width, height])
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "Nonzero input/output dimensions and indices are at most 2560, so fixed-point center coordinates and interpolation weights fit u64"
)]
fn axis(at: u32, output: u32, input: u32) -> (usize, usize, u32) {
    let position =
        ((2 * u64::from(at) + 1) * u64::from(input) * 128 / u64::from(output)).saturating_sub(128);
    let first = (position / 256).min(u64::from(input - 1));
    let next = (first + 1).min(u64::from(input - 1));
    (
        usize::try_from(first).unwrap_or_default(),
        usize::try_from(next).unwrap_or_default(),
        u32::try_from(position % 256).unwrap_or_default(),
    )
}
