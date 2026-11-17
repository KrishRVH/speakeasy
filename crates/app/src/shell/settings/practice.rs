use std::ops::{ControlFlow, Range};

use gpui::{Context, Task, Window};
use zeroize::Zeroizing;

use super::{Notice, Settings, is_demo, onboarding::Step};
use crate::{
    gpui_ext::{EntityUpdate as _, follow},
    runtime::PracticeEvent,
    shell::Services,
};

#[derive(Default)]
pub(super) struct Practice {
    pub(super) text: Zeroizing<String>,
    pub(super) live: Option<Live>,
    pub(super) picked: Option<Range<usize>>,
}

pub(super) struct Live {
    pub(super) _events: Task<()>,
    pub(super) _levels: Task<()>,
    pub(super) ready: bool,
    pub(super) level: f32,
}

impl Practice {
    pub(super) fn stop(&mut self) {
        *self = Self::default();
    }

    pub(super) fn recognized(&self) -> bool {
        !self.text.trim().is_empty()
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
        if is_demo(cx) && self.onboarding == Some(Step::Practice) {
            self.practice.text = Zeroizing::new("A little practice goes a long way.".into());
            return;
        }
        if self.practice.live.is_some() {
            self.end_practice(cx);
            return;
        }
        let mut receipt = match cx.global::<Services>().practice() {
            Ok(receipt) => receipt,
            Err(message) => {
                self.notice = Some(Notice::Problem(message.into()));
                return;
            },
        };
        self.notice = None;
        let levels = follow(
            cx.global::<Services>().meter.subscribe(),
            cx,
            |meter| meter.level.clamp(0.0, 1.0),
            |view, level, cx| {
                if let Some(live) = &mut view.practice.live {
                    live.level = level;
                    cx.notify();
                }
            },
        );
        let events = cx.spawn(async move |this, cx| {
            while let Some(event) = receipt.recv().await {
                let Ok(ControlFlow::Continue(())) =
                    this.update(cx, |view, cx| view.practice_event(event, cx))
                else {
                    return;
                };
            }
            this.update_if_alive(cx, Self::end_practice);
        });
        self.practice = Practice {
            live: Some(Live {
                _events: events,
                _levels: levels,
                ready: false,
                level: 0.0,
            }),
            ..Practice::default()
        };
    }

    fn practice_event(
        &mut self,
        event: PracticeEvent,
        cx: &mut Context<'_, Self>,
    ) -> ControlFlow<()> {
        match event {
            PracticeEvent::Ready => {
                if let Some(live) = &mut self.practice.live {
                    live.ready = true;
                }
            },
            PracticeEvent::Text(text) => {
                self.practice.text = text;
                self.practice.picked = None;
            },
            PracticeEvent::Unavailable(message) => {
                self.notice = Some(Notice::Problem(message.into()));
                self.end_practice(cx);
                return ControlFlow::Break(());
            },
        }
        cx.notify();
        ControlFlow::Continue(())
    }
}
