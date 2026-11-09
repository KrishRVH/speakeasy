use std::fs;

use super::{
    library::{Row, list},
    *,
};
use crate::show::store::recall::Health;

mod narration;
mod offers;
mod source;

fn row(name: &str) -> Row {
    Row {
        kind: library::Kind::Saved,
        directory: name.into(),
        health: Health::Ok,
        duration: None,
        frames: 2,
        marks: 0,
        pinned: Some(false),
        early: false,
        narration_lost: None,
    }
}

#[test]
fn an_old_show_or_frame_result_cannot_replace_a_newer_choice_even_when_revisited() {
    let mut library = Library::default();
    let first = library.replace(vec![row("first"), row("second")]).unwrap();
    let second = library.select("second".into(), 0);
    let revisited = library.select("first".into(), 0);
    assert!(!library.accepts(&first));
    assert!(!library.accepts(&second));
    assert!(library.accepts(&revisited));
    let next_frame = library.select("first".into(), 1);
    let first_again = library.select("first".into(), 0);
    assert!(!library.accepts(&revisited));
    assert!(!library.accepts(&next_frame));
    assert!(library.accepts(&first_again));
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
    assert!(library.gone);
    assert!(library.selected.is_none());
    assert!(library.replace(vec![row("newest"), row("newer")]).is_none());
    assert!(library.selected.is_none());
}

#[test]
fn the_more_menu_opens_once_per_press_and_keyboard_skips_unavailable_actions() {
    use menu::Menu;
    let enabled = [false, true, false, true];
    let open = Menu::Closed.pressed(false, &enabled);
    assert_eq!(open, Menu::Open(None));
    assert_eq!(
        open.dismissed().pressed(false, &enabled),
        Menu::Closed,
        "Pressing the trigger of an open menu reopened it"
    );
    assert_eq!(
        open.dismissed().pointer().pressed(false, &enabled),
        Menu::Open(None),
        "A later press on the trigger did not open a menu dismissed elsewhere"
    );
    let keyed = Menu::Dismissed.pressed(true, &enabled);
    assert_eq!(keyed, Menu::Open(Some(1)));
    assert_eq!(keyed.step(&enabled, true), Menu::Open(Some(3)));
    assert_eq!(
        keyed.step(&enabled, true).step(&enabled, true),
        Menu::Open(Some(1))
    );
    assert_eq!(keyed.step(&enabled, false), Menu::Open(Some(3)));
    assert_eq!(Menu::Open(None).step(&[false], true), Menu::Open(None));
    assert_eq!(Menu::Closed.step(&enabled, true), Menu::Closed);
}

#[tokio::test]
async fn a_cropped_keyframe_says_so_and_where_the_whole_screen_is() -> anyhow::Result<()> {
    let root = Source::Demo.load()?;
    let (rows, _) = list(&root.directory)?;
    let detail = Detail::open(&rows[0].directory, library::Mode::Keyframes)?;
    let cropped = detail.frames.iter().filter(|frame| frame.cropped).count();
    assert!(cropped > 0 && cropped < detail.frames.len(), "{cropped}");
    let note = "Cropped to where you worked";
    for frame in &detail.frames {
        let alone = frame.caption(detail.frames.len(), false);
        let pointed = frame.caption(detail.frames.len(), true);
        assert_eq!(alone.ends_with(note), frame.cropped, "{alone}");
        assert_eq!(
            pointed.ends_with(&format!(
                "{note}; All retained frames shows the whole screen"
            )),
            frame.cropped,
            "{pointed}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn the_demo_is_isolated_and_damage_does_not_hide_the_current_summary() -> anyhow::Result<()> {
    let root = Source::Demo.load()?;
    assert!(root.fixture.is_some());
    let (rows, _) = list(&root.directory)?;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].pinned, Some(true));
    assert!(rows[1].early && rows[1].narration_lost.is_some());
    assert_eq!(rows[2].health, Health::Damaged);
    let detail = Detail::open(&rows[2].directory, library::Mode::Keyframes)?;
    assert_eq!(detail.frames.len(), 5);
    let mut reads = crate::show::preview::Reads::default();
    for (index, valid) in [(2, false), (1, true)] {
        let frame = &detail.frames[index];
        let work = reads.submit(crate::show::preview::Request {
            directory: rows[2].directory.clone(),
            content: frame.file.clone(),
            at: frame.at,
            annotated: false,
            fixture: root.fixture.clone(),
        })?;
        let crate::show::preview::Work::Reading(receipt) = work else {
            anyhow::bail!("Synthetic frame read was not drained");
        };
        assert_eq!(receipt.await.is_ok(), valid);
    }
    let path = &rows[0].directory;
    let manifest_path = path.join("show.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    manifest["keyframes"][2]["event"] = "SECRET_SPOKEN_CUE".into();
    manifest["keyframes"][2]["said"] = "SECRET_NARRATION".into();
    manifest["keyframes"][2]["marks"] = serde_json::json!([0, 0]);
    fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;
    fs::write(path.join("summary.md"), "SECRET_USER_EDIT")?;
    let detail = Detail::open(path, library::Mode::Keyframes)?;
    assert!(
        detail
            .warnings
            .iter()
            .any(|warning| warning == "Mark 1 has no saved frame.")
    );
    assert_eq!(detail.summary.as_ref().unwrap(), "SECRET_USER_EDIT");
    assert!(!format!("{:?} {:?}", list(&root.directory)?, detail.frames).contains("SECRET"));
    fs::write(&manifest_path, br#"{"version":99}"#)?;
    let detail = Detail::open(path, library::Mode::Keyframes)?;
    assert!(detail.frames.is_empty());
    let (rows, _) = list(&root.directory)?;
    assert_eq!(rows[0].pinned, Some(true));
    assert_eq!(rows[0].retention(std::time::SystemTime::now()), "Kept");
    fs::remove_file(path.join(".pinned"))?;
    let (rows, _) = list(&root.directory)?;
    assert_eq!(rows[0].pinned, Some(false));
    assert!(
        rows[0]
            .retention(std::time::SystemTime::now())
            .starts_with("Expires in")
    );
    assert!(detail.warnings[0].contains("99"));
    assert_eq!(detail.summary.unwrap(), "SECRET_USER_EDIT");
    fs::write(&manifest_path, b"malformed SECRET_SPEECH")?;
    let detail = Detail::open(path, library::Mode::Keyframes)?;
    assert!(!detail.warnings.join(" ").contains("SECRET"));
    assert_eq!(detail.summary.unwrap(), "SECRET_USER_EDIT");
    assert_eq!(
        fs::read_to_string(path.join("summary.md"))?,
        "SECRET_USER_EDIT"
    );
    fs::write(path.join("summary.md"), b"")?;
    assert!(
        recall::summary(path)?.is_empty(),
        "An intentionally emptied summary is a valid edit"
    );
    fs::write(path.join("summary.md"), b"PRIVATE\xff")?;
    assert!(
        !recall::summary(path)
            .unwrap_err()
            .to_string()
            .contains("PRIVATE")
    );
    Ok(())
}
