//! The crate's failure type.

use std::fmt;

/// A failure to build or encode an image or manifest, to choose keyframes without frames, or to
/// load the badge typeface.
#[derive(Debug)]
pub struct Error(Kind);

#[derive(Debug)]
enum Kind {
    Dimensions { width: u32, height: u32 },
    Frames { keyframes: usize, frames: usize },
    Unsampled,
    Manifest(serde_json::Error),
    Png(png::EncodingError),
    Typeface,
}

impl Error {
    pub(crate) const fn dimensions(width: u32, height: u32) -> Self {
        Self(Kind::Dimensions { width, height })
    }

    pub(crate) const fn frames(keyframes: usize, frames: usize) -> Self {
        Self(Kind::Frames { keyframes, frames })
    }

    pub(crate) const fn unsampled() -> Self {
        Self(Kind::Unsampled)
    }

    pub(crate) const fn manifest(error: serde_json::Error) -> Self {
        Self(Kind::Manifest(error))
    }

    pub(crate) const fn png(error: png::EncodingError) -> Self {
        Self(Kind::Png(error))
    }

    pub(crate) const fn typeface() -> Self {
        Self(Kind::Typeface)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Kind::Dimensions { width, height } => {
                write!(
                    f,
                    "a {width}×{height} image is empty, too large, or mismatched"
                )
            },
            Kind::Frames { keyframes, frames } => {
                write!(
                    f,
                    "{keyframes} keyframes were chosen but {frames} frames were supplied"
                )
            },
            Kind::Unsampled => f.write_str("no frames were sampled to choose keyframes from"),
            Kind::Manifest(_) => f.write_str("the manifest could not be written"),
            Kind::Png(_) => f.write_str("the PNG could not be encoded"),
            Kind::Typeface => f.write_str("the badge typeface could not be read"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.0 {
            Kind::Manifest(error) => Some(error),
            Kind::Png(error) => Some(error),
            Kind::Dimensions { .. } | Kind::Frames { .. } | Kind::Unsampled | Kind::Typeface => {
                None
            },
        }
    }
}
