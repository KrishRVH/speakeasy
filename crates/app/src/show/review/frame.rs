//! Bounded PNG loading without GPUI's persistent asset cache or decoder error logging.

use std::path::Path;

use gpui::{Image, ImageFormat};

use crate::show::store::recall;

const FILE_LIMIT: u64 = 32 << 20;
const LONGEST_EDGE: u32 = 4_096;

pub(super) fn read(directory: &Path, name: &str) -> Result<Image, String> {
    let bytes = recall::read_file(directory, name, FILE_LIMIT)
        .map_err(|error| format!("Cannot open {name}: {error}"))?;
    check_header(&bytes).map_err(str::to_owned)?;
    Ok(Image::from_bytes(ImageFormat::Png, bytes))
}

fn check_header(bytes: &[u8]) -> Result<(), &'static str> {
    // PNG requires a 13-byte IHDR first. Bound its dimensions before the decoder can allocate
    // pixels; the decoder still verifies the complete file, including the header's CRC.
    if bytes.get(..16) != Some(&b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"[..]) || bytes.len() < 33 {
        return Err("This frame has an invalid or truncated PNG header.");
    }
    let dimension = |range| {
        bytes
            .get(range)
            .and_then(|bytes: &[u8]| bytes.try_into().ok())
            .map(u32::from_be_bytes)
    };
    match (dimension(16..20), dimension(20..24)) {
        (Some(width), Some(height))
            if (1..=LONGEST_EDGE).contains(&width) && (1..=LONGEST_EDGE).contains(&height) =>
        {
            Ok(())
        },
        _ => Err("This frame is too large to display (maximum 4096 pixels per side)."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_and_oversized_png_headers_fail_before_decoding() {
        let mut header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        header.extend_from_slice(&1_u32.to_be_bytes());
        header.extend_from_slice(&1_u32.to_be_bytes());
        header.resize(33, 0);
        assert!(check_header(&header).is_ok());
        for length in 0..33 {
            assert!(check_header(&header[..length]).is_err());
        }
        header[16..20].copy_from_slice(&4_097_u32.to_be_bytes());
        assert!(check_header(&header).is_err());
        assert!(
            check_header(b"PRIVATE_SCREEN_CONTENT")
                .unwrap_err()
                .contains("PNG header")
        );
    }
}
