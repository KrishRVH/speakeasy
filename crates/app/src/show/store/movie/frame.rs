//! One decoded sample supplies exact session time, geometry, annotations, and optional crop.

use std::time::Duration;

use anyhow::Context as _;
use serde::Serialize;
use speakeasy_show::{
    AnnotationReport, Annotations, Image, Moment, Rect, Typeface, VideoViewport, locate::PixelRect,
};

use super::{Reading, geometry};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub(crate) enum AnnotationState {
    #[default]
    Off,
    Painted {
        drawn: usize,
        omitted: usize,
    },
    Unavailable,
    BeyondCoverage {
        through: Moment,
    },
}

impl AnnotationState {
    pub(crate) fn painted(self) -> bool {
        matches!(self, Self::Painted { .. })
    }

    pub(crate) fn caption(self) -> String {
        match self {
            Self::Off => "Original video · annotations off".into(),
            Self::Painted { omitted: 0, .. } => "Annotations on".into(),
            Self::Painted { omitted, .. } => {
                format!("Annotations on · {omitted} could not be placed on this frame")
            },
            Self::Unavailable => {
                "Annotations unavailable for this frame · showing original video".into()
            },
            Self::BeyondCoverage { through } => {
                format!("No annotation record after {through} · showing original video")
            },
        }
    }
}

pub(crate) struct Frame {
    pub at: Duration,
    pub image: Image,
    pub crop_px: Option<PixelRect>,
    pub annotations: AnnotationState,
}

pub(crate) struct Crop {
    pub bounds: Rect,
    pub part: Rect,
}

pub(crate) struct Sample {
    pub at: Duration,
    pub size: [u32; 2],
    pub bgra: Vec<u8>,
    viewport: Option<VideoViewport>,
    through: Moment,
}

impl Sample {
    pub(crate) fn render(
        self,
        annotations: Option<&Annotations>,
        crop: Option<Crop>,
        cancelled: &dyn Fn() -> bool,
    ) -> anyhow::Result<Option<Frame>> {
        if cancelled() {
            return Ok(None);
        }
        let mut image = Image::from_bgra(self.size[0], self.size[1], self.bgra)?;
        let status = match annotations {
            None => AnnotationState::Off,
            Some(_) if self.at > self.through.since(Moment::START) => {
                AnnotationState::BeyondCoverage {
                    through: self.through,
                }
            },
            Some(scene) => match self.viewport {
                None => AnnotationState::Unavailable,
                Some(viewport) => {
                    let Some(AnnotationReport { drawn, omitted }) = scene.paint(
                        &mut image,
                        self.at,
                        viewport,
                        &Typeface::parse(crate::BRAND_FONT_FILE)?,
                        cancelled,
                    )?
                    else {
                        return Ok(None);
                    };
                    AnnotationState::Painted { drawn, omitted }
                },
            },
        };
        let (image, crop_px) = geometry::crop(image, self.viewport, crop)?;
        Ok(Some(Frame {
            at: self.at,
            image,
            crop_px,
            annotations: status,
        }))
    }
}

pub(crate) fn decoder_time(at: Duration) -> Duration {
    // Windows decoding reports sample times in 100 ns units.
    at.saturating_sub(Duration::from_nanos(u64::from(at.subsec_nanos() % 100)))
}

impl Reading {
    pub(crate) fn frame(
        self,
        at: Moment,
        crop: Option<Crop>,
        annotations: Option<&Annotations>,
        cancelled: impl Fn() -> bool,
    ) -> anyhow::Result<Option<Frame>> {
        let metadata = &self.movie.metadata;
        let coverage = metadata
            .coverage
            .as_ref()
            .context("Movie has no published coverage")?;
        anyhow::ensure!(
            (metadata.origin_ms..=coverage.end_ms).contains(&at.as_millis()),
            "Requested time is outside the retained video coverage"
        );
        let requested = Duration::from_millis(at.as_millis().saturating_sub(metadata.origin_ms));
        let Some(sample) = self.sample(requested, &cancelled)? else {
            return Ok(None);
        };
        sample.render(annotations, crop, &cancelled)
    }

    /// The actual session timestamp and geometry accompany one native decode through cleanup.
    pub(crate) fn sample(
        self,
        at: Duration,
        cancelled: impl Fn() -> bool,
    ) -> anyhow::Result<Option<Sample>> {
        anyhow::ensure!(
            at <= self.movie.prefix.duration,
            "Requested time is outside the retained video coverage"
        );
        let through = Moment::from_millis(
            self.movie
                .metadata
                .coverage
                .as_ref()
                .context("Movie has no published coverage")?
                .annotations_through_ms,
        );
        let Some(frame) = speakeasy_platform::read_movie_frame(
            self.file,
            self.movie.prefix.bytes,
            self.movie.metadata.size,
            at,
            cancelled,
        )?
        else {
            return Ok(None);
        };
        anyhow::ensure!(
            frame.at < self.movie.prefix.duration,
            "Decoded video sample exceeds its retained coverage"
        );
        let viewport = geometry::viewport(&self.movie, frame.at);
        Ok(Some(Sample {
            at: Duration::from_millis(self.movie.metadata.origin_ms).saturating_add(frame.at),
            size: frame.size,
            bgra: frame.bgra,
            viewport,
            through,
        }))
    }
}
