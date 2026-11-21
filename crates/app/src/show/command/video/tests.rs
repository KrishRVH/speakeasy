use speakeasy_show::{Manifest, Moment};

use super::{super::Command, *};
use crate::show::store::movie;

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
    let end = recall::Entry::open(directory.path())?
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
fn time_focus_can_extend_past_the_manifest_into_retained_video() -> anyhow::Result<()> {
    let (directory, end) = recovered()?;
    let clip = widened(
        directory.path(),
        &Query::At(Moment::from_millis(1000)),
        Duration::from_millis(200),
    )?;
    assert_eq!(clip.start().as_millis(), 800);
    assert_eq!(clip.end().as_millis(), 1200);
    assert!(clip.end().as_millis() < end);
    Ok(())
}

#[test]
fn a_time_whose_clip_starts_after_the_retained_video_names_the_end() -> anyhow::Result<()> {
    let (directory, end) = recovered()?;
    let around = Duration::from_millis(200);
    let tail = widened(
        directory.path(),
        &Query::At(Moment::from_millis(end)),
        around,
    )?;
    assert_eq!(tail.start().as_millis(), end - 200);
    let past = Moment::from_millis(end + 200);
    let error = widened(directory.path(), &Query::At(past), around)
        .err()
        .context("A clip after the retained video was accepted")?;
    assert_eq!(
        error.to_string(),
        format!(
            "The requested time is past this Show's end at {}; choose an earlier --at time",
            Moment::from_millis(end)
        )
    );
    Ok(())
}

fn read(args: &[&str]) -> anyhow::Result<Command> {
    parse(args.iter().map(OsString::from))
}

#[test]
fn video_requires_explicit_output_and_unambiguous_focus_without_printing_the_query()
-> anyhow::Result<()> {
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
    let command = read(&[
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
    ])?;
    assert!(!format!("{command:?}").contains("private phrase"));
    let Command::Video(request) = command else {
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
