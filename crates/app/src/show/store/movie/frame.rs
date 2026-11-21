use std::time::Duration;

use anyhow::Context as _;
use serde::Serialize;
use speakeasy_platform::BgraImage;
use speakeasy_show::{
    AnnotationReport, Annotations, Image, Moment, PixelRect, Rect, Typeface, VideoViewport,
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
    pub image: BgraImage,
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
        let [width, height] = self.image.size();
        let mut image = Image::from_bgra(width, height, self.image.into_pixels())?;
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
    if cfg!(target_os = "macos") {
        at
    } else {
        at.saturating_sub(Duration::from_nanos(u64::from(at.subsec_nanos() % 100)))
    }
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

    pub(crate) fn sample(
        self,
        at: Duration,
        cancelled: impl Fn() -> bool,
    ) -> anyhow::Result<Option<Sample>> {
        let Self { file, movie } = self;
        let retained = movie.prefix.duration();
        anyhow::ensure!(
            at <= retained,
            "Requested time is outside the retained video coverage"
        );
        let through = Moment::from_millis(
            movie
                .metadata
                .coverage
                .as_ref()
                .context("Movie has no published coverage")?
                .annotations_through_ms,
        );
        let Some(frame) = speakeasy_platform::read_movie_frame(file, &movie.prefix, at, cancelled)?
        else {
            return Ok(None);
        };
        anyhow::ensure!(
            frame.at < retained,
            "Decoded video sample exceeds its retained coverage"
        );
        Ok(Some(Sample {
            at: Duration::from_millis(movie.metadata.origin_ms).saturating_add(frame.at),
            image: frame.image,
            viewport: geometry::viewport(&movie, frame.at),
            through,
        }))
    }
}
