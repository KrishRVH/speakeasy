//! Single-line editing with UTF-16 IME ranges and grapheme cursor movement. Content never prints.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation as _;

pub(super) enum Composition {
    Commit,
    Mark(Option<Range<usize>>),
}

#[derive(Default)]
pub(super) struct Edit {
    pub(super) text: String,
    pub(super) generation: u64,
    pub(super) cursor: usize,
    anchor: usize,
    pub(super) marked: Option<Range<usize>>,
}

impl Edit {
    pub(super) fn selection(&self) -> Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor)
    }

    pub(super) const fn reversed(&self) -> bool {
        self.cursor < self.anchor
    }

    pub(super) fn select(&mut self, offset: usize, extend: bool) {
        self.cursor = self.floor_grapheme(offset);
        if !extend {
            self.anchor = self.cursor;
        }
    }

    pub(super) fn select_range(&mut self, anchor: usize, focus: usize) -> bool {
        if self.floor_grapheme(anchor) != anchor || self.floor_grapheme(focus) != focus {
            return false;
        }
        self.anchor = anchor;
        self.cursor = focus;
        true
    }

    pub(super) fn unmark(&mut self) {
        let selection = self.selection();
        let end = self.ceil_grapheme(selection.end);
        let start = if selection.is_empty() {
            end
        } else {
            self.floor_grapheme(selection.start)
        };
        (self.anchor, self.cursor) = if self.reversed() {
            (end, start)
        } else {
            (start, end)
        };
        self.marked = None;
    }

    fn floor_grapheme(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .map(|(at, _)| at)
            .chain([self.text.len()])
            .take_while(|at| *at <= offset)
            .last()
            .unwrap_or(0)
    }

    fn ceil_grapheme(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .map(|(at, _)| at)
            .find(|at| *at >= offset)
            .unwrap_or(self.text.len())
    }

    pub(super) fn step(&mut self, forward: bool, extend: bool) {
        if !extend && !self.selection().is_empty() {
            let selection = self.selection();
            self.select(
                if forward {
                    selection.end
                } else {
                    selection.start
                },
                false,
            );
            return;
        }
        let boundary = if forward {
            self.text
                .grapheme_indices(true)
                .find_map(|(at, _)| (at > self.cursor).then_some(at))
                .unwrap_or(self.text.len())
        } else {
            self.text
                .grapheme_indices(true)
                .rev()
                .find_map(|(at, _)| (at < self.cursor).then_some(at))
                .unwrap_or(0)
        };
        self.select(boundary, extend);
    }

    pub(super) fn utf16(&self, range: Range<usize>) -> Range<usize> {
        let convert = |offset| {
            self.text
                .get(..self.text.floor_char_boundary(offset))
                .unwrap_or_default()
                .encode_utf16()
                .count()
        };
        convert(range.start)..convert(range.end)
    }

    pub(super) fn utf8(&self, range: Range<usize>) -> Range<usize> {
        let start = from_utf16(&self.text, range.start);
        start..from_utf16(&self.text, range.end).max(start)
    }

    /// A composition's selection is relative to the inserted text, never the whole field.
    /// Reject oversized or multiline input before mutation; callers can show a local remedy.
    pub(super) fn replace(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        composition: Composition,
    ) -> bool {
        let range = range
            .map(|range| self.utf8(range))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection());
        let retained = self.text.len().saturating_sub(range.len());
        if retained.saturating_add(text.len()) > 2_048 || text.chars().any(char::is_control) {
            return false;
        }
        self.text.replace_range(range.clone(), text);
        self.generation = self.generation.wrapping_add(1);
        let end = range.start.saturating_add(text.len());
        self.marked = matches!(composition, Composition::Mark(_))
            .then_some(range.start..end)
            .filter(|range| !range.is_empty());
        if let Composition::Mark(Some(selected)) = composition {
            self.anchor = range.start.saturating_add(from_utf16(text, selected.start));
            self.cursor = range.start.saturating_add(from_utf16(text, selected.end));
        } else {
            self.anchor = end;
            self.cursor = end;
        }
        if self.marked.is_none() {
            self.unmark();
        }
        true
    }

    pub(super) fn set(&mut self, text: String) {
        self.text = text;
        self.generation = self.generation.wrapping_add(1);
        self.cursor = self.text.len();
        self.anchor = self.cursor;
        self.marked = None;
    }
}

fn from_utf16(text: &str, offset: usize) -> usize {
    let mut units = 0_usize;
    for (at, ch) in text.char_indices() {
        if units >= offset {
            return at;
        }
        units = units.saturating_add(ch.len_utf16());
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_selection_cannot_orphan_a_combining_or_emoji_grapheme() {
        for grapheme in ["e\u{301}", "👩🏽‍💻"] {
            for offset in 1..grapheme.len() {
                let mut edit = Edit::default();
                edit.set(format!("a{grapheme}b"));
                edit.select(1 + offset, false);
                assert_eq!(edit.cursor, 1);
                edit.step(false, true);
                assert!(edit.replace(None, "", Composition::Commit));
                assert_eq!(edit.text, format!("{grapheme}b"));
            }
        }
    }

    #[test]
    fn a_committed_insertion_keeps_the_caret_after_a_joined_grapheme() {
        let mut edit = Edit::default();
        edit.set("\u{301}b".into());
        edit.select(0, false);
        assert!(edit.replace(None, "e", Composition::Commit));
        assert_eq!(edit.cursor, "e\u{301}".len());
        edit.step(false, true);
        assert!(edit.replace(None, "", Composition::Commit));
        assert_eq!(edit.text, "b");
    }

    #[test]
    fn accessible_selection_rejects_partial_graphemes_before_changing_either_endpoint() {
        let mut edit = Edit::default();
        edit.set("ae\u{301}👩🏽‍💻".into());
        let end = edit.text.len();
        assert!(edit.select_range(end, 1));
        for (anchor, focus) in [(2, 1), (0, 2), (0, usize::MAX)] {
            assert!(!edit.select_range(anchor, focus));
            assert_eq!(edit.anchor, end);
            assert_eq!(edit.cursor, 1);
        }
        assert!(edit.replace(None, "", Composition::Commit));
        assert_eq!(edit.text, "a");
    }

    #[test]
    fn unmarking_restores_graphemes_without_rounding_an_active_utf16_composition() {
        for selected in [1..1, 1..2, Range { start: 2, end: 1 }] {
            let mut edit = Edit::default();
            assert!(edit.replace(None, "e\u{301}", Composition::Mark(Some(selected.clone()))));
            assert_eq!(
                edit.utf16(edit.selection()),
                selected.start.min(selected.end)..selected.start.max(selected.end)
            );
            assert_eq!(edit.reversed(), selected.start > selected.end);
            edit.unmark();
            assert!(edit.marked.is_none());
            if selected.start == selected.end {
                assert_eq!(edit.selection(), 3..3);
            } else {
                assert_eq!(edit.selection(), 0..3);
                assert_eq!(edit.reversed(), selected.start > selected.end);
            }
            if edit.selection().is_empty() {
                edit.step(false, true);
            }
            assert!(edit.replace(None, "", Composition::Commit));
            assert_eq!(edit.text, "");
        }
    }

    #[test]
    fn deletion_and_selection_do_not_split_combining_or_emoji_graphemes() {
        let mut edit = Edit::default();
        edit.set("a👩🏽‍💻e\u{301}".into());
        edit.step(false, true);
        assert_eq!(edit.text.get(edit.selection()), Some("e\u{301}"));
        assert!(edit.replace(None, "", Composition::Commit));
        edit.step(false, true);
        assert_eq!(edit.text.get(edit.selection()), Some("👩🏽‍💻"));
        assert!(edit.replace(None, "", Composition::Commit));
        assert_eq!(edit.text, "a");
    }

    #[test]
    fn composing_after_a_non_ascii_prefix_uses_insert_relative_utf16_selection() {
        let mut edit = Edit::default();
        edit.set("🦀 café ".into());
        let prefix = edit.text.len();
        assert!(edit.replace(None, "日本", Composition::Mark(Some(1..2))));
        assert_eq!(edit.text.get(edit.selection()), Some("本"));
        assert_eq!(edit.marked, Some(prefix..prefix + "日本".len()));
        assert!(edit.replace(None, "日本語", Composition::Commit));
        assert_eq!(edit.text, "🦀 café 日本語");
        assert!(edit.marked.is_none());
        let unchanged = edit.text.clone();
        assert!(!edit.replace(None, "bad\ninput", Composition::Commit));
        assert!(!edit.replace(None, &"x".repeat(2_049), Composition::Commit));
        assert_eq!(edit.text, unchanged);
    }

    #[test]
    fn external_ranges_are_bounded_and_reversed_ranges_do_not_panic() {
        let mut edit = Edit::default();
        edit.set("a😀b".into());
        assert_eq!(edit.utf8(2..3), 5..5);
        assert_eq!(edit.utf8(Range { start: 100, end: 0 }), 6..6);
        assert!(edit.replace(Some(Range { start: 100, end: 0 }), "!", Composition::Commit));
        assert_eq!(edit.text, "a😀b!");
    }
}
