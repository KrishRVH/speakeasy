//! How a Show reads to its agent: marks, actions, keyframe events, and the markers beside
//! narration in the summary. Positions are normalized to the surface; screen text never appears.

use std::fmt;

use crate::{
    Keyframe, Mark, Point, Rect, Shape,
    recording::Action,
    select::{Basis, Reason},
};

/// What a keyframe chosen for `basis` shows, for a reader; `marks` are the selection's.
pub(crate) fn event(basis: &Basis, marks: &[Mark]) -> String {
    match basis {
        Basis::Mark { group, during } => marks
            .get(group.clone())
            .unwrap_or_default()
            .iter()
            .map(ToString::to_string)
            .chain(during.map(|action| Described(action).to_string()))
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

/// Describes the mark for a reader after its number, with positions normalized to the surface.
impl fmt::Display for Mark {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Rect {
            left,
            top,
            right,
            bottom,
        } = self.bounds;
        let strokes = Strokes(self.strokes.len());
        write!(f, "{} {} ", Numeral(self.number), self.shape.verb())?;
        match self.shape {
            Shape::Circle | Shape::Scribble => write!(
                f,
                "region x {left:.2}–{right:.2}, y {top:.2}–{bottom:.2}{strokes}"
            ),
            Shape::Underline => write!(
                f,
                "x {left:.2}–{right:.2} at y {:.2}{strokes}",
                top.midpoint(bottom)
            ),
            Shape::Line { from, to } => write!(f, "from {} to {}", At(from), At(to)),
            Shape::Arrow { from, to } => write!(f, "from {} pointing to {}", At(from), At(to)),
            Shape::Tap => write!(f, "{}{strokes}", At(self.bounds.center())),
            Shape::Pin { point } => write!(f, "{}", At(point)),
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
            Self::Pin { .. } => "pinned",
        }
    }
}

/// A mark's number as a circled numeral, ① through ㊿, then in parentheses.
struct Numeral(usize);

impl fmt::Display for Numeral {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Unicode circles each run of numbers in its own block, from the code point `glyph`.
        let circled = |glyph: u32, from: usize| {
            let offset = u32::try_from(self.0.checked_sub(from)?).ok()?;
            char::from_u32(glyph.checked_add(offset)?)
        };
        let glyph = match self.0 {
            1..=20 => circled(0x2460, 1),
            21..=35 => circled(0x3251, 21),
            36..=50 => circled(0x32B1, 36),
            _ => None,
        };
        match glyph {
            Some(glyph) => write!(f, "{glyph}"),
            None => write!(f, "({})", self.0),
        }
    }
}

/// What a keyframe shows happening beside the words said over it: the click or drag whose result
/// it shows, or what each of its numbered marks does and any click or drag it also reports.
pub(crate) struct Marker<'a>(pub(crate) &'a Keyframe, pub(crate) &'a [Mark]);

impl fmt::Display for Marker<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(keyframe, marks) = self;
        match (keyframe.reason, keyframe.action) {
            (Reason::Click, Some(action)) => write!(f, " [{}]", Verb(action)),
            (Reason::Mark, during) if !marks.is_empty() => {
                let marks = marks
                    .iter()
                    .map(|mark| format!("{} {}", Numeral(mark.number), mark.shape.verb()));
                let action = during.map(|action| Verb(action).to_string());
                let parts: Vec<String> = marks.chain(action).collect();
                write!(f, " [{}]", parts.join(", "))
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

/// How many strokes a mark joined, as a parenthetical when more than one.
struct Strokes(usize);

impl fmt::Display for Strokes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            0 | 1 => Ok(()),
            count => write!(f, " ({})", Count(count, "stroke")),
        }
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

    #[test]
    fn a_mark_keyframe_names_each_numbered_mark_and_the_click_made_while_it_was_drawn() {
        let scribble = Mark {
            number: 3,
            shape: Shape::Scribble,
            strokes: 0..2,
            span: Span::new(Moment::START, Moment::from_millis(2_000)),
            bounds: Rect {
                left: 0.2,
                top: 0.2,
                right: 0.4,
                bottom: 0.4,
            },
        };
        let pin = Mark {
            number: 4,
            shape: Shape::Pin {
                point: Point::new(0.6, 0.25),
            },
            strokes: 2..2,
            span: Span::at(Moment::from_millis(2_000)),
            bounds: Rect::at(Point::new(0.6, 0.25)),
        };
        let keyframe = |action| Keyframe {
            index: 1,
            at: Moment::from_millis(2_000),
            reason: Reason::Mark,
            event: String::new(),
            said: None,
            marks: 0..2,
            action,
        };
        let click = Action::Click {
            at: Moment::from_millis(1_000),
            point: Point::new(0.3, 0.3),
        };
        assert_eq!(
            Marker(&keyframe(None), std::slice::from_ref(&scribble)).to_string(),
            " [③ marked]"
        );
        assert_eq!(
            Marker(&keyframe(Some(click)), &[scribble.clone(), pin.clone()]).to_string(),
            " [③ marked, ④ pinned, clicked]"
        );
        assert_eq!(pin.to_string(), "④ pinned x 0.60, y 0.25");
        let twin = Mark {
            number: 5,
            ..scribble
        };
        assert_eq!(
            Marker(&keyframe(None), &[twin.clone(), Mark { number: 6, ..twin }]).to_string(),
            " [⑤ marked, ⑥ marked]"
        );
    }

    #[test]
    fn numerals_are_circled_through_fifty() {
        let numerals: Vec<String> = [1, 20, 21, 35, 36, 50, 51, 0]
            .into_iter()
            .map(|number| Numeral(number).to_string())
            .collect();
        assert_eq!(numerals, ["①", "⑳", "㉑", "㉟", "㊱", "㊿", "(51)", "(0)"]);
    }
}
