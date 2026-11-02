use gpui::{Pixels, px};

use super::Selection;

#[derive(Clone, Copy, Default)]
enum Tracking {
    #[default]
    Caret,
    Span(Selection),
}

#[derive(Clone, Copy, Default)]
pub(super) struct Viewport {
    pub(super) offset: Pixels,
    revision: u64,
    selection: Selection,
    tracking: Tracking,
}

impl Viewport {
    pub(super) fn layout(
        mut self,
        revision: u64,
        selection: Selection,
        x: impl Fn(usize) -> Pixels,
        width: Pixels,
        content_width: Pixels,
    ) -> Self {
        if revision != self.revision || selection != self.selection {
            self.tracking = Tracking::Caret;
        }
        self.revision = revision;
        self.selection = selection;
        let span = match self.tracking {
            Tracking::Caret => Selection {
                anchor: selection.focus,
                focus: selection.focus,
            },
            Tracking::Span(span) => span,
        };
        let (anchor, focus) = (x(span.anchor), x(span.focus));
        let width = width.max(px(0.0));
        let margin = px(3.0).min(width);
        let (start, end) = (anchor.min(focus), anchor.max(focus) + margin);
        let offset = if end - start <= width {
            self.offset.min(start).max(end - width)
        } else if focus < anchor {
            start
        } else {
            end - width
        };
        let limit = (content_width + margin - width).max(px(0.0));
        self.offset = offset.max(px(0.0)).min(limit);
        self
    }

    pub(super) fn reveal(&mut self, span: Selection, revision: u64, selection: Selection) {
        self.revision = revision;
        self.selection = selection;
        self.tracking = Tracking::Span(span);
    }

    pub(super) fn reveal_caret(&mut self) {
        self.tracking = Tracking::Caret;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn x(offset: usize) -> Pixels {
        px(f32::from(u16::try_from(offset).unwrap()) * 10.0)
    }

    fn span(anchor: usize, focus: usize) -> Selection {
        Selection { anchor, focus }
    }

    fn layout(view: Viewport, selection: Selection, width: f32) -> Viewport {
        view.layout(1, selection, x, px(width), px(500.0))
    }

    #[test]
    fn the_caret_stays_visible_with_minimum_movement() {
        let caret = span(40, 40);
        let mut view = layout(Viewport::default(), caret, 100.0);
        assert_eq!(view.offset, px(303.0));
        view = layout(view, span(40, 38), 100.0);
        assert_eq!(view.offset, px(303.0));
        view = layout(view, span(40, 1), 100.0);
        assert_eq!(view.offset, px(10.0));
        view = view.layout(2, span(40, 1), x, px(100.0), px(50.0));
        assert_eq!(view.offset, px(0.0));
    }

    #[test]
    fn a_revealed_span_stays_visible_across_resizes_until_the_selection_or_text_changes() {
        let caret = span(50, 50);
        let mut view = layout(Viewport::default(), caret, 100.0);
        view.reveal(span(5, 6), 1, caret);
        view = layout(view, caret, 100.0);
        assert_eq!(
            view.offset,
            px(50.0),
            "A span left of the field moved too far"
        );
        view.reveal(span(29, 30), 1, caret);
        view = layout(view, caret, 100.0);
        assert_eq!(
            view.offset,
            px(203.0),
            "A later reveal did not replace the earlier one"
        );
        view = layout(view, caret, 40.0);
        assert_eq!(view.offset, px(263.0), "Resizing lost the revealed span");
        view = layout(view, span(50, 49), 40.0);
        assert_eq!(
            view.offset,
            px(453.0),
            "A selection change kept the old span"
        );
        view.reveal(span(5, 6), 1, span(50, 49));
        view = view.layout(2, span(50, 49), x, px(40.0), px(500.0));
        assert_eq!(
            view.offset,
            px(453.0),
            "An edit kept a span from the old text"
        );
    }

    #[test]
    fn a_span_wider_than_the_field_aligns_its_focus_with_the_matching_edge() {
        let caret = span(0, 0);
        let mut view = layout(Viewport::default(), caret, 100.0);
        view.reveal(span(40, 10), 1, caret);
        view = layout(view, caret, 100.0);
        assert_eq!(view.offset, px(100.0));
        view.reveal(span(10, 40), 1, caret);
        view = layout(view, caret, 100.0);
        assert_eq!(view.offset, px(303.0));
    }

    #[test]
    fn a_reveal_after_an_unpainted_edit_or_selection_change_outranks_the_caret() {
        let mut view = layout(Viewport::default(), span(50, 50), 100.0);
        let selected = span(50, 49);
        view.reveal(span(5, 6), 1, selected);
        view = layout(view, selected, 100.0);
        assert_eq!(
            view.offset,
            px(50.0),
            "The selection change replaced a later reveal"
        );
        view.reveal(span(29, 30), 2, selected);
        view = view.layout(2, selected, x, px(100.0), px(500.0));
        assert_eq!(view.offset, px(203.0), "The edit replaced a later reveal");
    }
}
