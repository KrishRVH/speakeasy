use gpui::{
    Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, Point,
    point,
};

use super::*;

#[expect(
    clippy::future_not_send,
    reason = "Deletion acceptance uses GPUI on its UI thread"
)]
pub(super) async fn exercise(
    window: WindowHandle<Review>,
    previous: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let parent = previous
        .parent()
        .context("The fixture has a Shows folder")?;
    let mut paths = Vec::new();
    for _ in 0..2 {
        let path = crate::show::store::fresh_directory(parent, SystemTime::now())?;
        fs::create_dir(&path)?;
        fs::write(path.join("show.json"), b"damaged synthetic manifest")?;
        fs::write(path.join("summary.md"), b"synthetic edited summary")?;
        fs::write(path.join(".pinned"), b"")?;
        paths.push(path);
    }
    let first = paths.first().context("The first fixture exists")?;
    let second = paths.get(1).context("The second fixture exists")?;
    window.update(cx, |view, _, cx| view.refresh(cx))?;
    wait_for(
        window,
        |view| view.listing.is_none() && view.can_delete(),
        cx,
    )
    .await?;
    select(window, first, cx).await?;
    let (trashed, trash) = trash_of(window, second, cx)?;
    window.update(cx, |view, _, _| {
        view.export = Some(Export::Choosing {
            _task: Task::ready(()),
        });
    })?;
    press(window, trash, cx)?;
    window.update(cx, |view, _, _| {
        ensure!(
            view.library.is_selected(first) && view.confirm.is_none(),
            "A dimmed trash opened the row beneath it"
        );
        view.export = None;
        anyhow::Ok(())
    })??;
    press(window, trash, cx)?;
    window.update(cx, |view, _, cx| {
        ensure!(
            view.library.is_selected(second) && view.confirm == Some(Confirm::Delete(trashed)),
            "A row's trash didn't select its own Show and ask to delete it"
        );
        view.act(Action::CancelDelete, cx);
        anyhow::Ok(())
    })??;
    select(window, first, cx).await?;
    let offered = window.update(cx, |view, _, cx| {
        let offer = view
            .library
            .position()
            .context("The fixture Show is selected")?;
        view.act(Action::ConfirmDelete(offer), cx);
        assert!(view.write.is_none());
        view.menu = menu::Menu::Open(None);
        anyhow::Ok(offer)
    })??;
    key(window, "up", cx)?;
    release(window, "enter", cx)?;
    window.read_with(cx, |view, _| {
        assert!(view.confirm == Some(Confirm::Delete(offered)));
        assert!(view.write.is_none());
    })?;
    assert!(first.is_dir());
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))?;
    window.update(cx, |view, _, cx| view.act(Action::Delete(offered), cx))?;
    assert!(first.is_dir());
    window.update(cx, |view, _, _| {
        view.export = Some(Export::Choosing {
            _task: Task::ready(()),
        });
        assert!(!view.can_delete(), "An export left deletion available");
        assert!(
            view.can_dismiss(),
            "An export disabled the delete confirmation's Cancel"
        );
    })?;
    key(window, "escape", cx)?;
    window.update(cx, |view, _, _| {
        assert!(view.confirm.is_none());
        view.export = None;
    })?;

    select(window, first, cx).await?;
    window.update(cx, |view, _, cx| {
        let collection = view.collection;
        let can_copy = view.actions.can_copy;
        view.collection = Collection::Saved;
        view.actions.can_copy = |_| true;
        let offer = view
            .library
            .position()
            .context("The fixture Show is selected")?;
        view.act(Action::Delete(offer), cx);
        view.act(Action::ConfirmDelete(offer), cx);
        assert!(matches!(view.write, Some(Write::Deleting { .. })));
        assert!(
            !view.can_refresh(),
            "Refresh was offered while a deletion held the library"
        );
        assert!(view.detail.is_none() && matches!(view.image, ImageState::Empty));
        assert!(!view.can_copy(cx));
        let next = view
            .library
            .rows
            .iter()
            .position(|row| row.directory == *second)
            .context("The second fixture is listed")?;
        view.act(Action::Show(next), cx);
        assert!(view.can_copy(cx));
        view.collection = collection;
        view.actions.can_copy = can_copy;
        view.notice = Some("Synthetic newer selection notice".into());
        anyhow::Ok(())
    })??;
    wait_for(window, |view| view.write.is_none() && view.can_delete(), cx).await?;
    assert!(!first.exists() && second.is_dir());
    window.read_with(cx, |view, _| {
        assert!(view.library.is_selected(second));
        assert_eq!(
            view.notice.as_deref(),
            Some("Synthetic newer selection notice")
        );
        assert!(!view.library.rows.iter().any(|row| row.directory == *first));
    })?;
    window.update(cx, |view, _, cx| {
        let offer = view
            .library
            .position()
            .context("The fixture Show is selected")?;
        view.act(Action::Delete(offer), cx);
        view.act(Action::ConfirmDelete(offer), cx);
        anyhow::Ok(())
    })??;
    wait_for(window, |view| view.write.is_none(), cx).await?;
    assert!(!second.exists());
    window.read_with(cx, |view, _| {
        assert!(view.library.selected().is_none() && view.library.is_gone());
        assert!(view.detail.is_none() && matches!(view.image, ImageState::Empty));
    })?;
    select(window, previous, cx).await?;
    assert!(previous.join("summary.md").is_file());
    Ok(())
}

/// The listed row for `path` and the center of its trash button, top right in the row.
fn trash_of(
    window: WindowHandle<Review>,
    path: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<(usize, Point<Pixels>)> {
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))?;
    window.read_with(cx, |view, _| {
        let index = view
            .library
            .rows
            .iter()
            .position(|row| row.directory == path)
            .context("The fixture is listed")?;
        let row = view
            .library_scroll
            .bounds_for_item(index)
            .context("The library has no bounds for the fixture's row")?;
        let offset = view.library_scroll.offset();
        Ok((
            index,
            point(
                row.right() + offset.x - px(20.0),
                row.top() + offset.y + px(20.0),
            ),
        ))
    })?
}

fn press(
    window: WindowHandle<Review>,
    position: Point<Pixels>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    cx.update_window(window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
            }),
            cx,
        );
    })
}

#[expect(
    clippy::future_not_send,
    reason = "Owned Show selection uses GPUI on its UI thread"
)]
async fn select(
    window: WindowHandle<Review>,
    path: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    window.update(cx, |view, _, cx| {
        let index = view
            .library
            .rows
            .iter()
            .position(|row| row.directory == path)
            .context("The fixture is listed")?;
        view.act(Action::Show(index), cx);
        anyhow::Ok(())
    })??;
    wait_for(window, Review::can_delete, cx).await
}
