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
    let command = Command::parse([OsString::from("--root"), missing.into_os_string()].into_iter())?;
    assert!(!format!("{command:?}").contains("missing root"));
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

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_requested_and_canonical_roots_are_refused_without_private_paths() -> anyhow::Result<()>
{
    use std::os::unix::{ffi::OsStringExt as _, fs::symlink};

    let temp = tempfile::tempdir()?;
    let private = temp
        .path()
        .join(OsString::from_vec(b"PRIVATE_ROOT_\xff".to_vec()));
    fs::create_dir_all(private.join("shows"))?;
    let public = temp.path().join("public");
    fs::create_dir_all(public.join("shows"))?;
    let alias = temp.path().join("alias");
    symlink(&private, &alias)?;
    let private_alias = temp
        .path()
        .join(OsString::from_vec(b"PRIVATE_ALIAS_\xff".to_vec()));
    symlink(&public, &private_alias)?;
    for path in [private, alias.join("shows"), private_alias.join("shows")] {
        let error = Roots::open(&[path], None).err().unwrap();
        assert_eq!(error, "An MCP root path must be valid UTF-8");
    }
    Ok(())
}

#[test]
fn roots_refuse_outside_sibling_prefix_parent_steps_and_ambiguous_names() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("root");
    let sibling = temp.path().join("root-sibling");
    let inside = fixture(&root, "exact")?;
    let outside = fixture(&sibling, "exact")?;
    let roots = Roots::open(slice::from_ref(&root), None).map_err(anyhow::Error::msg)?;
    assert_eq!(roots.resolve("exact").unwrap().path, inside.canonicalize()?);
    for path in [outside, root.join("../root/exact"), temp.path().to_owned()] {
        assert!(roots.resolve(path.to_str().unwrap()).is_err());
    }
    assert!(roots.resolve("latest").is_err());
    let roots = Roots::open(&[root, sibling], None).map_err(anyhow::Error::msg)?;
    assert!(roots.resolve("exact").is_err());
    assert_eq!(
        roots.resolve(inside.to_str().unwrap()).unwrap().path,
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
    for root in [linked.clone(), linked.join(""), linked.join(".")] {
        assert!(Roots::open(&[root], None).is_err());
    }
    let roots = Roots::open(&[root], None).map_err(anyhow::Error::msg)?;
    assert!(roots.resolve(linked.to_str().unwrap()).is_err());
    assert!(roots.list(None).unwrap().shows.is_empty());
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
    let first = roots.list(None).map_err(anyhow::Error::msg)?;
    assert_eq!(first.shows.len(), 100);
    let cursor = first.next_cursor.unwrap();
    let next = roots.list(Some(&cursor)).map_err(anyhow::Error::msg)?;
    assert_eq!(next.shows.len(), 2);
    assert!(next.next_cursor.is_none());
    assert!(first.shows.iter().all(|first| {
        next.shows
            .iter()
            .all(|next| first.directory != next.directory)
    }));
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
    let roots = Roots::open(&[temp.path().to_owned(), alias.join("shows")], None)
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        roots
            .resolve(alias.join("shows/exact").to_str().unwrap())
            .unwrap()
            .path,
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

#[test]
fn internal_show_directories_are_neither_listed_nor_resolved() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let roots = Roots::open(&[temp.path().to_owned()], None).map_err(anyhow::Error::msg)?;
    for name in [
        ".expired-20261020T143205Z",
        ".partial-example",
        ".checkpoints",
    ] {
        let hidden = fixture(temp.path(), name)?.canonicalize()?;
        assert!(roots.resolve(name).is_err());
        assert!(roots.resolve(hidden.to_str().unwrap()).is_err());
    }
    assert!(roots.list(None).unwrap().shows.is_empty());
    Ok(())
}

#[test]
fn a_show_whose_manifest_cannot_be_opened_is_listed_with_that_problem() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let single = fixture(temp.path(), "single")?.canonicalize()?;
    let container = temp.path().join("container");
    let nested = fixture(&container, "nested")?.canonicalize()?;
    for directory in [&single, &nested] {
        fs::remove_file(directory.join(Manifest::FILE))?;
        fs::create_dir(directory.join(Manifest::FILE))?;
    }
    let roots = Roots::open(&[single.clone(), container], None).map_err(anyhow::Error::msg)?;
    let listing = serde_json::to_value(roots.list(None).map_err(anyhow::Error::msg)?)?;
    let rows = listing["shows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{listing}");
    for (row, directory) in rows.iter().zip([&nested, &single]) {
        assert_eq!(row["directory"], json!(directory));
        assert_eq!(row["status"], "damaged");
        assert_eq!(
            row["problems"][0]["file"],
            json!(directory.join(Manifest::FILE))
        );
    }
    assert_eq!(
        roots.resolve("single").map_err(anyhow::Error::msg)?.path,
        single
    );
    Ok(())
}

#[test]
fn a_replaced_or_moved_root_neither_mixes_folders_nor_hides_other_roots() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let parent = temp.path().join("parent");
    let kept = fixture(&parent, "kept")?.canonicalize()?;
    let nested = parent.join("nested");
    fixture(&nested, "original")?;
    let moved = temp.path().join("moved");
    fs::create_dir(&moved)?;
    let moved = moved.canonicalize()?;
    let roots =
        Roots::open(&[parent, nested.clone(), moved.clone()], None).map_err(anyhow::Error::msg)?;
    fs::rename(&nested, temp.path().join("old nested"))?;
    let replacement = fixture(&nested, "replacement")?.canonicalize()?;
    fs::rename(&moved, temp.path().join("elsewhere"))?;
    let listing = roots.list(None).map_err(anyhow::Error::msg)?;
    let listed: Vec<_> = listing.shows.iter().map(|show| &show.directory).collect();
    assert_eq!(listed, [&kept, &replacement]);
    let unavailable: Vec<_> = listing
        .unavailable_roots
        .iter()
        .map(|root| &root.file)
        .collect();
    assert_eq!(unavailable, [&moved]);
    for (name, listed) in [("kept", &kept), ("replacement", &replacement)] {
        let resolved = roots.resolve(name).map_err(anyhow::Error::msg)?;
        assert_eq!(&resolved.path, listed);
        assert!(resolved.files.is_at(listed)?);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_root_that_cannot_be_searched_is_unavailable_rather_than_a_damaged_show() -> anyhow::Result<()>
{
    use std::os::unix::fs::PermissionsExt as _;
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("root");
    fixture(&root, "inside")?;
    let root = root.canonicalize()?;
    let roots = Roots::open(slice::from_ref(&root), None).map_err(anyhow::Error::msg)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o600))?;
    let listing = roots.list(None);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let listing = listing.map_err(anyhow::Error::msg)?;
    let unavailable: Vec<_> = listing
        .unavailable_roots
        .iter()
        .map(|root| &root.file)
        .collect();
    assert_eq!(unavailable, [&root]);
    assert!(
        listing.shows.is_empty(),
        "{}",
        serde_json::to_value(&listing.shows)?
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_full_page_lists_within_a_small_descriptor_limit() -> anyhow::Result<()> {
    let work = tempfile::tempdir()?;
    for index in 0..150 {
        let directory = work.path().join(format!("root/show-{index:03}"));
        fs::create_dir_all(&directory)?;
        fs::write(directory.join(Manifest::FILE), b"broken manifest")?;
    }
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(
            "ulimit -n 64 && exec \"$0\" --exact show::mcp::tests::listing_child_fixture --ignored",
        )
        .arg(std::env::current_exe()?)
        .env("SPEAKEASY_MCP_TEST_ROOT", work.path())
        .stdin(std::process::Stdio::null())
        .output()?;
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{report}");
    assert!(work.path().join("listed").exists(), "{report}");
    Ok(())
}

#[test]
#[ignore = "Subprocess helper for the descriptor limit test; does nothing unless that test spawns it"]
fn listing_child_fixture() -> anyhow::Result<()> {
    let Some(work) = std::env::var_os("SPEAKEASY_MCP_TEST_ROOT").map(PathBuf::from) else {
        return Ok(());
    };
    let roots = Roots::open(&[work.join("root")], None).map_err(anyhow::Error::msg)?;
    let listing = roots.list(None).map_err(anyhow::Error::msg)?;
    let damaged = listing
        .shows
        .iter()
        .filter(|show| {
            show.problems
                .iter()
                .any(|problem| problem.file == show.directory)
        })
        .count();
    anyhow::ensure!(
        listing.shows.len() == scope::PAGE && listing.truncated && damaged == 0,
        "listed {} Shows, {damaged} unopened, truncated {}",
        listing.shows.len(),
        listing.truncated
    );
    fs::write(work.join("listed"), b"")?;
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[test]
fn a_show_folder_that_cannot_be_opened_is_listed_with_that_problem() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let sealed = fixture(temp.path(), "sealed")?.canonicalize()?;
    let roots = Roots::open(&[temp.path().to_owned()], None).map_err(anyhow::Error::msg)?;
    seal(&sealed, true)?;
    let listing = roots.list(None);
    seal(&sealed, false)?;
    let listing = serde_json::to_value(listing.map_err(anyhow::Error::msg)?)?;
    let rows = listing["shows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{listing}");
    assert_eq!(rows[0]["directory"], json!(sealed));
    assert_eq!(rows[0]["status"], "damaged");
    assert_eq!(rows[0]["problems"][0]["file"], json!(sealed));
    Ok(())
}

#[cfg(target_os = "macos")]
fn seal(directory: &Path, sealed: bool) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = if sealed { 0o000 } else { 0o700 };
    fs::set_permissions(directory, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn seal(directory: &Path, sealed: bool) -> anyhow::Result<()> {
    let change = if sealed {
        ["/deny", "*S-1-1-0:(R)"]
    } else {
        ["/remove:d", "*S-1-1-0"]
    };
    let output = std::process::Command::new("icacls")
        .arg(directory)
        .args(change)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "icacls could not change the test folder"
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
        Some(PathBuf::from(executable)),
        Path::new("/home/public/Shows"),
        None,
        &mut output,
    )?;
    let output = String::from_utf8(output)?;
    assert_eq!(output.matches(executable).count(), 3);
    assert!(!output.contains(".mount_"));
    let mut refused = Vec::new();
    assert!(setup(None, Path::new("/home/public/Shows"), None, &mut refused).is_err());
    assert_eq!(refused, [] as [u8; 0]);
    Ok(())
}
