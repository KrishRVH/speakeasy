use super::*;
use accesskit::{Node as Data, NodeId, Role, TextDirection, Tree as TreeData, TreeId, TreeUpdate};
use accesskit_consumer::Tree;

fn tree(clip: Rect, runs: Vec<Data>) -> Tree {
    let mut root = Data::new(Role::TextInput);
    root.set_children(vec![NodeId(1)]);
    let mut viewport = Data::new(Role::GenericContainer);
    viewport.set_clips_children();
    viewport.set_bounds(clip);
    viewport.set_children(
        (0..runs.len())
            .map(|index| NodeId(index as u64 + 2))
            .collect::<Vec<_>>(),
    );
    let mut nodes = vec![(NodeId(0), root), (NodeId(1), viewport)];
    nodes.extend(
        runs.into_iter()
            .enumerate()
            .map(|(index, node)| (NodeId(index as u64 + 2), node)),
    );
    Tree::new(
        TreeUpdate {
            nodes,
            tree: Some(TreeData::new(NodeId(0))),
            tree_id: TreeId::ROOT,
            focus: NodeId(0),
        },
        true,
    )
}

fn run(text: &str, start: f64, positions: &[f32], widths: &[f32]) -> Data {
    let mut node = Data::new(Role::TextRun);
    node.set_value(text);
    node.set_character_lengths(
        text.chars()
            .map(|ch| ch.len_utf8() as u8)
            .collect::<Vec<_>>(),
    );
    node.set_character_positions(positions.to_vec());
    node.set_character_widths(widths.to_vec());
    node.set_text_direction(TextDirection::LeftToRight);
    let width = positions.last().unwrap_or(&0.0) + widths.last().unwrap_or(&0.0);
    node.set_bounds(Rect::new(start, 10.0, start + f64::from(width), 30.0));
    node
}

#[test]
fn scrolling_clips_ranges_but_keeps_the_complete_unicode_document() {
    let tree = tree(
        Rect::new(5.0, 10.0, 25.0, 30.0),
        vec![run("a🦀bc", 0.0, &[0.0, 10.0, 20.0, 30.0], &[10.0; 4])],
    );
    let root = tree.state().root();
    let document = root.document_range();
    assert_eq!(document.text(), "a🦀bc");
    assert_eq!(
        bounding_rectangles(&document),
        vec![Rect::new(5.0, 10.0, 25.0, 30.0)]
    );
    let visible = visible_ranges(&document);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].text(), "a🦀b");
    let start = root.text_position_from_global_usv_index(3).unwrap();
    let mut hidden = start.to_degenerate_range();
    hidden.set_end(document.end());
    assert!(bounding_rectangles(&hidden).is_empty());
    assert!(bounding_rectangles(&document.start().to_degenerate_range()).is_empty());
}

#[test]
fn continuous_text_across_runs_has_one_visible_range() {
    let mut first = run("ab", 0.0, &[0.0, 10.0], &[10.0; 2]);
    first.set_next_on_line(NodeId(3));
    let mut second = run("cd", 20.0, &[0.0, 10.0], &[10.0; 2]);
    second.set_previous_on_line(NodeId(2));
    let tree = tree(Rect::new(5.0, 10.0, 35.0, 30.0), vec![first, second]);
    let root = tree.state().root();
    let document = root.document_range();
    let visible = visible_ranges(&document);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].text(), "abcd");
    assert_eq!(
        bounding_rectangles(&document),
        vec![Rect::new(5.0, 10.0, 35.0, 30.0)]
    );
}

#[test]
fn fractional_advances_do_not_fragment_one_line() {
    let positions = [22.43115, 35.24895];
    let widths = [12.817799, 8.0];
    assert!(positions[0] + widths[0] < positions[1]);
    let tree = tree(
        Rect::new(0.0, 0.0, 100.0, 40.0),
        vec![run("ab", 0.0, &positions, &widths)],
    );
    let root = tree.state().root();
    let rectangles = bounding_rectangles(&root.document_range());
    assert_eq!(rectangles.len(), 1);
    assert_eq!(rectangles[0].x0, f64::from(positions[0]));
    assert_eq!(rectangles[0].x1, f64::from(positions[1] + widths[1]));
}

#[test]
fn zero_advance_text_requires_a_nonempty_visible_viewport() {
    for clip in [
        Rect::new(10.0, 0.0, 10.0, 40.0),
        Rect::new(0.0, 0.0, 20.0, 0.0),
    ] {
        let tree = tree(clip, vec![run("\u{301}", 10.0, &[0.0], &[0.0])]);
        let root = tree.state().root();
        let document = root.document_range();
        assert!(bounding_rectangles(&document).is_empty());
        let visible = visible_ranges(&document);
        assert_eq!(visible.len(), 1);
        assert!(visible[0].is_degenerate());
    }
    let tree = tree(
        Rect::new(0.0, 0.0, 20.0, 40.0),
        vec![run("\u{301}", 10.0, &[0.0], &[0.0])],
    );
    let root = tree.state().root();
    assert_eq!(visible_ranges(&root.document_range())[0].text(), "\u{301}");
    assert!(bounding_rectangles(&root.document_range()).is_empty());
}

#[test]
fn inverted_clip_cannot_be_normalized_into_a_visible_gap() {
    let tree = tree(
        Rect::new(200.0, 10.0, 100.0, 30.0),
        vec![run("ab", 180.0, &[0.0, 10.0], &[10.0; 2])],
    );
    let root = tree.state().root();
    let document = root.document_range();
    assert_eq!(document.text(), "ab");
    assert!(bounding_rectangles(&document).is_empty());
    let visible = visible_ranges(&document);
    assert_eq!(visible.len(), 1);
    assert!(visible[0].is_degenerate());
}
