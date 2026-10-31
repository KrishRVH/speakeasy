use super::*;
use crate::{FontId, GlyphId, LineLayout, ShapedGlyph, ShapedRun, point, px, size};
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
    builder.pending_text.push(Registration {
        parent,
        line: line(text),
        origin: point(px(-20.0), px(10.0)),
        viewport: Bounds::new(point(px(5.0), px(10.0)), size(px(30.0), px(20.0))),
        line_height: px(20.0),
        scale: 1.5,
        selection: AccessibleTextSelection { anchor: 0, focus },
        revision: 0,
        listener: Box::new(|_, _, _| {}),
    });
    builder.pop();
    parent
}

fn begin(builder: &mut A11yNodeBuilder) {
    builder.begin_frame(size(px(200.0), px(100.0)), 1.5);
}

#[test]
fn scalar_offsets_preserve_unicode_direction_and_reject_foreign_or_oversized_endpoints() {
    let text = "a🦀e\u{301}👩🏽‍💻";
    let mut next_id = 1;
    let snapshot = Snapshot::new(text.to_owned().into(), 0, &mut next_id).unwrap();
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
    let foreign = Snapshot::new(text.to_owned().into(), 0, &mut next_id).unwrap();
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
    let empty = Snapshot::new("".into(), 0, &mut next_id).unwrap();
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
    assert_eq!(runs[1].1.character_lengths(), &[1, 1, 1, 1, 1, 1, 4, 1, 2]);
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
