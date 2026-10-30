//! What a Show captured besides its frames: ink, pins, pointer actions, and narration.

use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{Moment, Point, Span};

/// Everything a Show captured except its frames.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    /// How long the Show ran.
    pub duration: Moment,
    /// How the screen recording ended. `show.json` leaves it out when the Show ran until it was
    /// finished, and bundles written before it existed leave it out whatever happened.
    #[serde(default, skip_serializing_if = "Ending::is_finished")]
    pub ending: Ending,
    /// The captured surface's size in logical pixels, which ink is measured against.
    pub surface: Size,
    /// Ink in the order it was drawn.
    pub strokes: Vec<Stroke>,
    /// Numbered pins in the order they were dropped.
    pub pins: Vec<Pin>,
    /// Clicks and drags that passed through to the apps underneath.
    pub actions: Vec<Action>,
    /// What was said while the Show ran.
    pub narration: Narration,
}

/// How a Show's screen recording ended. Every way but [`Finished`](Self::Finished) stopped it at
/// [`Recording::duration`], before the Show was finished, so any later steps are missing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Ending {
    /// The Show ran until it was finished, by the user or the recording limit.
    #[default]
    Finished,
    /// Screen capture stopped on its own.
    ScreenStopped,
    /// The recorded window closed.
    WindowClosed,
    /// The recorded window was minimized or hidden, moved to another desktop, or moved off the
    /// recorded display.
    WindowOffScreen,
}

impl Ending {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's skip_serializing_if passes the field by reference"
    )]
    const fn is_finished(&self) -> bool {
        matches!(self, Self::Finished)
    }
}

/// A size in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Size {
    /// Horizontal extent.
    pub width: f32,
    /// Vertical extent.
    pub height: f32,
}

/// A sampled position of the pen.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "(f32, f32, Moment)", into = "(f32, f32, Moment)")]
pub struct InkPoint {
    /// Where the pen was, normalized to the surface.
    pub point: Point,
    /// When it was there.
    pub at: Moment,
}

impl From<(f32, f32, Moment)> for InkPoint {
    fn from((x, y, at): (f32, f32, Moment)) -> Self {
        Self {
            point: Point::new(x, y),
            at,
        }
    }
}

impl From<InkPoint> for (f32, f32, Moment) {
    fn from(ink: InkPoint) -> Self {
        (ink.point.x, ink.point.y, ink.at)
    }
}

/// One continuous line of ink: at least one point, in the order it was drawn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Vec<InkPoint>", into = "Vec<InkPoint>")]
pub struct Stroke {
    points: Vec<InkPoint>,
    /// The ends of `points`, which is never empty, so reading them cannot fail.
    first: InkPoint,
    last: InkPoint,
}

impl Stroke {
    /// The stroke through `points`, ordered by time; `None` without points.
    #[must_use]
    pub fn new(mut points: Vec<InkPoint>) -> Option<Self> {
        points.sort_by_key(|ink| ink.at);
        Some(Self {
            first: *points.first()?,
            last: *points.last()?,
            points,
        })
    }

    /// The sampled pen positions, in time order.
    #[must_use]
    pub fn points(&self) -> &[InkPoint] {
        &self.points
    }

    /// When the pen went down and when it lifted.
    #[must_use]
    pub fn span(&self) -> Span {
        Span::new(self.first.at, self.last.at)
    }

    /// Where the pen went down.
    #[must_use]
    pub const fn first(&self) -> InkPoint {
        self.first
    }

    /// Where the pen lifted.
    #[must_use]
    pub const fn last(&self) -> InkPoint {
        self.last
    }

    /// Every position the pen passed through, in time order.
    pub fn path(&self) -> impl Iterator<Item = Point> {
        self.points.iter().map(|ink| ink.point)
    }

    /// The positions the pen passed through by `moment`.
    pub(crate) fn path_until(&self, moment: Moment) -> impl Iterator<Item = Point> {
        self.points
            .iter()
            .take_while(move |ink| ink.at <= moment)
            .map(|ink| ink.point)
    }
}

/// A stroke needs at least one point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmptyStroke;

impl fmt::Display for EmptyStroke {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a stroke needs at least one point")
    }
}

impl std::error::Error for EmptyStroke {}

impl TryFrom<Vec<InkPoint>> for Stroke {
    type Error = EmptyStroke;

    fn try_from(points: Vec<InkPoint>) -> Result<Self, Self::Error> {
        Self::new(points).ok_or(EmptyStroke)
    }
}

impl From<Stroke> for Vec<InkPoint> {
    fn from(stroke: Stroke) -> Self {
        stroke.points
    }
}

/// A pin dropped where the pointer was, numbered in sequence with the marks ink makes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pin {
    /// When it was dropped.
    pub at: Moment,
    /// Where, normalized to the surface.
    pub point: Point,
}

/// A pointer action that passed through to the app underneath.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Action {
    /// A press and release in about the same place.
    Click {
        /// When the button was released.
        at: Moment,
        /// Where, normalized to the surface.
        point: Point,
    },
    /// A press that moved before its release.
    Drag {
        /// From press to release.
        span: Span,
        /// Where the press happened.
        from: Point,
        /// Where the release happened.
        to: Point,
    },
}

impl Action {
    /// When the action happened: a click's release, or a drag from press to release.
    #[must_use]
    pub const fn span(self) -> Span {
        match self {
            Self::Click { at, .. } => Span::at(at),
            Self::Drag { span, .. } => span,
        }
    }

    /// Where the action lands: a click's point, or where a drag was released.
    #[must_use]
    pub const fn point(self) -> Point {
        match self {
            Self::Click { point, .. } | Self::Drag { to: point, .. } => point,
        }
    }
}

/// The narration, as the recognizer reported it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Narration {
    /// Words with the moments they were spoken, in order; empty when nothing was said.
    Words(Vec<Word>),
    /// Text without timings, which can only be attached to the first keyframe.
    Text(String),
}

impl Default for Narration {
    fn default() -> Self {
        Self::Words(Vec::new())
    }
}

/// Leaves out untimed text, which is the user's speech; timed words redact their own.
impl fmt::Debug for Narration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Words(words) => f.debug_tuple("Words").field(words).finish(),
            Self::Text(text) => f.debug_struct("Text").field("bytes", &text.len()).finish(),
        }
    }
}

impl Narration {
    /// The same narration, every word `by` later, such as from the start of the audio to the
    /// start of the Show.
    #[must_use]
    pub fn delayed(self, by: Duration) -> Self {
        match self {
            Self::Words(words) => Self::Words(
                words
                    .into_iter()
                    .map(|word| Word {
                        span: word.span.delayed(by),
                        ..word
                    })
                    .collect(),
            ),
            text @ Self::Text(_) => text,
        }
    }

    /// The timed words, or none when the recognizer gave no timings.
    pub(crate) fn words(&self) -> &[Word] {
        match self {
            Self::Words(words) => words,
            Self::Text(_) => &[],
        }
    }
}

/// A recognized word and when it was spoken.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Word {
    /// When the word was spoken.
    pub span: Span,
    /// The word as recognized, with any attached punctuation.
    pub text: String,
}

/// Leaves out the word, which is the user's speech.
impl fmt::Debug for Word {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { span, text: _ } = self;
        f.debug_struct("Word")
            .field("span", span)
            .finish_non_exhaustive()
    }
}

impl Word {
    /// Whether the word holds a letter or digit, rather than only punctuation.
    pub(crate) fn is_spoken(&self) -> bool {
        self.text.chars().any(char::is_alphanumeric)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Narration of each `(start, text)` word, each spoken for 200 ms.
    pub(crate) fn words(spoken: &[(u64, &str)]) -> Narration {
        Narration::Words(
            spoken
                .iter()
                .map(|&(start, text)| Word {
                    span: Span::new(
                        Moment::from_millis(start),
                        Moment::from_millis(start.saturating_add(200)),
                    ),
                    text: text.to_owned(),
                })
                .collect(),
        )
    }

    fn ink(x: f32, at: u64) -> InkPoint {
        InkPoint {
            point: Point::new(x, 0.5),
            at: Moment::from_millis(at),
        }
    }

    #[test]
    fn strokes_order_their_points_and_refuse_to_be_empty() {
        let stroke = Stroke::new(vec![ink(0.3, 900), ink(0.1, 100)]).unwrap();
        assert_eq!(
            stroke.span(),
            Span::new(Moment::from_millis(100), Moment::from_millis(900))
        );
        let json = serde_json::to_string(&stroke).unwrap();
        assert_eq!(json, "[[0.1,0.5,100],[0.3,0.5,900]]");
        assert_eq!(serde_json::from_str::<Stroke>(&json).unwrap(), stroke);
        assert!(Stroke::new(Vec::new()).is_none());
        assert!(serde_json::from_str::<Stroke>("[]").is_err());
    }

    #[test]
    fn debugging_narration_never_prints_what_was_said() {
        let shown = |said: &str| {
            format!(
                "{:?}",
                [words(&[(0, said)]), Narration::Text(said.to_owned())]
            )
        };
        assert_eq!(shown("alpha"), shown("omega"));
    }
}
