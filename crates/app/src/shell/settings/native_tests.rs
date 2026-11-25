use anyhow::{Context as _, ensure};
use gpui::{
    AppContext as _, Bounds, EntityInputHandler as _, Focusable as _, Modifiers, MouseButton,
    MouseDownEvent, MouseUpEvent, PlatformInput, WindowBounds, WindowOptions, point, px, size,
};

use super::{
    native_interaction::{activate, draw, wait_for},
    *,
};

#[test]
#[ignore = "Opt-in owned windows; Linux requires a private X11 display. No microphone, input, or clipboard"]
fn native_phrase_editor_handles_ime() -> anyhow::Result<()> {
    let (completed, outcome) = std::sync::mpsc::channel();
    let (application, _) = crate::application::new()?;
    application.run(move |cx| {
        cx.spawn(async move |cx| {
            let result = async {
                let window = cx.open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(0.0), px(0.0)),
                            size(px(300.0), px(60.0)),
                        ))),
                        ..Default::default()
                    },
                    synthetic_field,
                )?;
                window.update(cx, |field, window, cx| {
                    field.set_text("🦀 café ".into(), cx);
                    field.replace_and_mark_text_in_range(None, "日本", Some(1..2), window, cx);
                    let selection = field
                        .selected_text_range(false, window, cx)
                        .context("The marked composition has no selection")?;
                    ensure!(
                        selection.range == (9..10),
                        "The marked composition has the wrong selection"
                    );
                    composition_keys(field, window, cx)?;
                    field.replace_text_in_range(None, "日本語", window, cx);
                    ensure!(
                        field.text() == "🦀 café 日本語",
                        "Committing the composition changed its text"
                    );
                    field.select_accessible(
                        text_field::Selection {
                            anchor: field.text().len(),
                            focus: 4,
                        },
                        window,
                        cx,
                    );
                    let selected = field
                        .selected_text_range(false, window, cx)
                        .context("The accessible selection was lost")?;
                    ensure!(
                        selected.reversed,
                        "The accessible selection lost its direction"
                    );
                    ensure!(
                        selected.range == (2..11),
                        "The accessible selection has the wrong range"
                    );
                    anyhow::Ok(())
                })??;
                draw(window, cx)?;
                window.update(cx, |_, window, _| activate(window))??;
                wait_for(
                    window,
                    gpui::Window::is_window_active,
                    "The phrase editor didn't activate",
                    cx,
                )
                .await?;
                window.update(cx, |field, window, cx| {
                    let bounds = field.bounds_for_range(8..11, Bounds::default(), window, cx);
                    ensure!(bounds.is_some(), "The field didn't shape its text");
                    field.focus_handle(cx).focus(window, cx);
                    field.set_text("ab".into(), cx);
                    anyhow::Ok(())
                })??;
                draw(window, cx)?;
                let altgr = gpui::Keystroke {
                    modifiers: Modifiers::control() | Modifiers::alt(),
                    key: "a".into(),
                    key_char: Some("æ".into()),
                };
                ensure!(
                    cx.update_window(window.into(), |_, window, cx| {
                        window.dispatch_keystroke(altgr, cx)
                    })?,
                    "AltGr+A reached neither the field nor its input handler"
                );
                window.update(cx, |field, window, cx| {
                    ensure!(
                        field.text() == "abæ",
                        "AltGr+A selected or swallowed its character"
                    );
                    field.replace_and_mark_text_in_range(None, "仮", None, window, cx);
                    field.key_down(
                        &gpui::KeyDownEvent {
                            keystroke: gpui::Keystroke::parse("tab")?,
                            is_held: false,
                            prefer_character_input: false,
                        },
                        window,
                        cx,
                    );
                    ensure!(
                        field.marked_text_range(window, cx).is_none(),
                        "Tab left a marked range"
                    );
                    ensure!(
                        field.text() == "abæ仮",
                        "Tab didn't commit the composed text"
                    );
                    field.replace_and_mark_text_in_range(None, "にほ", None, window, cx);
                    anyhow::Ok(())
                })??;
                draw(window, cx)?;
                click(window.into(), cx)?;
                window.update(cx, |field, window, cx| {
                    ensure!(
                        field.marked_text_range(window, cx).is_some(),
                        "A click ended the composition without the input method"
                    );
                    field.replace_and_mark_text_in_range(None, "にほん", None, window, cx);
                    field.replace_text_in_range(None, "日本", window, cx);
                    ensure!(
                        field.text() == "abæ仮日本",
                        "A click during composition moved or duplicated the composed text"
                    );
                    field.replace_and_mark_text_in_range(None, "名", None, window, cx);
                    anyhow::Ok(())
                })??;
                draw(window, cx)?;
                window.update(cx, |_, window, cx| window.blur(cx))?;
                draw(window, cx)?;
                window.update(cx, |field, window, cx| {
                    ensure!(
                        field.marked_text_range(window, cx).is_none(),
                        "Blur left a marked range"
                    );
                    ensure!(
                        field.text() == "abæ仮日本名",
                        "Blur didn't commit the composed text"
                    );
                    window.remove_window();
                    anyhow::Ok(())
                })??;
                anyhow::Ok(())
            }
            .await;
            completed.send(result).unwrap();
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    outcome
        .try_recv()
        .context("Phrase editor acceptance didn't complete")?
}

fn synthetic_field(
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> gpui::Entity<text_field::TextField> {
    cx.new(|cx| text_field::TextField::new("Synthetic field", Theme::default(), window, cx))
}

fn click(window: gpui::AnyWindowHandle, cx: &mut gpui::AsyncApp) -> anyhow::Result<()> {
    let position = point(px(14.0), px(14.0));
    let modifiers = Modifiers::default();
    for input in [
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }),
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
        }),
    ] {
        cx.update_window(window, |_, window, cx| window.dispatch_event(input, cx))?;
    }
    Ok(())
}

fn composition_keys(
    field: &mut text_field::TextField,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<'_, text_field::TextField>,
) -> anyhow::Result<()> {
    field.select_accessible(
        text_field::Selection {
            anchor: 0,
            focus: 0,
        },
        window,
        cx,
    );
    for key in ["backspace", "delete", "left", "right", "enter"] {
        field.key_down(
            &gpui::KeyDownEvent {
                keystroke: gpui::Keystroke::parse(key)?,
                is_held: false,
                prefer_character_input: false,
            },
            window,
            cx,
        );
        ensure!(
            field.text() == "🦀 café 日本",
            "A key changed the active composition text"
        );
        ensure!(
            field.marked_text_range(window, cx) == Some(8..10),
            "A key changed the marked composition range"
        );
        let selected = field
            .selected_text_range(false, window, cx)
            .context("A key removed the composition selection")?;
        ensure!(
            selected.range == (9..10),
            "A key changed the composition selection"
        );
    }
    Ok(())
}
