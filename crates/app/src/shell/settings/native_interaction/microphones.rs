use super::*;

fn devices(ids: &[&str]) -> Vec<InputDevice> {
    ids.iter()
        .map(|id| InputDevice {
            id: (*id).into(),
            name: format!("Microphone {id}"),
        })
        .collect()
}

#[expect(
    clippy::future_not_send,
    reason = "The microphone menu acceptance drives GPUI on its UI thread"
)]
pub(super) async fn exercise(
    settings: WindowHandle<Settings>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    settings.update(cx, |view, window, cx| {
        view.microphones_listed(Ok(devices(&["first", "second", "third"])), cx);
        view.toggle_menu(Menu::Microphone, window, cx);
        view.menus.cursor = 2;
        cx.notify();
    })?;
    settle(cx).await;
    settings.update(cx, |view, _, cx| {
        view.microphones_listed(Ok(devices(&["third", "second"])), cx);
        let rows = view.menu_items(Menu::Microphone);
        ensure!(
            rows.get(2)
                .is_some_and(|item| item.label.as_ref() == "Microphone second"),
            "A microphone refresh moved the displayed choice"
        );
        ensure!(
            rows.get(1)
                .is_some_and(|item| item.label.as_ref() == "Microphone first (disconnected)"),
            "A disconnected microphone lost its row"
        );
        anyhow::Ok(())
    })??;
    key(settings, "enter", cx)?;
    settings.update(cx, |view, window, cx| {
        ensure!(
            view.config.microphone.as_deref() == Some("second"),
            "A microphone refresh changed the selected device"
        );
        view.toggle_menu(Menu::Microphone, window, cx);
        ensure!(
            view.microphones.len() == 2 && view.menus.cursor == 1,
            "Reopening did not compact disconnected rows before positioning the cursor"
        );
        view.microphones_listed(Ok(devices(&["third"])), cx);
        view.menus.cursor = 1;
        anyhow::Ok(())
    })??;
    key(settings, "enter", cx)?;
    settings.update(cx, |view, window, cx| {
        ensure!(
            view.config.microphone.as_deref() == Some("second") && view.menus.open.is_none(),
            "A disconnected microphone changed the setting or left the menu open"
        );
        view.act(Action::SelectMicrophone(None), window, cx);
        anyhow::Ok(())
    })??;
    Ok(())
}
