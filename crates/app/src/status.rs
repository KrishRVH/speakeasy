use crate::{
    config::Engine,
    runtime::{ModelState, Phase, Snapshot},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Indicator {
    Ready,
    Paused,
    Busy,
    Recording,
    Attention,
}

pub(crate) fn indicator(snapshot: &Snapshot, running: bool, pausing: bool) -> Indicator {
    if pausing {
        Indicator::Busy
    } else if snapshot.phase == Phase::Error {
        Indicator::Attention
    } else if !running {
        Indicator::Paused
    } else if !snapshot.desktop_ready {
        Indicator::Busy
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

pub(crate) fn description(
    snapshot: &Snapshot,
    running: bool,
    pausing: bool,
    engine: Engine,
) -> String {
    if pausing {
        return "Pausing dictation…".into();
    }
    if snapshot.phase == Phase::Error {
        return "Needs attention · Open Settings".into();
    }
    if !running {
        return "Dictation paused".into();
    }
    if !snapshot.desktop_ready {
        return "Waiting for desktop access…".into();
    }
    match snapshot.phase {
        Phase::Starting => "Opening microphone…".into(),
        Phase::Recording if snapshot.hands_free => "Listening · Hands-free".into(),
        Phase::Recording => "Listening".into(),
        Phase::Stopping => "Finishing capture…".into(),
        Phase::Processing => "Transcribing locally…".into(),
        _ => match snapshot.model {
            ModelState::Loading => crate::runtime::LOADING.into(),
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

    #[test]
    fn desktop_access_gates_readiness_and_keeps_pause_and_errors_visible() {
        let mut snapshot = Snapshot {
            model: ModelState::Ready,
            desktop_ready: false,
            ..Snapshot::default()
        };
        assert_eq!(indicator(&snapshot, true, false), Indicator::Busy);
        assert_eq!(
            description(&snapshot, true, false, Engine::Parakeet),
            "Waiting for desktop access…"
        );
        assert_eq!(indicator(&snapshot, false, false), Indicator::Paused);
        assert_eq!(
            description(&snapshot, false, false, Engine::Parakeet),
            "Dictation paused"
        );
        assert_eq!(
            description(&snapshot, true, true, Engine::Parakeet),
            "Pausing dictation…"
        );
        snapshot.phase = Phase::Error;
        assert_eq!(indicator(&snapshot, true, false), Indicator::Attention);
        assert_eq!(
            description(&snapshot, true, false, Engine::Parakeet),
            "Needs attention · Open Settings"
        );
        snapshot.phase = Phase::Idle;
        snapshot.desktop_ready = true;
        assert_eq!(indicator(&snapshot, true, false), Indicator::Ready);
    }
}
