use std::hash::{DefaultHasher, Hash as _, Hasher as _};

use gpui::{
    A11ySubtreeBuilder, AccessibleAction, Bounds, EntityId, Pixels, ShapedLine, Window,
    accesskit::{ActionData, Node, NodeId, Rect, Role, TextDirection, TextPosition, TextSelection},
    point, px, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

use super::{Selection, edit::Edit};

#[cfg(test)]
mod tests;

const RUN_LENGTH: usize = 255;

pub(super) enum Request<'a> {
    Replace(&'a str),
    Select(Selection),
    Reveal(Selection),
}

impl<'a> Request<'a> {
    pub(super) fn new(
        action: AccessibleAction,
        data: &'a ActionData,
        field: EntityId,
        edit: &Edit,
    ) -> Option<Self> {
        match (action, data) {
            (AccessibleAction::SetValue, ActionData::Value(value)) => Some(Self::Replace(value)),
            (AccessibleAction::SetTextSelection, ActionData::SetTextSelection(selection)) => {
                Document::new(field, edit)?
                    .selection(*selection)
                    .map(Self::Select)
            },
            (AccessibleAction::ScrollIntoView, ActionData::SetTextSelection(span)) => {
                Document::new(field, edit)?
                    .selection(*span)
                    .map(Self::Reveal)
            },
            _ => None,
        }
    }
}

struct Document {
    field: EntityId,
    revision: u64,
    starts: Vec<usize>,
    len: usize,
}

struct Run<'a> {
    id: NodeId,
    first: usize,
    starts: &'a [usize],
    end: usize,
}

impl Run<'_> {
    fn start(&self) -> usize {
        self.starts.first().copied().unwrap_or(self.end)
    }

    fn boundaries(&self) -> impl Iterator<Item = usize> + '_ {
        self.starts.iter().copied().chain([self.end])
    }

    fn graphemes(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.starts.iter().copied().zip(self.boundaries().skip(1))
    }

    fn position(&self, offset: usize) -> Option<TextPosition> {
        self.boundaries()
            .position(|boundary| boundary == offset)
            .map(|character_index| TextPosition {
                node: self.id,
                character_index,
            })
    }
}

impl Document {
    fn new(field: EntityId, edit: &Edit) -> Option<Self> {
        let starts: Vec<usize> = edit.text.grapheme_indices(true).map(|(at, _)| at).collect();
        let len = edit.text.len();
        let representable = starts
            .iter()
            .zip(starts.iter().skip(1).chain([&len]))
            .all(|(start, end)| u8::try_from(end.saturating_sub(*start)).is_ok());
        representable.then_some(Self {
            field,
            revision: edit.generation,
            starts,
            len,
        })
    }

    fn runs(&self) -> Vec<Run<'_>> {
        let mut chunks: Vec<&[usize]> = self.starts.chunks(RUN_LENGTH).collect();
        if chunks.is_empty() {
            chunks.push(&[]);
        }
        let ends: Vec<usize> = chunks
            .iter()
            .skip(1)
            .filter_map(|chunk| chunk.first().copied())
            .chain([self.len])
            .collect();
        chunks
            .into_iter()
            .zip(ends)
            .enumerate()
            .map(|(index, (starts, end))| {
                let mut hasher = DefaultHasher::new();
                ("speakeasy.text-run", self.field, self.revision, index).hash(&mut hasher);
                Run {
                    id: NodeId(hasher.finish()),
                    first: index.saturating_mul(RUN_LENGTH),
                    starts,
                    end,
                }
            })
            .collect()
    }

    fn boundaries(&self) -> impl Iterator<Item = usize> + '_ {
        self.starts.iter().copied().chain([self.len])
    }

    fn position(&self, offset: usize) -> Option<TextPosition> {
        self.runs().iter().find_map(|run| run.position(offset))
    }

    fn offset(&self, position: TextPosition) -> Option<usize> {
        self.runs()
            .iter()
            .find(|run| run.id == position.node)?
            .boundaries()
            .nth(position.character_index)
    }

    fn selection(&self, TextSelection { anchor, focus }: TextSelection) -> Option<Selection> {
        Some(Selection {
            anchor: self.offset(anchor)?,
            focus: self.offset(focus)?,
        })
    }
}

struct Frame {
    bounds: Bounds<Pixels>,
    visible: Bounds<Pixels>,
    scroll: Pixels,
    scale: f32,
}

impl Frame {
    fn physical(&self, bounds: Bounds<Pixels>) -> Rect {
        let scaled = |value: Pixels| f64::from(f32::from(value) * self.scale);
        Rect::new(
            scaled(bounds.left()),
            scaled(bounds.top()),
            scaled(bounds.right().max(bounds.left())),
            scaled(bounds.bottom().max(bounds.top())),
        )
    }
}

pub(super) struct Painted {
    document: Document,
    text: String,
    edges: Vec<Pixels>,
    selection: TextSelection,
    frame: Frame,
}

impl Painted {
    pub(super) fn new(
        field: EntityId,
        edit: &Edit,
        line: &ShapedLine,
        bounds: Bounds<Pixels>,
        scroll: Pixels,
        window: &Window,
    ) -> Option<Self> {
        let document = Document::new(field, edit)?;
        let edges = document
            .boundaries()
            .map(|offset| line.x_for_index(offset))
            .collect();
        Self::measured(
            document,
            edit,
            edges,
            Frame {
                bounds,
                visible: bounds.intersect(&window.content_mask().bounds),
                scroll,
                scale: window.scale_factor(),
            },
        )
    }

    fn measured(document: Document, edit: &Edit, edges: Vec<Pixels>, frame: Frame) -> Option<Self> {
        let Frame { scroll, scale, .. } = frame;
        let measurable =
            scale.is_finite() && scale > 0.0 && f32::from(scroll).is_finite() && px(0.0) <= scroll;
        let ends = edit.ends();
        let selection = TextSelection {
            anchor: document.position(ends.anchor)?,
            focus: document.position(ends.focus)?,
        };
        measurable.then(|| Self {
            document,
            text: edit.text.clone(),
            edges,
            selection,
            frame,
        })
    }

    pub(super) fn run_ids(&self) -> Vec<NodeId> {
        self.document.runs().iter().map(|run| run.id).collect()
    }

    pub(super) fn publish(self, subtree: &mut A11ySubtreeBuilder<'_>) {
        let mut published = true;
        for (id, node) in self.runs() {
            published &= subtree.push_child(id, node);
        }
        let field = subtree.parent_node();
        field.set_bounds(self.frame.physical(self.frame.visible));
        field.set_clips_children();
        if published {
            field.set_text_selection(self.selection);
        }
    }

    fn runs(&self) -> Vec<(NodeId, Node)> {
        let words: Vec<usize> = self
            .text
            .split_word_bound_indices()
            .filter(|(_, word)| !word.chars().all(char::is_whitespace))
            .map(|(at, _)| at)
            .collect();
        let runs = self.document.runs();
        runs.iter()
            .enumerate()
            .map(|(index, run)| {
                let mut node = self.run(run, &words);
                if let Some(previous) = index.checked_sub(1).and_then(|index| runs.get(index)) {
                    node.set_previous_on_line(previous.id);
                }
                if let Some(next) = index.checked_add(1).and_then(|index| runs.get(index)) {
                    node.set_next_on_line(next.id);
                }
                (run.id, node)
            })
            .collect()
    }

    fn run(&self, run: &Run<'_>, words: &[usize]) -> Node {
        let scale = self.frame.scale;
        let edges = self
            .edges
            .get(run.first..=run.first.saturating_add(run.starts.len()))
            .unwrap_or_default();
        let start = edges.first().copied().unwrap_or_default();
        let end = edges.last().copied().unwrap_or_default();
        let mut node = Node::new(Role::TextRun);
        node.set_value(self.text.get(run.start()..run.end).unwrap_or_default());
        node.set_character_lengths(
            run.graphemes()
                .map(|(from, to)| u8::try_from(to.saturating_sub(from)).unwrap_or(u8::MAX))
                .collect::<Vec<_>>(),
        );
        node.set_character_positions(
            edges
                .iter()
                .take(run.starts.len())
                .map(|x| f32::from(*x - start) * scale)
                .collect::<Vec<_>>(),
        );
        node.set_character_widths(
            edges
                .iter()
                .zip(edges.iter().skip(1))
                .map(|(from, to)| f32::from(*to - *from) * scale)
                .collect::<Vec<_>>(),
        );
        node.set_word_starts(
            words
                .iter()
                .filter_map(|word| run.starts.binary_search(word).ok())
                .filter_map(|index| u8::try_from(index).ok())
                .collect::<Vec<_>>(),
        );
        node.set_text_direction(TextDirection::LeftToRight);
        let Frame { bounds, scroll, .. } = self.frame;
        node.set_bounds(self.frame.physical(Bounds::new(
            point(bounds.left() - scroll + start, bounds.top()),
            size(end - start, bounds.size.height),
        )));
        node
    }
}
