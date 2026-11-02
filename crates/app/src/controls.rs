use gpui::{
    App, BoxShadow, ClickEvent, Div, ElementId, FontWeight, Pixels, SharedString, Stateful, Window,
    div, point, prelude::*, px, rgb, svg,
};

use crate::{
    BRAND_FONT,
    icons::{JEWEL_CUT, JEWEL_HOLLOW, KEYSTONE},
    theme::{Palette, alpha, mix},
};

const MONO_FONT: &str = if cfg!(target_os = "windows") {
    "Consolas"
} else if cfg!(target_os = "macos") {
    "Menlo"
} else {
    "DejaVu Sans Mono"
};

const LABEL: Pixels = px(11.0);

const TRACKING: Pixels = px(1.5);

const WORD_SPACE: Pixels = px(7.0);

pub(crate) const HEIGHT: Pixels = px(38.0);

const MAX_WIDTH: Pixels = px(390.0);

pub(crate) const CORNER: Pixels = px(2.0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Weight {
    Primary,
    Secondary,
    Quiet,
    Destroy,
    ConfirmDestroy,
}

#[derive(Clone, Copy)]
pub(crate) struct Kit<'a> {
    palette: &'a Palette,
}

impl<'a> Kit<'a> {
    pub(crate) const fn new(palette: &'a Palette) -> Self {
        Self { palette }
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
        let (fill, edge, ink, hover, ring) = match weight {
            Weight::Primary => (
                alpha(palette.lamp, 1.0),
                alpha(palette.lamp, 1.0),
                palette.room,
                alpha(mix(palette.lamp, palette.ink, 0.35), 1.0),
                palette.lamp,
            ),
            Weight::Secondary => (
                alpha(palette.lamp, 0.0),
                alpha(palette.lamp, 0.38),
                palette.ink,
                alpha(palette.lamp, 0.1),
                palette.lamp,
            ),
            Weight::Quiet => (
                alpha(palette.lamp, 0.0),
                alpha(palette.lamp, 0.0),
                palette.muted,
                alpha(palette.lamp, 0.06),
                palette.lamp,
            ),
            Weight::Destroy => (
                alpha(palette.warn, 0.0),
                alpha(palette.warn, 0.6),
                palette.warn,
                alpha(palette.warn, 0.12),
                palette.warn,
            ),
            Weight::ConfirmDestroy => (
                alpha(palette.warn, 1.0),
                alpha(palette.warn, 1.0),
                palette.room,
                alpha(mix(palette.warn, palette.ink, 0.35), 1.0),
                palette.warn,
            ),
        };
        let plate = matches!(weight, Weight::Primary | Weight::ConfirmDestroy);
        self.pressable(id, label.clone(), gpui::Role::Button, ring, plate, on_click)
            .justify_center()
            .gap(px(8.0))
            .px(px(14.0))
            .max_w(MAX_WIDTH)
            .overflow_hidden()
            .border_color(edge)
            .bg(fill)
            .text_color(rgb(ink))
            .when(enabled, |control| {
                control.hover(move |style| {
                    let style = style.bg(hover);
                    if matches!(weight, Weight::Secondary | Weight::Quiet) {
                        style.text_color(rgb(palette.ink))
                    } else {
                        style
                    }
                })
            })
            .when(plate, |button| button.child(jewel(palette.room, 6.0)))
            .map(|button| {
                if weight == Weight::Quiet {
                    button.text_size(px(12.0)).child(label)
                } else {
                    button.child(caps(&label))
                }
            })
    }

    pub(crate) fn tab_bar(self, id: impl Into<ElementId>, label: &'static str) -> Stateful<Div> {
        div()
            .id(id)
            .role(gpui::Role::TabList)
            .aria_label(label)
            .flex()
            .items_end()
            .gap(px(6.0))
            .border_b_1()
            .border_color(alpha(self.palette.lamp, 0.16))
    }

    pub(crate) fn tab(
        self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        selected: bool,
        on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let enabled = on_click.is_some();
        let label = label.into();
        self.pressable(
            id,
            label.clone(),
            gpui::Role::Tab,
            palette.lamp,
            false,
            on_click,
        )
        .aria_selected(selected)
        .flex_none()
        .flex_col()
        .px(px(12.0))
        .border_color(alpha(palette.lamp, 0.0))
        .text_color(rgb(if selected { palette.ink } else { palette.muted }))
        .when(enabled, |control| {
            control.hover(move |style| style.text_color(rgb(palette.ink)))
        })
        .child(div().flex_1().flex().items_center().child(caps(&label)))
        .child(
            div()
                .w_full()
                .h(px(2.0))
                .when(selected, |rule| rule.bg(rgb(palette.lamp))),
        )
    }

    pub(crate) fn toggle(
        self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        accessible: impl Into<SharedString>,
        on: bool,
        on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let enabled = on_click.is_some();
        self.pressable(
            id,
            accessible.into(),
            gpui::Role::Switch,
            palette.lamp,
            false,
            on_click,
        )
        .aria_toggled(if on {
            gpui::accesskit::Toggled::True
        } else {
            gpui::accesskit::Toggled::False
        })
        .gap(px(9.0))
        .px(px(10.0))
        .border_color(alpha(palette.lamp, 0.0))
        .text_size(px(12.0))
        .text_color(rgb(if on { palette.ink } else { palette.muted }))
        .when(enabled, |control| {
            control.hover(move |style| {
                style
                    .bg(alpha(palette.lamp, 0.06))
                    .text_color(rgb(palette.ink))
            })
        })
        .child(if on {
            jewel(palette.lamp, 9.0)
        } else {
            hollow(palette.muted, 9.0)
        })
        .child(label.into())
    }

    pub(crate) fn picker(
        self,
        id: impl Into<ElementId>,
        eyebrow: &'static str,
        value: impl Into<SharedString>,
        on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let enabled = on_click.is_some();
        let value = value.into();
        self.pressable(
            id,
            format!("{eyebrow}: {value}").into(),
            gpui::Role::Button,
            palette.lamp,
            false,
            on_click,
        )
        .flex_col()
        .items_start()
        .justify_center()
        .gap(px(2.0))
        .px(px(12.0))
        .py(px(5.0))
        .max_w(MAX_WIDTH)
        .min_w(px(0.0))
        .overflow_hidden()
        .border_color(alpha(palette.lamp, 0.24))
        .when(enabled, |control| {
            control.hover(move |style| style.bg(alpha(palette.lamp, 0.08)))
        })
        .child(self.label(eyebrow))
        .child(
            div()
                .max_w_full()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(12.0))
                .text_color(rgb(palette.ink))
                .child(value),
        )
    }

    pub(crate) fn option(
        self,
        id: impl Into<ElementId>,
        label: &'static str,
        chosen: bool,
        highlighted: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        let palette = self.palette;
        div()
            .id(id)
            .role(gpui::Role::ListBoxOption)
            .aria_label(label)
            .aria_selected(highlighted)
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(12.0))
            .py(px(8.0))
            .cursor_pointer()
            .text_color(rgb(if chosen { palette.lamp } else { palette.ink }))
            .when(highlighted, |row| row.bg(rgb(palette.raise)))
            .hover(|row| row.bg(rgb(palette.raise)))
            .on_click(on_click)
            .child(
                div()
                    .flex_none()
                    .size(px(7.0))
                    .when(chosen, |slot| slot.child(jewel(palette.lamp, 7.0))),
            )
            .child(label)
    }

    pub(crate) fn item(
        self,
        id: impl Into<ElementId>,
        selected: bool,
        on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let enabled = on_click.is_some();
        div()
            .id(id)
            .focusable()
            .tab_stop(enabled)
            .px(px(10.0))
            .py(px(7.0))
            .rounded(CORNER)
            .border_1()
            .border_color(if selected {
                rgb(palette.lamp)
            } else {
                alpha(palette.lamp, 0.16)
            })
            .bg(rgb(if selected {
                mix(palette.raise, palette.lamp, 0.12)
            } else {
                palette.raise
            }))
            .when(!enabled, |item| item.opacity(0.45))
            .focus(move |style| {
                style
                    .border_color(rgb(palette.lamp))
                    .shadow(ring(palette.lamp))
            })
            .when_some(on_click, |item, on_click| {
                item.cursor_pointer()
                    .hover(move |style| style.bg(rgb(mix(palette.raise, palette.ink, 0.08))))
                    .on_click(on_click)
            })
    }

    pub(crate) fn ticket(
        self,
        id: impl Into<ElementId>,
        title: impl IntoElement,
        selected: bool,
        on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
    ) -> Stateful<Div> {
        let palette = self.palette;
        let enabled = on_click.is_some();
        div()
            .id(id)
            .focusable()
            .tab_stop(enabled)
            .relative()
            .flex()
            .flex_col()
            .items_start()
            .gap(px(5.0))
            .pl(px(18.0))
            .pr(px(12.0))
            .py(px(14.0))
            .border_b_1()
            .border_color(alpha(palette.lamp, 0.12))
            .when(selected, |ticket| {
                ticket.bg(alpha(palette.lamp, 0.07)).child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(2.0))
                        .bg(rgb(palette.lamp)),
                )
            })
            .when(!enabled, |ticket| ticket.opacity(0.45))
            .focus(move |style| {
                style
                    .bg(alpha(palette.lamp, 0.1))
                    .shadow(ring(palette.lamp))
            })
            .when_some(on_click, |ticket, on_click| {
                ticket
                    .cursor_pointer()
                    .hover(move |style| style.bg(alpha(palette.lamp, 0.1)))
                    .on_click(on_click)
            })
            .child(
                div().flex().items_center().gap(px(8.0)).child(title).child(
                    div()
                        .flex_none()
                        .size(px(7.0))
                        .when(selected, |slot| slot.child(jewel(palette.lamp, 7.0))),
                ),
            )
    }

    pub(crate) fn vacancy(self, eyebrow: &'static str, text: impl Into<SharedString>) -> Div {
        div()
            .w(px(360.0))
            .max_w_full()
            .p(px(16.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(12.0))
            .child(self.keystone_rule().w(px(180.0)))
            .child(self.eyebrow(eyebrow))
            .child(
                div()
                    .text_center()
                    .text_size(px(13.0))
                    .line_height(px(20.0))
                    .text_color(rgb(self.palette.muted))
                    .child(text.into()),
            )
    }

    pub(crate) fn masthead(self, signal: u32) -> Div {
        div()
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(jewel(signal, 10.0))
            .child(
                div()
                    .flex()
                    .gap(px(5.0))
                    .font_family(BRAND_FONT)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(19.0))
                    .text_color(rgb(self.palette.ink))
                    .children(
                        "SPEAKEASY"
                            .chars()
                            .map(|letter| div().child(SharedString::from(letter.to_string()))),
                    ),
            )
    }

    pub(crate) fn label(self, text: &str) -> Div {
        caps(text)
            .font_family(MONO_FONT)
            .text_size(px(9.0))
            .text_color(rgb(self.palette.muted))
    }

    pub(crate) fn eyebrow(self, text: &'static str) -> Stateful<Div> {
        div()
            .id(text)
            .role(gpui::Role::Heading)
            .aria_label(text)
            .flex()
            .items_center()
            .gap(px(8.0))
            .text_color(rgb(self.palette.lamp))
            .child(jewel(self.palette.lamp, 6.0))
            .child(caps(text).font_family(MONO_FONT).text_size(px(10.0)))
    }

    pub(crate) fn badge(label: &'static str, color: u32) -> Div {
        div()
            .flex()
            .items_center()
            .gap(px(5.0))
            .px(px(6.0))
            .py(px(3.0))
            .rounded(CORNER)
            .bg(alpha(color, 0.1))
            .text_size(px(9.0))
            .text_color(rgb(color))
            .child(facet(color, 8.0))
            .child(caps(label))
    }

    pub(crate) fn inlay(self) -> Div {
        let lamp = self.palette.lamp;
        div()
            .relative()
            .p(px(16.0))
            .rounded(CORNER)
            .bg(rgb(self.palette.panel))
            .border_1()
            .border_color(alpha(lamp, 0.24))
            .child(
                div()
                    .absolute()
                    .top(px(3.0))
                    .left(px(3.0))
                    .right(px(3.0))
                    .bottom(px(3.0))
                    .rounded(px(1.0))
                    .border_1()
                    .border_color(alpha(lamp, 0.1)),
            )
    }

    pub(crate) fn double_rule(self) -> Div {
        let lamp = self.palette.lamp;
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(div().h(px(1.0)).bg(alpha(lamp, 0.3)))
            .child(div().h(px(1.0)).bg(alpha(lamp, 0.14)))
    }

    pub(crate) fn keystone_rule(self) -> Div {
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(self.double_rule().flex_1())
            .child(jewel(self.palette.lamp, 6.0))
            .child(self.double_rule().flex_1())
    }

    fn pressable(
        self,
        id: impl Into<ElementId>,
        label: SharedString,
        role: gpui::Role,
        ring_color: u32,
        plate: bool,
        on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
    ) -> Stateful<Div> {
        let enabled = on_click.is_some();
        let room = self.palette.room;
        div()
            .id(id)
            .role(role)
            .aria_label(label)
            .aria_disabled(!enabled)
            .focusable()
            .tab_stop(enabled)
            .flex()
            .items_center()
            .min_h(HEIGHT)
            .rounded(CORNER)
            .border_1()
            .text_size(LABEL)
            .when(!enabled, |control| control.opacity(0.45))
            .focus(move |style| {
                style
                    .border_color(rgb(if plate { room } else { ring_color }))
                    .shadow(ring(ring_color))
            })
            .when_some(on_click, |control, on_click| {
                control.cursor_pointer().on_click(on_click)
            })
    }
}

pub(crate) fn jewel(color: u32, size: f32) -> gpui::Svg {
    jewel_with(KEYSTONE, color, size)
}

pub(crate) fn facet(color: u32, size: f32) -> gpui::Svg {
    jewel_with(JEWEL_CUT, color, size)
}

pub(crate) fn hollow(color: u32, size: f32) -> gpui::Svg {
    jewel_with(JEWEL_HOLLOW, color, size)
}

fn jewel_with(path: &'static str, color: u32, size: f32) -> gpui::Svg {
    svg()
        .path(path)
        .size(px(size))
        .text_color(rgb(color))
        .flex_none()
}

fn ring(color: u32) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: alpha(color, 0.55).into(),
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: px(2.0),
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
