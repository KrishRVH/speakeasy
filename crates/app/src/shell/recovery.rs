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
            Self::Copy => copy(cx),
            Self::Open => recover(
                |show| Ok(show.folder()?.map(Path::to_owned)),
                |folder, cx| cx.open_with_system(&folder),
                cx,
            ),
        }
    }
}

/// Copy preparation and publication share one owned worker and the idle generation observed when
/// the user asks. A late disk read cannot acquire fresh authority after a newer recording ends.
fn copy(cx: &mut App) {
    let services = cx.global_mut::<Services>();
    let Some(show) = services.recallable_show(&services.output.borrow()).cloned() else {
        return;
    };
    let receipt = match services.copy_show(crate::show::copy::Request::Saved(show.clone())) {
        Ok(receipt) => receipt,
        Err(error) => {
            cx.global::<Services>().report(&error);
            return;
        },
    };
    let copying = cx.spawn(async move |cx| {
        let result = receipt.await;
        cx.update_if_running(|cx| {
            let services = cx.global::<Services>();
            match result {
                Ok(crate::show::copy::Outcome::Copied) => services.report(Report::ShowCopied),
                Ok(crate::show::copy::Outcome::Missing) => services.report(Report::ShowGone(show)),
                Err(error) => services.report(&anyhow!("{error}")),
            }
        });
    });
    cx.global_mut::<Services>().recovery = Some(copying);
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
