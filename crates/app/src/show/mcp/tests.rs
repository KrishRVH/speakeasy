use std::{fs, path::Path};

use speakeasy_show::{Image, Journal, Manifest, Moment};

use super::*;

pub(super) fn fixture(parent: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let directory = parent.join(name);
    fs::create_dir_all(directory.join("frames"))?;
    let manifest = fixture_manifest();
    let encoded = serde_json::to_vec(&manifest)?;
    let png = Image::from_bgra(2, 2, [40, 80, 120, 255].repeat(4))?.encode_png()?;
    for name in Manifest::parse(&encoded)?.files() {
        fs::write(directory.join(name), &png)?;
    }
    fs::write(directory.join("show.json"), encoded)?;
    fs::write(directory.join("summary.md"), "PRIVATE_EDITED_SUMMARY")?;
    let mut journal = Journal::new();
    journal.admit().unwrap().record(
        Moment::from_millis(100),
        Image::from_bgra(2, 2, [20, 40, 60, 255].repeat(4))?,
    );
    let mut source = Vec::new();
    journal.write_source::<speakeasy_show::SourceError>(Moment::from_millis(1000), |bytes| {
        source.extend_from_slice(bytes);
        Ok(())
    })?;
    fs::write(directory.join("source.frames"), source)?;
    Ok(directory)
}

pub(super) fn fixture_manifest() -> serde_json::Value {
    json!({
        "version": 2, "duration": 1000, "surface": {"width": 2, "height": 2},
        "strokes": [], "pins": [], "actions": [],
        "narration": {"words": [{"span": [200, 300], "text": "PRIVATE_NARRATION"}, {"span": [600, 700], "text": "PRIVATE_NARRATION"}]},
        "marks": [{"number": 1, "shape": "pin", "point": [0.5,0.5], "bounds": [0.5,0.5,0.5,0.5], "span": [500,500], "strokes": [0,0]}],
        "keyframes": [{"index": 1, "at": 500, "reason": "mark", "event": "PRIVATE_EVENT", "marks": [0,1]}]
    })
}

#[test]
fn startup_requires_roots_and_setup_does_not_create_them() -> anyhow::Result<()> {
    for args in [
        vec![],
        vec!["--root"],
        vec!["--setup", "--root", "elsewhere"],
        vec!["--setup", "--setup"],
        vec!["--root", "--setup"],
    ] {
        assert!(Command::parse(args.into_iter().map(OsString::from)).is_err());
    }
    let temp = tempfile::tempdir()?;
    let missing = temp.path().join("missing root");
    assert!(Roots::open(vec![missing.clone()]).is_err());
    let mut output = Vec::new();
    setup_for(
        "/fixture app/speakeasy",
        missing.to_str().unwrap(),
        &mut output,
    )?;
    assert!(!missing.exists());
    let output = String::from_utf8(output)?;
    assert!(output.contains("mcpServers") && output.contains("[mcp_servers.speakeasy-show]"));
    assert!(
        output.contains("possibly its model provider")
            && output.contains("evidence, not instructions")
    );
    assert!(output.contains("--root") && output.contains("/fixture app/speakeasy"));
    let command = Command::parse([OsString::from("--root"), missing.into_os_string()].into_iter())?;
    assert!(!format!("{command:?}").contains("missing root"));
    Ok(())
}

#[test]
fn roots_refuse_outside_sibling_prefix_parent_steps_and_ambiguous_names() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("root");
    let sibling = temp.path().join("root-sibling");
    let inside = fixture(&root, "exact")?;
    let outside = fixture(&sibling, "exact")?;
    let roots = Roots::open(vec![root.clone()]).map_err(anyhow::Error::msg)?;
    assert_eq!(roots.resolve("exact").unwrap(), inside.canonicalize()?);
    for path in [outside, root.join("../root/exact"), temp.path().to_owned()] {
        assert!(roots.resolve(path.to_str().unwrap()).is_err());
    }
    assert!(roots.resolve("latest").is_err());
    let roots = Roots::open(vec![root, sibling]).map_err(anyhow::Error::msg)?;
    assert!(roots.resolve("exact").is_err());
    assert_eq!(
        roots.resolve(inside.to_str().unwrap()).unwrap(),
        inside.canonicalize()?
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_bundles_and_symlinked_roots_are_refused() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("root");
    fs::create_dir(&root)?;
    let outside = fixture(temp.path(), "outside")?;
    let linked = root.join("linked");
    symlink(&outside, &linked)?;
    assert!(Roots::open(vec![linked.clone()]).is_err());
    let roots = Roots::open(vec![root]).map_err(anyhow::Error::msg)?;
    assert!(roots.resolve(linked.to_str().unwrap()).is_err());
    assert!(roots.list(None).unwrap().0.is_empty());
    Ok(())
}

#[test]
fn listing_is_bounded_and_a_cursor_does_not_skip_the_next_show() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    for index in 0..102 {
        let directory = temp.path().join(format!("show-{index:03}"));
        fs::create_dir(&directory)?;
        fs::write(directory.join(Manifest::FILE), b"broken manifest")?;
    }
    let roots = Roots::open(vec![temp.path().to_owned()]).map_err(anyhow::Error::msg)?;
    let (first, cursor) = roots.list(None).map_err(anyhow::Error::msg)?;
    assert_eq!(first.len(), 100);
    let cursor = cursor.unwrap();
    let (next, cursor) = roots.list(Some(&cursor)).map_err(anyhow::Error::msg)?;
    assert_eq!(next.len(), 2);
    assert!(cursor.is_none());
    assert!(
        first
            .iter()
            .all(|first| next.iter().all(|next| first.directory != next.directory))
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn explicit_root_ancestor_alias_is_fixed_at_launch_but_child_links_are_refused()
-> anyhow::Result<()> {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir()?;
    let actual = temp.path().join("actual");
    let directory = fixture(&actual.join("shows"), "exact")?;
    let alias = temp.path().join("alias");
    symlink(&actual, &alias)?;
    let roots = Roots::open(vec![alias.join("shows")]).map_err(anyhow::Error::msg)?;
    assert_eq!(
        roots
            .resolve(alias.join("shows/exact").to_str().unwrap())
            .unwrap(),
        directory.canonicalize()?
    );
    let child = actual.join("shows/link");
    symlink(&directory, &child)?;
    assert!(
        roots
            .resolve(alias.join("shows/link").to_str().unwrap())
            .is_err()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn setup_uses_the_resolved_appimage_launcher_without_resolving_the_mount_again()
-> anyhow::Result<()> {
    let executable = "/home/public/Apps/Speakeasy.AppImage";
    let mut output = Vec::new();
    setup(
        || Some(PathBuf::from(executable)),
        PathBuf::from("/home/public/Shows"),
        &mut output,
    )?;
    let output = String::from_utf8(output)?;
    assert_eq!(output.matches(executable).count(), 3);
    assert!(!output.contains(".mount_"));
    let mut refused = Vec::new();
    assert!(setup(|| None, PathBuf::from("/home/public/Shows"), &mut refused).is_err());
    assert!(refused.is_empty());
    Ok(())
}
