use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, Corner, Div, ElementId, FontWeight, Pixels, Rgba,
    SharedString, Stateful, Window, anchored, deferred, div, point, prelude::*, px, relative, rgb,
    svg,
};

use crate::{
    icons::{self, JEWEL_CUT, JEWEL_HOLLOW},
    theme::{Palette, alpha, mix},
};

pub(crate) const HEIGHT: Pixels = px(32.0);

pub(crate) const CORNER: Pixels = px(6.0);

pub(crate) const PANEL_CORNER: Pixels = px(10.0);

pub(crate) const CONTROL_TEXT: Pixels = px(13.0);

pub(crate) const TITLE_TEXT: Pixels = px(14.0);

pub(crate) const DETAIL_TEXT: Pixels = px(12.0);

pub(crate) const DETAIL_LINE: Pixels = px(17.0);

pub(crate) const DISABLED: f32 = 0.42;

const TRACKING: Pixels = px(1.5);

const WORD_SPACE: Pixels = px(7.0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Weight {
    Primary,
    Secondary,
    Quiet,
    Destroy,
    ConfirmDestroy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Lamp,
    Warn,
    Muted,
}

impl Tone {
    const fn color(self, palette: &Palette) -> u32 {
        match self {
            Self::Lamp => palette.lamp,
            Self::Warn => palette.warn,
            Self::Muted => palette.muted,
        }
    }

    fn wash(self, palette: &Palette) -> Rgba {
        match self {
            Self::Lamp => alpha(palette.lamp, 0.13),
            Self::Warn => alpha(palette.warn, 0.1),
            Self::Muted => palette.hover(),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Kit<'a> {
    palette: &'a Palette,
    keyboard: bool,
}

impl<'a> Kit<'a> {
    pub(crate) const fn new(palette: &'a Palette) -> Self {
        Self {
            palette,
            keyboard: false,
        }
    }

    pub(crate) const fn keyboard(self, keyboard: bool) -> Self {
        Self { keyboard, ..self }
    }

    pub(crate) fn button(
        self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        weight: Weight,
        on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let enabled = on_click.is_some();
        let label = label.into();
        let clear = alpha(palette.ink, 0.0);
        let (fill, edge, ink) = match weight {
            Weight::Primary => (rgb(palette.lamp), rgb(palette.lamp), palette.room),
            Weight::Secondary => (rgb(palette.raise), palette.edge(), palette.ink),
            Weight::Quiet => (clear, clear, palette.muted),
            Weight::Destroy => (clear, alpha(palette.warn, 0.55), palette.warn),
            Weight::ConfirmDestroy => (rgb(palette.warn), rgb(palette.warn), palette.room),
        };
        let filled = matches!(
            weight,
            Weight::Primary | Weight::Secondary | Weight::ConfirmDestroy
        );
        self.pressable(id, label.clone(), gpui::Role::Button, filled, on_click)
            .h(HEIGHT)
            .px(px(14.0))
            .justify_center()
            .gap(px(6.0))
            .rounded(CORNER)
            .border_1()
            .border_color(edge)
            .bg(fill)
            .text_size(CONTROL_TEXT)
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(ink))
            .whitespace_nowrap()
            .when(enabled, |button| {
                button.hover(move |style| match weight {
                    Weight::Primary => style.bg(rgb(mix(palette.lamp, palette.ink, 0.18))),
                    Weight::ConfirmDestroy => style.bg(rgb(mix(palette.warn, palette.ink, 0.18))),
                    Weight::Secondary => style.border_color(alpha(palette.ink, 0.28)),
                    Weight::Quiet => style.bg(palette.hover()).text_color(rgb(palette.ink)),
                    Weight::Destroy => style.bg(Tone::Warn.wash(palette)),
                })
            })
            .child(label)
    }

    pub(crate) fn icon_button(
        self,
        id: impl Into<ElementId>,
        path: &'static str,
        label: impl Into<SharedString>,
        on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let enabled = on_click.is_some();
        self.pressable(id, label.into(), gpui::Role::Button, false, on_click)
            .size(HEIGHT)
            .justify_center()
            .rounded(CORNER)
            .border_1()
            .border_color(alpha(palette.ink, 0.0))
            .when(enabled, |button| {
                button.hover(move |style| style.bg(palette.hover()))
            })
            .child(icon(path, 16.0, palette.muted))
    }

    pub(crate) fn switch(
        self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        on: bool,
        on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let enabled = on_click.is_some();
        self.pressable(id, label.into(), gpui::Role::Switch, true, on_click)
            .aria_toggled(if on {
                gpui::Toggled::True
            } else {
                gpui::Toggled::False
            })
            .relative()
            .w(px(40.0))
            .h(px(22.0))
            .rounded_full()
            .border_1()
            .border_color(if on {
                rgb(palette.lamp)
            } else {
                alpha(palette.muted, 0.7)
            })
            .bg(alpha(palette.lamp, if on { 1.0 } else { 0.0 }))
            .when(enabled && !on, |track| {
                track.hover(move |style| style.border_color(rgb(palette.ink)))
            })
            .child(
                div()
                    .absolute()
                    .top(px(3.0))
                    .left(px(if on { 21.0 } else { 3.0 }))
                    .size(px(14.0))
                    .rounded_full()
                    .bg(rgb(if on { palette.room } else { palette.muted })),
            )
    }

    pub(crate) fn dropdown(
        self,
        id: impl Into<ElementId>,
        label: &str,
        value: impl Into<SharedString>,
        expanded: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let value = value.into();
        self.pressable(
            id,
            format!("{label}: {value}").into(),
            gpui::Role::Button,
            true,
            Some(on_click),
        )
        .w(px(200.0))
        .max_w_full()
        .h(HEIGHT)
        .pl(px(10.0))
        .pr(px(8.0))
        .gap(px(8.0))
        .rounded(CORNER)
        .border_1()
        .border_color(if expanded {
            rgb(palette.lamp)
        } else {
            palette.edge()
        })
        .bg(rgb(palette.raise))
        .text_size(CONTROL_TEXT)
        .text_color(rgb(palette.ink))
        .when(!expanded, |trigger| {
            trigger.hover(move |style| style.border_color(alpha(palette.ink, 0.28)))
        })
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(value),
        )
        .child(icon(icons::CHEVRON_DOWN, 14.0, palette.muted))
    }

    pub(crate) fn menu(self, id: impl Into<ElementId>, label: &'static str) -> Stateful<Div> {
        let palette = self.palette;
        div()
            .id(id)
            .role(gpui::Role::ListBox)
            .aria_label(label)
            .occlude()
            .min_w(px(200.0))
            .max_w(px(320.0))
            .max_h(px(280.0))
            .overflow_y_scroll()
            .p(px(4.0))
            .flex()
            .flex_col()
            .gap(px(1.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(palette.edge())
            .bg(rgb(palette.panel))
            .shadow(vec![BoxShadow {
                color: alpha(0, 0.45).into(),
                offset: point(px(0.0), px(8.0)),
                blur_radius: px(24.0),
                spread_radius: px(0.0),
            }])
            .text_size(CONTROL_TEXT)
            .text_color(rgb(palette.ink))
    }

    pub(crate) fn option(
        self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        chosen: bool,
        highlighted: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let label = label.into();
        self.entry(id, label.clone(), gpui::Role::ListBoxOption, highlighted)
            .aria_selected(highlighted)
            .on_click(on_click)
            .child(
                div()
                    .flex_none()
                    .size(px(16.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(chosen, |slot| {
                        slot.child(icon(icons::CHECK, 14.0, palette.lamp))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(label),
            )
    }

    pub(crate) fn menu_action(
        self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        destructive: bool,
        highlighted: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        let label = label.into();
        self.entry(id, label.clone(), gpui::Role::MenuItem, highlighted)
            .px(px(10.0))
            .when(destructive, |entry| {
                entry.text_color(rgb(self.palette.warn))
            })
            .on_click(on_click)
            .child(label)
    }

    fn entry(
        self,
        id: impl Into<ElementId>,
        label: SharedString,
        role: gpui::Role,
        highlighted: bool,
    ) -> Stateful<Div> {
        let palette = self.palette;
        div()
            .id(id)
            .role(role)
            .aria_label(label)
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .h(HEIGHT)
            .px(px(8.0))
            .rounded(CORNER)
            .cursor_pointer()
            .when(highlighted, |entry| entry.bg(palette.hover()))
            .hover(move |style| style.bg(palette.hover()))
    }

    pub(crate) fn tab_bar(self, id: impl Into<ElementId>, label: &'static str) -> Stateful<Div> {
        div()
            .id(id)
            .role(gpui::Role::TabList)
            .aria_label(label)
            .flex()
            .items_end()
            .gap(px(24.0))
            .border_b_1()
            .border_color(self.palette.line())
    }

    pub(crate) fn tab(
        self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        selected: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let label = label.into();
        self.pressable(id, label.clone(), gpui::Role::Tab, false, Some(on_click))
            .aria_selected(selected)
            .flex_col()
            .h(px(40.0))
            .mb(px(-1.0))
            .rounded_t(CORNER)
            .text_size(TITLE_TEXT)
            .font_weight(if selected {
                FontWeight::MEDIUM
            } else {
                FontWeight::NORMAL
            })
            .text_color(rgb(if selected { palette.ink } else { palette.muted }))
            .hover(move |style| style.text_color(rgb(palette.ink)))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .px(px(2.0))
                    .child(label),
            )
            .child(
                div()
                    .h(px(2.0))
                    .rounded(px(1.0))
                    .when(selected, |rule| rule.bg(rgb(palette.lamp))),
            )
    }

    pub(crate) fn segmented(self, id: impl Into<ElementId>, label: &'static str) -> Stateful<Div> {
        div()
            .id(id)
            .role(gpui::Role::TabList)
            .aria_label(label)
            .flex()
            .flex_none()
            .gap(px(2.0))
            .p(px(2.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(self.palette.edge())
            .bg(rgb(self.palette.room))
    }

    pub(crate) fn segment(
        self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        selected: bool,
        on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let enabled = on_click.is_some();
        let label = label.into();
        self.pressable(id, label.clone(), gpui::Role::Tab, false, on_click)
            .aria_selected(selected)
            .h(px(26.0))
            .px(px(12.0))
            .justify_center()
            .rounded(CORNER)
            .border_1()
            .border_color(if selected {
                palette.edge()
            } else {
                alpha(palette.ink, 0.0)
            })
            .when(selected, |segment| segment.bg(rgb(palette.raise)))
            .text_size(CONTROL_TEXT)
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap()
            .text_color(rgb(if selected { palette.ink } else { palette.muted }))
            .when(enabled && !selected, |segment| {
                segment.hover(move |style| style.text_color(rgb(palette.ink)))
            })
            .child(label)
    }

    pub(crate) fn disclosure(
        self,
        id: impl Into<ElementId>,
        label: &'static str,
        open: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        let palette = self.palette;
        self.pressable(id, label.into(), gpui::Role::Button, false, Some(on_click))
            .h(HEIGHT)
            .pl(px(4.0))
            .pr(px(8.0))
            .gap(px(6.0))
            .rounded(CORNER)
            .border_1()
            .border_color(alpha(palette.ink, 0.0))
            .text_size(CONTROL_TEXT)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(palette.muted))
            .hover(move |style| style.text_color(rgb(palette.ink)))
            .child(icon(
                if open {
                    icons::CHEVRON_DOWN
                } else {
                    icons::CHEVRON_RIGHT
                },
                14.0,
                palette.muted,
            ))
            .child(label)
    }

    pub(crate) fn heading(self, text: &'static str) -> Stateful<Div> {
        div()
            .id(text)
            .role(gpui::Role::Heading)
            .aria_label(text)
            .px(px(2.0))
            .text_size(CONTROL_TEXT)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(self.palette.muted))
            .child(text)
    }

    pub(crate) fn section(self, title: &'static str, rows: Vec<AnyElement>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(self.heading(title))
            .child(self.group(rows))
    }

    pub(crate) fn group(self, rows: Vec<AnyElement>) -> Div {
        let line = self.palette.line();
        div()
            .flex()
            .flex_col()
            .rounded(PANEL_CORNER)
            .border_1()
            .border_color(line)
            .bg(rgb(self.palette.panel))
            .children(rows.into_iter().enumerate().map(move |(index, row)| {
                div()
                    .when(index > 0, |divided| divided.border_t_1().border_color(line))
                    .child(row)
            }))
    }

    pub(crate) fn setting(
        self,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        control: impl IntoElement,
    ) -> Div {
        div()
            .flex()
            .items_center()
            .gap(px(16.0))
            .min_h(px(60.0))
            .px(px(16.0))
            .py(px(12.0))
            .child(self.label(title, detail).flex_1())
            .child(div().flex_none().child(control))
    }

    pub(crate) fn label(
        self,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
    ) -> Div {
        div()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .text_size(TITLE_TEXT)
                    .text_color(rgb(self.palette.ink))
                    .child(title.into()),
            )
            .child(self.detail(detail))
    }

    pub(crate) fn detail(self, text: impl Into<SharedString>) -> Div {
        div()
            .text_size(DETAIL_TEXT)
            .line_height(DETAIL_LINE)
            .text_color(rgb(self.palette.muted))
            .child(text.into())
    }

    pub(crate) fn banner(
        self,
        tone: Tone,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        actions: Option<Div>,
    ) -> Div {
        let palette = self.palette;
        div()
            .flex()
            .gap(px(12.0))
            .p(px(14.0))
            .rounded(PANEL_CORNER)
            .border_1()
            .border_color(alpha(tone.color(palette), 0.3))
            .bg(tone.wash(palette))
            .child(div().pt(px(4.0)).child(facet(tone.color(palette), 10.0)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_size(TITLE_TEXT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.ink))
                            .child(title.into()),
                    )
                    .child(self.detail(detail))
                    .when_some(actions, |column, actions| {
                        column.child(actions.pt(px(6.0)).flex().flex_wrap().gap(px(8.0)))
                    }),
            )
    }

    pub(crate) fn keycap(self, label: impl Into<SharedString>, large: bool) -> Div {
        let palette = self.palette;
        let height = if large { 40.0 } else { 26.0 };
        div()
            .flex_none()
            .h(px(height))
            .min_w(px(height))
            .px(px(if large { 14.0 } else { 8.0 }))
            .flex()
            .items_center()
            .justify_center()
            .rounded(CORNER)
            .bg(rgb(palette.raise))
            .border_1()
            .border_b(px(if large { 3.0 } else { 2.0 }))
            .border_color(palette.edge())
            .text_size(px(if large { 16.0 } else { 12.0 }))
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(palette.ink))
            .whitespace_nowrap()
            .child(label.into())
    }

    pub(crate) fn keys(self, shortcut: &str, large: bool) -> Div {
        let palette = self.palette;
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(if large { 8.0 } else { 4.0 }))
            .children(shortcut.split(" + ").enumerate().flat_map(|(index, key)| {
                let joint = (index > 0).then(|| {
                    div()
                        .text_size(DETAIL_TEXT)
                        .text_color(rgb(palette.muted))
                        .child("+")
                        .into_any_element()
                });
                joint
                    .into_iter()
                    .chain([self.keycap(key.to_owned(), large).into_any_element()])
            }))
    }

    pub(crate) fn radio_card(
        self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        chosen: bool,
        on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let enabled = on_click.is_some();
        self.pressable(id, label.into(), gpui::Role::RadioButton, false, on_click)
            .aria_toggled(if chosen {
                gpui::Toggled::True
            } else {
                gpui::Toggled::False
            })
            .w_full()
            .items_start()
            .gap(px(12.0))
            .p(px(12.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(if chosen {
                rgb(palette.lamp)
            } else {
                palette.edge()
            })
            .when(chosen, |card| {
                card.opacity(1.0).bg(Tone::Lamp.wash(palette))
            })
            .when(enabled && !chosen, |card| {
                card.hover(move |style| style.border_color(alpha(palette.ink, 0.28)))
            })
            .child(
                div()
                    .flex_none()
                    .mt(px(2.0))
                    .size(px(16.0))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(if chosen { palette.lamp } else { palette.muted }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(chosen, |dot| {
                        dot.child(div().size(px(8.0)).rounded_full().bg(rgb(palette.lamp)))
                    }),
            )
    }

    pub(crate) fn chip(
        self,
        label: impl Into<SharedString>,
        remove: impl Into<ElementId>,
        remove_label: impl Into<SharedString>,
        on_remove: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Div {
        let palette = self.palette;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .h(px(28.0))
            .pl(px(10.0))
            .pr(px(2.0))
            .rounded(px(14.0))
            .bg(rgb(palette.raise))
            .border_1()
            .border_color(palette.edge())
            .text_size(CONTROL_TEXT)
            .text_color(rgb(palette.ink))
            .child(label.into())
            .child(
                self.icon_button(remove, icons::CLOSE, remove_label, Some(on_remove))
                    .size(px(22.0))
                    .rounded_full(),
            )
    }

    pub(crate) fn tag(self, label: impl Into<SharedString>, tone: Tone) -> Div {
        div()
            .flex_none()
            .h(px(20.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .rounded(px(10.0))
            .bg(tone.wash(self.palette))
            .text_size(px(11.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(tone.color(self.palette)))
            .whitespace_nowrap()
            .child(label.into())
    }

    pub(crate) fn progress(self, fraction: f32) -> Div {
        div()
            .h(px(4.0))
            .w_full()
            .rounded(px(2.0))
            .bg(self.palette.line())
            .child(
                div()
                    .h_full()
                    .rounded(px(2.0))
                    .bg(rgb(self.palette.lamp))
                    .w(relative(fraction.clamp(0.0, 1.0))),
            )
    }

    pub(crate) fn swatch(
        self,
        id: impl Into<ElementId>,
        name: &'static str,
        colors: &Palette,
        chosen: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let keyboard = self.keyboard;
        self.pressable(
            id,
            format!("Theme: {name}").into(),
            gpui::Role::RadioButton,
            true,
            Some(on_click),
        )
        .aria_toggled(if chosen {
            gpui::Toggled::True
        } else {
            gpui::Toggled::False
        })
        .size(px(34.0))
        .justify_center()
        .rounded_full()
        .border_2()
        .border_color(if chosen {
            rgb(palette.lamp)
        } else {
            alpha(palette.lamp, 0.0)
        })
        .when(!chosen, |swatch| {
            swatch.hover(move |style| style.border_color(palette.edge()))
        })
        .when(keyboard, |swatch| {
            swatch.focus(move |style| {
                style.shadow(vec![
                    BoxShadow {
                        color: alpha(palette.lamp, 0.6).into(),
                        offset: point(px(0.0), px(0.0)),
                        blur_radius: px(0.0),
                        spread_radius: px(4.0),
                    },
                    BoxShadow {
                        color: rgb(palette.panel).into(),
                        offset: point(px(0.0), px(0.0)),
                        blur_radius: px(0.0),
                        spread_radius: px(2.0),
                    },
                ])
            })
        })
        .child(
            div()
                .size(px(26.0))
                .rounded_full()
                .bg(rgb(colors.room))
                .border_1()
                .border_color(palette.edge())
                .flex()
                .items_center()
                .justify_center()
                .child(div().size(px(10.0)).rounded_full().bg(rgb(colors.lamp))),
        )
    }

    fn pressable(
        self,
        id: impl Into<ElementId>,
        label: SharedString,
        role: gpui::Role,
        filled: bool,
        on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
    ) -> Stateful<Div> {
        let enabled = on_click.is_some();
        let lamp = self.palette.lamp;
        div()
            .id(id)
            .role(role)
            .aria_label(label)
            .aria_disabled(!enabled)
            .focusable()
            .tab_stop(enabled)
            .flex()
            .flex_none()
            .items_center()
            .when(!enabled, |control| control.opacity(DISABLED))
            .when(self.keyboard, |control| {
                control.focus(move |style| {
                    let style = style.border_color(rgb(lamp));
                    if filled {
                        style.shadow(ring(lamp))
                    } else {
                        style.bg(alpha(lamp, 0.1))
                    }
                })
            })
            .when_some(on_click, |control, on_click| {
                control.cursor_pointer().on_click(on_click)
            })
    }
}

pub(crate) fn popover(menu: impl IntoElement, align_right: bool) -> Div {
    div()
        .absolute()
        .top(HEIGHT + px(4.0))
        .map(|anchor| {
            if align_right {
                anchor.right_0()
            } else {
                anchor.left_0()
            }
        })
        .child(
            deferred(
                anchored()
                    .anchor(if align_right {
                        Corner::TopRight
                    } else {
                        Corner::TopLeft
                    })
                    .snap_to_window_with_margin(px(8.0))
                    .child(menu),
            )
            .with_priority(1),
        )
}

pub(crate) fn icon(path: &'static str, size: f32, color: u32) -> gpui::Svg {
    svg()
        .path(path)
        .size(px(size))
        .text_color(rgb(color))
        .flex_none()
}

pub(crate) fn facet(color: u32, size: f32) -> gpui::Svg {
    icon(JEWEL_CUT, size, color)
}

pub(crate) fn hollow(color: u32, size: f32) -> gpui::Svg {
    icon(JEWEL_HOLLOW, size, color)
}

pub(crate) fn ring(color: u32) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: alpha(color, 0.45).into(),
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: px(3.0),
    }]
}

pub(crate) fn caps(label: &str) -> Div {
    div()
        .flex()
        .flex_wrap()
        .min_w(px(0.0))
        .items_center()
        .gap_x(WORD_SPACE)
        .gap_y(px(2.0))
        .children(label.split_whitespace().map(|word| {
            div().flex().gap(TRACKING).children(
                word.chars()
                    .flat_map(char::to_uppercase)
                    .map(|letter| div().child(SharedString::from(letter.to_string()))),
            )
        }))
}
