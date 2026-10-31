use super::*;
use accesskit::{
    Affine, Node as Data, NodeId as LocalId, Role, TextDirection, Tree as TreeData, TreeId,
    TreeUpdate,
};
use accesskit_consumer::Tree;

fn tree(offset: f64, limit: f64, viewport: Rect, transform: Affine) -> Tree {
    let mut root = Data::new(Role::TextInput);
    root.set_children(vec![LocalId(1)]);
    let mut clip = Data::new(Role::GenericContainer);
    clip.set_bounds(viewport);
    clip.set_transform(transform);
    clip.set_clips_children();
    clip.set_scroll_x(offset);
    clip.set_scroll_x_min(0.0);
    clip.set_scroll_x_max(limit);
    clip.add_action(Action::SetScrollOffset);
    clip.set_children(vec![LocalId(2), LocalId(3)]);
    let mut nodes = vec![(LocalId(0), root), (LocalId(1), clip)];
    for (index, text) in ["abcdefghi", "jklmnopqrst"].into_iter().enumerate() {
        let mut node = Data::new(Role::TextRun);
        node.set_value(text);
        node.set_character_lengths(vec![1; text.len()]);
        node.set_character_positions(
            (0..text.len())
                .map(|index| index as f32 * 10.0)
                .collect::<Vec<_>>(),
        );
        node.set_character_widths(vec![10.0; text.len()]);
        node.set_text_direction(TextDirection::LeftToRight);
        let left = 10.0 + index as f64 * 90.0 - offset;
        node.set_bounds(Rect::new(left, 20.0, left + text.len() as f64 * 10.0, 40.0));
        if index == 0 {
            node.set_next_on_line(LocalId(3));
        } else {
            node.set_previous_on_line(LocalId(2));
        }
        nodes.push((LocalId(index as u64 + 2), node));
    }
    Tree::new(
        TreeUpdate {
            nodes,
            tree: Some(TreeData::new(LocalId(0))),
            tree_id: TreeId::ROOT,
            focus: LocalId(0),
        },
        true,
    )
}

fn range<'a>(root: &'a Node<'_>, start: usize, end: usize) -> TextRange<'a> {
    let mut range = root
        .text_position_from_global_usv_index(start)
        .unwrap()
        .to_degenerate_range();
    range.set_end(root.text_position_from_global_usv_index(end).unwrap());
    range
}

fn viewport() -> Rect {
    Rect::new(10.0, 20.0, 50.0, 40.0)
}

#[test]
fn exact_positions_in_one_run_and_exclusive_ends_choose_the_minimum_offset() {
    let tree = tree(0.0, 163.0, viewport(), Affine::IDENTITY);
    let root = tree.state().root();
    assert_eq!(reveal(&range(&root, 5, 6), true).unwrap().point.x, 20.0);
    assert_eq!(reveal(&range(&root, 7, 8), false).unwrap().point.x, 40.0);
    assert_eq!(reveal(&range(&root, 0, 10), true).unwrap().point.x, 0.0);
    assert_eq!(reveal(&range(&root, 0, 10), false).unwrap().point.x, 60.0);
    assert_eq!(root.document_range().text(), "abcdefghijklmnopqrst");
}

#[test]
fn an_already_visible_later_request_restores_the_published_offset() {
    let tree = tree(60.0, 163.0, viewport(), Affine::IDENTITY);
    let root = tree.state().root();
    let first = reveal(&range(&root, 2, 3), true).unwrap();
    let later = reveal(&range(&root, 9, 10), true).unwrap();
    assert_eq!(first.target, later.target);
    assert_eq!(first.point.x, 20.0);
    assert_eq!(later.point.x, 60.0);
    let after_first = tree_for_offset(first.point.x);
    assert!(super::super::geometry::bounding_rectangles(&range(
        &after_first.state().root(),
        9,
        10
    ))
    .is_empty());
    let after_later = tree_for_offset(later.point.x);
    assert!(!super::super::geometry::bounding_rectangles(&range(
        &after_later.state().root(),
        9,
        10
    ))
    .is_empty());
}

fn tree_for_offset(offset: f64) -> Tree {
    tree(offset, 163.0, viewport(), Affine::IDENTITY)
}

#[test]
fn cross_run_ranges_and_carets_use_container_coordinates_at_fractional_scale() {
    let tree = tree(
        0.0,
        163.0,
        viewport(),
        Affine::new([1.75, 0.0, 0.0, 1.75, 123.5, 47.25]),
    );
    let root = tree.state().root();
    for align in [true, false] {
        assert!((reveal(&range(&root, 8, 12), align).unwrap().point.x - 80.0).abs() < 0.000_001);
    }
    assert!((reveal(&range(&root, 20, 20), true).unwrap().point.x - 161.0).abs() < 0.000_001);
    assert!(reveal(&range(&root, 0, 0), true).unwrap().point.x.abs() < 0.000_001);
}

#[test]
fn empty_inverted_unreachable_and_nonfinite_viewports_are_unsupported() {
    for bounds in [
        Rect::new(10.0, 20.0, 10.0, 40.0),
        Rect::new(50.0, 20.0, 10.0, 40.0),
        Rect::new(10.0, 20.0, 50.0, 20.0),
        Rect::new(10.0, 22.0, 50.0, 40.0),
    ] {
        let tree = tree(0.0, 163.0, bounds, Affine::IDENTITY);
        assert!(reveal(&range(&tree.state().root(), 0, 1), true).is_none());
    }
    let clipped = tree(
        0.0,
        163.0,
        Rect::new(10.0, 20.0, 30.0, 40.0),
        Affine::IDENTITY,
    );
    assert!(reveal(&range(&clipped.state().root(), 19, 20), false).is_none());
    for (offset, limit) in [(f64::NAN, 163.0), (0.0, f64::INFINITY), (0.0, -1.0)] {
        let tree = tree(offset, limit, viewport(), Affine::IDENTITY);
        assert!(reveal(&range(&tree.state().root(), 0, 1), true).is_none());
    }
}

#[test]
fn empty_documents_and_zero_advance_characters_reveal_their_position() {
    for (text, lengths, positions, widths) in [
        ("", vec![], vec![], vec![]),
        ("\u{301}", vec![2], vec![0.0], vec![0.0]),
    ] {
        let mut root = Data::new(Role::TextInput);
        root.set_children(vec![LocalId(1)]);
        let mut clip = Data::new(Role::GenericContainer);
        clip.set_children(vec![LocalId(2)]);
        clip.set_bounds(viewport());
        clip.set_clips_children();
        clip.set_scroll_x(0.0);
        clip.set_scroll_x_min(0.0);
        clip.set_scroll_x_max(100.0);
        clip.add_action(Action::SetScrollOffset);
        let mut run = Data::new(Role::TextRun);
        run.set_value(text);
        run.set_character_lengths(lengths);
        run.set_character_positions(positions);
        run.set_character_widths(widths);
        run.set_text_direction(TextDirection::LeftToRight);
        run.set_bounds(Rect::new(70.0, 20.0, 70.0, 40.0));
        let tree = Tree::new(
            TreeUpdate {
                nodes: vec![(LocalId(0), root), (LocalId(1), clip), (LocalId(2), run)],
                tree: Some(TreeData::new(LocalId(0))),
                tree_id: TreeId::ROOT,
                focus: LocalId(0),
            },
            true,
        );
        let root = tree.state().root();
        assert_eq!(reveal(&root.document_range(), true).unwrap().point.x, 21.0);
    }
}
