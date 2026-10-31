use gpui::{AccessibleTextSelection, Pixels, px};

#[derive(Clone, Copy)]
enum Tracking {
    Caret,
    Manual,
}

#[derive(Clone, Copy)]
pub(super) struct Viewport {
    pub(super) offset: Pixels,
    pub(super) limit: Pixels,
    revision: u64,
    selection: AccessibleTextSelection,
    tracking: Tracking,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            offset: px(0.0),
            limit: px(0.0),
            revision: 0,
            selection: AccessibleTextSelection {
                anchor: 0,
                focus: 0,
            },
            tracking: Tracking::Caret,
        }
    }
}

impl Viewport {
    pub(super) fn layout(
        mut self,
        revision: u64,
        selection: AccessibleTextSelection,
        caret: Pixels,
        width: Pixels,
        content_width: Pixels,
    ) -> Self {
        if revision != self.revision || selection != self.selection {
            self.tracking = Tracking::Caret;
        }
        self.revision = revision;
        self.selection = selection;
        let width = width.max(px(0.0));
        let margin = px(3.0).min(width);
        self.limit = (content_width + margin - width).max(px(0.0));
        self.offset = self.offset.max(px(0.0)).min(self.limit);
        if matches!(self.tracking, Tracking::Caret) {
            if caret < self.offset {
                self.offset = caret.max(px(0.0));
            } else if caret + margin > self.offset + width {
                self.offset = (caret + margin - width).max(px(0.0)).min(self.limit);
            }
        }
        self
    }

    pub(super) fn pan(&mut self, offset: Pixels) -> bool {
        if !f32::from(offset).is_finite() {
            return false;
        }
        let previous = self.offset;
        self.offset = offset.max(px(0.0)).min(self.limit);
        self.tracking = Tracking::Manual;
        self.offset != previous
    }

    pub(super) fn reveal_caret(&mut self) {
        self.tracking = Tracking::Caret;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_pan_survives_redraw_and_later_absolute_requests_win() {
        let selection = AccessibleTextSelection {
            anchor: 20,
            focus: 100,
        };
        let mut view = Viewport::default().layout(1, selection, px(400.0), px(100.0), px(500.0));
        assert_eq!(view.offset, px(303.0));
        assert!(view.pan(px(10.0)));
        assert!(view.pan(px(30.0)));
        view = view.layout(1, selection, px(400.0), px(100.0), px(500.0));
        assert_eq!(view.offset, px(30.0));
        assert_eq!(view.selection, selection);
        assert_eq!(view.revision, 1);
        view.reveal_caret();
        view = view.layout(1, selection, px(400.0), px(100.0), px(500.0));
        assert_eq!(view.offset, px(303.0));
    }

    #[test]
    fn edits_and_selection_changes_reveal_the_caret_with_minimum_movement() {
        let selection = AccessibleTextSelection {
            anchor: 40,
            focus: 100,
        };
        let mut view = Viewport::default().layout(1, selection, px(400.0), px(100.0), px(500.0));
        assert!(view.pan(px(10.0)));
        view = view.layout(2, selection, px(400.0), px(100.0), px(500.0));
        assert_eq!(view.offset, px(303.0));
        view = view.layout(
            2,
            AccessibleTextSelection {
                anchor: 40,
                focus: 90,
            },
            px(380.0),
            px(100.0),
            px(500.0),
        );
        assert_eq!(view.offset, px(303.0));
        view = view.layout(
            2,
            AccessibleTextSelection {
                anchor: 40,
                focus: 1,
            },
            px(4.0),
            px(100.0),
            px(500.0),
        );
        assert_eq!(view.offset, px(4.0));
    }

    #[test]
    fn resizing_clamps_manual_pan_and_nonfinite_requests_do_not_replace_it() {
        let selection = AccessibleTextSelection {
            anchor: 0,
            focus: 100,
        };
        let mut view = Viewport::default().layout(1, selection, px(400.0), px(100.0), px(500.0));
        assert!(view.pan(px(350.0)));
        assert!(!view.pan(px(f32::NAN)));
        assert!(!view.pan(px(f32::INFINITY)));
        view = view.layout(1, selection, px(400.0), px(450.0), px(500.0));
        assert_eq!(view.offset, px(53.0));
        view = view.layout(1, selection, px(400.0), px(600.0), px(500.0));
        assert_eq!(view.offset, px(0.0));
    }
}
