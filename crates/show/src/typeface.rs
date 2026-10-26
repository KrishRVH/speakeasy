//! Text drawn as vector paths from a TrueType face, for badges and the demo.

use tiny_skia::{Path, PathBuilder};
use ttf_parser::{Face, OutlineBuilder};

use crate::{Error, Point};

/// A parsed TrueType or OpenType face that draws text without a system font service.
///
/// The face borrows a font embedded in the executable. Glyphs advance without kerning, which the
/// short labels it draws do not need.
#[derive(Clone, Debug)]
pub struct Typeface {
    face: Face<'static>,
}

impl Typeface {
    /// Reads the first face in `data`.
    ///
    /// # Errors
    ///
    /// Fails when `data` is not a TrueType or OpenType font.
    pub fn parse(data: &'static [u8]) -> Result<Self, Error> {
        Face::parse(data, 0)
            .map(|face| Self { face })
            .map_err(|_malformed| Error::typeface())
    }

    fn scale(&self, size: f32) -> f32 {
        size / f32::from(self.face.units_per_em())
    }

    /// How far `text` advances at `size` pixels per em.
    pub(crate) fn width(&self, text: &str, size: f32) -> f32 {
        let units: f32 = text
            .chars()
            .filter_map(|character| self.face.glyph_index(character))
            .filter_map(|glyph| self.face.glyph_hor_advance(glyph))
            .map(f32::from)
            .sum();
        units * self.scale(size)
    }

    /// The height of capital letters and figures at `size`, for centering them optically.
    pub(crate) fn cap_height(&self, size: f32) -> f32 {
        let units = self
            .face
            .capital_height()
            .unwrap_or_else(|| self.face.ascender());
        f32::from(units) * self.scale(size)
    }

    /// The outlines of `text` at `size`, its baseline starting at `origin`.
    pub(crate) fn path(&self, text: &str, size: f32, origin: Point) -> Option<Path> {
        let mut pen = Pen {
            builder: PathBuilder::new(),
            scale: self.scale(size),
            origin,
        };
        for character in text.chars() {
            let Some(glyph) = self.face.glyph_index(character) else {
                continue;
            };
            // A glyph without an outline, such as a space, still advances the pen.
            self.face.outline_glyph(glyph, &mut pen);
            let advance = self.face.glyph_hor_advance(glyph).map_or(0.0, f32::from);
            pen.origin.x += advance * pen.scale;
        }
        pen.builder.finish()
    }
}

/// Converts font units, which rise upward from the baseline, to pixels that fall downward.
struct Pen {
    builder: PathBuilder,
    scale: f32,
    origin: Point,
}

impl Pen {
    fn map(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.origin.x + x * self.scale,
            self.origin.y - y * self.scale,
        )
    }
}

impl OutlineBuilder for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.map(x, y);
        self.builder.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.map(x, y);
        self.builder.line_to(x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let ((x1, y1), (x, y)) = (self.map(x1, y1), self.map(x, y));
        self.builder.quad_to(x1, y1, x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let ((x1, y1), (x2, y2), (x, y)) = (self.map(x1, y1), self.map(x2, y2), self.map(x, y));
        self.builder.cubic_to(x1, y1, x2, y2, x, y);
    }

    fn close(&mut self) {
        self.builder.close();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Speakeasy's wordmark face, which also draws badges.
    pub(crate) fn typeface() -> Typeface {
        Typeface::parse(include_bytes!("../../app/assets/JosefinSans-SemiBold.ttf")).unwrap()
    }

    #[test]
    fn text_measures_and_draws_below_its_cap_height() {
        let face = typeface();
        assert!(face.width("#12", 20.0) > face.width("#1", 20.0));
        let bounds = face
            .path("0:42", 20.0, Point::new(10.0, 30.0))
            .unwrap()
            .bounds();
        assert!(
            bounds.left() >= 10.0 && bounds.bottom() <= 30.5,
            "{bounds:?}"
        );
        assert!(
            (30.0 - bounds.top() - face.cap_height(20.0)).abs() < 1.5,
            "{bounds:?}"
        );
        assert!(Typeface::parse(b"not a font").is_err());
    }
}
