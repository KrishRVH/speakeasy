use std::{fs, path::Path, slice};

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
    let mut journal = Journal::default();
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
        "version": 3, "duration": 1000, "surface": {"width": 2, "height": 2},
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
    assert!(Roots::open(slice::from_ref(&missing), None).is_err());
    let mut output = Vec::new();
    setup(
        Some(PathBuf::from("/fixture app/speakeasy")),
        &missing,
        None,
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
    assert!(setup(None, &missing, None, &mut Vec::new()).is_err());
    Ok(())
}

#[test]
fn setup_gives_an_agent_in_wsl_the_windows_command_through_interop() -> anyhow::Result<()> {
    let executable = PathBuf::from(r"C:\Program Files\Speakeasy\speakeasy-cli.exe");
    let shows = r"C:\Users\ada\AppData\Local\speakeasy\shows";
    let print = |wsl: Option<&Wsl>| -> anyhow::Result<String> {
        let mut output = Vec::new();
        setup(Some(executable.clone()), Path::new(shows), wsl, &mut output)?;
        Ok(String::from_utf8(output)?)
    };
    let output = print(Some(&Wsl::configured("")))?;
    let (_, stanza) = output
        .split_once("Agent in WSL (")
        .and_then(|(_, heading)| heading.split_once('\n'))
        .context("A WSL agent gets its own stanza")?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(stanza)?,
        json!({
            "command": "/mnt/c/Program Files/Speakeasy/speakeasy-cli.exe",
            "args": ["show", "mcp", "--root", shows],
        })
    );
    let refused = print(Some(&Wsl::configured("[interop]\nenabled = false\n")))?;
    assert!(
        refused.contains("Agent in WSL: unavailable") && !refused.contains("/mnt/"),
        "{refused}"
    );
    assert!(!print(None)?.contains("WSL"));
    Ok(())
}

#[test]
fn shows_resolve_only_inside_the_roots_and_a_shared_name_is_ambiguous() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("root");
    let sibling = temp.path().join("root-sibling");
    let inside = fixture(&root, "exact")?;
    let outside = fixture(&sibling, "exact")?;
    let roots = Roots::open(slice::from_ref(&root), None).map_err(anyhow::Error::msg)?;
    assert_eq!(roots.resolve("exact").unwrap(), inside.canonicalize()?);
    for path in [
        outside,
        root.join("../root-sibling/exact"),
        temp.path().to_owned(),
    ] {
        assert!(roots.resolve(path.to_str().unwrap()).is_err());
    }
    assert!(roots.resolve("latest").is_err());
    let roots = Roots::open(&[root, sibling], None).map_err(anyhow::Error::msg)?;
    assert!(roots.resolve("exact").is_err());
    assert_eq!(
        roots.resolve(inside.to_str().unwrap()).unwrap(),
        inside.canonicalize()?
    );
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
    let roots = Roots::open(&[temp.path().to_owned()], None).map_err(anyhow::Error::msg)?;
    let first = roots.list(None);
    assert_eq!(first.shows.len(), 100);
    let cursor = first.next_cursor.unwrap();
    let next = roots.list(Some(&cursor));
    assert_eq!(next.shows.len(), 2);
    assert!(next.next_cursor.is_none());
    assert!(first.shows.iter().all(|first| {
        next.shows
            .iter()
            .all(|next| first.directory != next.directory)
    }));
    Ok(())
}

#[test]
fn a_show_whose_manifest_cannot_be_read_is_listed_with_that_problem() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let single = fixture(temp.path(), "single")?.canonicalize()?;
    let container = temp.path().join("container");
    let nested = fixture(&container, "nested")?.canonicalize()?;
    for directory in [&single, &nested] {
        fs::write(directory.join(Manifest::FILE), b"broken manifest")?;
    }
    let roots = Roots::open(&[single.clone(), container], None).map_err(anyhow::Error::msg)?;
    let listing = serde_json::to_value(roots.list(None))?;
    let rows = listing["shows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{listing}");
    for (row, directory) in rows.iter().zip([&nested, &single]) {
        assert_eq!(row["directory"], json!(directory));
        assert_eq!(row["status"], "unreadable");
        assert!(row["error"].is_string());
    }
    assert_eq!(roots.resolve("single").map_err(anyhow::Error::msg)?, single);
    Ok(())
}

#[test]
fn a_moved_root_is_unavailable_without_hiding_other_roots() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let parent = temp.path().join("parent");
    let kept = fixture(&parent, "kept")?.canonicalize()?;
    let moved = temp.path().join("moved");
    fs::create_dir(&moved)?;
    let moved = moved.canonicalize()?;
    let roots = Roots::open(&[parent, moved.clone()], None).map_err(anyhow::Error::msg)?;
    fs::rename(&moved, temp.path().join("elsewhere"))?;
    let listing = roots.list(None);
    let listed: Vec<_> = listing.shows.iter().map(|show| &show.directory).collect();
    assert_eq!(listed, [&kept]);
    assert_eq!(listing.unavailable_roots, [moved]);
    assert_eq!(roots.resolve("kept").map_err(anyhow::Error::msg)?, kept);
    Ok(())
}
