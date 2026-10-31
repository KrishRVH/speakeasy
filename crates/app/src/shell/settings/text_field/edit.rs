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
        self.cursor = self.text.floor_char_boundary(offset.min(self.text.len()));
        if !extend {
            self.anchor = self.cursor;
        }
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
