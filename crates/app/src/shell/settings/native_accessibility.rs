//! Static owned-window fixture for the Windows UI Automation acceptance script.

use std::time::Duration;

use anyhow::Context as _;
use gpui::{
    AppContext as _, Application, Bounds, Context, Entity, IntoElement, Render, Task, Window,
    WindowBounds, WindowOptions, div, point, prelude::*, px, size,
};

use super::text_field::TextField;
use crate::{gpui_ext::AppUpdate as _, theme::Theme};

struct Fixture {
    field: Entity<TextField>,
    narrow: bool,
    _timeout: Task<()>,
}

impl Render for Fixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let resize = cx.listener(|fixture, _: &(), _, cx| {
            fixture.narrow = !fixture.narrow;
            cx.notify();
        });
        div()
            .size_full()
            .child(
                div()
                    .w(px(if self.narrow { 140.0 } else { 280.0 }))
                    .child(self.field.clone()),
            )
            .child(
                div()
                    .id("resize-field")
                    .role(gpui::Role::Button)
                    .aria_label("Resize field")
                    .on_a11y_action(gpui::accesskit::Action::Click, move |_, window, cx| {
                        resize(&(), window, cx)
                    })
                    .child("Resize field"),
            )
    }
}

#[test]
#[ignore = "Opt-in static owned window; scripts/check-demo-windows.ps1 -StaticAccessibility drives its UIA provider"]
fn native_accessibility_static_fixture() -> anyhow::Result<()> {
    let (completed, outcome) = std::sync::mpsc::channel();
    Application::new().run(move |cx| {
        cx.open_window(
            WindowOptions {
                show: false,
                focus: false,
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(200.0), px(180.0)),
                    size(px(320.0), px(120.0)),
                ))),
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("Speakeasy accessibility fixture");
                window.on_window_should_close(cx, move |_, cx| {
                    completed.send(()).unwrap();
                    cx.quit();
                    true
                });
                let field = cx.new(|cx| {
                    let mut field = TextField::new("Synthetic field", Theme::default(), cx);
                    field.set("Synthetic private draft".into(), cx);
                    field
                });
                let timer = cx.background_executor().timer(Duration::from_secs(45));
                let timeout = cx.spawn(async move |cx| {
                    timer.await;
                    cx.update_if_running(|cx| cx.quit());
                });
                cx.new(|_| Fixture {
                    field,
                    narrow: false,
                    _timeout: timeout,
                })
            },
        )
        .unwrap();
    });
    outcome
        .try_recv()
        .context("Static accessibility fixture timed out")
}
