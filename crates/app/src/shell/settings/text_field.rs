mod accessible;
mod edit;
mod viewport;

use std::{ops::Range, sync::mpsc};

use accessible::{Painted, Request};
use edit::{Composition, Direction, Edit};
use gpui::{
    AccessibleAction, App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, KeyDownEvent, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, Pixels, Point, ShapedLine, Subscription, TextRun, UTF16Selection,
    UnderlineStyle, WeakEntity, Window, accesskit::ActionData, canvas, div, fill, point,
    prelude::*, px, size,
};
use viewport::Viewport;

use crate::{
    controls::{CONTROL_TEXT, CORNER, HEIGHT, ring},
    gpui_ext::EntityUpdate as _,
    theme::{Theme, alpha},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Selection {
    pub(super) anchor: usize,
    pub(super) focus: usize,
}

pub(super) struct TextField {
    label: &'static str,
    placeholder: &'static str,
    edit: Edit,
    viewport: Viewport,
    focus: FocusHandle,
    _blur: Subscription,
    theme: Theme,
    layout: Option<Layout>,
    selecting: bool,
    problem: Option<&'static str>,
}

struct Layout {
    line: ShapedLine,
    bounds: Bounds<Pixels>,
    scroll: Pixels,
}

impl Layout {
    fn origin(&self) -> Point<Pixels> {
        point(self.bounds.left() - self.scroll, self.bounds.top())
    }

    fn x_for_index(&self, index: usize) -> Pixels {
        self.origin().x + self.line.x_for_index(index)
    }

    fn index_for_x(&self, x: Pixels) -> usize {
        self.line.closest_index_for_x(x - self.origin().x)
    }

    fn span(&self, range: Range<usize>) -> Bounds<Pixels> {
        Bounds::from_corners(
            point(self.x_for_index(range.start), self.bounds.top()),
            point(self.x_for_index(range.end), self.bounds.bottom()),
        )
    }
}

pub(super) struct Submit;
impl EventEmitter<Submit> for TextField {}

impl TextField {
    pub(super) fn new(
        label: &'static str,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let focus = cx.focus_handle().tab_stop(true);
        let blur = cx.on_blur(&focus, window, |field, window, cx| {
            if field.edit.marked.is_some() {
                field.unmark_text(window, cx);
            }
        });
        Self {
            label,
            placeholder: "",
            edit: Edit::default(),
            viewport: Viewport::default(),
            focus,
            _blur: blur,
            theme,
            layout: None,
            selecting: false,
            problem: None,
        }
    }

    pub(super) fn placeholder(self, placeholder: &'static str) -> Self {
        Self {
            placeholder,
            ..self
        }
    }

    pub(super) fn text(&self) -> &str {
        &self.edit.text
    }

    pub(super) fn set_text(&mut self, text: String, cx: &mut Context<'_, Self>) {
        self.edit.set_text(text);
        self.layout = None;
        self.problem = None;
        cx.notify();
    }

    pub(super) fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    pub(super) fn key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let key = &event.keystroke;
        if self.edit.marked.is_some() {
            if key.key == "tab" {
                self.unmark_text(window, cx);
            }
            return;
        }
        let command = editing_shortcut(key.modifiers);
        match key.key.as_str() {
            "a" if command => {
                self.edit.select(0, false);
                self.edit.select(self.edit.text.len(), true);
            },
            "c" | "x" if command => {
                if let Some(text) = self
                    .edit
                    .text
                    .get(self.edit.selection())
                    .filter(|text| !text.is_empty())
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
                    if key.key == "x" {
                        self.replace_text_in_range(None, "", window, cx);
                    }
                }
            },
            "v" if command => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.replace_text_in_range(
                        None,
                        &text.replace(['\r', '\n', '\t'], " "),
                        window,
                        cx,
                    );
                }
            },
            "left" => self.edit.step(Direction::Backward, key.modifiers.shift),
            "right" => self.edit.step(Direction::Forward, key.modifiers.shift),
            "home" => self.edit.select(0, key.modifiers.shift),
            "end" => self.edit.select(self.edit.text.len(), key.modifiers.shift),
            "backspace" | "delete" => {
                if self.edit.selection().is_empty() {
                    let direction = if key.key == "delete" {
                        Direction::Forward
                    } else {
                        Direction::Backward
                    };
                    self.edit.step(direction, true);
                }
                self.replace_text_in_range(None, "", window, cx);
            },
            "enter" => cx.emit(Submit),
            _ => return,
        }
        cx.stop_propagation();
        self.viewport.reveal_caret();
        cx.notify();
    }

    fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        window.focus(&self.focus, cx);
        self.selecting = true;
        self.select_point(event.position, event.modifiers.shift);
        cx.notify();
    }

    fn respond(
        field: WeakEntity<Self>,
        action: AccessibleAction,
    ) -> impl FnMut(Option<&ActionData>, &mut Window, &mut App) + 'static {
        move |data, window, cx| {
            if let Some(data) = data {
                field.update_if_alive(cx, |field, cx| field.act(action, data, window, cx));
            }
        }
    }

    fn act(
        &mut self,
        action: AccessibleAction,
        data: &ActionData,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match Request::new(action, data, cx.entity_id(), &self.edit) {
            Some(Request::Replace(text)) => {
                let range = 0..self.edit.text.encode_utf16().count();
                self.replace(Some(range), text, Composition::Commit, cx);
            },
            Some(Request::Select(selection)) => self.select_accessible(selection, window, cx),
            Some(Request::Reveal(span)) => {
                self.viewport
                    .reveal(span, self.edit.revision, self.edit.ends());
                cx.notify();
            },
            None => {},
        }
    }

    pub(super) fn select_accessible(
        &mut self,
        selection: Selection,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.edit.select_range(selection) {
            return;
        }
        self.viewport.reveal_caret();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn shape(&self, window: &Window) -> ShapedLine {
        let style = window.text_style();
        let text = self.edit.text.clone();
        let base = TextRun {
            len: text.len(),
            font: style.font(),
            color: style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked) = &self.edit.marked {
            vec![
                TextRun {
                    len: marked.start,
                    ..base.clone()
                },
                TextRun {
                    len: marked.len(),
                    underline: Some(UnderlineStyle {
                        color: Some(style.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..base.clone()
                },
                TextRun {
                    len: text.len().saturating_sub(marked.end),
                    ..base
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect::<Vec<_>>()
        } else {
            vec![base]
        };
        window
            .text_system()
            .shape_line(text.into(), CONTROL_TEXT, &runs, None)
    }

    fn paint_text(&self, layout: &Layout, window: &mut Window, cx: &mut App) -> anyhow::Result<()> {
        let Layout { line, bounds, .. } = layout;
        let selection = self.edit.selection();
        let lamp = self.theme.palette().lamp;
        window.with_content_mask(Some(gpui::ContentMask { bounds: *bounds }), |window| {
            if !selection.is_empty() {
                window.paint_quad(fill(layout.span(selection), alpha(lamp, 0.25)));
            }
            if self.focus.is_focused(window) {
                window.paint_quad(fill(
                    Bounds::new(
                        point(layout.x_for_index(self.edit.cursor), bounds.top()),
                        size(px(1.0), bounds.size.height),
                    ),
                    gpui::rgb(lamp),
                ));
            }
            line.paint(
                layout.origin(),
                bounds.size.height,
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            )
        })
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        if self.selecting {
            self.select_point(event.position, true);
            cx.notify();
        }
    }

    fn select_point(&mut self, position: Point<Pixels>, extend: bool) {
        if let Some(layout) = &self.layout {
            self.edit.select(layout.index_for_x(position.x), extend);
            self.viewport.reveal_caret();
        }
    }

    fn replace(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        composition: Composition,
        cx: &mut Context<'_, Self>,
    ) {
        self.problem = (!self.edit.replace(range, text, composition))
            .then_some("Use a shorter phrase on one line.");
        cx.notify();
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<String> {
        let range = self.edit.utf8(range);
        *actual = Some(self.edit.utf16(range.clone()));
        self.edit.text.get(range).map(str::to_owned)
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.edit.utf16(self.edit.selection()),
            reversed: self.edit.reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<'_, Self>) -> Option<Range<usize>> {
        self.edit.marked.clone().map(|range| self.edit.utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.edit.unmark();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.replace(range, text, Composition::Commit, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.replace(range, text, Composition::Mark(selected), cx);
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(self.layout.as_ref()?.span(self.edit.utf8(range)))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<usize> {
        let index = self.layout.as_ref()?.index_for_x(position.x);
        Some(self.edit.utf16(index..index).start)
    }
}

fn editing_shortcut(modifiers: Modifiers) -> bool {
    let primary = if cfg!(target_os = "macos") {
        modifiers.platform && !modifiers.control
    } else {
        modifiers.control && !modifiers.platform
    };
    primary && !modifiers.alt && !modifiers.shift && !modifiers.function
}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let palette = self.theme.palette();
        let field = cx.entity();
        let this = cx.weak_entity();
        let placeholder = (self.edit.text.is_empty() && self.edit.marked.is_none())
            .then_some(self.placeholder)
            .filter(|placeholder| !placeholder.is_empty());
        let (publish, painted) = mpsc::sync_channel::<Painted>(1);
        div()
            .cursor_text()
            .relative()
            .w_full()
            .min_w(px(0.0))
            .min_h(HEIGHT)
            .flex()
            .flex_col()
            .justify_center()
            .border_1()
            .border_color(palette.edge())
            .hover(|style| style.border_color(gpui::rgb(palette.muted)))
            .when(self.focus.is_focused(window), |body| {
                body.border_color(gpui::rgb(palette.lamp))
                    .shadow(ring(palette.lamp))
            })
            .bg(gpui::rgb(palette.raise))
            .text_color(gpui::rgb(palette.ink))
            .text_size(CONTROL_TEXT)
            .rounded(CORNER)
            .px(px(10.0))
            .py(px(5.0))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|field, _, _, _| field.selecting = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|field, _, _, _| field.selecting = false),
            )
            .child(
                div()
                    .id("text-field")
                    .role(gpui::Role::TextInput)
                    .aria_label(self.label)
                    .aria_value(self.edit.text.clone())
                    .when(!self.placeholder.is_empty(), |input| {
                        input.aria_placeholder(self.placeholder)
                    })
                    .on_a11y_action(
                        AccessibleAction::SetValue,
                        Self::respond(this.clone(), AccessibleAction::SetValue),
                    )
                    .on_a11y_action(
                        AccessibleAction::SetTextSelection,
                        Self::respond(this, AccessibleAction::SetTextSelection),
                    )
                    .a11y_synthetic_children(move |subtree| {
                        if let Ok(painted) = painted.try_recv() {
                            painted.publish(subtree);
                        }
                    })
                    .track_focus(&self.focus)
                    .w_full()
                    .h(px(20.0))
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                let input = field.read(cx);
                                let line = input.shape(window);
                                let viewport = input.viewport.layout(
                                    input.edit.revision,
                                    input.edit.ends(),
                                    |offset| line.x_for_index(offset),
                                    bounds.size.width,
                                    line.width,
                                );
                                let layout = Layout {
                                    line,
                                    bounds,
                                    scroll: viewport.offset,
                                };
                                let runs = if window.is_a11y_active()
                                    && let Some(painted) =
                                        Painted::new(field.entity_id(), &input.edit, &layout, window)
                                {
                                    let runs = painted.run_ids();
                                    #[expect(
                                        clippy::let_underscore_must_use,
                                        reason = "A field without an accessibility node this frame has no reader for its text"
                                    )]
                                    let _ = publish.try_send(painted);
                                    runs
                                } else {
                                    Vec::new()
                                };
                                (field.clone(), layout, viewport, runs)
                            },
                            move |bounds, (field, layout, viewport, runs), window, cx| {
                                for run in runs {
                                    window.on_a11y_action(
                                        run,
                                        AccessibleAction::ScrollIntoView,
                                        Self::respond(
                                            field.downgrade(),
                                            AccessibleAction::ScrollIntoView,
                                        ),
                                    );
                                }
                                field.update(cx, |input, cx| {
                                    input.viewport = viewport;
                                    window.handle_input(
                                        &input.focus,
                                        ElementInputHandler::new(bounds, field.clone()),
                                        cx,
                                    );
                                    let failed = input.paint_text(&layout, window, cx).is_err();
                                    if failed && input.problem.is_none() {
                                        input.problem = Some("Can't render this phrase.");
                                        cx.notify();
                                    }
                                    input.layout = Some(layout);
                                });
                            },
                        )
                        .size_full(),
                    ),
            )
            .when_some(placeholder, |body, placeholder| {
                body.child(
                    div()
                        .absolute()
                        .inset_0()
                        .px(px(10.0))
                        .flex()
                        .items_center()
                        .text_color(gpui::rgb(palette.muted))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(placeholder),
                )
            })
            .when_some(self.problem, |body, problem| {
                body.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(gpui::rgb(palette.warn))
                        .child(problem),
                )
            })
            .overflow_hidden()
    }
}

#[cfg(test)]
mod tests {
    use gpui::Modifiers;

    use super::editing_shortcut;

    #[test]
    fn only_the_bare_primary_modifier_edits_so_altgr_and_shifted_chords_type() {
        let primary = if cfg!(target_os = "macos") {
            Modifiers::command()
        } else {
            Modifiers::control()
        };
        assert!(editing_shortcut(primary));
        for extra in [Modifiers::alt(), Modifiers::shift(), Modifiers::function()] {
            assert!(!editing_shortcut(primary | extra));
        }
        assert!(!editing_shortcut(Modifiers::control() | Modifiers::alt()));
        assert!(!editing_shortcut(
            Modifiers::control() | Modifiers::command()
        ));
        assert!(!editing_shortcut(Modifiers::default()));
    }
}
