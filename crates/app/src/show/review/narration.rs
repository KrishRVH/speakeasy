use speakeasy_show::{NarrationLoss, Recording, TimedClause, timed_clauses};

pub(super) enum Narration {
    Timed {
        clauses: Vec<TimedClause>,
        incomplete: bool,
    },
    Untimed {
        text: String,
        incomplete: bool,
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
                let clauses = timed_clauses(words);
                if clauses.is_empty() {
                    return Self::empty(recording.narration_lost);
                }
                Self::Timed {
                    clauses,
                    incomplete,
                }
            },
            speakeasy_show::Narration::Text(text) => {
                if text.trim().is_empty() {
                    return Self::empty(recording.narration_lost);
                }
                Self::Untimed {
                    text: text.clone(),
                    incomplete,
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
}

pub(super) fn excerpt(text: &str, most: usize) -> &str {
    text.split_at(text.floor_char_boundary(most)).0
}
