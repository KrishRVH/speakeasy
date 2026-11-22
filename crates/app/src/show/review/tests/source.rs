use super::*;

#[test]
fn retained_frames_add_a_mode_without_losing_keyframes_or_edits() -> anyhow::Result<()> {
    let root = Collection::Demo.load()?;
    let rows = list(&root.directory)?;
    let path = &rows[0].directory;
    let duration = recall::Entry::open(path, None)?
        .show?
        .manifest
        .recording()
        .duration;
    let before = Detail::open(path, Mode::Keyframes)?;
    fs::write(path.join("summary.md"), "Edited summary")?;
    fs::write(path.join("source.frames"), source_bytes(duration)?)?;
    let source = Detail::open(path, Mode::Source)?;
    assert!(source.source_available);
    assert_eq!(source.frames.len(), 1);
    let keyframes = Detail::open(path, Mode::Keyframes)?;
    assert_eq!(keyframes.frames.len(), before.frames.len());
    assert_eq!(keyframes.summary.unwrap(), "Edited summary");
    Ok(())
}
