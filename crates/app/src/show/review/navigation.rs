use std::{cmp::Ordering, fmt, ops::Range, path::Path, time::Duration};

use speakeasy_show::Moment;

use super::library::Frame;
use crate::show::{
    preview::Content,
    store::movie::{AnnotationState, decoder_time},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    Clause(usize),
    Mark(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Request {
    pub at: Moment,
    pub origin: Origin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Relation {
    At,
    Earlier(Duration),
    Later(Duration),
}

impl Relation {
    pub(super) fn between(requested: Moment, sampled: Duration) -> Self {
        let requested = requested.since(Moment::START);
        let sampled = decoder_time(sampled);
        match sampled.cmp(&requested) {
            Ordering::Equal => Self::At,
            Ordering::Less => Self::Earlier(requested.saturating_sub(sampled)),
            Ordering::Greater => Self::Later(sampled.saturating_sub(requested)),
        }
    }
}

impl fmt::Display for Relation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (gap, side) = match self {
            Self::At => return f.write_str("same timestamp"),
            Self::Earlier(gap) => (gap, "earlier"),
            Self::Later(gap) => (gap, "later"),
        };
        if gap.subsec_nanos().is_multiple_of(1_000_000) {
            write!(f, "{} ms {side}", gap.as_millis())
        } else {
            write!(f, "{:.3} ms {side}", gap.as_secs_f64() * 1000.0)
        }
    }
}

pub(super) struct Stamp(pub Moment);

impl fmt::Display for Stamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:03}", self.0, self.0.as_millis() % 1_000)
    }
}

impl Request {
    pub(super) fn caption(self, frame: &Frame, sampled: Duration) -> String {
        let origin = match self.origin {
            Origin::Clause(_) => "Narration clause".into(),
            Origin::Mark(number) => format!("Mark #{number}"),
        };
        let relation = Relation::between(self.at, sampled);
        let first = if matches!(relation, Relation::Later(_)) {
            "first "
        } else {
            ""
        };
        let sample = match frame.file {
            Content::Png(_) => format!("{first}keyframe #{}", frame.number),
            Content::Source { .. } => format!("{first}retained sample #{}", frame.number),
            Content::Video { .. } => "video sample".into(),
        };
        format!(
            "{origin} at {} · {sample} at {}, {relation}",
            Stamp(self.at),
            Stamp(sampled.into())
        )
    }
}

pub(super) fn pointer(
    directory: &Path,
    frame: &Frame,
    request: Option<Request>,
    sampled: Duration,
    annotations: AnnotationState,
) -> String {
    let at = Stamp(frame.at);
    let sample = match &frame.file {
        Content::Png(name) => format!(
            "Keyframe #{} at {at}: {}",
            frame.number,
            code(&directory.join(name))
        ),
        Content::Source { .. } => format!(
            "Retained source sample #{} at {at} (no annotations).\nUse the installed Speakeasy \
             command (named in summary.md when available) with `show frame SHOW --at {at} --out \
             NEW.png --json`; pass this folder as SHOW.",
            frame.number
        ),
        Content::Video { span, .. } => {
            let command = video_query(frame, span, sampled).map_or_else(String::new, |at| {
                let annotated = if annotations.painted() {
                    " --annotated"
                } else {
                    ""
                };
                format!(
                    "\nUse the installed Speakeasy command with `show frame SHOW \
                     --video{annotated} --at {} --out NEW.png --json`; pass this folder as SHOW.",
                    Stamp(at)
                )
            });
            format!(
                "Video sample at {} · {}.{command}",
                Stamp(sampled.into()),
                annotations.caption()
            )
        },
    };
    let request = request.map_or_else(String::new, |request| {
        format!("{}\n", request.caption(frame, sampled))
    });
    format!(
        "Show folder: {}\n{sample}\n{request}A frame shows its own timestamp; nearby samples do \
         not prove a missing moment.\n",
        code(directory)
    )
}

fn video_query(frame: &Frame, span: &Range<Duration>, sampled: Duration) -> Option<Moment> {
    let start = frame.presentation();
    if decoder_time(start) != decoder_time(sampled) {
        return None;
    }
    let end = start.checked_add(span.end.checked_sub(span.start)?)?;
    let requested = Duration::from_millis(
        u64::try_from(start.max(sampled).as_nanos().div_ceil(1_000_000)).ok()?,
    );
    (requested < decoder_time(end)).then(|| requested.into())
}

fn code(path: &Path) -> String {
    let text = path.display().to_string();
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.saturating_add(1));
    format!("{fence} {text} {fence}")
}
