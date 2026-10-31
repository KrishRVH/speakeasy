use accesskit::{Action, Point, Rect};
use accesskit_consumer::{Node, NodeId, TextRange};

#[cfg(test)]
mod tests;

pub(super) struct Request {
    pub(super) target: NodeId,
    pub(super) point: Point,
}

fn container<'a>(mut node: Node<'a>, root: NodeId) -> Option<Node<'a>> {
    loop {
        if node.data().clips_children() && node.data().supports_action(Action::SetScrollOffset) {
            return Some(node);
        }
        if node.id() == root {
            return None;
        }
        node = node.parent()?;
    }
}

fn bounds(range: &TextRange<'_>) -> Option<Rect> {
    range
        .bounding_boxes()
        .into_iter()
        .reduce(|left, right| left.union(right))
}

fn endpoint_bounds(range: &TextRange<'_>, start: bool) -> Option<Rect> {
    if range.is_degenerate() {
        return bounds(range);
    }
    let position = if start {
        range.start().biased_to_start()
    } else {
        range.end().backward_to_character_start()
    };
    let mut character = position.to_degenerate_range();
    character.set_end(position.forward_to_character_end());
    bounds(&character)
}

pub(super) fn reveal(range: &TextRange<'_>, start: bool) -> Option<Request> {
    let root = range.node().id();
    let target = container(*range.start().biased_to_start().inner_node(), root)?;
    let last = container(*range.end().biased_to_end().inner_node(), root)?;
    if target.id() != last.id() {
        return None;
    }
    let transform = target.transform();
    let [a, b, c, d, e, f] = transform.as_coeffs();
    if ![a, b, c, d, e, f].into_iter().all(f64::is_finite)
        || a <= 0.0
        || d <= 0.0
        || b != 0.0
        || c != 0.0
    {
        return None;
    }
    let raw = target.data().bounds()?;
    if raw.width() <= 0.0 || raw.height() <= 0.0 {
        return None;
    }
    let inverse = transform.inverse();
    let viewport =
        inverse.transform_rect_bbox(super::geometry::clipped(target.bounding_box()?, target)?);
    let mut interval = inverse.transform_rect_bbox(bounds(range)?);
    let rounding = f64::from(f32::EPSILON)
        * interval
            .x0
            .abs()
            .max(interval.x1.abs())
            .max(viewport.x0.abs())
            .max(viewport.x1.abs())
            .max(1.0)
        * 4.0;
    if interval.width() > viewport.width() + rounding {
        interval = inverse.transform_rect_bbox(endpoint_bounds(range, start)?);
        if interval.width() > viewport.width() + rounding {
            if start {
                interval.x1 = interval.x0 + viewport.width();
            } else {
                interval.x0 = interval.x1 - viewport.width();
            }
        }
    }
    if interval.width() == 0.0 {
        interval.x1 = interval.x0 + 1.0_f64.min(viewport.width());
    }
    let current = target.data().scroll_x()?;
    let minimum = target.data().scroll_x_min()?;
    let maximum = target.data().scroll_x_max()?;
    if ![
        current,
        minimum,
        maximum,
        interval.x0,
        interval.x1,
        interval.y0,
        interval.y1,
        viewport.x0,
        viewport.x1,
        viewport.y0,
        viewport.y1,
    ]
    .into_iter()
    .all(f64::is_finite)
        || minimum > maximum
        || current < minimum
        || current > maximum
    {
        return None;
    }
    let translation = if interval.x0 < viewport.x0 - rounding {
        interval.x0 - viewport.x0
    } else if interval.x1 > viewport.x1 + rounding {
        interval.x1 - viewport.x1
    } else {
        0.0
    };
    let offset = (current + translation).clamp(minimum, maximum);
    let movement = offset - current;
    if interval.x0 - movement < viewport.x0 - rounding
        || interval.x1 - movement > viewport.x1 + rounding
        || interval.y0 < viewport.y0 - rounding
        || interval.y1 > viewport.y1 + rounding
    {
        return None;
    }
    Some(Request {
        target: target.id(),
        point: Point::new(offset, 0.0),
    })
}
