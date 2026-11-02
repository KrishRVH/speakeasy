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

pub(super) fn frame(
    directory: &Path,
    focus: &Query,
    destination: PathBuf,
    crop: Option<usize>,
    annotated: bool,
) -> anyhow::Result<Report> {
    let manifest = recall::Entry::open(directory)?.show?.manifest;
    let reading = movie::Reading::open(directory, manifest.recording().duration)?;
    let metadata = &reading.movie.metadata;
    let coverage = metadata
        .coverage
        .as_ref()
        .context("Movie has no published coverage")?;
    let found = match focus {
        Query::At(at) => {
            anyhow::ensure!(
                (metadata.origin_ms..=coverage.end_ms).contains(&at.as_millis()),
                "Requested time is outside the retained video coverage"
            );
            let span = Span::at(*at);
            query::Found {
                kind: query::Kind::Time,
                number: None,
                occurrence: None,
                matches: vec![span],
                start_ms: Some(at.as_millis()),
                end_ms: Some(at.as_millis()),
                selected: Some(span),
            }
        },
        _ => focus.resolve(&manifest)?,
    };
    let at = found
        .selected
        .context("The phrase has several matches; choose --match N")?
        .start();
    let span = Span::new(
        Moment::from_millis(metadata.origin_ms),
        Moment::from_millis(coverage.end_ms),
    );
    let annotations = Moment::from_millis(coverage.annotations_through_ms);
    let crop = if let Some(mark) = crop_mark(&manifest, crop)? {
        let part = manifest
            .recording()
            .mark_part(mark)
            .context("This mark has mixed or unknown geometry; export without --crop")?;
        Some(movie::Crop {
            bounds: mark.bounds,
            part,
        })
    } else {
        None
    };
    let scene = annotated
        .then(|| Annotations::new(manifest.recording(), manifest.marks()))
        .transpose()?;
    let served = reading
        .frame(at, crop, scene.as_ref(), || false)?
        .context("Video frame export was cancelled")?;
    anyhow::ensure!(
        !annotated || served.annotations.painted(),
        "Annotations are unavailable at this video sample; export without --annotated"
    );
    let sampled_ms =
        u64::try_from(served.at.as_millis()).context("Video sample time is too large")?;
    let mut report = Report::new(&manifest, found);
    report.video = Some(Coverage {
        span,
        annotations,
        rendering: served.annotations,
    });
    report.requested_ms = Some(at.as_millis());
    report.sampled_ms = Some(sampled_ms);
    report.cells.push(locate::Cell {
        sampled_ms,
        crop_px: served.crop_px,
    });
    report.path = Some(export(destination, &served.image)?);
    Ok(report)
}

#[cfg(all(test, target_os = "windows"))]
mod native_tests;
