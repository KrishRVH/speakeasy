use super::{super::Command, *};

#[test]
fn time_focus_can_extend_past_the_manifest_into_retained_video() -> anyhow::Result<()> {
    use speakeasy_show::{Manifest, Moment};

    use crate::show::store::movie;

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
    let show = recall::Entry::open(directory.path())?.show?;
    let coverage = show
        .movie
        .context("Missing recovered video")?
        .metadata
        .coverage
        .context("Missing coverage")?;
    let clip = widened(
        directory.path(),
        &Query::At(Moment::from_millis(1000)),
        Duration::from_millis(200),
    )?;
    assert!(clip.start() > show.manifest.recording().duration);
    assert_eq!(clip.start().as_millis(), 800);
    assert_eq!(clip.end().as_millis(), 1200);
    assert!(clip.end().as_millis() < coverage.end_ms);
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
