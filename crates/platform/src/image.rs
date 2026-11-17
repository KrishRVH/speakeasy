use std::fmt;

pub struct BgraImage {
    size: [u32; 2],
    pixels: Vec<u8>,
}

impl BgraImage {
    #[must_use]
    pub fn new(size: [u32; 2], pixels: Vec<u8>) -> Option<Self> {
        let [width, height] = size.map(u64::from);
        let length = width.checked_mul(height)?.checked_mul(4)?;
        (length > 0 && u64::try_from(pixels.len()) == Ok(length)).then_some(Self { size, pixels })
    }

    #[must_use]
    pub const fn size(&self) -> [u32; 2] {
        self.size
    }

    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    #[cfg(test)]
    pub(crate) fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    #[must_use]
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }
}

impl fmt::Debug for BgraImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BgraImage")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

pub(crate) fn encode_png(width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_fill_exactly_a_nonempty_size() {
        assert!(BgraImage::new([2, 1], vec![0; 8]).is_some());
        for (size, length) in [
            ([2, 1], 7),
            ([2, 1], 9),
            ([0, 1], 0),
            ([1, 0], 0),
            ([u32::MAX, u32::MAX], 0),
        ] {
            assert!(
                BgraImage::new(size, vec![0; length]).is_none(),
                "{size:?} accepted {length} bytes"
            );
        }
    }
}
