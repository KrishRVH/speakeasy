//! The tray mirrors service state into a native icon and menu and routes menu commands to the
//! shell. Both backends present only states that differ from the last one shown.

mod icon;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod windows_macos;

use gpui::{App, AsyncApp};
use speakeasy_dictation::{
    runtime::{Phase, Snapshot},
    status::Status,
    theme::Theme,
};
use speakeasy_platform::{Activity, Input, ServiceState};
use tokio::sync::watch;

#[cfg(target_os = "linux")]
pub(crate) use self::linux::{Tray, install};
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) use self::windows_macos::{Tray, install};
use crate::shell::{self, Services};

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

fn activity(phase: Phase) -> Activity {
    match phase {
        Phase::Starting | Phase::Recording => Activity::Dictating,
        Phase::Stopping | Phase::Processing => Activity::Finishing,
        Phase::Idle | Phase::Done | Phase::Empty | Phase::Cancelled | Phase::Error => {
            Activity::Idle
        },
    }
}
