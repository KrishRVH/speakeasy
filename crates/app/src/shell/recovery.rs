//! Copy last Show and Open last Show in Speakeasy's menu. Each acts on exactly the Show the latest
//! snapshot names, and once that one has gone it says so, forgets it, and never looks for another.

use std::path::Path;

use anyhow::anyhow;
use gpui::{App, BorrowAppContext as _};

use super::Services;
use crate::{gpui_ext::AppUpdate as _, runtime::Report, show::SavedShow};

/// What the menu does with the last Show.
#[derive(Clone, Copy)]
pub(crate) enum LastShow {
    /// Copies its summary as its folder holds it now, and says so; it never pastes.
    Copy,
    /// Opens its folder in the file manager.
    Open,
}

impl LastShow {
    pub(crate) fn run(self, cx: &mut App) {
        match self {
            Self::Copy => recover(
                |show| show.summary()?.map(|summary| copy(&summary)).transpose(),
                |(), cx| cx.global::<Services>().report(Report::ShowCopied),
                cx,
            ),
            Self::Open => recover(
                |show| Ok(show.folder()?.map(Path::to_owned)),
                |folder, cx| cx.open_with_system(&folder),
                cx,
            ),
        }
    }
}

/// Puts the last Show's `summary` on the clipboard, saying what to do when another app holds it.
fn copy(summary: &str) -> anyhow::Result<()> {
    speakeasy_platform::copy_text(summary).map_err(|error| {
        anyhow!(
            "Cannot copy the last Show ({error:#}). Close any app holding the clipboard, then try \
             again."
        )
    })
}

/// Runs `work` off the UI thread on the Show the menu acts on, if it acts, replacing any recovery
/// still under way, then hands what `work` found to `done`. A Show found gone is reported as such,
/// and a failure as Services reports any other.
fn recover<T: Send + 'static>(
    work: impl FnOnce(&SavedShow) -> anyhow::Result<Option<T>> + Send + 'static,
    done: impl FnOnce(T, &mut App) + 'static,
    cx: &mut App,
) {
    cx.update_global::<Services, _>(|services, cx| {
        let Some(show) = services.recallable_show(&services.output.borrow()).cloned() else {
            return;
        };
        let found = cx.background_executor().spawn({
            let show = show.clone();
            async move { work(&show) }
        });
        services.recovery = Some(cx.spawn(async move |cx| {
            let found = found.await;
            cx.update_if_running(|cx| match found {
                Ok(Some(value)) => done(value, cx),
                Ok(None) => cx.global::<Services>().report(Report::ShowGone(show)),
                Err(error) => cx.global::<Services>().report(&error),
            });
        }));
    });
}
