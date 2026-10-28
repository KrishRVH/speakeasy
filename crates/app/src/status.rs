//! What Settings and the tray show for a runtime snapshot and the service state.

use std::borrow::Cow;

use crate::{
    config::Engine,
    runtime::{LOADING, ModelState, Phase, Snapshot},
};

/// The status while dictation pauses, shared by Services, Settings, and the tray.
pub(crate) const PAUSING: &str = "Pausing dictation…";
/// The status once dictation is paused, shared by Services, Settings, and the tray.
pub(crate) const PAUSED: &str = "Dictation paused";
/// The status once Quit has begun, shared by Services, Settings, and the tray.
pub(crate) const QUITTING: &str = "Quitting Speakeasy…";

/// Whether dictation runs, as the tray and Settings present it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServiceState {
    /// Dictation resources are live.
    Running,
    /// Dictation resources are being released.
    Pausing,
    /// Quit has begun; resources are released and nothing restarts them.
    Quitting,
    /// Dictation is off.
    Paused,
    /// Dictation is off because no speech engine is set up yet.
    NotSetUp,
}

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
                (ServiceState::Quitting, ..) => (Indicator::Busy, QUITTING.into()),
                (ServiceState::Pausing, ..) => (Indicator::Busy, PAUSING.into()),
                (_, Phase::Error, _) => (
                    Indicator::Attention,
                    "Needs attention · Open Settings".into(),
                ),
                (ServiceState::Paused, ..) => (Indicator::Paused, PAUSED.into()),
                (ServiceState::NotSetUp, ..) => {
                    (Indicator::Paused, "Not set up · Open Settings".into())
                },
                (ServiceState::Running, _, false) => {
                    (Indicator::Busy, "Waiting for desktop access…".into())
                },
                (ServiceState::Running, Phase::Starting, true) => {
                    (Indicator::Busy, "Opening microphone…".into())
                },
                (ServiceState::Running, Phase::Recording, true) => (
                    Indicator::Recording,
                    match (snapshot.show, snapshot.hands_free) {
                        (true, _) => "Recording a Show",
                        (false, true) => "Listening · Hands-free",
                        (false, false) => "Listening",
                    }
                    .into(),
                ),
                (ServiceState::Running, Phase::Stopping, true) => {
                    (Indicator::Recording, "Finishing capture…".into())
                },
                (ServiceState::Running, Phase::Processing, true) => (
                    Indicator::Busy,
                    if snapshot.show {
                        "Preparing the Show…"
                    } else {
                        "Transcribing locally…"
                    }
                    .into(),
                ),
                (
                    ServiceState::Running,
                    Phase::Idle | Phase::Done | Phase::Copied | Phase::Empty | Phase::Cancelled,
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

    fn description(snapshot: &Snapshot, service: ServiceState) -> Cow<'static, str> {
        Status::new(snapshot, service, Engine::Whisper).description
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
    fn copied_deliveries_follow_the_model_instead_of_needing_attention() {
        let mut snapshot = Snapshot {
            phase: Phase::Copied,
            model: ModelState::Ready,
            message: "Text copied. Paste manually.".into(),
            ..Snapshot::default()
        };
        assert_eq!(
            indicator(&snapshot, ServiceState::Running),
            Indicator::Ready
        );
        snapshot.model = ModelState::Loading;
        assert_eq!(indicator(&snapshot, ServiceState::Running), Indicator::Busy);
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
            description(&snapshot, ServiceState::Running),
            "Waiting for desktop access…"
        );
        assert_eq!(
            indicator(&snapshot, ServiceState::Paused),
            Indicator::Paused
        );
        assert_eq!(
            description(&snapshot, ServiceState::Paused),
            "Dictation paused"
        );
        assert_eq!(
            description(&snapshot, ServiceState::Pausing),
            "Pausing dictation…"
        );
        snapshot.phase = Phase::Error;
        assert_eq!(
            description(&snapshot, ServiceState::Quitting),
            "Quitting Speakeasy…"
        );
        assert_eq!(
            indicator(&snapshot, ServiceState::Running),
            Indicator::Attention
        );
        assert_eq!(
            description(&snapshot, ServiceState::Running),
            "Needs attention · Open Settings"
        );
        snapshot.phase = Phase::Idle;
        snapshot.desktop_ready = true;
        assert_eq!(
            indicator(&snapshot, ServiceState::Running),
            Indicator::Ready
        );
    }

    #[test]
    fn a_show_and_a_missing_engine_say_so_and_errors_still_win() {
        let mut snapshot = Snapshot {
            phase: Phase::Recording,
            hands_free: true,
            show: true,
            model: ModelState::Ready,
            desktop_ready: true,
            ..Snapshot::default()
        };
        assert_eq!(
            description(&snapshot, ServiceState::Running),
            "Recording a Show"
        );
        snapshot.phase = Phase::Processing;
        assert_eq!(
            description(&snapshot, ServiceState::Running),
            "Preparing the Show…"
        );
        snapshot.phase = Phase::Idle;
        assert_eq!(
            description(&snapshot, ServiceState::NotSetUp),
            "Not set up · Open Settings"
        );
        snapshot.phase = Phase::Error;
        assert_eq!(
            indicator(&snapshot, ServiceState::NotSetUp),
            Indicator::Attention
        );
    }
}
