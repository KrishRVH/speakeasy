use super::*;
use crate::{
    self as gpui, FontId, GlyphId, LineLayout, ShapedGlyph, ShapedRun, TestAppContext, point, px,
    size,
};
use std::sync::Arc;

fn line(text: &str) -> ShapedLine {
    let glyphs = text
        .char_indices()
        .enumerate()
        .map(|(index, (offset, _))| ShapedGlyph {
            id: GlyphId(1),
            position: point(px(index as f32 * 10.0), px(0.0)),
            index: offset,
            is_emoji: false,
        })
        .collect();
    ShapedLine {
        text: text.to_owned().into(),
        layout: Arc::new(LineLayout {
            width: px(text.chars().count() as f32 * 10.0),
            len: text.len(),
            runs: vec![ShapedRun {
                font_id: FontId(0),
                glyphs,
            }],
            ..LineLayout::default()
        }),
        ..ShapedLine::default()
    }
}

fn register(builder: &mut A11yNodeBuilder, text: &str, focus: usize) -> NodeId {
    let key = super::super::ElementKey(7);
    assert!(builder.push(key, Node::new(Role::TextInput)));
    let parent = builder.native_id(key).unwrap();
    builder.pending_text.push(registration(parent, text, focus));
    builder.pop();
    parent
}

fn registration(parent: NodeId, text: &str, focus: usize) -> Registration {
    Registration {
        parent,
        line: line(text),
        origin: point(px(-20.0), px(10.0)),
        viewport: Bounds::new(point(px(5.0), px(10.0)), size(px(30.0), px(20.0))),
        full_viewport: Bounds::new(point(px(5.0), px(10.0)), size(px(30.0), px(20.0))),
        scroll_offset: px(0.0),
        scroll_limit: px(100.0),
        line_height: px(20.0),
        scale: 1.5,
        selection: AccessibleTextSelection { anchor: 0, focus },
        revision: 0,
        listener: Box::new(|_, _, _| {}),
        scroll_listener: Box::new(|_, _, _| {}),
    }
}

fn begin(builder: &mut A11yNodeBuilder) {
    builder.begin_frame(size(px(200.0), px(100.0)), 1.5);
}

#[test]
fn grapheme_offsets_preserve_direction_and_reject_partial_or_foreign_endpoints() {
    let text = "a🦀e\u{301}👩🏽‍💻";
    let mut next_id = 1;
    let snapshot = Snapshot::new(&registration(NodeId(0), text, 0), &mut next_id).unwrap();
    let native = TextSelection {
        anchor: snapshot.position(text.len()).unwrap(),
        focus: snapshot.position(1).unwrap(),
    };
    assert_eq!(
        selection(&snapshot.runs, &native),
        Some(AccessibleTextSelection {
            anchor: text.len(),
            focus: 1
        })
    );
    assert!(snapshot.position(2).is_none());
    assert!(snapshot.position(6).is_none());
    assert!(snapshot.position(12).is_none());
    let foreign = Snapshot::new(&registration(NodeId(0), text, 0), &mut next_id).unwrap();
    assert!(
        selection(
            &snapshot.runs,
            &TextSelection {
                focus: foreign.position(1).unwrap(),
                ..native
            }
        )
        .is_none()
    );
    assert!(
        selection(
            &snapshot.runs,
            &TextSelection {
                focus: TextPosition {
                    character_index: usize::MAX,
                    ..native.focus
                },
                ..native
            }
        )
        .is_none()
    );
    let empty = Snapshot::new(&registration(NodeId(0), "", 0), &mut next_id).unwrap();
    assert_eq!(empty.position(0).unwrap().character_index, 0);
    assert!(empty.position(1).is_none());
}

#[test]
fn long_words_cross_runs_without_new_word_or_line_boundaries() {
    let text = format!("{} 🦀e\u{301}", "a".repeat(260));
    let mut builder = A11yNodeBuilder::new();
    begin(&mut builder);
    register(&mut builder, &text, text.len());
    builder.finish_text();
    let update = builder.finalize();
    let runs: Vec<_> = update
        .nodes
        .iter()
        .filter(|(_, node)| node.role() == Role::TextRun)
        .collect();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].1.character_lengths().len(), 255);
    assert_eq!(runs[0].1.next_on_line(), Some(runs[1].0));
    assert_eq!(runs[1].1.previous_on_line(), Some(runs[0].0));
    assert!(!runs[1].1.word_starts().contains(&0));
    assert_eq!(runs[1].1.word_starts(), &[6, 7]);
    assert_eq!(runs[1].1.character_lengths(), &[1, 1, 1, 1, 1, 1, 4, 3]);
}

#[test]
fn unrepresentable_graphemes_retire_ranges_without_splitting_or_losing_the_value() {
    let representable = format!("a{}", "\u{301}".repeat(127));
    let oversized = format!("{representable}\u{301}");
    let mut builder = A11yNodeBuilder::new();
    begin(&mut builder);
    let parent = register(&mut builder, &representable, representable.len());
    builder.finish_text();
    let original = builder.text_snapshots[&parent].position(0).unwrap();
    let update = builder.finalize();
    let run = update.nodes.iter().find(|(_, node)| node.role() == Role::TextRun).unwrap();
    assert_eq!(run.1.character_lengths(), &[255]);

    begin(&mut builder);
    register(&mut builder, &oversized, oversized.len());
    builder.node_mut(parent).unwrap().set_value(oversized.clone());
    assert!(builder.finish_text().is_empty());
    assert!(builder.text_snapshots.is_empty());
    assert!(!builder.contains_native(original.node));
    let update = builder.finalize();
    let field = update.nodes.iter().find(|(id, _)| *id == parent).unwrap();
    assert_eq!(field.1.value(), Some(oversized.as_str()));
    assert!(field.1.text_selection().is_none());
    assert!(update.nodes.iter().all(|(_, node)| node.role() != Role::TextRun));

    begin(&mut builder);
    register(&mut builder, &representable, representable.len());
    builder.finish_text();
    assert_ne!(builder.text_snapshots[&parent].position(0), Some(original));
}

#[test]
fn text_snapshots_retire_after_edits_or_absence_but_survive_selection() {
    let mut builder = A11yNodeBuilder::new();
    begin(&mut builder);
    let parent = register(&mut builder, "abc", 3);
    builder.finish_text();
    let original = builder.text_snapshots[&parent].position(1).unwrap();
    builder.finalize();
    begin(&mut builder);
    assert_eq!(register(&mut builder, "abc", 1), parent);
    builder.finish_text();
    assert_eq!(builder.text_snapshots[&parent].position(1), Some(original));
    builder.finalize();
    begin(&mut builder);
    register(&mut builder, "abc", 1);
    builder.pending_text[0].revision = 1;
    builder.finish_text();
    assert_ne!(builder.text_snapshots[&parent].position(1), Some(original));
    builder.finalize();
    begin(&mut builder);
    register(&mut builder, "xyz", 1);
    builder.finish_text();
    assert_ne!(builder.text_snapshots[&parent].position(1), Some(original));
    assert!(!builder.contains_native(original.node));
    builder.finalize();
    begin(&mut builder);
    let key = super::super::ElementKey(7);
    assert!(builder.push(key, Node::new(Role::TextInput)));
    builder.pop();
    builder.finish_text();
    builder.finalize();
    assert!(builder.text_snapshots.is_empty());
}

#[test]
fn aborted_text_does_not_leave_parent_selection_or_live_run_ids() {
    let mut builder = A11yNodeBuilder::new();
    begin(&mut builder);
    let checkpoint = builder.checkpoint();
    register(&mut builder, "rejected", 8);
    builder.rollback(checkpoint);
    assert!(builder.finish_text().is_empty());
    let update = builder.finalize();
    assert_eq!(update.nodes.len(), 1);
    assert!(update.nodes[0].1.text_selection().is_none());
    assert!(builder.text_snapshots.is_empty());
}

#[test]
fn scrolling_changes_clip_and_geometry_without_changing_document() {
    let mut builder = A11yNodeBuilder::new();
    begin(&mut builder);
    let parent = register(&mut builder, "a🦀b", 1);
    builder.finish_text();
    let update = builder.finalize();
    let run = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::TextRun)
        .unwrap();
    assert_eq!(run.1.value(), Some("a🦀b"));
    assert_eq!(
        run.1.bounds(),
        Some(accesskit::Rect::new(-30.0, 15.0, 15.0, 45.0))
    );
    assert_eq!(
        run.1.character_positions(),
        Some([0.0, 15.0, 30.0].as_slice())
    );
    let clip = update
        .nodes
        .iter()
        .find(|(_, node)| node.clips_children())
        .unwrap();
    assert_eq!(
        clip.1.bounds(),
        Some(accesskit::Rect::new(7.5, 15.0, 52.5, 45.0))
    );
    let field = update.nodes.iter().find(|(id, _)| *id == parent).unwrap();
    assert_eq!(field.1.text_selection().unwrap().focus.character_index, 1);
}

#[test]
fn separated_viewport_and_ancestor_publish_no_visible_gap() {
    let mut builder = A11yNodeBuilder::new();
    begin(&mut builder);
    register(&mut builder, "clipped", 7);
    builder.pending_text[0].viewport =
        Bounds::new(point(px(200.0), px(10.0)), size(px(100.0), px(20.0))).intersect(&Bounds::new(
            point(px(0.0), px(10.0)),
            size(px(100.0), px(20.0)),
        ));
    builder.finish_text();
    let update = builder.finalize();
    let clip = update
        .nodes
        .iter()
        .find(|(_, node)| node.clips_children())
        .unwrap();
    assert!(clip.1.bounds().unwrap().is_empty());
    assert_eq!(clip.1.bounds().unwrap().x0, 300.0);
}

#[test]
fn scrolling_preserves_action_identity_while_layout_changes_retire_only_the_container() {
    let mut builder = A11yNodeBuilder::new();
    begin(&mut builder);
    let parent = register(&mut builder, "abc", 1);
    builder.finish_text();
    let original_clip = builder.text_snapshots[&parent].scroll.id;
    let original_text = builder.text_snapshots[&parent].position(1).unwrap();
    builder.finalize();
    begin(&mut builder);
    register(&mut builder, "abc", 2);
    builder.pending_text[0].scroll_offset = px(15.0);
    builder.finish_text();
    assert_eq!(builder.text_snapshots[&parent].scroll.id, original_clip);
    assert_eq!(
        builder.text_snapshots[&parent].position(1),
        Some(original_text)
    );
    builder.finalize();

    let changes: [fn(&mut Registration); 7] = [
        |registration| registration.origin.x += px(1.0),
        |registration| registration.viewport.size.width -= px(1.0),
        |registration| registration.full_viewport.size.width += px(1.0),
        |registration| registration.scale = 2.0,
        |registration| registration.line_height += px(1.0),
        |registration| registration.scroll_limit += px(1.0),
        |registration| Arc::get_mut(&mut registration.line.layout).unwrap().width += px(1.0),
    ];
    for change in changes {
        begin(&mut builder);
        register(&mut builder, "abc", 1);
        builder.finish_text();
        let previous = builder.text_snapshots[&parent].scroll.id;
        builder.finalize();
        begin(&mut builder);
        register(&mut builder, "abc", 1);
        change(&mut builder.pending_text[0]);
        builder.finish_text();
        assert_ne!(builder.text_snapshots[&parent].scroll.id, previous);
        assert!(!builder.contains_native(previous));
        assert_eq!(
            builder.text_snapshots[&parent].position(1),
            Some(original_text)
        );
        builder.finalize();
    }
}

#[gpui::test]
fn scroll_actions_use_admitted_scale_and_reject_invalid_offsets(cx: &mut TestAppContext) {
    let mut builder = A11yNodeBuilder::new();
    begin(&mut builder);
    register(&mut builder, "abc", 1);
    let (sent, received) = std::sync::mpsc::channel();
    builder.pending_text[0].scroll_listener =
        Box::new(move |offset, _, _| sent.send(offset).unwrap());
    let (_, _, mut listener) = builder
        .finish_text()
        .into_iter()
        .find(|(_, action, _)| *action == Action::SetScrollOffset)
        .unwrap();
    let cx = cx.add_empty_window();
    cx.update(|window, cx| {
        listener(
            Some(&ActionData::SetScrollOffset(accesskit::Point::new(
                30.0, 0.0,
            ))),
            window,
            cx,
        );
        assert_eq!(received.try_recv().unwrap(), px(20.0));
        for point in [
            accesskit::Point::new(f64::NAN, 0.0),
            accesskit::Point::new(f64::INFINITY, 0.0),
            accesskit::Point::new(-1.0, 0.0),
            accesskit::Point::new(151.0, 0.0),
            accesskit::Point::new(30.0, 1.0),
        ] {
            listener(Some(&ActionData::SetScrollOffset(point)), window, cx);
        }
        assert!(matches!(
            received.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
    });
}
