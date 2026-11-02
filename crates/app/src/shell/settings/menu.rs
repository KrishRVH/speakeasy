use gpui::{
    Context, FocusHandle, KeyDownEvent, KeyUpEvent, ScrollHandle, SharedString, Subscription,
    Window,
};

use super::{Action, Settings, presets};
use crate::{
    config::{Config, Engine, presets::LIMIT},
    models::{self, LANGUAGES, WHISPER_LANGUAGES},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Menu {
    Language,
    Microphone,
    #[cfg(target_os = "macos")]
    Shortcut,
    #[cfg(target_os = "linux")]
    Delivery,
    #[cfg(target_os = "linux")]
    Bindings,
    Preset(usize),
}

impl Menu {
    const fn slot(self) -> usize {
        match self {
            Self::Language => 0,
            Self::Microphone => 1,
            #[cfg(target_os = "macos")]
            Self::Shortcut => 2,
            #[cfg(target_os = "linux")]
            Self::Delivery => 2,
            #[cfg(target_os = "linux")]
            Self::Bindings => 3,
            Self::Preset(index) => FIXED.saturating_add(index),
        }
    }
}

const FIXED: usize = 4;

pub(super) struct Item {
    pub(super) label: SharedString,
    pub(super) action: Action,
    pub(super) chosen: Option<bool>,
    pub(super) destructive: bool,
}

impl Item {
    fn choice(label: impl Into<SharedString>, action: Action, chosen: bool) -> Self {
        Self {
            label: label.into(),
            action,
            chosen: Some(chosen),
            destructive: false,
        }
    }
}

pub(super) struct Menus {
    pub(super) open: Option<Menu>,
    pub(super) cursor: usize,
    pub(super) group: FocusHandle,
    pub(super) list: FocusHandle,
    pub(super) scroll: ScrollHandle,
    triggers: Vec<FocusHandle>,
    focus_out: Option<Subscription>,
}

impl Menus {
    pub(super) fn new(cx: &Context<'_, Settings>) -> Self {
        Self {
            open: None,
            cursor: 0,
            group: cx.focus_handle(),
            list: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            triggers: (0..FIXED.saturating_add(LIMIT))
                .map(|_| cx.focus_handle().tab_stop(true))
                .collect(),
            focus_out: None,
        }
    }

    pub(super) fn dismiss_presets(&mut self) {
        if matches!(self.open, Some(Menu::Preset(_))) {
            self.open = None;
            self.focus_out = None;
        }
    }

    pub(super) fn trigger(&self, menu: Menu) -> &FocusHandle {
        self.triggers.get(menu.slot()).unwrap_or(&self.group)
    }
}

pub(super) fn languages(config: &Config) -> &'static [(&'static str, &'static str)] {
    if config.uses_nemotron() {
        LANGUAGES
    } else {
        WHISPER_LANGUAGES
    }
}

pub(super) fn language(config: &Config) -> &str {
    if config.uses_nemotron() {
        &config.language
    } else {
        models::whisper_language(&config.language)
    }
}

pub(super) fn language_name(config: &Config) -> &'static str {
    let code = language(config);
    languages(config)
        .iter()
        .find_map(|&(language, name)| (language == code).then_some(name))
        .unwrap_or(if config.engine == Engine::Whisper {
            "Other"
        } else {
            "Detect automatically"
        })
}

#[cfg(target_os = "linux")]
pub(super) const fn delivery_label(delivery: crate::config::TextDelivery) -> &'static str {
    use crate::config::TextDelivery;
    match delivery {
        TextDelivery::Paste => "Paste (Ctrl+V)",
        TextDelivery::TerminalPaste => "Paste (Ctrl+Shift+V)",
        TextDelivery::Type => "Type (keeps clipboard)",
        TextDelivery::Copy => "Copy for manual paste",
    }
}

#[cfg(target_os = "linux")]
pub(super) const fn bindings_label(external: bool) -> &'static str {
    if external {
        "Desktop bindings"
    } else {
        "Built in"
    }
}

impl Settings {
    pub(super) fn menu_items(&self, menu: Menu) -> Vec<Item> {
        match menu {
            Menu::Language => {
                let selected = language(&self.config);
                languages(&self.config)
                    .iter()
                    .map(|&(code, name)| {
                        Item::choice(name, Action::SelectLanguage(code), code == selected)
                    })
                    .collect()
            },
            Menu::Microphone => std::iter::once(Item::choice(
                "System default",
                Action::SelectMicrophone(None),
                self.config.microphone.is_none(),
            ))
            .chain(self.microphones.iter().enumerate().map(|(index, device)| {
                Item::choice(
                    device.name.clone(),
                    Action::SelectMicrophone(Some(index)),
                    self.config.microphone.as_deref() == Some(device.id.as_str()),
                )
            }))
            .collect(),
            #[cfg(target_os = "macos")]
            Menu::Shortcut => {
                use speakeasy_platform::MacShortcut;
                [
                    MacShortcut::Function,
                    MacShortcut::RightOption,
                    MacShortcut::RightCommand,
                ]
                .map(|shortcut| {
                    Item::choice(
                        shortcut.label(),
                        Action::SelectShortcut(shortcut),
                        self.config.mac_shortcut == shortcut,
                    )
                })
                .into()
            },
            #[cfg(target_os = "linux")]
            Menu::Delivery => {
                use crate::config::TextDelivery;
                [
                    TextDelivery::Paste,
                    TextDelivery::TerminalPaste,
                    TextDelivery::Type,
                    TextDelivery::Copy,
                ]
                .map(|delivery| {
                    Item::choice(
                        delivery_label(delivery),
                        Action::SelectDelivery(delivery),
                        self.config.text_delivery == delivery,
                    )
                })
                .into()
            },
            #[cfg(target_os = "linux")]
            Menu::Bindings => [false, true]
                .map(|external| {
                    Item::choice(
                        bindings_label(external),
                        Action::SelectBindings(external),
                        self.config.linux.external_shortcut == external,
                    )
                })
                .into(),
            Menu::Preset(index) => vec![
                Item {
                    label: "Update with current settings".into(),
                    action: Action::Preset(presets::Edit::Update(index)),
                    chosen: None,
                    destructive: false,
                },
                Item {
                    label: "Delete".into(),
                    action: Action::Preset(presets::Edit::Delete(index)),
                    chosen: None,
                    destructive: true,
                },
            ],
        }
    }

    pub(super) fn toggle_menu(
        &mut self,
        menu: Menu,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.menus.open == Some(menu) {
            self.close_menu(window);
            return;
        }
        if menu == Menu::Microphone {
            self.refresh_microphones(cx);
        }
        self.menus.focus_out =
            Some(
                cx.on_focus_out(&self.menus.group, window, |view, _, window, cx| {
                    let inside = view.menus.group.contains_focused(window, cx);
                    if inside && window.is_window_active() {
                        return;
                    }
                    let Some(menu) = view.menus.open.take() else {
                        return;
                    };
                    if inside {
                        view.menus.trigger(menu).focus(window);
                    }
                    cx.notify();
                }),
            );
        self.menus.cursor = self
            .menu_items(menu)
            .iter()
            .position(|item| item.chosen == Some(true))
            .unwrap_or(0);
        self.menus.open = Some(menu);
        self.menus.scroll.scroll_to_item(self.menus.cursor);
        self.menus.list.focus(window);
    }

    pub(super) fn close_menu(&mut self, window: &mut Window) {
        self.menus.focus_out = None;
        if let Some(menu) = self.menus.open.take() {
            self.menus.trigger(menu).focus(window);
        }
    }

    pub(super) fn menu_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(menu) = self.menus.open else {
            return;
        };
        let last = self.menu_items(menu).len().saturating_sub(1);
        let cursor = self.menus.cursor;
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
                self.close_menu(window);
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                }
                cx.notify();
                return;
            },
            _ => return,
        };
        self.menus.cursor = next;
        self.menus.scroll.scroll_to_item(next);
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn menu_release(
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
        let Some(menu) = self.menus.open else {
            return;
        };
        if let Some(item) = self.menu_items(menu).get(self.menus.cursor) {
            self.act(item.action, window, cx);
        }
        cx.stop_propagation();
    }
}
