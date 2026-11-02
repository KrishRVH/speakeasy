use gpui::{Context, Task, Window};
use zeroize::Zeroizing;

use super::{Notice, Settings};
use crate::{
    gpui_ext::{EntityUpdate as _, follow},
    runtime::PracticeEvent,
    shell::Services,
};

#[derive(Default)]
pub(super) struct Practice {
    pub(super) task: Option<Task<()>>,
    pub(super) ready: bool,
    pub(super) level: f32,
    levels: Option<Task<()>>,
    pub(super) text: Zeroizing<String>,
}

impl Practice {
    pub(super) fn stop(&mut self) {
        self.task = None;
        self.ready = false;
        self.levels = None;
        self.level = 0.0;
        self.text = Zeroizing::default();
    }
}

impl Settings {
    pub(in crate::shell) fn watch_practice_focus(window: &mut Window, cx: &Context<'_, Self>) {
        cx.observe_window_activation(window, |view, window, cx| {
            if !window.is_window_active() {
                view.end_practice(cx);
            }
        })
        .detach();
    }

    pub(in crate::shell) fn end_practice(&mut self, cx: &mut Context<'_, Self>) {
        self.practice.stop();
        cx.notify();
    }

    pub(super) fn toggle_practice(&mut self, cx: &mut Context<'_, Self>) {
        if super::is_demo(cx) && self.onboarding == Some(super::onboarding::Step::Practice) {
            self.practice.text = Zeroizing::new("A little practice goes a long way.".into());
            return;
        }
        if self.practice.task.is_some() {
            self.end_practice(cx);
            return;
        }
        let receipt = cx.global::<Services>().practice();
        let mut receipt = match receipt {
            Ok(receipt) => receipt,
            Err(message) => {
                self.notice = Some(Notice::Problem(message.into()));
                return;
            },
        };
        self.practice.stop();
        self.notice = None;
        self.practice.levels = Some(follow(
            cx.global::<Services>().meter.subscribe(),
            cx,
            |meter| meter.level.clamp(0.0, 1.0),
            |view, level, cx| {
                view.practice.level = level;
                cx.notify();
            },
        ));
        self.practice.task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = receipt.recv().await {
                let current = this
                    .update(cx, |view, cx| view.practice_event(event, cx))
                    .unwrap_or(false);
                if !current {
                    return;
                }
            }
            this.update_if_alive(cx, |view, cx| {
                view.end_practice(cx);
            });
        }));
    }

    fn practice_event(&mut self, event: PracticeEvent, cx: &mut Context<'_, Self>) -> bool {
        match event {
            PracticeEvent::Ready => self.practice.ready = true,
            PracticeEvent::Text(text) => self.practice.text = text,
            PracticeEvent::Unavailable(message) => {
                self.notice = Some(Notice::Problem(message.into()));
                self.end_practice(cx);
                return false;
            },
        }
        cx.notify();
        true
    }
}
