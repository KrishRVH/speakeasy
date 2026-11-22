use speakeasy_show::{Manifest, Moment};

use super::{super::Command, *};
use crate::show::store::{movie, recall};

fn recovered() -> anyhow::Result<(tempfile::TempDir, u64)> {
    let directory = tempfile::tempdir()?;
    std::fs::write(
        directory.path().join(Manifest::FILE),
        br#"{
        "version":3,"duration":500,"surface":{"width":4,"height":2},
        "strokes":[],"pins":[],"actions":[],"narration":{"words":[]},"marks":[],
        "keyframes":[{"index":1,"at":250,"reason":"middle","event":"fixture","marks":[0,0]}]
    }"#,
    )?;
    movie::tests::recovered_movie(directory.path(), Moment::from_millis(500))?;
    let end = recall::Entry::open(directory.path(), None)?
        .show?
        .movie
        .context("Missing recovered video")?
        .metadata
        .coverage
        .context("Missing coverage")?
        .end_ms;
    Ok((directory, end))
}

#[test]
fn a_time_past_the_retained_video_reaches_the_exporter_that_knows_the_voice() -> anyhow::Result<()>
{
    let (directory, end) = recovered()?;
    let clip = widened(
        &recall::Entry::open(directory.path(), None)?.show?.manifest,
        &Query::At(Moment::from_millis(end + 2000)),
        Duration::from_millis(500),
    )?;
    assert_eq!(
        (clip.start().as_millis(), clip.end().as_millis()),
        (end + 1500, end + 2500)
    );
    Ok(())
}

fn read(args: &[&str]) -> anyhow::Result<Command> {
    parse(args.iter().map(OsString::from))
}

#[test]
fn video_requires_explicit_output_and_unambiguous_focus() -> anyhow::Result<()> {
    for args in [
        vec![],
        vec!["show"],
        vec!["show", "--out", "clip.mp4", "--match", "1"],
        vec!["show", "--out", "clip.mp4", "--around", "1000"],
        vec!["show", "--out", "clip.mp4", "--at", "1", "--mark", "1"],
        vec![
            "show", "--out", "clip.mp4", "--phrase", "secret", "--around", "10001",
        ],
    ] {
        assert!(read(&args).is_err());
    }
    let Command::Video(request) = read(&[
        "show",
        "--out",
        "clip.mp4",
        "--phrase",
        "private phrase",
        "--match",
        "2",
        "--around",
        "0",
        "--captions",
        "--json",
    ])?
    else {
        anyhow::bail!("Expected export");
    };
    assert!(matches!(
        request.focus,
        Some(Query::Match { occurrence: 2, .. })
    ));
    assert!(request.captions && request.json);
    assert_eq!(request.around, Duration::ZERO);
    Ok(())
}
