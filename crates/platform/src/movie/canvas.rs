use anyhow::{Context as _, ensure};
use speakeasy_show::MAX_FRAME_EDGE;

use crate::{Frame, Part};

const _: () = assert!(MAX_FRAME_EDGE <= 2_560);

pub(crate) struct Canvas {
    size: [u32; 2],
    pixels: Vec<u8>,
    content: [u32; 4],
    part: Part,
}

impl Canvas {
    pub(crate) fn new(size: [u32; 2]) -> anyhow::Result<Self> {
        ensure!(within_limits(size), "Invalid movie canvas");
        let size = size.map(|edge| edge.saturating_add(1) & !1);
        let bytes = length(size)?;
        Ok(Self {
            size,
            pixels: vec![0; bytes],
            content: [0, 0, size[0], size[1]],
            part: Part::WHOLE,
        })
    }

    pub(crate) const fn size(&self) -> [u32; 2] {
        self.size
    }

    pub(crate) fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub(crate) const fn content(&self) -> [u32; 4] {
        self.content
    }

    pub(crate) const fn part(&self) -> Part {
        self.part
    }

    pub(crate) fn paint(&mut self, frame: &Frame) -> anyhow::Result<()> {
        let source = frame.image.size();
        ensure!(within_limits(source), "Invalid movie frame");
        if source == self.size {
            self.pixels.copy_from_slice(frame.image.pixels());
            self.content = [0, 0, source[0], source[1]];
        } else {
            self.pixels.fill(0);
            self.content = fit(frame.image.pixels(), source, &mut self.pixels, self.size)?;
        }
        self.part = frame.part;
        Ok(())
    }
}

fn within_limits(size: [u32; 2]) -> bool {
    size.into_iter()
        .all(|edge| (1..=MAX_FRAME_EDGE).contains(&edge))
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
    reason = "Both nonzero image dimensions are validated at most 2560; byte offsets and interpolation totals fit usize on supported 64-bit hosts"
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
    let content = [(dw - width) / 2, (dh - height) / 2, width, height];
    let [left, top, width, height] = content.map(|value| value as usize);
    let [sw, sh, dw] = [sw, sh, dw].map(|edge| edge as usize);
    let source_row = |index: usize| {
        source
            .get(index * sw * 4..(index + 1) * sw * 4)
            .context("Missing movie row")
    };
    let columns: Vec<_> = (0..width).map(|column| axis(column, width, sw)).collect();
    for (row, line) in destination
        .chunks_exact_mut(dw * 4)
        .skip(top)
        .take(height)
        .enumerate()
    {
        let (y0, y1, fy) = axis(row, height, sh);
        let (upper, lower) = (source_row(y0)?, source_row(y1)?);
        let output = line
            .get_mut(left * 4..(left + width) * 4)
            .context("Movie fit exceeds its canvas")?;
        for (pixel, &(x0, x1, fx)) in output.as_chunks_mut::<4>().0.iter_mut().zip(&columns) {
            for (channel, byte) in pixel.iter_mut().enumerate() {
                let get = |row: &[u8], x: usize| {
                    row.get(x * 4 + channel)
                        .copied()
                        .map(usize::from)
                        .context("Missing movie pixel")
                };
                let a = get(upper, x0)? * (256 - fx) + get(upper, x1)? * fx;
                let b = get(lower, x0)? * (256 - fx) + get(lower, x1)? * fx;
                *byte = u8::try_from((a * (256 - fy) + b * fy + 32_768) / 65_536)?;
            }
        }
    }
    Ok(content)
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "Nonzero input/output dimensions and indices are at most 2560, so fixed-point center coordinates and interpolation weights fit usize"
)]
fn axis(at: usize, output: usize, input: usize) -> (usize, usize, usize) {
    let position = ((2 * at + 1) * input * 128 / output).saturating_sub(128);
    let first = (position / 256).min(input - 1);
    (first, (first + 1).min(input - 1), position % 256)
}

#[cfg(test)]
mod tests;
