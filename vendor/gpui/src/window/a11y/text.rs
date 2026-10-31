use super::{A11yActionListener, A11yNodeBuilder};
use crate::{App, Bounds, Pixels, Point, ShapedLine, SharedString, Window};
use accesskit::{Action, ActionData, Node, NodeId, Role, TextPosition, TextSelection};
use collections::FxHashSet;
use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
mod tests;

/// A single-line editor's selection, in UTF-8 byte offsets. Direction is preserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessibleTextSelection {
    /// The stationary end of the selection.
    pub anchor: usize,
    /// The caret or moving end of the selection.
    pub focus: usize,
}

type SelectionListener = Box<dyn FnMut(AccessibleTextSelection, &mut Window, &mut App)>;

/// The current painted state of a single-line editor, registered during prepaint.
pub struct AccessibleText<'a> {
    /// The same shaped text used for painting.
    pub line: &'a ShapedLine,
    /// The scrolled line origin in logical window coordinates.
    pub origin: Point<Pixels>,
    /// The paint viewport in logical window coordinates, intersected with the current content mask.
    pub viewport: Bounds<Pixels>,
    /// Current byte-based anchor and caret.
    pub selection: AccessibleTextSelection,
    /// Changes on every accepted text replacement, including replacement with identical text.
    pub revision: u64,
}

pub(super) struct Registration {
    parent: NodeId,
    line: ShapedLine,
    origin: Point<Pixels>,
    viewport: Bounds<Pixels>,
    line_height: Pixels,
    scale: f32,
    selection: AccessibleTextSelection,
    revision: u64,
    listener: SelectionListener,
}

pub(super) struct Snapshot {
    text: SharedString,
    revision: u64,
    clip: NodeId,
    runs: Vec<Run>,
}

#[derive(Clone)]
struct Run {
    id: NodeId,
    offsets: Vec<usize>,
}

impl Snapshot {
    fn new(text: SharedString, revision: u64, next_id: &mut u64) -> Option<Self> {
        let mut allocate = || {
            let id = NodeId(*next_id);
            *next_id = next_id.checked_add(1)?;
            Some(id)
        };
        let clip = allocate()?;
        let offsets: Vec<_> = text
            .char_indices()
            .map(|(at, _)| at)
            .chain([text.len()])
            .collect();
        let count = offsets.len() - 1;
        let mut runs = Vec::new();
        for start in (0..count.max(1)).step_by(255) {
            let end = (start + 255).min(count);
            runs.push(Run {
                id: allocate()?,
                offsets: offsets[start..=end].to_vec(),
            });
        }
        Some(Self {
            text,
            revision,
            clip,
            runs,
        })
    }

    fn position(&self, offset: usize) -> Option<TextPosition> {
        self.runs.iter().find_map(|run| {
            run.offsets
                .binary_search(&offset)
                .ok()
                .map(|character_index| TextPosition {
                    node: run.id,
                    character_index,
                })
        })
    }
}

fn selection(runs: &[Run], selection: &TextSelection) -> Option<AccessibleTextSelection> {
    let offset = |position: TextPosition| {
        runs.iter()
            .find(|run| run.id == position.node)?
            .offsets
            .get(position.character_index)
            .copied()
    };
    Some(AccessibleTextSelection {
        anchor: offset(selection.anchor)?,
        focus: offset(selection.focus)?,
    })
}

impl Window {
    /// Registers the current single-line text during prepaint, under its enclosing TextInput.
    /// `origin` and `viewport` are logical window coordinates from the same layout used to paint.
    /// Invalid byte selections, hidden parents, and aborted layout publish no text ranges.
    /// Native selection actions are validated against the current text snapshot before dispatch.
    pub fn register_accessible_text(
        &mut self,
        text: AccessibleText<'_>,
        listener: impl FnMut(AccessibleTextSelection, &mut Window, &mut App) + 'static,
    ) {
        if !self.a11y.is_active() {
            return;
        }
        let AccessibleText {
            line,
            origin,
            viewport,
            selection,
            revision,
        } = text;
        let Some(parent) = self.a11y.nodes.parent() else {
            return;
        };
        if !line.text.is_char_boundary(selection.anchor)
            || !line.text.is_char_boundary(selection.focus)
        {
            return;
        }
        self.a11y.nodes.pending_text.push(Registration {
            parent,
            line: line.clone(),
            origin,
            viewport: viewport.intersect(&self.content_mask().bounds),
            line_height: viewport.size.height,
            scale: self.scale_factor(),
            selection,
            revision,
            listener: Box::new(listener),
        });
    }
}

impl A11yNodeBuilder {
    pub(super) fn finish_text(&mut self) -> Vec<(NodeId, A11yActionListener)> {
        let mut admitted = FxHashSet::default();
        let mut listeners: Vec<(NodeId, A11yActionListener)> = Vec::new();
        for registration in std::mem::take(&mut self.pending_text) {
            let parent = registration.parent;
            if !self.contains_native(parent) || admitted.contains(&parent) {
                continue;
            }
            if !self
                .node_mut(parent)
                .is_some_and(|node| node.role() == Role::TextInput && !node.is_disabled())
            {
                continue;
            }
            if !self.text_snapshots.get(&parent).is_some_and(|snapshot| {
                snapshot.text == registration.line.text
                    && snapshot.revision == registration.revision
            }) {
                let Some(snapshot) = Snapshot::new(
                    registration.line.text.clone(),
                    registration.revision,
                    &mut self.next_id,
                ) else {
                    continue;
                };
                self.text_snapshots.insert(parent, snapshot);
            }
            let Some(snapshot) = self.text_snapshots.get(&parent) else {
                continue;
            };
            let (Some(anchor), Some(focus)) = (
                snapshot.position(registration.selection.anchor),
                snapshot.position(registration.selection.focus),
            ) else {
                continue;
            };
            let mut clip = Node::new(Role::GenericContainer);
            clip.set_bounds(physical_bounds(registration.viewport, registration.scale));
            clip.set_clips_children();
            clip.set_children(snapshot.runs.iter().map(|run| run.id).collect::<Vec<_>>());
            let clip_id = snapshot.clip;
            self.live_ids.insert(clip_id);
            self.all_nodes.push((clip_id, clip));
            for (index, run) in snapshot.runs.iter().enumerate() {
                self.live_ids.insert(run.id);
                self.all_nodes
                    .push((run.id, run_node(snapshot, index, &registration)));
            }
            let runs = snapshot.runs.clone();
            if let Some(node) = self.node_mut(parent) {
                node.push_child(clip_id);
                node.set_text_selection(TextSelection { anchor, focus });
                node.add_action(Action::SetTextSelection);
            }
            let mut listener = registration.listener;
            listeners.push((
                parent,
                Box::new(move |data, window, cx| {
                    if let Some(ActionData::SetTextSelection(native)) = data
                        && let Some(selected) = selection(&runs, native)
                    {
                        listener(selected, window, cx);
                    }
                }),
            ));
            admitted.insert(parent);
        }
        self.text_snapshots
            .retain(|parent, _| admitted.contains(parent));
        listeners
    }
}

fn physical_bounds(bounds: Bounds<Pixels>, scale: f32) -> accesskit::Rect {
    accesskit::Rect::new(
        f64::from(bounds.left().0 * scale),
        f64::from(bounds.top().0 * scale),
        f64::from(bounds.right().max(bounds.left()).0 * scale),
        f64::from(bounds.bottom().max(bounds.top()).0 * scale),
    )
}

fn run_node(snapshot: &Snapshot, index: usize, registration: &Registration) -> Node {
    let run = &snapshot.runs[index];
    let start = run.offsets[0];
    let end = run.offsets[run.offsets.len() - 1];
    let origin_x = registration.line.x_for_index(start);
    let end_x = registration.line.x_for_index(end);
    let mut node = Node::new(Role::TextRun);
    node.set_value(&snapshot.text[start..end]);
    node.set_character_lengths(
        run.offsets
            .windows(2)
            .map(|pair| (pair[1] - pair[0]) as u8)
            .collect::<Vec<_>>(),
    );
    node.set_character_positions(
        run.offsets[..run.offsets.len() - 1]
            .iter()
            .map(|offset| {
                (registration.line.x_for_index(*offset) - origin_x).0 * registration.scale
            })
            .collect::<Vec<_>>(),
    );
    node.set_character_widths(
        run.offsets
            .windows(2)
            .map(|pair| {
                (registration.line.x_for_index(pair[1]) - registration.line.x_for_index(pair[0])).0
                    * registration.scale
            })
            .collect::<Vec<_>>(),
    );
    node.set_word_starts(
        snapshot
            .text
            .split_word_bound_indices()
            .filter(|(_, word)| !word.chars().all(char::is_whitespace))
            .filter_map(|(offset, _)| {
                run.offsets[..run.offsets.len() - 1]
                    .binary_search(&offset)
                    .ok()
                    .map(|index| index as u8)
            })
            .collect::<Vec<_>>(),
    );
    node.set_text_direction(accesskit::TextDirection::LeftToRight);
    node.set_bounds(physical_bounds(
        Bounds::new(
            crate::point(registration.origin.x + origin_x, registration.origin.y),
            crate::size(end_x - origin_x, registration.line_height),
        ),
        registration.scale,
    ));
    if let Some(previous) = index
        .checked_sub(1)
        .and_then(|index| snapshot.runs.get(index))
    {
        node.set_previous_on_line(previous.id);
    }
    if let Some(next) = snapshot.runs.get(index + 1) {
        node.set_next_on_line(next.id);
    }
    node
}
