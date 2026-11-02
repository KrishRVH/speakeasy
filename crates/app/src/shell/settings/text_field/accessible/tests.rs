use gpui::{
    EntityId,
    accesskit::{Point, ScrollHint},
};

use super::*;
use crate::shell::settings::text_field::edit::Composition;

const FIELD: u64 = 7;

fn edit(text: &str) -> Edit {
    let mut edit = Edit::default();
    edit.set(text.into());
    edit
}

fn document(edit: &Edit) -> Document {
    Document::new(EntityId::from(FIELD), edit).unwrap()
}

fn frame(scroll: f32) -> Frame {
    let bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(100.0), px(20.0)));
    Frame {
        bounds,
        visible: bounds,
        scroll: px(scroll),
        scale: 2.0,
    }
}

fn painted(edit: &Edit, scroll: f32) -> Option<Painted> {
    let document = document(edit);
    let edges = (0..)
        .map(|step: u16| px(f32::from(step) * 10.0))
        .take(document.boundaries().count())
        .collect();
    Painted::measured(document, edit, edges, frame(scroll))
}

fn request(edit: &Edit, data: &ActionData) -> Option<Selection> {
    let request = Request::new(
        AccessibleAction::SetTextSelection,
        data,
        EntityId::from(FIELD),
        edit,
    );
    match request? {
        Request::Select(selection) => Some(selection),
        Request::Replace(_) | Request::Reveal(_) => None,
    }
}

fn select(anchor: TextPosition, focus: TextPosition) -> ActionData {
    ActionData::SetTextSelection(TextSelection { anchor, focus })
}

#[test]
fn native_selections_keep_direction_and_reject_partial_foreign_or_stale_endpoints() {
    let mut text = edit("ae\u{301}👩🏽‍💻");
    let len = text.text.len();
    let doc = document(&text);
    let (end, after_a) = (doc.position(len).unwrap(), doc.position(1).unwrap());
    assert_eq!(
        request(&text, &select(end, after_a)),
        Some(Selection {
            anchor: len,
            focus: 1
        })
    );
    assert!(
        doc.position(2).is_none(),
        "A combining mark split its grapheme"
    );
    assert!(
        doc.position(5).is_none(),
        "An emoji sequence split its grapheme"
    );
    let foreign = TextPosition {
        node: NodeId(1),
        character_index: 0,
    };
    assert_eq!(request(&text, &select(foreign, after_a)), None);
    let beyond = TextPosition {
        character_index: 4,
        ..end
    };
    assert_eq!(request(&text, &select(beyond, after_a)), None);
    text.select(0, false);
    assert_eq!(document(&text).position(1), Some(after_a));
    text.set(text.text.clone());
    assert_eq!(
        request(&text, &select(end, after_a)),
        None,
        "A replaced text accepted a selection from its previous revision"
    );

    let empty = edit("");
    let empty_doc = document(&empty);
    assert_eq!(empty_doc.position(0).unwrap().character_index, 0);
    assert!(empty_doc.position(1).is_none());
}

#[test]
fn long_text_splits_into_linked_runs_without_inventing_word_starts() {
    let text = edit(&format!("{}abcdef🦀日", "x".repeat(255)));
    let runs = painted(&text, 0.0).unwrap().runs();
    let [(first_id, first), (second_id, second)] = runs.as_slice() else {
        panic!("Expected two runs, got {}", runs.len());
    };
    assert_eq!(first.character_lengths().len(), 255);
    assert_eq!(first.next_on_line(), Some(*second_id));
    assert_eq!(second.previous_on_line(), Some(*first_id));
    assert_eq!(second.value(), Some("abcdef🦀日"));
    assert_eq!(second.character_lengths(), &[1, 1, 1, 1, 1, 1, 4, 3]);
    assert_eq!(
        second.word_starts(),
        &[6, 7],
        "A run boundary inside a word announced a new word"
    );
}

#[test]
fn unrepresentable_graphemes_and_an_ime_caret_inside_a_grapheme_publish_no_text() {
    let oversized = edit(&format!("e{}", "\u{301}".repeat(200)));
    assert!(Document::new(EntityId::from(FIELD), &oversized).is_none());
    assert!(
        request(
            &oversized,
            &select(
                TextPosition {
                    node: NodeId(1),
                    character_index: 0
                },
                TextPosition {
                    node: NodeId(1),
                    character_index: 0
                },
            )
        )
        .is_none()
    );

    let mut composing = Edit::default();
    assert!(composing.replace(None, "e\u{301}", Composition::Mark(Some(1..1))));
    assert!(
        painted(&composing, 0.0).is_none(),
        "A caret inside a grapheme published a partial text range"
    );
}

#[test]
fn scrolling_moves_geometry_without_retiring_runs() {
    let text = edit("a🦀b");
    let still = painted(&text, 0.0).unwrap().runs();
    let scrolled = painted(&text, 15.0).unwrap().runs();
    let [(still_id, still)] = still.as_slice() else {
        panic!("Expected one run");
    };
    let [(scrolled_id, scrolled)] = scrolled.as_slice() else {
        panic!("Expected one run");
    };
    assert_eq!(still_id, scrolled_id);
    assert_eq!(scrolled.value(), Some("a🦀b"));
    assert_eq!(
        scrolled.character_positions(),
        Some([0.0, 20.0, 40.0].as_slice())
    );
    assert_eq!(
        scrolled.character_widths(),
        Some([20.0, 20.0, 20.0].as_slice())
    );
    assert_eq!(still.bounds().unwrap().x0, 20.0);
    assert_eq!(scrolled.bounds().unwrap().x0, -10.0);
    for invalid in [-1.0, f32::NAN, f32::INFINITY] {
        assert!(
            painted(&text, invalid).is_none(),
            "A scroll of {invalid} was published"
        );
    }
}

#[test]
fn a_field_scrolled_out_of_its_clip_publishes_an_empty_rectangle_at_the_clip_edge() {
    let mut separated = frame(0.0);
    separated.visible = separated.bounds.intersect(&Bounds::new(
        point(px(150.0), px(0.0)),
        size(px(50.0), px(100.0)),
    ));
    let rect = separated.physical(separated.visible);
    assert_eq!(rect.width(), 0.0);
    assert_eq!(rect.x0, 300.0);
}

#[test]
fn reveal_requests_name_ranges_of_the_current_revision_and_match_their_action() {
    let mut text = edit("ab🦀c");
    let doc = document(&text);
    let span = TextSelection {
        anchor: doc.position(text.text.len()).unwrap(),
        focus: doc.position(1).unwrap(),
    };
    let reveal = |text: &Edit, action, data: &ActionData| match Request::new(
        action,
        data,
        EntityId::from(FIELD),
        text,
    )? {
        Request::Reveal(span) => Some(span),
        Request::Replace(_) | Request::Select(_) => None,
    };
    let named = ActionData::SetTextSelection(span);
    assert_eq!(
        reveal(&text, AccessibleAction::ScrollIntoView, &named),
        Some(Selection {
            anchor: text.text.len(),
            focus: 1
        })
    );
    assert!(
        Request::new(
            AccessibleAction::SetTextSelection,
            &ActionData::Value("ab".into()),
            EntityId::from(FIELD),
            &text
        )
        .is_none(),
        "A value was accepted as a selection"
    );
    for data in [
        ActionData::ScrollHint(ScrollHint::TopEdge),
        ActionData::SetScrollOffset(Point { x: 10.0, y: 0.0 }),
    ] {
        assert_eq!(reveal(&text, AccessibleAction::ScrollIntoView, &data), None);
    }
    text.set(text.text.clone());
    assert_eq!(
        reveal(&text, AccessibleAction::ScrollIntoView, &named),
        None,
        "A replaced text accepted a reveal from its previous revision"
    );
}
