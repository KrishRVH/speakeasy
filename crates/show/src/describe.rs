//! How a Show reads to its agent: marks, actions, keyframe events, and the markers beside
//! narration in the summary. Positions are fractions of the surface; screen text never appears.

use std::fmt;

use crate::{
    Keyframe, Mark, Point, Rect, Shape,
    recording::Action,
    select::{Basis, Reason},
};

/// What a keyframe chosen for `basis` shows, for a reader; `marks` are the selection's.
pub(crate) fn event(basis: &Basis, marks: &[Mark]) -> String {
    match basis {
        Basis::Mark(group) => marks
            .get(group.clone())
            .unwrap_or_default()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "),
        Basis::Click(action) => Described(*action).to_string(),
        Basis::SceneChange => "scene changed".to_owned(),
        Basis::Cue {
            word,
            snapped: true,
        } => format!("said “{word}” as the screen changed"),
        Basis::Cue {
            word,
            snapped: false,
        } => format!("said “{word}”"),
        Basis::Middle => "the screen midway through".to_owned(),
    }
}

/// Describes the mark for a reader, with positions as fractions of the surface.
impl fmt::Display for Mark {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Rect {
            left,
            top,
            right,
            bottom,
        } = self.bounds;
        let region = format!("x {left:.2}–{right:.2}, y {top:.2}–{bottom:.2}");
        let strokes = match self.stroke_count() {
            0 | 1 => String::new(),
            count => format!(" ({count} strokes)"),
        };
        let verb = self.shape.verb();
        match self.shape {
            Shape::Circle | Shape::Scribble => write!(f, "{verb} region {region}{strokes}"),
            Shape::Underline => write!(
                f,
                "{verb} x {left:.2}–{right:.2} at y {:.2}{strokes}",
                top.midpoint(bottom)
            ),
            Shape::Line { from, to } => write!(f, "{verb} from {} to {}", At(from), At(to)),
            Shape::Arrow { from, to } => {
                write!(f, "{verb} from {} pointing to {}", At(from), At(to))
            },
            Shape::Tap => write!(f, "{verb} {}{strokes}", At(self.bounds.center())),
        }
    }
}

impl Shape {
    /// What drawing the shape did, in a few words.
    const fn verb(self) -> &'static str {
        match self {
            Self::Tap => "tapped",
            Self::Circle => "circled",
            Self::Underline => "underlined",
            Self::Line { .. } => "drew a line",
            Self::Arrow { .. } => "drew an arrow",
            Self::Scribble => "marked",
        }
    }
}

/// What a keyframe shows happening beside the words said over it: the click or drag whose result
/// it shows, or what its ink does.
pub(crate) struct Marker<'a>(pub(crate) &'a Keyframe, pub(crate) &'a [Mark]);

impl fmt::Display for Marker<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(keyframe, marks) = self;
        match (keyframe.reason, keyframe.action) {
            (Reason::Click, Some(action)) => write!(f, " [{}]", Verb(action)),
            (Reason::Mark, _) if !marks.is_empty() => {
                let mut verbs: Vec<&str> = Vec::with_capacity(marks.len());
                for verb in marks.iter().map(|mark| mark.shape.verb()) {
                    if !verbs.contains(&verb) {
                        verbs.push(verb);
                    }
                }
                write!(f, " [{}]", verbs.join(", "))
            },
            _ => Ok(()),
        }
    }
}

/// A count and its noun, plural unless the count is one.
pub(crate) struct Count(pub(crate) usize, pub(crate) &'static str);

impl fmt::Display for Count {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(count, noun) = *self;
        let plural = if count == 1 { "" } else { "s" };
        write!(f, "{count} {noun}{plural}")
    }
}

/// What an action did, in a word or two, such as "clicked" or "dragged right".
struct Verb(Action);

impl fmt::Display for Verb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Action::Click { .. } => f.write_str("clicked"),
            Action::Drag { from, to, .. } => write!(f, "dragged {}", Direction(to - from)),
        }
    }
}

/// An action described for a reader, with where it happened.
struct Described(Action);

impl fmt::Display for Described {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verb = Verb(self.0);
        match self.0 {
            Action::Click { point, .. } => write!(f, "{verb} ({})", At(point)),
            Action::Drag { from, to, .. } => write!(f, "{verb} ({} → {})", At(from), At(to)),
        }
    }
}

/// The main direction of a movement, or "a little" when it barely moved.
struct Direction(Point);

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MOVED: f32 = 0.04;
        let Point { x, y } = self.0;
        let horizontal = (x.abs() > MOVED).then_some(if x > 0.0 { "right" } else { "left" });
        let vertical = (y.abs() > MOVED).then_some(if y > 0.0 { "down" } else { "up" });
        match (vertical, horizontal) {
            (_, Some(horizontal)) if x.abs() > 2.0 * y.abs() => f.write_str(horizontal),
            (Some(vertical), _) if y.abs() > 2.0 * x.abs() => f.write_str(vertical),
            (Some(vertical), Some(horizontal)) => write!(f, "{vertical} and {horizontal}"),
            (Some(only), None) | (None, Some(only)) => f.write_str(only),
            (None, None) => f.write_str("a little"),
        }
    }
}

/// A normalized position, written for a reader.
struct At(Point);

impl fmt::Display for At {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "x {:.2}, y {:.2}", self.0.x, self.0.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Moment, Span};

    #[test]
    fn drags_describe_their_direction() {
        let drag = |to: [f32; 2]| {
            Described(Action::Drag {
                span: Span::at(Moment::START),
                from: Point::new(0.5, 0.5),
                to: to.into(),
            })
            .to_string()
        };
        assert_eq!(
            drag([0.5, 0.9]),
            "dragged down (x 0.50, y 0.50 → x 0.50, y 0.90)"
        );
        assert_eq!(
            drag([0.2, 0.2]),
            "dragged up and left (x 0.50, y 0.50 → x 0.20, y 0.20)"
        );
        assert_eq!(
            drag([0.51, 0.5]),
            "dragged a little (x 0.50, y 0.50 → x 0.51, y 0.50)"
        );
    }
}
