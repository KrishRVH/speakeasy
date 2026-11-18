use std::borrow::Cow;

use speakeasy_core::machine::{ModelState, Phase};

use crate::runtime::{LOADING, Snapshot};

pub(crate) const PAUSING: &str = "Pausing dictation…";
pub(crate) const PAUSED: &str = "Dictation paused";
pub(crate) const QUITTING: &str = "Quitting Speakeasy…";
pub(crate) const UNCONFIGURED: &str = "Speakeasy is not set up yet.";
pub(crate) const RESETTING: &str = "Removing settings and downloads…";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServiceState {
    Running,
    Pausing,
    Quitting,
    Paused,
    NotSetUp,
}

impl ServiceState {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Pausing => "pausing",
            Self::Quitting => "quitting",
            Self::Paused => "paused",
            Self::NotSetUp => "not_set_up",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Indicator {
    Ready,
    Paused,
    Busy,
    Recording,
    Attention,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub indicator: Indicator,
    pub description: Cow<'static, str>,
    points_to_settings: bool,
}

impl Status {
    pub(crate) fn new(snapshot: &Snapshot, service: ServiceState, model: &'static str) -> Self {
        let (indicator, description) = match (service, snapshot.phase, snapshot.desktop_ready) {
            (ServiceState::Quitting, ..) => (Indicator::Busy, QUITTING.into()),
            (ServiceState::Pausing, ..) => (Indicator::Busy, PAUSING.into()),
            (_, Phase::Error, _) => (Indicator::Attention, "Needs attention".into()),
            (ServiceState::Paused, ..) => (Indicator::Paused, PAUSED.into()),
            (ServiceState::NotSetUp, ..) => (Indicator::Paused, "Not set up".into()),
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
                Phase::Idle
                | Phase::Done
                | Phase::Copied
                | Phase::Notice
                | Phase::Empty
                | Phase::Cancelled
                | Phase::Unchosen,
                true,
            ) => match snapshot.model {
                ModelState::Loading => (Indicator::Busy, LOADING.into()),
                ModelState::Ready => (Indicator::Ready, format!("Ready · {model}").into()),
                ModelState::Unavailable => {
                    (Indicator::Attention, "Speech model unavailable".into())
                },
            },
        };
        Self {
            indicator,
            description,
            points_to_settings: indicator == Indicator::Attention
                || service == ServiceState::NotSetUp,
        }
    }

    pub(crate) fn outside_settings(&self) -> Cow<'_, str> {
        if self.points_to_settings {
            Cow::Owned(format!("{} · Open Settings", self.description))
        } else {
            Cow::Borrowed(&self.description)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indicator(snapshot: &Snapshot, service: ServiceState) -> Indicator {
        Status::new(snapshot, service, "Whisper").indicator
    }

    fn description(snapshot: &Snapshot, service: ServiceState) -> Cow<'static, str> {
        Status::new(snapshot, service, "Whisper").description
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
    fn copies_and_notices_follow_the_model_instead_of_needing_attention() {
        for (phase, message) in [
            (Phase::Copied, "Text copied. Paste manually."),
            (Phase::Notice, "The last Show has expired or was deleted."),
        ] {
            let mut snapshot = Snapshot {
                phase,
                model: ModelState::Ready,
                message: message.into(),
                ..Snapshot::default()
            };
            assert_eq!(
                indicator(&snapshot, ServiceState::Running),
                Indicator::Ready,
                "{phase:?}"
            );
            snapshot.model = ModelState::Loading;
            assert_eq!(
                indicator(&snapshot, ServiceState::Running),
                Indicator::Busy,
                "{phase:?}"
            );
        }
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
            "Needs attention"
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
        assert_eq!(description(&snapshot, ServiceState::NotSetUp), "Not set up");
        snapshot.phase = Phase::Error;
        assert_eq!(
            indicator(&snapshot, ServiceState::NotSetUp),
            Indicator::Attention
        );
    }

    #[test]
    fn only_the_tray_points_to_settings_and_only_where_settings_can_help() {
        let at = |phase, model| Snapshot {
            phase,
            model,
            desktop_ready: true,
            ..Snapshot::default()
        };
        let ready = at(Phase::Idle, ModelState::Ready);
        let unavailable = at(Phase::Idle, ModelState::Unavailable);
        let failed = at(Phase::Error, ModelState::Ready);
        for (snapshot, service, inside, outside) in [
            (
                &ready,
                ServiceState::Running,
                "Ready · Parakeet v3",
                "Ready · Parakeet v3",
            ),
            (&ready, ServiceState::Paused, PAUSED, PAUSED),
            (
                &ready,
                ServiceState::NotSetUp,
                "Not set up",
                "Not set up · Open Settings",
            ),
            (
                &unavailable,
                ServiceState::Running,
                "Speech model unavailable",
                "Speech model unavailable · Open Settings",
            ),
            (
                &failed,
                ServiceState::Running,
                "Needs attention",
                "Needs attention · Open Settings",
            ),
        ] {
            let status = Status::new(snapshot, service, "Parakeet v3");
            assert_eq!(status.description, inside, "{service:?}");
            assert_eq!(status.outside_settings(), outside, "{service:?}");
        }
    }
}
