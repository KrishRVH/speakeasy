use std::time::Duration;

use super::*;

mod fixture {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../show/tests/fixtures/movie.rs"
    ));
}

pub(in crate::show) fn checkpoint(directory: &Path, truncated: bool) -> anyhow::Result<()> {
    let started = Instant::now();
    let (_metadata, mut geometry) = Metadata::begin(
        directory,
        started,
        started.checked_add(Duration::from_millis(250)).unwrap(),
        [4, 2],
    )?;
    geometry.record(0, [0, 0, 4, 2], [0.0, 0.0, 1.0, 1.0])?;
    let mut fixture = fixture::movie();
    assert!(fixture.ends[0] > fixture.initialized);
    if truncated {
        fixture.bytes.truncate(fixture.ends[0].saturating_add(12));
    }
    fs::write(directory.join(FILE), fixture.bytes)?;
    Ok(())
}

#[test]
fn publication_keeps_only_complete_video_and_separate_annotation_coverage() -> anyhow::Result<()> {
    let source = tempfile::tempdir()?;
    let destination = tempfile::tempdir()?;
    checkpoint(source.path(), true)?;
    let original = fs::read(source.path().join(FILE))?;
    let inspected = open(source.path())?.unwrap();
    assert!(inspected.movie.prefix.truncated);
    retain(destination.path(), inspected, Moment::from_millis(500))?;
    let kept = saved(destination.path(), Moment::from_millis(500))?.unwrap();
    let coverage = kept.coverage.unwrap();
    assert_eq!(kept.origin_ms, 250);
    assert_eq!(coverage.end_ms, 1_250);
    assert_eq!(coverage.annotations_through_ms, 500);
    assert_eq!(coverage.frames, 1);
    assert!(coverage.partial);
    assert_eq!(fs::read(source.path().join(FILE))?, original);
    assert!(fs::metadata(destination.path().join(FILE))?.len() < u64::try_from(original.len())?);
    Ok(())
}

#[test]
fn lost_video_or_edited_coverage_never_passes_inspection_as_an_ordinary_show() -> anyhow::Result<()>
{
    let source = tempfile::tempdir()?;
    let destination = tempfile::tempdir()?;
    checkpoint(source.path(), false)?;
    let mut metadata = retain(
        destination.path(),
        open(source.path())?.unwrap(),
        Moment::from_millis(500),
    )?;
    metadata.coverage.as_mut().unwrap().frames += 1;
    metadata.replace(destination.path())?;
    assert!(saved(destination.path(), Moment::from_millis(500)).is_err());
    fs::remove_file(destination.path().join(FILE))?;
    assert!(inspect(destination.path()).is_err());
    Ok(())
}

#[test]
fn a_refused_copy_preserves_its_source_and_a_replaced_path_cannot_substitute_video()
-> anyhow::Result<()> {
    let source = tempfile::tempdir()?;
    let destination = tempfile::tempdir()?;
    checkpoint(source.path(), false)?;
    let original = fs::read(source.path().join(FILE))?;
    fs::create_dir(destination.path().join(FILE))?;
    assert!(
        retain(
            destination.path(),
            open(source.path())?.unwrap(),
            Moment::START
        )
        .is_err()
    );
    assert_eq!(fs::read(source.path().join(FILE))?, original);
    fs::remove_dir(destination.path().join(FILE))?;
    let inspected = open(source.path())?.unwrap();
    fs::rename(source.path().join(FILE), source.path().join("original.mp4"))?;
    fs::write(source.path().join(FILE), vec![0; original.len()])?;
    retain(destination.path(), inspected, Moment::START)?;
    assert_eq!(fs::read(destination.path().join(FILE))?, original);
    assert!(saved(destination.path(), Moment::START)?.is_some());
    Ok(())
}

#[test]
fn finalized_fragments_cannot_claim_time_beyond_the_native_capture_receipt() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    checkpoint(directory.path(), false)?;
    let mut metadata = read_metadata(directory.path())?;
    metadata.finished = Some(Finished {
        last_ms: 500,
        through_ms: 500,
        submitted: 1,
        finalized: true,
    });
    metadata.replace(directory.path())?;
    assert!(inspect(directory.path()).is_err());
    Ok(())
}

#[test]
fn malformed_complete_first_fragment_stays_in_its_checkpoint_for_repair() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    checkpoint(directory.path(), false)?;
    let path = directory.path().join(FILE);
    let mut bytes = fs::read(&path)?;
    let at = bytes.windows(4).position(|bytes| bytes == b"trun").unwrap();
    bytes[at + 12..at + 16].copy_from_slice(&0_u32.to_be_bytes());
    fs::write(&path, &bytes)?;
    let error = super::checkpoint(directory.path()).err().unwrap();
    assert!(
        error
            .to_string()
            .contains("move recording.mp4, recording.json, and any recording.geometry")
    );
    assert_eq!(fs::read(path)?, bytes);
    Ok(())
}

#[cfg(target_os = "windows")]
pub(in crate::show) fn native_geometry_fixture(show: &Path) -> anyhow::Result<()> {
    native_fixture(show)?;
    fs::remove_file(show.join(FILE))?;
    fs::remove_file(show.join(HEADER))?;
    let fixtures = std::path::PathBuf::from(
        std::env::var_os("SPEAKEASY_MOVIE_GEOMETRY_DIRECTORY")
            .context("Choose the synthetic geometry fixture directory")?,
    );
    let checkpoint = tempfile::tempdir()?;
    let started = Instant::now();
    let (_metadata, mut geometry) = Metadata::begin(
        checkpoint.path(),
        started,
        started.checked_add(Duration::from_millis(250)).unwrap(),
        [320, 240],
    )?;
    for line in fs::read_to_string(fixtures.join("geometry.csv"))?.lines() {
        let values: Vec<_> = line.split(',').collect();
        anyhow::ensure!(values.len() == 9, "Synthetic geometry fixture is invalid");
        geometry.record(
            values[0].parse()?,
            [
                values[1].parse()?,
                values[2].parse()?,
                values[3].parse()?,
                values[4].parse()?,
            ],
            [
                values[5].parse()?,
                values[6].parse()?,
                values[7].parse()?,
                values[8].parse()?,
            ],
        )?;
    }
    drop(geometry);
    fs::copy(fixtures.join("geometry.mp4"), checkpoint.path().join(FILE))?;
    retain(
        show,
        open(checkpoint.path())?.unwrap(),
        Moment::from_millis(1000),
    )?;
    Ok(())
}

#[cfg(target_os = "windows")]
pub(in crate::show) fn native_fixture(show: &Path) -> anyhow::Result<()> {
    let fixtures = std::path::PathBuf::from(
        std::env::var_os("SPEAKEASY_MOVIE_READ_DIRECTORY")
            .context("Choose the synthetic movie prefix directory")?,
    );
    fs::copy(fixtures.join("moving.mp4"), show.join(FILE))?;
    let original = fs::read(show.join(FILE))?;
    fs::write(
        show.join(HEADER),
        serde_json::to_vec(&serde_json::json!({
            "version":1,"originMs":250,"size":[1280,720],"finished":null,
            "coverage":{"endMs":4250,"frames":120,"bytes":original.len(),"annotationsThroughMs":1000,"partial":true}
        }))?,
    )?;
    fs::write(
        show.join("show.json"),
        serde_json::to_vec(&serde_json::json!({
            "version":2,"duration":1000,"surface":{"width":1280,"height":720},
            "strokes":[],"pins":[],"actions":[],
            "marks":[{"number":1,"shape":"pin","point":[0.5,0.5],"bounds":[0.5,0.5,0.5,0.5],"span":[750,750],"strokes":[0,0]}],
            "narration":{"words":[{"span":[500,700],"text":"PRIVATE_PHRASE"}]},
            "keyframes":[{"index":1,"at":500,"reason":"middle","event":"PRIVATE_EVENT","marks":[0,0]}]
        }))?,
    )?;
    fs::write(show.join("summary.md"), "Synthetic video review")?;
    fs::create_dir(show.join("frames"))?;
    fs::write(
        show.join("frames/frame-01.png"),
        speakeasy_show::Image::from_bgra(2, 2, [50, 50, 50, 255].repeat(4))?.encode_png()?,
    )?;
    Ok(())
}
