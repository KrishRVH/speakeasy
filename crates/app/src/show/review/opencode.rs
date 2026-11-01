use std::path::PathBuf;

use gpui::{Context, Task};

use super::{Review, Source};
use crate::{
    gpui_ext::EntityUpdate as _,
    show::opencode::{Listing, Request},
};

pub(super) enum Handoff {
    Closed,
    Loading {
        directory: PathBuf,
        _task: Task<()>,
    },
    Choosing {
        directory: PathBuf,
        listing: Listing,
    },
    Sending {
        directory: PathBuf,
        _task: Task<()>,
    },
}

impl Review {
    pub(super) fn open_handoff(&mut self, cx: &mut Context<'_, Self>) {
        if self.source != Source::Saved
            || !matches!(self.handoff, Handoff::Closed)
            || self.detail.is_none()
        {
            return;
        }
        let Some(directory) = self
            .library
            .selected
            .as_ref()
            .map(|key| key.directory.clone())
        else {
            return;
        };
        let receipt = match (self.actions.sessions)(cx) {
            Ok(receipt) => receipt,
            Err(error) => {
                self.notice = Some(error.to_string());
                cx.notify();
                return;
            },
        };
        let selected = directory.clone();
        let task = cx.spawn(async move |view, cx| {
            let result = receipt.await;
            view.update_if_alive(cx, |view, cx| {
                if !view.library.is_selected(&selected)
                    || !matches!(&view.handoff, Handoff::Loading { directory, .. } if *directory == selected)
                {
                    return;
                }
                view.handoff = match result {
                    Ok(listing) => Handoff::Choosing { directory: selected, listing },
                    Err(reason) => {
                        view.notice = Some(reason);
                        Handoff::Closed
                    },
                };
                cx.notify();
            });
        });
        self.handoff = Handoff::Loading {
            directory,
            _task: task,
        };
        self.notice = None;
        cx.notify();
    }

    pub(super) fn send_handoff(&mut self, session: String, cx: &mut Context<'_, Self>) {
        let Handoff::Choosing { directory, listing } = &self.handoff else {
            return;
        };
        if !self.library.is_selected(directory)
            || !listing.sessions.iter().any(|row| row.id == session)
        {
            return;
        }
        let directory = directory.clone();
        let receipt = match (self.actions.send)(
            Request {
                directory: directory.clone(),
                session,
                expected_endpoint: Some(listing.endpoint.clone()),
            },
            cx,
        ) {
            Ok(receipt) => receipt,
            Err(error) => {
                self.notice = Some(error.to_string());
                cx.notify();
                return;
            },
        };
        let selected = directory.clone();
        let task = cx.spawn(async move |view, cx| {
            let result = receipt.await;
            view.update_if_alive(cx, |view, cx| {
                if view.library.is_selected(&selected)
                    && matches!(&view.handoff, Handoff::Sending { directory, .. } if *directory == selected)
                {
                    view.handoff = Handoff::Closed;
                    view.notice = Some(result.note());
                    cx.notify();
                }
            });
        });
        self.handoff = Handoff::Sending {
            directory,
            _task: task,
        };
        self.notice = Some("Sending this Show to the selected OpenCode session…".into());
        cx.notify();
    }

    pub(super) fn close_handoff(&mut self, cx: &mut Context<'_, Self>) {
        if matches!(self.handoff, Handoff::Sending { .. }) {
            self.notice = Some("The handoff may already be queued. Check the selected session before sending again.".into());
        }
        self.handoff = Handoff::Closed;
        cx.notify();
    }
}
