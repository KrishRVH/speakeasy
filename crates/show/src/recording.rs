use std::time::Duration;

use serde::{Deserialize, Serialize, Serializer};

use crate::{Mark, Moment, Point, Rect, Span};

pub(crate) mod spaces;
pub use spaces::Spaces;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    pub duration: Moment,
    #[serde(default, skip_serializing_if = "Ending::is_finished")]
    pub ending: Ending,
    /// Points are normalized to this surface; shapes and drag distances use its pixels.
    pub surface: Size,
    #[serde(default, skip_serializing_if = "Spaces::is_empty")]
    pub spaces: Spaces,
    pub strokes: Vec<Stroke>,
    pub pins: Vec<Pin>,
    pub actions: Vec<Action>,
    pub narration: Narration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narration_lost: Option<NarrationLoss>,
}

impl Recording {
    #[must_use]
    pub fn mark_part(&self, mark: &Mark) -> Option<Rect> {
        if self.spaces_valid() {
            self.spaces.mark(mark, &self.pins)
        } else {
            None
        }
    }

    pub(crate) fn spaces_valid(&self) -> bool {
        self.spaces.valid(
            self.strokes.len(),
            self.pins.len(),
            self.actions.len(),
            self.duration,
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Ending {
    #[default]
    Finished,
    ScreenStopped,
    WindowClosed,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NarrationLoss {
    Incomplete,
    AudioStopped,
    AudioSilent,
    NotTranscribed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "(f32, f32, Moment)", into = "(f32, f32, Moment)")]
pub struct InkPoint {
    pub point: Point,
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

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "Vec<InkPoint>")]
pub struct Stroke {
    points: Vec<InkPoint>,
    first: InkPoint,
    last: InkPoint,
}

impl Stroke {
    #[must_use]
    pub fn new(mut points: Vec<InkPoint>) -> Option<Self> {
        points.sort_by_key(|ink| ink.at);
        Some(Self {
            first: *points.first()?,
            last: *points.last()?,
            points,
        })
    }

    #[must_use]
    pub(crate) fn points(&self) -> &[InkPoint] {
        &self.points
    }

    #[must_use]
    pub(crate) fn span(&self) -> Span {
        Span::new(self.first.at, self.last.at)
    }

    #[must_use]
    pub(crate) const fn first(&self) -> InkPoint {
        self.first
    }

    #[must_use]
    pub(crate) const fn last(&self) -> InkPoint {
        self.last
    }

    pub fn path(&self) -> impl Iterator<Item = Point> {
        self.points.iter().map(|ink| ink.point)
    }

    pub(crate) fn path_until(&self, moment: Moment) -> impl Iterator<Item = Point> {
        self.points
            .iter()
            .take_while(move |ink| ink.at <= moment)
            .map(|ink| ink.point)
    }
}

impl TryFrom<Vec<InkPoint>> for Stroke {
    type Error = &'static str;

    fn try_from(points: Vec<InkPoint>) -> Result<Self, Self::Error> {
        Self::new(points).ok_or("a stroke needs at least one point")
    }
}

impl Serialize for Stroke {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.points.serialize(serializer)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pin {
    pub at: Moment,
    pub point: Point,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Action {
    /// `at` is the release and `point` the press.
    Click { at: Moment, point: Point },
    /// `span` runs from press to release.
    Drag { span: Span, from: Point, to: Point },
}

impl Action {
    #[must_use]
    pub(crate) const fn span(self) -> Span {
        match self {
            Self::Click { at, .. } => Span::at(at),
            Self::Drag { span, .. } => span,
        }
    }

    #[must_use]
    pub(crate) const fn point(self) -> Point {
        match self {
            Self::Click { point, .. } | Self::Drag { to: point, .. } => point,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Narration {
    Words(Vec<Word>),
    /// Untimed text from a recognizer without word times; no keyframe owns it.
    Text(String),
}

impl Default for Narration {
    fn default() -> Self {
        Self::Words(Vec::new())
    }
}

impl Narration {
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

    pub(crate) fn words(&self) -> &[Word] {
        match self {
            Self::Words(words) => words,
            Self::Text(_) => &[],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Word {
    pub span: Span,
    pub text: String,
}

impl Word {
    pub(crate) fn is_spoken(&self) -> bool {
        self.text.chars().any(char::is_alphanumeric)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

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
}
