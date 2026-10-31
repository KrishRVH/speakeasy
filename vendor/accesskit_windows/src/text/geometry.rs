use accesskit::Rect;
use accesskit_consumer::{Node, TextRange};

#[cfg(test)]
mod tests;

pub(super) fn clipped(mut rect: Rect, node: Node<'_>) -> Option<Rect> {
    let zero_advance = rect.width() == 0.0;
    let mut parent = node.parent();
    while let Some(node) = parent {
        if node.data().clips_children() {
            let bounds = node.data().bounds()?;
            if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
                return None;
            }
            let clip = node.bounding_box()?;
            if clip.is_empty() {
                return None;
            }
            if zero_advance && (rect.x0 < clip.x0 || rect.x0 > clip.x1) {
                return None;
            }
            rect = rect.intersect(clip);
        }
        parent = node.parent();
    }
    (rect.height() > 0.0 && (rect.width() > 0.0 || zero_advance)).then_some(rect)
}

fn visible_characters<'a>(range: &TextRange<'a>, mut visit: impl FnMut(TextRange<'a>, Rect)) {
    let end = range.end();
    let mut start = range.start().biased_to_start();
    while start < end {
        let next = start.forward_to_character_end();
        let next = if next > end { end } else { next };
        if next <= start {
            break;
        }
        let mut character = start.to_degenerate_range();
        character.set_end(next);
        for rect in character.bounding_boxes() {
            if let Some(rect) = clipped(rect, *start.inner_node()) {
                visit(character.clone(), rect);
            }
        }
        start = next.biased_to_start();
    }
}

pub(super) fn bounding_rectangles(range: &TextRange<'_>) -> Vec<Rect> {
    let mut spans: Vec<(Rect, TextRange<'_>)> = Vec::new();
    visible_characters(range, |character, rect| {
        if let Some((previous, previous_range)) = spans.last_mut() {
            if previous.y0 == rect.y0
                && previous.y1 == rect.y1
                && previous_range.end().biased_to_start() == character.start().biased_to_start()
                && previous_range.start().inner_node().parent_id()
                    == character.start().inner_node().parent_id()
            {
                *previous = previous.union(rect);
                previous_range.set_end(character.end());
                return;
            }
        }
        if rect.width() > 0.0 {
            spans.push((rect, character));
        }
    });
    spans.into_iter().map(|(rect, _)| rect).collect()
}

pub(crate) fn visible_ranges<'a>(range: &TextRange<'a>) -> Vec<TextRange<'a>> {
    let mut ranges: Vec<TextRange<'a>> = Vec::new();
    visible_characters(range, |character, _| {
        if let Some(previous) = ranges.last_mut() {
            if previous.end().biased_to_start() == character.start().biased_to_start() {
                previous.set_end(character.end());
                return;
            }
        }
        ranges.push(character);
    });
    if ranges.is_empty() {
        ranges.push(range.start().to_degenerate_range());
    }
    ranges
}
