use super::{A11yActionListener, Registration, allocate_id, physical_bounds};
use crate::{App, Bounds, Pixels, Point, Window};
use accesskit::{Action, ActionData, Node, NodeId, Role};
use unicode_segmentation::UnicodeSegmentation as _;

pub(super) type Listener = Box<dyn FnMut(Pixels, &mut Window, &mut App)>;

#[derive(PartialEq)]
struct Geometry {
    origin: Point<Pixels>,
    viewport: Bounds<Pixels>,
    full_viewport: Bounds<Pixels>,
    scale: f32,
    line_height: Pixels,
    limit: Pixels,
    advances: Vec<Pixels>,
}

impl Geometry {
    fn new(registration: &Registration) -> Self {
        Self {
            origin: registration.origin,
            viewport: registration.viewport,
            full_viewport: registration.full_viewport,
            scale: registration.scale,
            line_height: registration.line_height,
            limit: registration.scroll_limit,
            advances: registration
                .line
                .text
                .grapheme_indices(true)
                .map(|(offset, _)| offset)
                .chain([registration.line.text.len()])
                .map(|offset| registration.line.x_for_index(offset))
                .collect(),
        }
    }
}

pub(super) struct ScrollTarget {
    pub(super) id: NodeId,
    geometry: Geometry,
}

impl ScrollTarget {
    pub(super) fn new(registration: &Registration, next_id: &mut u64) -> Option<Self> {
        Some(Self {
            id: allocate_id(next_id)?,
            geometry: Geometry::new(registration),
        })
    }

    pub(super) fn refresh(&mut self, registration: &Registration, next_id: &mut u64) -> Option<()> {
        let geometry = Geometry::new(registration);
        if self.geometry != geometry {
            self.id = allocate_id(next_id)?;
            self.geometry = geometry;
        }
        Some(())
    }

    pub(super) fn node(&self, offset: Pixels, children: Vec<NodeId>) -> Node {
        let mut node = Node::new(Role::GenericContainer);
        node.set_bounds(physical_bounds(self.geometry.viewport, self.geometry.scale));
        node.set_clips_children();
        node.set_children(children);
        node.set_scroll_x(f64::from(offset.0 * self.geometry.scale));
        node.set_scroll_x_min(0.0);
        node.set_scroll_x_max(f64::from(self.geometry.limit.0 * self.geometry.scale));
        node.add_action(Action::SetScrollOffset);
        node
    }

    pub(super) fn listener(&self, mut listener: Listener) -> A11yActionListener {
        let scale = self.geometry.scale;
        let limit = f64::from(self.geometry.limit.0 * scale);
        Box::new(move |data, window, cx| {
            if let Some(ActionData::SetScrollOffset(offset)) = data
                && offset.x.is_finite()
                && offset.y == 0.0
                && (0.0..=limit).contains(&offset.x)
            {
                listener(crate::px((offset.x / f64::from(scale)) as f32), window, cx);
            }
        })
    }
}
