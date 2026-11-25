use std::fs;

use speakeasy_show::{Image, Journal, Moment, SourceError};

use super::{
    library::{Row, list},
    *,
};

mod handoff;
mod narration;
mod navigation;
mod source;

fn source_bytes() -> anyhow::Result<Vec<u8>> {
    let mut journal = Journal::default();
    journal
        .admit()
        .context("Synthetic frame wasn't admitted")?
        .record(
            Moment::START,
            Image::from_bgra(2, 2, [40, 50, 60, 255].repeat(4))?,
        );
    let mut bytes = Vec::new();
    journal.write_source::<SourceError>(Moment::from_millis(1_000), |chunk| {
        bytes.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(bytes)
}

fn row(name: &str) -> Row {
    Row {
        kind: library::Kind::Saved,
        directory: name.into(),
        readable: true,
        duration: None,
        frames: 2,
        marks: 0,
        pinned: Some(false),
        early: false,
        narration_lost: None,
    }
}

#[test]
fn refresh_preserves_the_explicit_path_but_never_substitutes_for_a_deleted_show() {
    let mut library = Library::default();
    library.replace(vec![row("first"), row("second")]);
    library.select("second".into(), 1);
    let selected = library
        .replace(vec![row("newer"), row("second"), row("first")])
        .unwrap();
    assert_eq!(selected.directory, PathBuf::from("second"));
    assert_eq!(selected.frame, 1);
    assert!(library.replace(vec![row("newer"), row("first")]).is_none());
    assert!(library.is_gone());
    assert!(library.selected().is_none());
    assert!(library.replace(vec![row("newest"), row("newer")]).is_none());
    assert!(library.selected().is_none());
}

#[test]
fn the_more_menu_opens_once_per_press_and_keyboard_skips_unavailable_actions() {
    use menu::Menu;
    let enabled = [false, true, false, true];
    let open = Menu::Closed.pressed(Modality::Pointer, &enabled);
    assert_eq!(open, Menu::Open(None));
    assert_eq!(
        open.dismissed().pressed(Modality::Pointer, &enabled),
        Menu::Closed,
        "Pressing the trigger of an open menu reopened it"
    );
    assert_eq!(
        open.dismissed()
            .pointer()
            .pressed(Modality::Pointer, &enabled),
        Menu::Open(None),
        "A later press on the trigger didn't open a menu dismissed elsewhere"
    );
    let keyed = Menu::Dismissed.pressed(Modality::Keyboard, &enabled);
    assert_eq!(keyed, Menu::Open(Some(1)));
    let (down, up) = (Direction::Forward, Direction::Back);
    assert_eq!(keyed.step(&enabled, down), Menu::Open(Some(3)));
    assert_eq!(
        keyed.step(&enabled, down).step(&enabled, down),
        Menu::Open(Some(1))
    );
    assert_eq!(keyed.step(&enabled, up), Menu::Open(Some(3)));
    assert_eq!(Menu::Open(None).step(&[false], down), Menu::Open(None));
    assert_eq!(Menu::Closed.step(&enabled, down), Menu::Closed);
}

#[test]
fn a_cropped_keyframe_says_so_and_offers_the_whole_screen() -> anyhow::Result<()> {
    let root = Collection::Demo.load()?;
    let rows = list(&root.directory)?;
    let detail = Detail::open(&rows[0].directory, library::Mode::Keyframes)?;
    assert!(detail.frames.iter().any(|frame| frame.cropped));
    let uncropped = library::Frame {
        number: 1,
        presentation: Duration::ZERO,
        marker: String::new(),
        cropped: false,
        content: preview::Content::Png("frames/frame-01.png".to_owned()),
    };
    for frame in detail.frames.iter().chain([&uncropped]) {
        let caption = frame.caption(detail.frames.len());
        assert_eq!(
            caption.ends_with(" · Cropped to where you worked"),
            frame.cropped,
            "{caption}"
        );
        assert_eq!(frame.offers_whole_screen(true), frame.cropped);
        assert!(!frame.offers_whole_screen(false));
    }
    Ok(())
}

#[test]
fn the_demo_is_an_isolated_set_of_pinned_early_and_damaged_shows() -> anyhow::Result<()> {
    let root = Collection::Demo.load()?;
    assert!(root.fixture.is_some());
    let rows = list(&root.directory)?;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].pinned, Some(true));
    assert!(rows[1].early && rows[1].narration_lost.is_some());
    Ok(())
}

#[tokio::test]
async fn a_damaged_show_fails_only_the_preview_of_its_missing_frame() -> anyhow::Result<()> {
    let root = Collection::Demo.load()?;
    let rows = list(&root.directory)?;
    let damaged = &rows[2].directory;
    let detail = Detail::open(damaged, library::Mode::Keyframes)?;
    assert_eq!(detail.frames.len(), 5);
    let mut reads = preview::Reads::default();
    for (index, valid) in [(2, false), (1, true)] {
        let frame = &detail.frames[index];
        let work = reads.submit(preview::Request {
            directory: damaged.clone(),
            content: frame.content.clone(),
            at: frame.at(),
            annotated: false,
            fixture: root.fixture.clone(),
        })?;
        let preview::Work::Reading(receipt) = work else {
            anyhow::bail!("Synthetic frame read wasn't drained");
        };
        assert_eq!(receipt.await.is_ok(), valid);
    }
    Ok(())
}

#[test]
fn a_mark_without_a_frame_is_named_in_the_warnings() -> anyhow::Result<()> {
    let root = Collection::Demo.load()?;
    let rows = list(&root.directory)?;
    let path = &rows[0].directory;
    let manifest_path = path.join("show.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    manifest["keyframes"][2]["marks"] = serde_json::json!([0, 0]);
    fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;
    let detail = Detail::open(path, library::Mode::Keyframes)?;
    assert!(
        detail
            .warnings
            .iter()
            .any(|warning| warning == "Mark 1 has no saved frame.")
    );
    Ok(())
}

#[test]
fn an_unreadable_manifest_keeps_the_summary_and_names_the_problem() -> anyhow::Result<()> {
    let root = Collection::Demo.load()?;
    let rows = list(&root.directory)?;
    let path = &rows[0].directory;
    fs::write(path.join("summary.md"), "Edited summary")?;
    fs::write(path.join("show.json"), br#"{"version":99}"#)?;
    let unsupported = Detail::open(path, library::Mode::Keyframes)?;
    assert!(unsupported.frames.is_empty());
    assert!(unsupported.warnings[0].contains("99"));
    assert_eq!(unsupported.summary.unwrap(), "Edited summary");
    Ok(())
}

#[test]
fn an_unreadable_manifest_still_reports_the_shows_pin() -> anyhow::Result<()> {
    let root = Collection::Demo.load()?;
    let rows = list(&root.directory)?;
    let path = &rows[0].directory;
    fs::write(path.join("show.json"), br#"{"version":99}"#)?;
    let unreadable = list(&root.directory)?;
    assert_eq!(unreadable[0].pinned, Some(true));
    assert_eq!(unreadable[0].retention(SystemTime::now()), "Kept");
    fs::remove_file(path.join(".pinned"))?;
    let unpinned = list(&root.directory)?;
    assert_eq!(unpinned[0].pinned, Some(false));
    assert!(
        unpinned[0]
            .retention(SystemTime::now())
            .starts_with("Expires in")
    );
    Ok(())
}
