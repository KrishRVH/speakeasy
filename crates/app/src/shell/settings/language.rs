//! A language picker with one keyboard focus, visible arrow navigation, and focus restored on close.

use gpui::{Context, FocusHandle, KeyDownEvent, KeyUpEvent, ScrollHandle, Subscription, Window};

use super::{Action, Settings};
use crate::{
    config::{Config, Engine},
    models::{LANGUAGES, WHISPER_LANGUAGES},
};

pub(super) struct Picker {
    pub(super) group: FocusHandle,
    pub(super) button: FocusHandle,
    pub(super) menu: FocusHandle,
    pub(super) scroll: ScrollHandle,
    pub(super) cursor: Option<usize>,
    // Opening binds focus cleanup to the popup's window; Settings owns that subscription.
    focus_out: Option<Subscription>,
}

impl Picker {
    pub(super) fn new(cx: &Context<'_, Settings>) -> Self {
        Self {
            group: cx.focus_handle(),
            button: cx.focus_handle().tab_stop(true),
            menu: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            cursor: None,
            focus_out: None,
        }
    }
}

pub(super) fn options(config: &Config) -> &'static [(&'static str, &'static str)] {
    if config.engine == Engine::Whisper {
        WHISPER_LANGUAGES
    } else {
        LANGUAGES
    }
}

pub(super) fn selected(config: &Config) -> &str {
    if config.engine == Engine::Whisper {
        crate::models::whisper_language(&config.language)
    } else {
        &config.language
    }
}

impl Settings {
    pub(super) fn toggle_language(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.language.cursor.is_some() {
            self.close_language(window);
        } else {
            self.language.focus_out =
                Some(
                    cx.on_focus_out(&self.language.group, window, |view, _, window, cx| {
                        if (!window.is_window_active()
                            || !view.language.group.contains_focused(window, cx))
                            && view.language.cursor.take().is_some()
                        {
                            cx.notify();
                        }
                    }),
                );
            let cursor = options(&self.config)
                .iter()
                .position(|&(code, _)| code == selected(&self.config))
                .unwrap_or(0);
            self.language.cursor = Some(cursor);
            self.language.scroll.scroll_to_item(cursor);
            self.language.menu.focus(window);
        }
    }

    pub(super) fn close_language(&mut self, window: &mut Window) {
        self.language.cursor = None;
        self.language.button.focus(window);
    }

    pub(super) fn language_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(cursor) = self.language.cursor else {
            return;
        };
        let languages = options(&self.config);
        let last = languages.len().saturating_sub(1);
        let next = match event.keystroke.key.as_str() {
            "up" => cursor.saturating_sub(1),
            "down" => cursor.saturating_add(1).min(last),
            "home" => 0,
            "end" => last,
            "enter" | "space" => {
                cx.stop_propagation();
                return;
            },
            "escape" | "tab" => {
                self.close_language(window);
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                }
                cx.notify();
                return;
            },
            _ => return,
        };
        self.language.cursor = Some(next);
        self.language.scroll.scroll_to_item(next);
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn language_release(
        &mut self,
        event: &KeyUpEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if event.keystroke.modifiers.modified()
            || !matches!(event.keystroke.key.as_str(), "enter" | "space")
        {
            return;
        }
        let Some(cursor) = self.language.cursor else {
            return;
        };
        // Select on release, like GPUI buttons: returning focus on key-down would let the same
        // key-up activate the Language button and reopen this menu.
        if let Some(&(language, _)) = options(&self.config).get(cursor) {
            self.act(Action::SelectLanguage(language), window, cx);
        }
        cx.stop_propagation();
    }
}
