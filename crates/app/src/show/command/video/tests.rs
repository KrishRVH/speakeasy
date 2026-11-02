use super::{super::Command, *};

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
