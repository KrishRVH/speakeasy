//! Bounded visual clipboard representations and the final authorization check after native access.
//! Clipboard contents stay out of Debug output.

use anyhow::{Context as _, ensure};

const TEXT_LIMIT: usize = 1 << 20;
const EDGE_LIMIT: u32 = 4096;

/// Text and, for a visual moment, matching lossless image representations. Prepare this off the
/// UI thread; it encodes at most 4096 × 4096 pixels and retains no native resources.
pub struct ClipboardContent {
    pub(crate) text: String,
    pub(crate) image: Option<Image>,
}

pub(crate) struct Image {
    pub(crate) png: Vec<u8>,
    #[cfg(any(target_os = "windows", test))]
    pub(crate) dib: Vec<u8>,
}

impl std::fmt::Debug for ClipboardContent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClipboardContent")
            .field("text_bytes", &self.text.len())
            .field(
                "image_bytes",
                &self.image.as_ref().map(|image| image.png.len()),
            )
            .finish_non_exhaustive()
    }
}

impl ClipboardContent {
    /// Prepares plain text without changing the clipboard.
    ///
    /// # Errors
    /// Refuses text larger than 1 MiB or containing NUL, which native text formats truncate.
    pub fn text(text: String) -> anyhow::Result<Self> {
        ensure!(text.len() <= TEXT_LIMIT, "Clipboard text exceeds 1 MiB");
        ensure!(
            !text.contains('\0'),
            "Clipboard text contains a NUL character"
        );
        Ok(Self { text, image: None })
    }

    /// Prepares the displayed moment and its textual context. Pixels are straight-alpha BGRA8,
    /// top to bottom, without row padding; both image formats describe those exact pixels.
    ///
    /// # Errors
    /// Refuses invalid dimensions, mismatched pixels, invalid text, or PNG encoding failure.
    pub fn moment(
        text: String,
        width: u32,
        height: u32,
        mut bgra: Vec<u8>,
    ) -> anyhow::Result<Self> {
        let mut content = Self::text(text)?;
        ensure!(
            (1..=EDGE_LIMIT).contains(&width) && (1..=EDGE_LIMIT).contains(&height),
            "Clipboard images must be between 1 and 4096 pixels per side"
        );
        let bytes = usize::try_from(
            width
                .checked_mul(height)
                .and_then(|pixels| pixels.checked_mul(4))
                .context("Clipboard image size overflowed")?,
        )?;
        ensure!(
            bgra.len() == bytes,
            "Clipboard image dimensions do not match its pixels"
        );
        #[cfg(any(target_os = "windows", test))]
        let dib = dib(width, height, &bgra)?;
        for [blue, _, red, _] in bgra.as_chunks_mut::<4>().0 {
            std::mem::swap(blue, red);
        }
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder
                .write_header()
                .context("Cannot encode clipboard image")?;
            writer
                .write_image_data(&bgra)
                .context("Cannot encode clipboard image")?;
            writer.finish().context("Cannot finish clipboard image")?;
        }
        content.image = Some(Image {
            png,
            #[cfg(any(target_os = "windows", test))]
            dib,
        });
        Ok(content)
    }
}

/// Copies prepared text/image formats without pasting. A receiver chooses which formats it reads;
/// this does not promise that every application accepts both image and text.
///
/// Call off the UI thread. `authorized` runs after native clipboard preparation, immediately before
/// replacing its contents. False returns `Ok(false)` without a write. The caller owns and drains
/// this operation, and its authorization must reject cancellation and newer recordings.
///
/// # Errors
/// Fails when clipboard access or any representation fails. Privacy markers are installed before
/// content. Linux reports unsupported, matching its current Show clipboard behavior.
pub fn copy_content(
    content: &ClipboardContent,
    authorized: impl FnOnce() -> bool,
) -> anyhow::Result<bool> {
    crate::native::copy_content(content, authorized)
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
pub(crate) fn copy_when<L>(
    authorized: impl FnOnce() -> bool,
    acquire: impl FnOnce() -> anyhow::Result<L>,
    write: impl FnOnce(L) -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    let lease = acquire()?;
    if !authorized() {
        return Ok(false);
    }
    write(lease)?;
    Ok(true)
}

#[cfg(any(target_os = "windows", test))]
fn dib(width: u32, height: u32, bgra: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(bgra.len().saturating_add(124));
    // BITMAPV5HEADER uses little-endian fields, top-down rows, explicit BGRA masks and sRGB.
    out.extend(124_u32.to_le_bytes());
    out.extend(i32::try_from(width)?.to_le_bytes());
    out.extend(
        i32::try_from(height)?
            .checked_neg()
            .context("Clipboard image height overflowed")?
            .to_le_bytes(),
    );
    out.extend(1_u16.to_le_bytes());
    out.extend(32_u16.to_le_bytes());
    out.extend(3_u32.to_le_bytes()); // BI_BITFIELDS
    out.extend(u32::try_from(bgra.len())?.to_le_bytes());
    out.resize(40, 0);
    for mask in [
        0x00FF_0000_u32,
        0x0000_FF00,
        0x0000_00FF,
        0xFF00_0000,
        0x7352_4742,
    ] {
        out.extend(mask.to_le_bytes());
    }
    out.resize(108, 0);
    out.extend(4_u32.to_le_bytes()); // LCS_GM_IMAGES
    out.resize(124, 0);
    out.extend_from_slice(bgra);
    Ok(out)
}

#[cfg(test)]
mod tests;
