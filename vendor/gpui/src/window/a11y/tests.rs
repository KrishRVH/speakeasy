use super::*;
use crate::{self as gpui, TestAppContext, div, point, prelude::*, px, size};

fn owner() -> A11y {
    let mut owner = A11y::new(
        Arc::new(AtomicBool::new(true)),
        Arc::new(AtomicU64::new(0)),
        Vec::new(),
    );
    owner.sync_active_flag();
    owner
}

fn frame(owner: &mut A11y) {
    owner.begin_frame(size(px(200.0), px(100.0)), 1.5);
}

fn button(owner: &mut A11y, key: u64) -> NodeId {
    let key = ElementKey(key);
    assert!(
        owner
            .nodes
            .push(key, accesskit::Node::new(accesskit::Role::Button))
    );
    let id = owner.nodes.native_id(key).unwrap();
    owner.nodes.pop();
    id
}

#[test]
fn ordinary_redraw_keeps_actions_but_removal_retires_native_identity() {
    let mut owner = owner();
    frame(&mut owner);
    let old = button(&mut owner, 100);
    owner.end_frame();
    frame(&mut owner);
    assert_eq!(button(&mut owner, 100), old);
    owner.end_frame();
    assert!(owner.accepts(0, old));

    frame(&mut owner);
    owner.end_frame();
    assert!(!owner.accepts(0, old));
    frame(&mut owner);
    let replacement = button(&mut owner, 100);
    owner.end_frame();
    assert_ne!(replacement, old);
    assert!(!owner.accepts(0, old));
    assert!(owner.accepts(0, replacement));
}

#[test]
fn hide_invalidates_queued_and_delayed_provider_actions() {
    let mut owner = owner();
    frame(&mut owner);
    let old = button(&mut owner, 100);
    owner.end_frame();
    owner.revision.fetch_add(1, Ordering::SeqCst);
    assert!(!owner.accepts(0, old));
    frame(&mut owner);
    let replacement = button(&mut owner, 100);
    owner.end_frame();
    assert_ne!(old, replacement);
    assert!(!owner.accepts(1, old));
    assert!(owner.accepts(1, replacement));
    owner.active_flag.store(false, Ordering::SeqCst);
    assert!(!owner.accepts(1, replacement));
}

#[test]
fn rejected_layout_restores_parent_children_and_focus() {
    let mut owner = owner();
    frame(&mut owner);
    let first = button(&mut owner, 100);
    owner.nodes.set_focus(ElementKey(100));
    let checkpoint = owner.nodes.checkpoint();
    let aborted = button(&mut owner, 101);
    owner
        .focus_ids
        .insert(aborted, FocusId::from(slotmap::KeyData::from_ffi(1)));
    owner.rollback(checkpoint);
    assert!(!owner.nodes.contains_native(aborted));
    assert!(owner.focus_ids.is_empty());
    let replacement = button(&mut owner, 101);
    let update = owner.end_frame();
    let root = update
        .nodes
        .iter()
        .find(|(id, _)| *id == ROOT_NODE_ID)
        .unwrap();
    assert_eq!(root.1.children(), &[first, replacement]);
    assert_eq!(update.focus, first);
    assert_eq!(update.nodes.len(), 3);
}

#[test]
fn deferred_child_keeps_its_accessible_container() {
    let mut owner = owner();
    frame(&mut owner);
    assert!(owner.nodes.push(
        ElementKey(100),
        accesskit::Node::new(accesskit::Role::TabList)
    ));
    let group = owner.nodes.native_id(ElementKey(100)).unwrap();
    owner.nodes.pop();
    let sibling = button(&mut owner, 101);
    owner.nodes.scope_parent = Some(group);
    let child = button(&mut owner, 102);
    owner.nodes.scope_parent = Some(ROOT_NODE_ID);
    let update = owner.end_frame();
    let node = |id| {
        &update
            .nodes
            .iter()
            .find(|(candidate, _)| *candidate == id)
            .unwrap()
            .1
    };
    assert_eq!(node(ROOT_NODE_ID).children(), &[group, sibling]);
    assert_eq!(node(group).children(), &[child]);
    assert_eq!(
        node(ROOT_NODE_ID).bounds(),
        Some(accesskit::Rect::new(0.0, 0.0, 300.0, 150.0))
    );
}

#[gpui::test]
fn hidden_controls_and_descendants_are_not_published(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    cx.update(|window, cx| {
        window.a11y = owner();
        frame(&mut window.a11y);
        window
            .invalidator
            .set_phase(crate::window::DrawPhase::Prepaint);
        let mut invisible = div()
            .id("invisible")
            .role(accesskit::Role::Group)
            .aria_label("Synthetic hidden group")
            .child(
                div()
                    .id("child")
                    .role(accesskit::Role::Button)
                    .aria_label("Synthetic hidden child"),
            );
        invisible.style().visibility = Some(crate::Visibility::Hidden);
        let mut element = div()
            .children([
                div()
                    .id("display-none")
                    .role(accesskit::Role::Button)
                    .aria_label("Synthetic hidden button")
                    .hidden(),
                invisible,
            ])
            .into_any_element();
        element.prepaint_as_root(
            point(px(0.0), px(0.0)),
            size(px(200.0), px(100.0)).into(),
            window,
            cx,
        );
        window
            .invalidator
            .set_phase(crate::window::DrawPhase::Paint);
        element.paint(window, cx);
        window.invalidator.set_phase(crate::window::DrawPhase::None);
        let update = window.a11y.end_frame();
        assert_eq!(update.nodes.len(), 1);
        assert_eq!(update.focus, ROOT_NODE_ID);
        assert!(window.a11y.action_listeners.is_empty());
    });
}

#[gpui::test]
fn aborted_window_transaction_retries_at_final_bounds(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    cx.update(|window, cx| {
        window.a11y = owner();
        frame(&mut window.a11y);
        window
            .invalidator
            .set_phase(crate::window::DrawPhase::Prepaint);
        let element = || {
            div()
                .id("retry")
                .role(accesskit::Role::Button)
                .aria_label("Synthetic retry")
                .w(px(20.0))
                .h(px(10.0))
                .into_any_element()
        };
        let mut aborted = element();
        let result = window.transact(|window| {
            aborted.prepaint_as_root(
                point(px(0.0), px(0.0)),
                size(px(200.0), px(100.0)).into(),
                window,
                cx,
            );
            Err::<(), _>(())
        });
        assert!(result.is_err());
        let mut final_element = element();
        final_element.prepaint_as_root(
            point(px(30.0), px(20.0)),
            size(px(200.0), px(100.0)).into(),
            window,
            cx,
        );
        window.invalidator.set_phase(crate::window::DrawPhase::None);
        let update = window.a11y.end_frame();
        assert_eq!(update.nodes.len(), 2);
        let button = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == accesskit::Role::Button)
            .unwrap();
        let scale = f64::from(window.scale_factor());
        assert_eq!(
            button.1.bounds(),
            Some(accesskit::Rect::new(
                30.0 * scale,
                20.0 * scale,
                50.0 * scale,
                30.0 * scale
            ))
        );
    });
}
