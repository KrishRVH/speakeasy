use speakeasy_show::{NarrationLoss, Recording, TimedClause, timed_clauses};

pub(super) const MOST_TEXT: usize = 64 << 10;
const MOST_ROWS: usize = 512;

pub(super) enum Narration {
    Timed {
        clauses: Vec<TimedClause>,
        incomplete: bool,
        truncated: bool,
    },
    Untimed {
        text: String,
        incomplete: bool,
        truncated: bool,
    },
    Silent,
    Lost(NarrationLoss),
    Unavailable,
}

impl Narration {
    pub(super) fn from_recording(recording: &Recording) -> Self {
        let incomplete = recording.narration_lost == Some(NarrationLoss::Incomplete);
        match &recording.narration {
            speakeasy_show::Narration::Words(words) => {
                let mut clauses = timed_clauses(words);
                if clauses.is_empty() {
                    return Self::empty(recording.narration_lost);
                }
                let mut truncated = clauses.len() > MOST_ROWS;
                clauses.truncate(MOST_ROWS);
                let mut remaining = MOST_TEXT;
                clauses.retain_mut(|clause| {
                    let end = clause.text.floor_char_boundary(remaining);
                    truncated |= end < clause.text.len();
                    clause.text.truncate(end);
                    remaining = remaining.saturating_sub(end);
                    !clause.text.is_empty()
                });
                Self::Timed {
                    clauses,
                    incomplete,
                    truncated,
                }
            },
            speakeasy_show::Narration::Text(text) => {
                if text.trim().is_empty() {
                    return Self::empty(recording.narration_lost);
                }
                let shown = excerpt(text, MOST_TEXT);
                Self::Untimed {
                    text: shown.to_owned(),
                    incomplete,
                    truncated: shown.len() < text.len(),
                }
            },
        }
    }

    fn empty(loss: Option<NarrationLoss>) -> Self {
        loss.map_or(Self::Silent, Self::Lost)
    }

    pub(super) fn clauses(&self) -> &[TimedClause] {
        match self {
            Self::Timed { clauses, .. } => clauses,
            _ => &[],
        }
    }

    pub(super) const fn incomplete(&self) -> bool {
        matches!(
            self,
            Self::Timed {
                incomplete: true,
                ..
            } | Self::Untimed {
                incomplete: true,
                ..
            }
        )
    }

    pub(super) const fn truncated(&self) -> bool {
        matches!(
            self,
            Self::Timed {
                truncated: true,
                ..
            } | Self::Untimed {
                truncated: true,
                ..
            }
        )
    }
}

pub(super) fn excerpt(text: &str, most: usize) -> &str {
    text.split_at(text.floor_char_boundary(most)).0
}
