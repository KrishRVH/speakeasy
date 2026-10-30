//! Inspection uses temporary bundles and leaves their files and retention untouched.

use serde_json::json;

use super::*;

fn manifest() -> serde_json::Value {
    json!({
        "version": 2, "duration": 1000, "surface": {"width": 800, "height": 600},
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
fn discovery_includes_damage_and_sorts_same_second_suffixes_numerically() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let older = write(root.path(), "20261020T143205Z")?;
    let first = write(root.path(), "20261021T143205Z")?;
    let second = write(root.path(), "20261021T143205Z-2")?;
    let tenth = write(root.path(), "20261021T143205Z-10")?;
    fs::write(older.join(PIN), b"")?;
    fs::remove_file(first.join("frames/frame-01.png"))?;
    fs::write(second.join(Manifest::FILE), b"not JSON")?;
    fs::write(tenth.join(Manifest::FILE), br#"{"version":3}"#)?;
    write(root.path(), ".partial-interrupted")?;
    write(root.path(), "exported-show")?;
    fs::write(root.path().join("20261022T143205Z"), b"not a folder")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&older, root.path().join("20261023T143205Z"))?;
    let found = list(root.path())?.collect::<io::Result<Vec<_>>>()?;
    assert_eq!(
        found
            .iter()
            .map(|entry| &entry.directory)
            .collect::<Vec<_>>(),
        [&tenth, &second, &first, &older]
    );
    assert!(matches!(
        found[0].show,
        Err(ReadError::Manifest(Unreadable::Unsupported(3)))
    ));
    assert!(matches!(
        found[1].show,
        Err(ReadError::Manifest(Unreadable::Malformed { .. }))
    ));
    let damaged = found[2].show.as_ref().unwrap();
    assert_eq!(damaged.problems.len(), 1);
    assert_eq!(damaged.problems[0].name, "frames/frame-01.png");
    assert!(found[3].show.as_ref().unwrap().pinned);
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
    let entry = Entry::open(&moved)?;
    assert_eq!(entry.directory, std::path::absolute(&moved)?);
    assert!(entry.show?.problems.is_empty());
    assert!(list(root.path())?.next().is_none());
    for (name, bytes) in before {
        assert_eq!(fs::read(moved.join(name))?, bytes);
    }
    assert!(!moved.join(PIN).exists());
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
    assert!(matches!(found[0].show, Err(ReadError::File(_))));
    Ok(())
}

#[test]
fn expiry_between_discovery_and_reading_removes_only_that_entry() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let older = write(root.path(), "20261020T143205Z")?;
    let newer = write(root.path(), "20261021T143205Z")?;
    let listing = list(root.path())?;
    fs::remove_dir_all(&newer)?;
    let found = listing.collect::<io::Result<Vec<_>>>()?;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].directory, older);
    assert!(
        Entry::open(&newer).is_err(),
        "Explicit inspection must not substitute another Show"
    );
    Ok(())
}

#[test]
fn a_directory_in_place_of_the_manifest_is_damage() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = write(root.path(), "20261020T143205Z")?;
    fs::remove_file(directory.join(Manifest::FILE))?;
    fs::create_dir(directory.join(Manifest::FILE))?;
    assert!(matches!(
        Entry::open(&directory)?.show,
        Err(ReadError::File(_))
    ));
    Ok(())
}

#[test]
fn oversized_manifests_and_summaries_are_bounded() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = write(root.path(), "20261020T143205Z")?;
    fs::File::options()
        .write(true)
        .open(directory.join(Bundle::SUMMARY))?
        .set_len(SUMMARY_LIMIT + 1)?;
    let show = Entry::open(&directory)?.show?;
    assert_eq!(show.problems.len(), 1);
    assert_eq!(show.problems[0].name, Bundle::SUMMARY);
    fs::File::options()
        .write(true)
        .open(directory.join(Manifest::FILE))?
        .set_len(MANIFEST_LIMIT + 1)?;
    let error = Entry::open(&directory)?.show.unwrap_err();
    assert!(error.to_string().contains("too large"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn manifest_and_frame_links_are_not_followed() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir()?;
    let directory = write(root.path(), "20261020T143205Z")?;
    let other = write(root.path(), "elsewhere")?;
    fs::remove_dir_all(directory.join("frames"))?;
    symlink(other.join("frames"), directory.join("frames"))?;
    assert_eq!(
        Entry::open(&directory)?.show?.problems[0].name,
        "frames/frame-01.png"
    );
    fs::remove_file(directory.join(Manifest::FILE))?;
    symlink(other.join(Manifest::FILE), directory.join(Manifest::FILE))?;
    assert!(matches!(
        Entry::open(&directory)?.show,
        Err(ReadError::File(_))
    ));
    Ok(())
}

#[test]
fn five_minutes_of_dense_ink_has_room_in_the_read_limit() {
    let mut value = manifest();
    // The native pointer sampler runs every 8 ms; retain every sample for the full five minutes.
    let points: Vec<_> = (0..37_500_u64)
        .map(|sample| json!([0.123_456_78, 0.876_543_21, sample * 8]))
        .collect();
    value["duration"] = json!(300_000);
    value["strokes"] = json!([points]);
    let bytes = serde_json::to_vec_pretty(&value).unwrap();
    assert!(u64::try_from(bytes.len()).unwrap() < MANIFEST_LIMIT / 2);
    assert!(Manifest::parse(&bytes).is_ok());
}
