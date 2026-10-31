//! Exact request/sample relationships and content-free evidence references. A sampled frame says
//! nothing about the screen between samples, even when a narration clause leads the reader there.

use std::{fmt, path::Path, time::Duration};

use speakeasy_show::Moment;

use super::library::{Frame, Mode};

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
            std::cmp::Ordering::Equal => Self::At,
            std::cmp::Ordering::Less => Self::Earlier(requested.saturating_sub(sampled)),
            std::cmp::Ordering::Greater => Self::Later(sampled.saturating_sub(requested)),
        }
    }
}

pub(super) fn decoder_time(at: Duration) -> Duration {
    // Windows video decoding reports sample times in 100 ns units.
    at.saturating_sub(Duration::from_nanos(u64::from(at.subsec_nanos() % 100)))
}

impl fmt::Display for Relation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::At => f.write_str("same timestamp"),
            Self::Earlier(gap) => write!(f, "{} ms earlier", milliseconds(*gap)),
            Self::Later(gap) => write!(f, "{} ms later", milliseconds(*gap)),
        }
    }
}

fn milliseconds(gap: Duration) -> String {
    if gap.subsec_nanos().is_multiple_of(1_000_000) {
        gap.as_millis().to_string()
    } else {
        format!("{:.3}", gap.as_secs_f64() * 1000.0)
    }
}

pub(super) struct Stamp(pub Moment);

impl fmt::Display for Stamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:03}", self.0, self.0.as_millis() % 1_000)
    }
}

impl Request {
    pub(super) fn caption(self, frame: &Frame, mode: Mode, sampled: Duration) -> String {
        let origin = match self.origin {
            Origin::Clause(_) => "Narration clause".into(),
            Origin::Mark(number) => format!("Mark #{number}"),
        };
        let relation = Relation::between(self.at, sampled);
        if mode == Mode::Video {
            return format!(
                "{origin} at {} · video sample at {}, {relation}",
                Stamp(self.at),
                Stamp(sampled.into())
            );
        }
        let kind = match (mode, relation) {
            (Mode::Keyframes, Relation::Later(_)) => "first keyframe",
            (Mode::Source, Relation::Later(_)) => "first retained sample",
            (Mode::Keyframes, _) => "keyframe",
            (Mode::Source, _) => "retained sample",
            (Mode::Video, _) => "video sample",
        };
        format!(
            "{origin} at {} · {kind} #{} at {}, {}",
            Stamp(self.at),
            frame.number,
            Stamp(sampled.into()),
            relation
        )
    }
}

/// References the displayed frame and, if present, the explicit request that selected it. It
/// never accepts narration or summary text, so copying a moment cannot rewrite either.
pub(super) fn pointer(
    directory: &Path,
    frame: &Frame,
    mode: Mode,
    request: Option<Request>,
    sampled: Duration,
) -> String {
    let sample = match mode {
        Mode::Keyframes => format!(
            "Keyframe #{} at {}: {}",
            frame.number,
            Stamp(frame.at),
            path(&directory.join(format!("frames/frame-{:02}.png", frame.number)))
        ),
        Mode::Source => format!(
            "Retained source sample #{} at {} (no annotations).\nUse the installed Speakeasy command (named in summary.md when available) with `show frame SHOW --at {} --out NEW.png --json`; pass this folder as SHOW.",
            frame.number,
            Stamp(frame.at),
            Stamp(frame.at)
        ),
        Mode::Video => {
            let command = video_query(frame, sampled).map_or_else(String::new, |at| format!("\nUse the installed Speakeasy command with `show frame SHOW --video --at {} --out NEW.png --json`; pass this folder as SHOW.", Stamp(at)));
            format!(
                "Video sample at {} (no drawn annotations).{command}",
                Stamp(sampled.into())
            )
        },
    };
    let request = request.map_or_else(String::new, |request| {
        format!("{}\n", request.caption(frame, mode, sampled))
    });
    format!(
        "Show folder: {}\n{sample}\n{request}A frame shows its own timestamp; nearby samples do not prove a missing moment.\n",
        path(directory)
    )
}

fn video_query(frame: &Frame, sampled: Duration) -> Option<Moment> {
    let crate::show::preview::Content::Video { span, .. } = &frame.file else {
        return None;
    };
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

fn path(path: &Path) -> String {
    let text = path.display().to_string();
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.saturating_add(1));
    // Padding is stripped by CommonMark and protects paths whose first or last character is a
    // backtick or a space. The span names a file; it is never advertised as shell quoting.
    format!("{fence} {text} {fence}")
}
