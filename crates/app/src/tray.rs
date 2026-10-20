//! The tray mirrors service state into a native icon and menu and routes menu commands to the
//! shell. The dictation entries' labels and enable rules are decided here once; both backends show
//! them as given and present only states that differ from the last one shown.

mod icon;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod windows_macos;

use gpui::{App, AsyncApp};
use speakeasy_platform::{Input, TrayItem, TrayMenu};
use tokio::sync::watch;

#[cfg(target_os = "linux")]
pub(crate) use self::linux::{Tray, install};
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) use self::windows_macos::{Tray, install};
use crate::{
    runtime::{Phase, Snapshot},
    shell::{self, Services},
    status::{ServiceState, Status},
    theme::Theme,
};

#[derive(PartialEq, Eq)]
struct TrayState {
    status: Status,
    theme: Theme,
    service: ServiceState,
    activity: Activity,
    desktop_ready: bool,
    /// Whether the dictation commands act; a demo shows them disabled.
    interactive: bool,
}

impl TrayState {
    fn new(services: &Services, snapshot: &Snapshot) -> Self {
        Self {
            status: services.status(snapshot),
            theme: services.config.theme,
            service: services.service_state(),
            activity: activity(snapshot.phase),
            desktop_ready: snapshot.desktop_ready,
            interactive: !services.mode.is_demo(),
        }
    }

    /// The dictation entries, each enabled only while its command can act.
    fn menu(&self) -> TrayMenu {
        let running = self.service == ServiceState::Running;
        TrayMenu {
            pause: TrayItem {
                label: match self.service {
                    ServiceState::Running => "Pause dictation",
                    ServiceState::Pausing => "Pausing…",
                    ServiceState::Paused => "Resume dictation",
                },
                enabled: self.interactive && self.service != ServiceState::Pausing,
            },
            toggle: TrayItem {
                label: match self.activity {
                    Activity::Dictating => "Finish dictation",
                    Activity::Idle | Activity::Finishing => "Start dictation",
                },
                // The owner ignores Toggle until desktop input is ready.
                enabled: self.interactive
                    && running
                    && self.desktop_ready
                    && self.activity != Activity::Finishing,
            },
            cancel: TrayItem {
                label: "Cancel dictation",
                enabled: self.interactive && running && self.activity != Activity::Idle,
            },
        }
    }
}

/// The tray state of each published snapshot, skipping any equal to the last one yielded.
struct TrayChanges {
    snapshots: watch::Receiver<Snapshot>,
    last: Option<TrayState>,
}

impl TrayChanges {
    fn new(cx: &App) -> Self {
        let mut snapshots = cx.global::<Services>().output.subscribe();
        // The first `next` presents the current snapshot instead of waiting for a change.
        snapshots.mark_changed();
        Self {
            snapshots,
            last: None,
        }
    }

    /// Resolves to `None` once the app or the snapshot publisher is gone.
    #[expect(
        clippy::future_not_send,
        reason = "Tray states read Services through GPUI's thread-affine async context"
    )]
    async fn next(&mut self, cx: &AsyncApp) -> Option<&TrayState> {
        while self.snapshots.changed().await.is_ok() {
            let snapshot = self.snapshots.borrow_and_update().clone();
            let state = cx
                .update(|cx| TrayState::new(cx.global::<Services>(), &snapshot))
                .ok()?;
            if self.last.as_ref() != Some(&state) {
                return Some(self.last.insert(state));
            }
        }
        None
    }
}

#[derive(Clone, Copy)]
enum TrayCommand {
    Settings,
    Pause,
    Toggle,
    Cancel,
    Quit,
}

impl TrayCommand {
    fn run(self, cx: &mut App) {
        match self {
            Self::Settings => shell::reveal(cx),
            Self::Pause => shell::toggle_enabled(cx),
            Self::Toggle => shell::send(Input::Toggle, cx),
            Self::Cancel => shell::send(Input::Cancel, cx),
            Self::Quit => shell::request_quit(cx),
        }
    }
}

/// What dictation is doing, as the tray presents it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Activity {
    /// No recording is in progress.
    Idle,
    /// Capture is starting or recording.
    Dictating,
    /// Capture is ending or speech is being transcribed.
    Finishing,
}

fn activity(phase: Phase) -> Activity {
    match phase {
        Phase::Starting | Phase::Recording => Activity::Dictating,
        Phase::Stopping | Phase::Processing => Activity::Finishing,
        Phase::Idle
        | Phase::Done
        | Phase::Copied
        | Phase::Empty
        | Phase::Cancelled
        | Phase::Error => Activity::Idle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Indicator;

    fn menu(
        service: ServiceState,
        activity: Activity,
        desktop_ready: bool,
        interactive: bool,
    ) -> TrayMenu {
        TrayState {
            status: Status {
                indicator: Indicator::Ready,
                description: "".into(),
            },
            theme: Theme::default(),
            service,
            activity,
            desktop_ready,
            interactive,
        }
        .menu()
    }

    #[test]
    fn dictation_entries_are_enabled_only_while_their_command_can_act() {
        use Activity::{Dictating, Finishing, Idle};
        use ServiceState::{Paused, Pausing, Running};
        // Service, activity, desktop ready, interactive: whether Pause, Toggle, and Cancel act.
        let table = [
            (Running, Idle, true, true, [true, true, false]),
            (Running, Idle, false, true, [true, false, false]),
            (Running, Dictating, true, true, [true, true, true]),
            (Running, Finishing, true, true, [true, false, true]),
            (Pausing, Dictating, true, true, [false, false, false]),
            (Paused, Idle, true, true, [true, false, false]),
            (Running, Dictating, true, false, [false, false, false]),
            (Paused, Idle, true, false, [false, false, false]),
        ];
        for (service, activity, desktop_ready, interactive, expected) in table {
            let TrayMenu {
                pause,
                toggle,
                cancel,
            } = menu(service, activity, desktop_ready, interactive);
            assert_eq!(
                [pause.enabled, toggle.enabled, cancel.enabled],
                expected,
                "{service:?}, {activity:?}, desktop ready {desktop_ready}, interactive {interactive}"
            );
        }
    }
}
