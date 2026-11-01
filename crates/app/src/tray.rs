mod icon;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod windows_macos;

use gpui::{App, AsyncApp};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use speakeasy_platform::Scope;
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
    menu: TrayMenu,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    last_show: bool,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    shortcut: std::sync::Arc<str>,
}

impl TrayState {
    fn new(services: &Services, snapshot: &Snapshot) -> Self {
        Self {
            status: services.status(snapshot),
            theme: services.config.theme,
            menu: dictation_menu(
                services.service_state(),
                activity(snapshot),
                snapshot.desktop_ready,
                !services.mode.is_demo(),
            ),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            last_show: services.recallable_show(snapshot).is_some(),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            shortcut: std::sync::Arc::clone(&snapshot.shortcut),
        }
    }
}

fn dictation_menu(
    service: ServiceState,
    activity: Activity,
    desktop_ready: bool,
    interactive: bool,
) -> TrayMenu {
    let running = service == ServiceState::Running;

    let startable = interactive && running && desktop_ready;
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let show = |label| TrayItem {
        label,
        enabled: startable && activity == Activity::Idle,
    };
    TrayMenu {
        pause: TrayItem {
            label: match service {
                ServiceState::Running => "Pause dictation",
                ServiceState::Pausing => "Pausing…",
                ServiceState::Quitting => "Quitting…",
                ServiceState::Paused | ServiceState::NotSetUp => "Resume dictation",
            },

            enabled: interactive && matches!(service, ServiceState::Running | ServiceState::Paused),
        },
        toggle: TrayItem {
            label: match activity {
                Activity::Dictating => "Finish dictation",
                Activity::Showing => "Finish Show",
                Activity::Idle | Activity::Finishing | Activity::Saving => "Start dictation",
            },
            enabled: startable
                && matches!(
                    activity,
                    Activity::Idle | Activity::Dictating | Activity::Showing
                ),
        },
        cancel: TrayItem {
            label: match activity {
                Activity::Showing | Activity::Saving => "Cancel Show",
                Activity::Idle | Activity::Dictating | Activity::Finishing => "Cancel dictation",
            },

            enabled: interactive
                && running
                && matches!(
                    activity,
                    Activity::Dictating | Activity::Showing | Activity::Finishing
                ),
        },
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        show_screen: show("Show screen"),
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        show_area: show("Show area…"),
    }
}

struct TrayChanges {
    snapshots: watch::Receiver<Snapshot>,
    last: Option<TrayState>,
}

impl TrayChanges {
    fn new(cx: &App) -> Self {
        let mut snapshots = cx.global::<Services>().output.subscribe();
        snapshots.mark_changed();
        Self {
            snapshots,
            last: None,
        }
    }

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
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    Shows,
    Pause,
    Toggle,
    Cancel,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    Show(Scope),
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    LastShow(shell::LastShow),
    Quit,
}

impl TrayCommand {
    fn run(self, cx: &mut App) {
        match self {
            Self::Settings => shell::reveal(cx),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            Self::Shows => shell::reveal_shows(cx),
            Self::Pause => shell::toggle_enabled(cx),
            Self::Toggle => shell::send(Input::Toggle, cx),
            Self::Cancel => shell::send(Input::Cancel, cx),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            Self::Show(scope) => shell::send(Input::StartShow(scope), cx),
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            Self::LastShow(action) => action.run(cx),
            Self::Quit => shell::request_quit(cx),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Activity {
    Idle,
    Dictating,
    Showing,
    Finishing,
    Saving,
}

fn activity(snapshot: &Snapshot) -> Activity {
    match (snapshot.phase, snapshot.show) {
        (Phase::Starting | Phase::Recording, false) => Activity::Dictating,
        (Phase::Stopping | Phase::Processing, false) => Activity::Finishing,

        (Phase::Starting | Phase::Recording, true) if snapshot.hands_free => Activity::Showing,
        (Phase::Starting | Phase::Recording | Phase::Stopping | Phase::Processing, true) => {
            Activity::Saving
        },
        (
            Phase::Idle
            | Phase::Done
            | Phase::Copied
            | Phase::Notice
            | Phase::Empty
            | Phase::Cancelled
            | Phase::Unchosen
            | Phase::Error,
            _,
        ) => Activity::Idle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictation_entries_are_enabled_only_while_their_command_can_act() {
        use Activity::{Dictating, Finishing, Idle, Saving, Showing};
        use ServiceState::{NotSetUp, Paused, Pausing, Quitting, Running};

        let table = [
            (Running, Idle, true, true, [true, true, false]),
            (Running, Idle, false, true, [true, false, false]),
            (Running, Dictating, true, true, [true, true, true]),
            (Running, Finishing, true, true, [true, false, true]),
            (Running, Showing, true, true, [true, true, true]),
            (Running, Saving, true, true, [true, false, false]),
            (Pausing, Dictating, true, true, [false, false, false]),
            (Quitting, Idle, true, true, [false, false, false]),
            (Paused, Idle, true, true, [true, false, false]),
            (NotSetUp, Idle, true, true, [false, false, false]),
            (Running, Dictating, true, false, [false, false, false]),
            (Paused, Idle, true, false, [false, false, false]),
        ];
        for (service, activity, desktop_ready, interactive, expected) in table {
            let TrayMenu {
                pause,
                toggle,
                cancel,
                ..
            } = dictation_menu(service, activity, desktop_ready, interactive);
            assert_eq!(
                [pause.enabled, toggle.enabled, cancel.enabled],
                expected,
                "{service:?}, {activity:?}, desktop ready {desktop_ready}, interactive {interactive}"
            );
        }
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn show_entries_act_exactly_when_start_dictation_would() {
        use Activity::{Dictating, Finishing, Idle, Saving, Showing};
        use ServiceState::{NotSetUp, Paused, Pausing, Quitting, Running};
        for service in [Running, Pausing, Quitting, Paused, NotSetUp] {
            for activity in [Idle, Dictating, Showing, Finishing, Saving] {
                for (desktop_ready, interactive) in
                    [(true, true), (false, true), (true, false), (false, false)]
                {
                    let menu = dictation_menu(service, activity, desktop_ready, interactive);
                    let starts = menu.toggle.enabled && menu.toggle.label == "Start dictation";
                    assert_eq!(
                        [menu.show_screen.enabled, menu.show_area.enabled],
                        [starts; 2],
                        "{service:?}, {activity:?}, desktop ready {desktop_ready}, interactive \
                         {interactive}"
                    );
                }
            }
        }
    }

    #[test]
    fn finish_and_cancel_name_a_recording_show_and_disable_once_it_finishes() {
        use Phase::{Copied, Processing, Recording, Starting, Stopping};
        let snapshot = |phase, show, hands_free| Snapshot {
            phase,
            hands_free,
            show,
            ..Snapshot::default()
        };
        let dictating = [("Finish dictation", true), ("Cancel dictation", true)];
        let finishing = [("Start dictation", false), ("Cancel dictation", true)];
        let showing = [("Finish Show", true), ("Cancel Show", true)];
        let saving = [("Start dictation", false), ("Cancel Show", false)];
        let idle = [("Start dictation", true), ("Cancel dictation", false)];

        let table = [
            (Recording, false, false, dictating),
            (Processing, false, false, finishing),
            (Starting, true, true, showing),
            (Recording, true, true, showing),
            (Starting, true, false, saving),
            (Recording, true, false, saving),
            (Stopping, true, false, saving),
            (Processing, true, false, saving),
            (Copied, true, false, idle),
        ];
        for (phase, show, hands_free, expected) in table {
            let TrayMenu { toggle, cancel, .. } = dictation_menu(
                ServiceState::Running,
                activity(&snapshot(phase, show, hands_free)),
                true,
                true,
            );
            assert_eq!(
                [
                    (toggle.label, toggle.enabled),
                    (cancel.label, cancel.enabled)
                ],
                expected,
                "{phase:?}, Show {show}, hands-free {hands_free}"
            );
        }
    }
}
