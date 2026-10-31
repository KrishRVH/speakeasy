//! Explicit video exports keep their decoded timestamps and annotation coverage distinct.

use std::path::Path;

use speakeasy_show::{Annotations, Span};

use super::*;
use crate::show::store::movie;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Coverage {
    #[serde(rename = "rangeMs")]
    pub span: Span,
    #[serde(rename = "annotationsThroughMs")]
    pub annotations: Moment,
    pub rendering: movie::AnnotationState,
}

pub(super) fn write(
    request: Request,
    directory: &Path,
    out: &mut impl io::Write,
) -> anyhow::Result<()> {
    let manifest = recall::Entry::open(directory)?.show?.manifest;
    let reading = movie::Reading::open(directory, manifest.recording().duration)?;
    let metadata = &reading.movie.metadata;
    let coverage = metadata
        .coverage
        .as_ref()
        .context("Movie has no published coverage")?;
    let query = request
        .focus
        .as_ref()
        .context("Choose a video frame by time, mark, or phrase")?;
    let found = match query {
        Query::At(at) => {
            anyhow::ensure!(
                (metadata.origin_ms..=coverage.end_ms).contains(&at.as_millis()),
                "Requested time is outside the retained video coverage"
            );
            let span = Span::at(*at);
            query::Found {
                kind: "time",
                number: None,
                occurrence: None,
                matches: vec![span],
                start_ms: Some(at.as_millis()),
                end_ms: Some(at.as_millis()),
                selected: Some(span),
            }
        },
        _ => query.resolve(&manifest)?,
    };
    let at = found
        .selected
        .context("The phrase has several matches; choose --match N")?
        .start();
    let Output::Frame { destination, crop } = request.output else {
        anyhow::bail!("Video export needs show frame");
    };
    let mut report = Report::new(&manifest, &found);
    report.video = Some(Coverage {
        span: Span::new(
            Moment::from_millis(metadata.origin_ms),
            Moment::from_millis(coverage.end_ms),
        ),
        annotations: Moment::from_millis(coverage.annotations_through_ms),
        rendering: movie::AnnotationState::Off,
    });
    let crop = if let Some(bounds) = crop_bounds(&manifest, query, crop)? {
        let Query::Mark(number) = query else {
            anyhow::bail!("--crop requires --mark");
        };
        let mark = manifest
            .marks()
            .iter()
            .find(|mark| mark.number == *number)
            .context("The selected mark is unavailable")?;
        let part = manifest
            .recording()
            .mark_part(mark)
            .context("This mark has mixed or unknown geometry; export without --crop")?;
        Some(movie::Crop { bounds, part })
    } else {
        None
    };
    let scene = request
        .annotated
        .then(|| Annotations::new(manifest.recording(), manifest.marks()))
        .transpose()?;
    let served = reading
        .frame(at, crop, scene.as_ref(), || false)?
        .context("Video frame export was cancelled")?;
    anyhow::ensure!(
        !request.annotated || served.annotations.painted(),
        "Annotations are unavailable at this video sample; export without --annotated"
    );
    if let Some(video) = &mut report.video {
        video.rendering = served.annotations;
    }
    let sampled_ms =
        u64::try_from(served.at.as_millis()).context("Video sample time is too large")?;
    report.requested_ms = Some(at.as_millis());
    report.sampled_ms = Some(sampled_ms);
    report.cells.push(locate::Cell {
        sampled_ms,
        crop_px: served.crop_px,
    });
    report.path = Some(export(destination, &served.image)?);
    report.write(request.json, out)
}

#[cfg(all(test, target_os = "windows"))]
mod native_tests;
