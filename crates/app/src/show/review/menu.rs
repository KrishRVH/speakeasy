use gpui::{
    App, ClickEvent, Context, Div, KeyUpEvent, MouseDownEvent, Stateful, Window, div, prelude::*,
    px,
};

use super::{Action, CopyKind, ExportKind, ImageState, Review, render::id};
use crate::{
    controls::{CORNER, DISABLED, HEIGHT, Kit, popover},
    icons,
    theme::Palette,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Menu {
    Closed,
    Open(Option<usize>),
    Dismissed,
}

impl Menu {
    pub(super) fn pressed(self, keyboard: bool, enabled: &[bool]) -> Self {
        match (self, keyboard) {
            (Self::Open(_), _) | (Self::Dismissed, false) => Self::Closed,
            (Self::Closed | Self::Dismissed, true) => Self::Open(None).step(enabled, true),
            (Self::Closed, false) => Self::Open(None),
        }
    }

    pub(super) fn dismissed(self) -> Self {
        match self {
            Self::Open(_) => Self::Dismissed,
            other => other,
        }
    }

    pub(super) fn pointer(self) -> Self {
        match self {
            Self::Dismissed => Self::Closed,
            other => other,
        }
    }

    pub(super) fn step(self, enabled: &[bool], forward: bool) -> Self {
        let Self::Open(current) = self else {
            return self;
        };
        let mut choices = enabled
            .iter()
            .enumerate()
            .filter_map(|(index, &on)| on.then_some(index));
        let next = if forward {
            current
                .and_then(|current| choices.clone().find(|&index| index > current))
                .or_else(|| choices.next())
        } else {
            current
                .and_then(|current| choices.clone().rev().find(|&index| index < current))
                .or_else(|| choices.next_back())
        };
        Self::Open(next.or(current))
    }

    pub(super) const fn highlighted(self) -> Option<usize> {
        match self {
            Self::Open(index) => index,
            Self::Closed | Self::Dismissed => None,
        }
    }

    pub(super) const fn is_open(self) -> bool {
        matches!(self, Self::Open(_))
    }
}

struct Item {
    action: Action,
    label: &'static str,
    enabled: bool,
}

impl Review {
    fn items(&self, cx: &App) -> Vec<Item> {
        let movie = self
            .detail
            .as_ref()
            .is_some_and(|detail| detail.movie.is_some());
        let idle = self.exporting.is_none();
        [
            (!cfg!(target_os = "linux")).then(|| Item {
                action: Action::Copy(CopyKind::Evidence),
                label: "Copy image + summary + narration",
                enabled: self.can_copy(cx)
                    && self.loading.is_none()
                    && matches!(self.image, ImageState::Ready(_)),
            }),
            Some(Item {
                action: Action::Folder,
                label: "Open folder",
                enabled: true,
            }),
            movie.then_some(Item {
                action: Action::Movie,
                label: "Play video",
                enabled: true,
            }),
            movie.then_some(Item {
                action: Action::Export(ExportKind::Video),
                label: "Export video…",
                enabled: idle,
            }),
            Some(Item {
                action: Action::Export(ExportKind::Archive),
                label: if idle {
                    "Export ZIP…"
                } else {
                    "Exporting…"
                },
                enabled: idle && self.detail.is_some(),
            }),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    fn enabled_items(&self, cx: &App) -> Vec<bool> {
        self.items(cx).iter().map(|item| item.enabled).collect()
    }

    pub(super) fn more(&self, kit: Kit<'_>, palette: &Palette, cx: &Context<'_, Self>) -> Div {
        let open = self.menu.is_open();
        let trigger = kit
            .icon_button(
                "shows-more",
                icons::MORE,
                "More actions",
                Some(cx.listener(Self::toggle_menu)),
            )
            .when(open, |trigger| trigger.bg(palette.hover()));
        div()
            .relative()
            .flex_none()
            .child(trigger)
            .when(open, |anchor| {
                anchor.child(popover(self.menu_list(kit, cx), true))
            })
    }

    fn toggle_menu(&mut self, event: &ClickEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        let enabled = self.enabled_items(cx);
        self.menu = self.menu.pressed(event.is_keyboard(), &enabled);
        cx.notify();
    }

    fn menu_list(&self, kit: Kit<'_>, cx: &Context<'_, Self>) -> Stateful<Div> {
        let highlighted = self.menu.highlighted();
        kit.menu("shows-more-menu", "More actions")
            .role(gpui::Role::Menu)
            .capture_any_mouse_down(cx.listener(|view, _: &MouseDownEvent, _, cx| {
                view.modality(false, cx);
            }))
            .on_mouse_down_out(cx.listener(|view, _: &MouseDownEvent, _, cx| {
                view.menu = view.menu.dismissed();
                cx.notify();
            }))
            .children(
                self.items(cx)
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| entry(kit, item, highlighted == Some(index), cx)),
            )
    }

    pub(super) fn menu_key(&mut self, key: &str, cx: &mut Context<'_, Self>) -> bool {
        match key {
            "escape" => self.menu = Menu::Closed,
            "up" | "down" => {
                let enabled = self.enabled_items(cx);
                self.menu = self.menu.step(&enabled, key == "down");
            },
            "tab" => {
                self.menu = Menu::Closed;
                cx.notify();
                return false;
            },
            _ => return false,
        }
        cx.notify();
        true
    }

    pub(super) fn menu_key_up(
        &mut self,
        event: &KeyUpEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let keystroke = &event.keystroke;
        if !matches!(keystroke.key.as_str(), "enter" | "space") || keystroke.modifiers.modified() {
            return;
        }
        let Some(item) = self
            .menu
            .highlighted()
            .and_then(|index| self.items(cx).into_iter().nth(index))
            .filter(|item| item.enabled)
        else {
            return;
        };
        window.prevent_default();
        cx.stop_propagation();
        self.act(item.action, window, cx);
    }
}

fn entry(kit: Kit<'_>, item: Item, highlighted: bool, cx: &Context<'_, Review>) -> Stateful<Div> {
    let Item {
        action,
        label,
        enabled,
    } = item;
    if !enabled {
        return div()
            .id(id(&action))
            .role(gpui::Role::MenuItem)
            .aria_label(label)
            .aria_disabled(true)
            .flex()
            .flex_none()
            .items_center()
            .h(HEIGHT)
            .px(px(10.0))
            .rounded(CORNER)
            .opacity(DISABLED)
            .child(label);
    }
    kit.menu_action(
        id(&action),
        label,
        false,
        highlighted,
        cx.listener(move |view, _: &ClickEvent, window, cx| {
            view.act(action.clone(), window, cx);
        }),
    )
}
