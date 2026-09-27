use crate::{
    config::Engine,
    runtime::{ModelState, Phase, Snapshot},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indicator {
    Ready,
    Paused,
    Busy,
    Recording,
    Attention,
}

pub fn indicator(snapshot: &Snapshot, running: bool, pausing: bool) -> Indicator {
    if pausing {
        Indicator::Busy
    } else if snapshot.phase == Phase::Error {
        Indicator::Attention
    } else if !running {
        Indicator::Paused
    } else {
        match snapshot.phase {
            Phase::Recording | Phase::Stopping => Indicator::Recording,
            Phase::Starting | Phase::Processing => Indicator::Busy,
            _ => match snapshot.model {
                ModelState::Loading => Indicator::Busy,
                ModelState::Ready => Indicator::Ready,
                ModelState::Unavailable => Indicator::Attention,
            },
        }
    }
}

pub fn description(snapshot: &Snapshot, running: bool, pausing: bool, engine: Engine) -> String {
    if pausing {
        return "Pausing dictation…".into();
    }
    if snapshot.phase == Phase::Error {
        return "Needs attention · Open Settings".into();
    }
    if !running {
        return "Dictation paused".into();
    }
    match snapshot.phase {
        Phase::Starting => "Opening microphone…".into(),
        Phase::Recording if snapshot.hands_free => "Listening · Hands-free".into(),
        Phase::Recording => "Listening".into(),
        Phase::Stopping => "Finishing capture…".into(),
        Phase::Processing => "Transcribing locally…".into(),
        _ => match snapshot.model {
            ModelState::Loading => "Loading local model…".into(),
            ModelState::Unavailable => "Model unavailable · Open Settings".into(),
            ModelState::Ready => format!(
                "Ready · {}",
                match engine {
                    Engine::Whisper => "Whisper",
                    Engine::Parakeet => "Parakeet",
                }
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_and_capture_indicators_follow_resources_not_completion() {
        let mut snapshot = Snapshot {
            phase: Phase::Cancelled,
            model: ModelState::Loading,
            ..Snapshot::default()
        };
        assert_eq!(indicator(&snapshot, true, false), Indicator::Busy);
        snapshot.phase = Phase::Starting;
        assert_ne!(indicator(&snapshot, true, false), Indicator::Recording);
        snapshot.phase = Phase::Recording;
        assert_eq!(indicator(&snapshot, true, false), Indicator::Recording);
        assert_eq!(indicator(&snapshot, true, true), Indicator::Busy);
        snapshot.phase = Phase::Done;
        snapshot.model = ModelState::Ready;
        assert_eq!(indicator(&snapshot, true, false), Indicator::Ready);
        snapshot.model = ModelState::Unavailable;
        assert_eq!(indicator(&snapshot, true, false), Indicator::Attention);
        assert_eq!(indicator(&snapshot, false, false), Indicator::Paused);
        snapshot.phase = Phase::Error;
        assert_eq!(indicator(&snapshot, false, false), Indicator::Attention);
    }
}
