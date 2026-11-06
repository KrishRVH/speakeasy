use speakeasy_show::{Image, Journal, Moment, SourceError};

use super::*;

#[test]
fn retained_frames_need_the_saved_shows_timeline_without_losing_keyframes_or_edits()
-> anyhow::Result<()> {
    let root = Source::Demo.load()?;
    let (rows, _) = list(&root.directory)?;
    let path = &rows[0].directory;
    let duration = recall::Entry::open(path)?
        .show?
        .manifest
        .recording()
        .duration;
    let before = Detail::open(path, Mode::Keyframes)?;
    fs::write(path.join("summary.md"), "PUBLIC_EDIT")?;
    for (source_duration, accepted) in [(duration, true), (Moment::from_millis(1_000), false)] {
        let mut journal = Journal::new();
        journal.admit().unwrap().record(
            Moment::START,
            Image::from_bgra(2, 2, [40, 50, 60, 255].repeat(4))?,
        );
        let mut bytes = Vec::new();
        journal.write_source::<SourceError>(source_duration, |chunk| {
            bytes.extend_from_slice(chunk);
            Ok(())
        })?;
        fs::write(path.join("source.frames"), bytes)?;
        let source = Detail::open(path, Mode::Source)?;
        assert_eq!(source.source_available, accepted);
        assert_eq!(source.frames.len(), usize::from(accepted));
        let keyframes = Detail::open(path, Mode::Keyframes)?;
        assert_eq!(keyframes.frames.len(), before.frames.len());
        assert_eq!(keyframes.summary.unwrap(), "PUBLIC_EDIT");
        if !accepted {
            assert!(
                source
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("timeline"))
            );
        }
    }
    fs::write(path.join("show.json"), b"invalid manifest")?;
    let source = Detail::open(path, Mode::Source)?;
    assert!(!source.source_available);
    assert_eq!(source.frames.len(), 0);
    assert_eq!(source.summary.unwrap(), "PUBLIC_EDIT");
    Ok(())
}
