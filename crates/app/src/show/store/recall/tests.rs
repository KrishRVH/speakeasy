use serde_json::json;

use super::*;

fn manifest() -> serde_json::Value {
    json!({
        "version": 3, "duration": 1000, "surface": {"width": 800, "height": 600},
        "strokes": [], "pins": [], "actions": [], "narration": {"text": "PRIVATE_NARRATION"},
        "marks": [], "keyframes": [{"index": 1, "at": 500, "reason": "middle",
            "event": "PRIVATE_EVENT", "marks": [0, 0]}]
    })
}

fn write(parent: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let directory = parent.join(name);
    fs::create_dir_all(directory.join("frames"))?;
    let bytes = serde_json::to_vec(&manifest())?;
    for file in Manifest::parse(&bytes)?.files() {
        fs::write(directory.join(file), b"fixture file, not decoded pixels")?;
    }
    fs::write(directory.join(Manifest::FILE), bytes)?;
    Ok(directory)
}

#[test]
fn discovery_includes_unreadable_shows_and_sorts_same_second_suffixes_numerically()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let older = write(root.path(), "20261020T143205Z")?;
    let first = write(root.path(), "20261021T143205Z")?;
    let second = write(root.path(), "20261021T143205Z-2")?;
    let tenth = write(root.path(), "20261021T143205Z-10")?;
    fs::write(older.join(super::super::PIN), b"")?;
    fs::write(second.join(Manifest::FILE), b"not JSON")?;
    fs::write(tenth.join(Manifest::FILE), br#"{"version":4}"#)?;
    write(root.path(), "exported-show")?;
    fs::write(root.path().join("20261022T143205Z"), b"not a folder")?;
    let found = list(root.path())?.collect::<io::Result<Vec<_>>>()?;
    assert_eq!(
        found
            .iter()
            .map(|entry| &entry.directory)
            .collect::<Vec<_>>(),
        [&tenth, &second, &first, &older]
    );
    assert!(found[0].show.is_err() && found[1].show.is_err());
    assert!(found[2].show.is_ok() && !found[2].pinned);
    assert!(found[3].show.is_ok() && found[3].pinned);
    Ok(())
}

#[test]
fn inspection_preserves_edits_and_a_moved_bundle_uses_its_current_directory() -> anyhow::Result<()>
{
    let root = tempfile::tempdir()?;
    let directory = write(root.path(), "20261020T143205Z")?;
    fs::write(
        directory.join(Bundle::SUMMARY),
        b"user-edited summary with an old path",
    )?;
    let before: Vec<_> = Manifest::parse(&fs::read(directory.join(Manifest::FILE))?)?
        .files()
        .map(|name| Ok((name.clone(), fs::read(directory.join(name))?)))
        .collect::<io::Result<_>>()?;
    let moved = root.path().join("moved show");
    fs::rename(&directory, &moved)?;
    let entry = Entry::open(&moved, None)?;
    assert_eq!(entry.directory, std::path::absolute(&moved)?);
    assert!(entry.show.is_ok());
    assert!(list(root.path())?.next().is_none());
    for (name, bytes) in before {
        assert_eq!(fs::read(moved.join(name))?, bytes);
    }
    Ok(())
}

#[test]
fn missing_parent_is_not_created_and_missing_manifest_is_not_hidden() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let parent = root.path().join("absent");
    assert!(list(&parent)?.next().is_none());
    assert!(!parent.exists());
    fs::create_dir_all(parent.join("20261020T143205Z"))?;
    let found = list(&parent)?.collect::<io::Result<Vec<_>>>()?;
    assert_eq!(found.len(), 1);
    assert!(found[0].show.is_err());
    Ok(())
}
