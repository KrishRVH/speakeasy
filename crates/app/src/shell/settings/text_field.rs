//! A private single-line GPUI field with selection, IME composition, and explicit clipboard edits.
//! Input-handler and shaping integration follows GPUI's Apache-2.0 input example; editing ranges
//! are bounded here, and neither content nor shaping failures enter diagnostics.

mod edit;

use std::ops::Range;

use edit::{Composition, Edit};
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels,
    Point, ShapedLine, TextRun, UTF16Selection, UnderlineStyle, Window, canvas, div, fill, point,
    prelude::*, px, size,
};

use crate::theme::{Theme, alpha};

pub(super) struct TextField {
    edit: Edit,
    focus: FocusHandle,
    theme: Theme,
    layout: Option<(ShapedLine, Bounds<Pixels>, Pixels)>,
    selecting: bool,
    problem: Option<&'static str>,
}

pub(super) struct Submit;
impl EventEmitter<Submit> for TextField {}

impl TextField {
    pub(super) fn new(theme: Theme, cx: &Context<'_, Self>) -> Self {
        Self {
            edit: Edit::default(),
            focus: cx.focus_handle().tab_stop(true),
            theme,
            layout: None,
            selecting: false,
            problem: None,
        }
    }

    pub(super) fn text(&self) -> &str {
        &self.edit.text
    }

    pub(super) fn set(&mut self, text: String, cx: &mut Context<'_, Self>) {
        self.edit.set(text);
        self.layout = None;
        self.problem = None;
        cx.notify();
    }

    pub(super) fn theme(&mut self, theme: Theme) {
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
            return;
        }
        let command = if cfg!(target_os = "macos") {
            key.modifiers.platform
        } else {
            key.modifiers.control
        };
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
            "left" => self.edit.step(false, key.modifiers.shift),
            "right" => self.edit.step(true, key.modifiers.shift),
            "home" => self.edit.select(0, key.modifiers.shift),
            "end" => self.edit.select(self.edit.text.len(), key.modifiers.shift),
            "backspace" | "delete" => {
                if self.edit.selection().is_empty() {
                    self.edit.step(key.key == "delete", true);
                }
                self.replace_text_in_range(None, "", window, cx);
            },
            "enter" => cx.emit(Submit),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        window.focus(&self.focus);
        self.selecting = true;
        self.select_point(event.position, event.modifiers.shift);
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        if self.selecting {
            self.select_point(event.position, true);
            cx.notify();
        }
    }

    fn select_point(&mut self, position: Point<Pixels>, extend: bool) {
        if let Some((line, bounds, scroll)) = &self.layout {
            let at = line.closest_index_for_x(position.x - bounds.left() + *scroll);
            self.edit.select(at, extend);
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
        self.edit.marked = None;
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
        let (line, bounds, scroll) = self.layout.as_ref()?;
        let range = self.edit.utf8(range);
        Some(Bounds::from_corners(
            point(
                bounds.left() + line.x_for_index(range.start) - *scroll,
                bounds.top(),
            ),
            point(
                bounds.left() + line.x_for_index(range.end) - *scroll,
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<usize> {
        let (line, bounds, scroll) = self.layout.as_ref()?;
        let index = line.closest_index_for_x(position.x - bounds.left() + *scroll);
        Some(self.edit.utf16(index..index).start)
    }
}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TextField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let palette = self.theme.palette();
        let field = cx.entity();
        div()
            .id("text-field")
            .track_focus(&self.focus)
            .cursor_text()
            .w_full()
            .min_w(px(0.0))
            .border_1()
            .border_color(alpha(palette.lamp, 0.25))
            .focus(|style| style.border_color(gpui::rgb(palette.lamp)))
            .bg(gpui::rgb(palette.room))
            .text_color(gpui::rgb(palette.ink))
            .rounded(px(4.0))
            .px(px(8.0))
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
                canvas(
                    move |bounds, window, cx| {
                        let input = field.read(cx);
                        let style = window.text_style();
                        let text = input.edit.text.clone();
                        let base = TextRun {
                            len: text.len(),
                            font: style.font(),
                            color: style.color,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let runs = if let Some(marked) = &input.edit.marked {
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
                        let line =
                            window
                                .text_system()
                                .shape_line(text.into(), px(12.0), &runs, None);
                        let cursor = line.x_for_index(input.edit.cursor);
                        let scroll = (cursor - bounds.size.width + px(3.0)).max(px(0.0));
                        (field.clone(), line, scroll)
                    },
                    move |bounds, (field, line, scroll), window, cx| {
                        let input = field.read(cx);
                        window.handle_input(
                            &input.focus,
                            ElementInputHandler::new(bounds, field.clone()),
                            cx,
                        );
                        let origin = point(bounds.left() - scroll, bounds.top());
                        let selection = input.edit.selection();
                        if !selection.is_empty() {
                            window.paint_quad(fill(
                                Bounds::from_corners(
                                    point(
                                        origin.x + line.x_for_index(selection.start),
                                        bounds.top(),
                                    ),
                                    point(
                                        origin.x + line.x_for_index(selection.end),
                                        bounds.bottom(),
                                    ),
                                ),
                                alpha(palette.lamp, 0.25),
                            ));
                        }
                        if input.focus.is_focused(window) {
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(
                                        origin.x + line.x_for_index(input.edit.cursor),
                                        bounds.top(),
                                    ),
                                    size(px(1.0), bounds.size.height),
                                ),
                                gpui::rgb(palette.lamp),
                            ));
                        }
                        let failed = line.paint(origin, bounds.size.height, window, cx).is_err();
                        field.update(cx, |input, cx| {
                            if failed && input.problem.is_none() {
                                input.problem = Some("Cannot render this phrase.");
                                cx.notify();
                            }
                            input.layout = Some((line, bounds, scroll));
                        });
                    },
                )
                .w_full()
                .h(px(20.0)),
            )
            .when_some(self.problem, |body, problem| {
                body.child(
                    div()
                        .text_size(px(10.0))
                        .text_color(gpui::rgb(palette.warn))
                        .child(problem),
                )
            })
            .overflow_hidden()
    }
}
