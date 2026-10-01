//! What Settings and the tray show for a runtime snapshot and the service state.

use std::borrow::Cow;

use speakeasy_platform::ServiceState;

use crate::{
    config::Engine,
    runtime::{LOADING, ModelState, Phase, Snapshot},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Indicator {
    Ready,
    Paused,
    Busy,
    Recording,
    Attention,
}

/// What Settings and the tray show for a snapshot: one indicator and the text that explains it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub indicator: Indicator,
    pub description: Cow<'static, str>,
}

impl Status {
    pub(crate) fn new(snapshot: &Snapshot, service: ServiceState, engine: Engine) -> Self {
        let (indicator, description): (_, Cow<'static, str>) =
            match (service, snapshot.phase, snapshot.desktop_ready) {
                (ServiceState::Pausing, ..) => (Indicator::Busy, "Pausing dictation…".into()),
                (_, Phase::Error, _) => (
                    Indicator::Attention,
                    "Needs attention · Open Settings".into(),
                ),
                (ServiceState::Paused, ..) => (Indicator::Paused, "Dictation paused".into()),
                (ServiceState::Running, _, false) => {
                    (Indicator::Busy, "Waiting for desktop access…".into())
                },
                (ServiceState::Running, Phase::Starting, true) => {
                    (Indicator::Busy, "Opening microphone…".into())
                },
                (ServiceState::Running, Phase::Recording, true) => (
                    Indicator::Recording,
                    if snapshot.hands_free {
                        "Listening · Hands-free"
                    } else {
                        "Listening"
                    }
                    .into(),
                ),
                (ServiceState::Running, Phase::Stopping, true) => {
                    (Indicator::Recording, "Finishing capture…".into())
                },
                (ServiceState::Running, Phase::Processing, true) => {
                    (Indicator::Busy, "Transcribing locally…".into())
                },
                (
                    ServiceState::Running,
                    Phase::Idle | Phase::Done | Phase::Empty | Phase::Cancelled,
                    true,
                ) => match snapshot.model {
                    ModelState::Loading => (Indicator::Busy, LOADING.into()),
                    ModelState::Ready => (
                        Indicator::Ready,
                        format!("Ready · {}", engine.label()).into(),
                    ),
                    ModelState::Unavailable => (
                        Indicator::Attention,
                        "Model unavailable · Open Settings".into(),
                    ),
                },
            };
        Self {
            indicator,
            description,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indicator(snapshot: &Snapshot, service: ServiceState) -> Indicator {
        Status::new(snapshot, service, Engine::Whisper).indicator
    }

    fn description(
        snapshot: &Snapshot,
        service: ServiceState,
        engine: Engine,
    ) -> Cow<'static, str> {
        Status::new(snapshot, service, engine).description
    }

    #[test]
    fn readiness_and_capture_indicators_follow_resources_not_completion() {
        let mut snapshot = Snapshot {
            phase: Phase::Cancelled,
            model: ModelState::Loading,
            ..Snapshot::default()
        };
        assert_eq!(indicator(&snapshot, ServiceState::Running), Indicator::Busy);
        snapshot.phase = Phase::Starting;
        assert_ne!(
            indicator(&snapshot, ServiceState::Running),
            Indicator::Recording
        );
        snapshot.phase = Phase::Recording;
        assert_eq!(
            indicator(&snapshot, ServiceState::Running),
            Indicator::Recording
        );
        assert_eq!(indicator(&snapshot, ServiceState::Pausing), Indicator::Busy);
        snapshot.phase = Phase::Done;
        snapshot.model = ModelState::Ready;
        assert_eq!(
            indicator(&snapshot, ServiceState::Running),
            Indicator::Ready
        );
        snapshot.model = ModelState::Unavailable;
        assert_eq!(
            indicator(&snapshot, ServiceState::Running),
            Indicator::Attention
        );
        assert_eq!(
            indicator(&snapshot, ServiceState::Paused),
            Indicator::Paused
        );
        snapshot.phase = Phase::Error;
        assert_eq!(
            indicator(&snapshot, ServiceState::Paused),
            Indicator::Attention
        );
    }

    #[test]
    fn desktop_access_gates_readiness_and_keeps_pause_and_errors_visible() {
        let mut snapshot = Snapshot {
            model: ModelState::Ready,
            desktop_ready: false,
            ..Snapshot::default()
        };
        assert_eq!(indicator(&snapshot, ServiceState::Running), Indicator::Busy);
        assert_eq!(
            description(&snapshot, ServiceState::Running, Engine::Parakeet),
            "Waiting for desktop access…"
        );
        assert_eq!(
            indicator(&snapshot, ServiceState::Paused),
            Indicator::Paused
        );
        assert_eq!(
            description(&snapshot, ServiceState::Paused, Engine::Parakeet),
            "Dictation paused"
        );
        assert_eq!(
            description(&snapshot, ServiceState::Pausing, Engine::Parakeet),
            "Pausing dictation…"
        );
        snapshot.phase = Phase::Error;
        assert_eq!(
            indicator(&snapshot, ServiceState::Running),
            Indicator::Attention
        );
        assert_eq!(
            description(&snapshot, ServiceState::Running, Engine::Parakeet),
            "Needs attention · Open Settings"
        );
        snapshot.phase = Phase::Idle;
        snapshot.desktop_ready = true;
        assert_eq!(
            indicator(&snapshot, ServiceState::Running),
            Indicator::Ready
        );
    }
}
