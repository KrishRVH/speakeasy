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
    let offered = window.update(cx, |view, _, cx| {
        let offer = view.library.offer(
            view.library
                .position()
                .context("The fixture Show is selected")?,
        );
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

    window.update(cx, |view, _, cx| view.act(Action::Delete(offered), cx))?;
    select(window, second, cx).await?;
    let newer = window.update(cx, |view, _, cx| {
        let newer = view.library.offer(
            view.library
                .position()
                .context("The fixture Show is selected")?,
        );
        view.act(Action::Delete(newer), cx);
        view.act(Action::ConfirmDelete(offered), cx);
        assert!(view.confirm == Some(Confirm::Delete(newer)) && view.write.is_none());
        view.act(Action::Refresh, cx);
        assert!(view.confirm.is_none());
        view.act(Action::ConfirmDelete(newer), cx);
        assert!(view.write.is_none());
        anyhow::Ok(newer)
    })??;
    assert!(first.is_dir() && second.is_dir());
    wait_for(
        window,
        |view| view.listing.is_none() && view.can_delete(),
        cx,
    )
    .await?;
    window.update(cx, |view, _, cx| {
        view.act(Action::Delete(newer), cx);
        assert!(view.confirm.is_none());
    })?;

    select(window, first, cx).await?;
    window.update(cx, |view, _, cx| {
        let source = view.source;
        let can_copy = view.actions.can_copy;
        view.source = Source::Saved;
        view.actions.can_copy = |_| true;
        let offer = view.library.offer(
            view.library
                .position()
                .context("The fixture Show is selected")?,
        );
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
        view.act(Action::Show(view.library.offer(next)), cx);
        assert!(view.can_copy(cx));
        view.source = source;
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
        let offer = view.library.offer(
            view.library
                .position()
                .context("The fixture Show is selected")?,
        );
        view.act(Action::Delete(offer), cx);
        view.act(Action::ConfirmDelete(offer), cx);
        anyhow::Ok(())
    })??;
    wait_for(window, |view| view.write.is_none(), cx).await?;
    assert!(!second.exists());
    window.read_with(cx, |view, _| {
        assert!(view.library.selected.is_none() && view.library.gone);
        assert!(view.detail.is_none() && matches!(view.image, ImageState::Empty));
    })?;
    select(window, previous, cx).await?;
    assert!(previous.join("summary.md").is_file());
    Ok(())
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
        view.act(Action::Show(view.library.offer(index)), cx);
        anyhow::Ok(())
    })??;
    wait_for(window, Review::can_delete, cx).await
}
