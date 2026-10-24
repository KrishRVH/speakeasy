//! The portal insertion pipeline and the remedies it reports when paste or direct text cannot run.

use std::{os::fd::OwnedFd, time::Instant};

use anyhow::Context;
use async_channel::{Receiver, Sender};
use futures_util::{FutureExt, future::LocalBoxFuture};
use zbus::{
    Message, Proxy,
    zvariant::{OwnedObjectPath, Value},
};

use super::{
    Bound, MAX_TRANSFERS, Notice, Options, PLAIN_TEXT, SELECTION_TIMEOUT, SelectionHandoff,
    Transfers, deadline,
    transfer::{Selection, transfer},
};
use crate::{
    Delivery, DesktopOptions, InsertPermit, Inserted,
    insertion::{Insertion, MODIFIER_WAIT, Reply},
    linux::{
        CLIPBOARD_CHANGED, COPIED_FOR_MANUAL_PASTE, NO_PASTE_BINDING,
        ei::{self, PasteBlocked, TextBlocked},
        x11,
    },
};

/// The insertion pipeline. A paste request waits for released modifiers unless the user pastes
/// manually, then copies through the portal or X11 clipboard and confirms ownership; direct text
/// skips both. Every request checks focus before it pastes or types through libei.
///
/// The serving loop borrows `transfers`, `preparing`, and `bound` at once while it waits on them,
/// so those fields stay visible to it.
pub(super) struct Insertions<'a> {
    pub(super) transfers: Transfers<'a>,
    pub(super) preparing: Option<Preparing<'a>>,
    pub(super) bound: &'a mut Bound,
    pending: Option<(Insertion, Instant)>,
    selection: Option<Selection>,
    /// The latest recording whose lost selection revoked it; its paste reports the loss instead of
    /// a cancellation.
    forfeited: Option<InsertPermit>,
    notices: &'a Receiver<Notice>,
    selecting: &'a Sender<SelectionHandoff>,
    worker: &'a x11::ClipboardWorker,
    clipboard: &'a Proxy<'static>,
    options: &'a DesktopOptions,
}

impl<'a> Insertions<'a> {
    pub(super) fn new(
        bound: &'a mut Bound,
        notices: &'a Receiver<Notice>,
        selecting: &'a Sender<SelectionHandoff>,
        worker: &'a x11::ClipboardWorker,
        clipboard: &'a Proxy<'static>,
        options: &'a DesktopOptions,
    ) -> Self {
        Self {
            transfers: Transfers::new(),
            preparing: None,
            bound,
            pending: None,
            selection: None,
            forfeited: None,
            notices,
            selecting,
            worker,
            clipboard,
            options,
        }
    }

    pub(super) fn is_idle(&self) -> bool {
        self.pending.is_none() && self.preparing.is_none()
    }

    pub(super) fn accepts_transfers(&self) -> bool {
        self.bound.clipboard_session().is_some()
            && self.transfers.len() < MAX_TRANSFERS
            && !self
                .preparing
                .as_ref()
                .is_some_and(|work| work.setting_selection)
    }

    fn apply_queued_notices(&mut self) {
        while let Ok(notice) = self.notices.try_recv() {
            self.apply(notice);
        }
    }

    pub(super) fn apply(&mut self, notice: Notice) {
        apply_notice(
            notice,
            self.bound.sender_mut(),
            &mut self.selection,
            &mut self.forfeited,
        );
    }

    pub(super) fn queue(&mut self, request: Insertion) {
        self.pending = Some((request, Instant::now()));
    }

    /// When the pending request stops waiting for released modifiers.
    pub(super) fn modifier_wait(&self) -> Option<tokio::time::Sleep> {
        self.pending
            .as_ref()
            .map(|(_, started)| deadline(*started, MODIFIER_WAIT))
    }

    /// Catches up on notices and libei events, then starts the pending request once its wait for
    /// released modifiers is over.
    pub(super) fn advance_pending(&mut self) -> anyhow::Result<()> {
        self.apply_queued_notices();
        self.bound.dispatch()?;
        let Some((request, _)) = self.pending.take_if(|(request, started)| {
            !request.permit.active()
                || self.options.manual_paste
                || request.delivery == Delivery::Direct
                || self.bound.sender().is_some_and(ei::Sender::can_paste)
                || started.elapsed() >= MODIFIER_WAIT
        }) else {
            return Ok(());
        };
        if !request.permit.active() {
            request.reply.send(Ok(Inserted::Cancelled));
            return Ok(());
        }
        let (task, setting_selection) = match request.delivery {
            Delivery::Direct => (
                query_focus(self.worker, request.permit.clone(), Route::Direct),
                false,
            ),
            Delivery::Paste => (
                self.copy(&request),
                self.bound.clipboard_session().is_some(),
            ),
        };
        self.preparing = Some(Preparing {
            request,
            setting_selection,
            task,
        });
        Ok(())
    }

    fn copy(&self, request: &Insertion) -> LocalBoxFuture<'a, anyhow::Result<Prepared>> {
        let permit = request.permit.clone();
        match self.bound.clipboard_session() {
            Some(session) => set_selection(self.clipboard, session.clone(), permit),
            None => copy_with_x11(self.worker, request.text.clone(), permit),
        }
    }

    pub(super) fn advance_preparation(
        &mut self,
        prepared: anyhow::Result<Prepared>,
    ) -> anyhow::Result<()> {
        let request = self
            .preparing
            .take()
            .context("Missing clipboard preparation")?
            .request;
        let permit = request.permit.clone();
        let task = match prepared {
            Ok(Prepared::SelectionSet) => {
                self.selection = Some(Selection::new(&request.text, permit.clone()));
                confirm_selection(self.selecting, permit)
            },
            Ok(Prepared::Copied { owner: Some(owner) }) => {
                confirm_owner(self.worker, owner, permit)
            },
            Ok(Prepared::Ownership { owned }) => {
                query_focus(self.worker, permit, Route::Paste { owned })
            },
            Ok(Prepared::Copied { owner: None }) => {
                let outcome = if permit.active() {
                    CLIPBOARD_CHANGED
                } else {
                    Inserted::Cancelled
                };
                request.reply.send(Ok(outcome));
                return Ok(());
            },
            Ok(Prepared::Target { external, route }) => {
                return self.finish(request, external, route);
            },
            Err(error) => {
                request.reply.send(Err(error));
                return Ok(());
            },
        };
        self.preparing = Some(Preparing {
            request,
            setting_selection: false,
            task,
        });
        Ok(())
    }

    /// Submits a prepared request. A libei disconnect found while catching up ends the service, as
    /// it does between requests, because `ei::Sender::dispatch` reports it only once.
    fn finish(&mut self, request: Insertion, external: bool, route: Route) -> anyhow::Result<()> {
        // Notices queued while preparation awaited other processes must apply first: a missed
        // press could paste while the shortcut is held.
        self.apply_queued_notices();
        self.bound.dispatch()?;
        let outcome = match route {
            Route::Direct => insert_direct(self.bound, &request, external),
            Route::Paste { owned } => finish_paste(
                self.bound,
                &request,
                self.options,
                owned,
                external,
                self.forfeited.as_ref(),
            ),
        };
        request.reply.send(Ok(outcome));
        Ok(())
    }

    pub(super) fn serve_transfer(&self, event: &Message) -> anyhow::Result<()> {
        let (path, mime, serial): (OwnedObjectPath, String, u32) = event.body().deserialize()?;
        if self.bound.clipboard_session() != Some(&path) {
            return Ok(());
        }
        let payload = self
            .selection
            .as_ref()
            .filter(|_| PLAIN_TEXT.contains(&mime.as_str()))
            .map(Selection::transfer);
        let clipboard = self.clipboard;
        let session = path.clone();
        let write = async move {
            let fd: zbus::zvariant::OwnedFd =
                clipboard.call("SelectionWrite", &(session, serial)).await?;
            Ok(OwnedFd::from(fd))
        };
        let done = move |success: bool| async move {
            clipboard
                .call::<_, _, ()>("SelectionWriteDone", &(path, serial, success))
                .await?;
            Ok(())
        };
        self.transfers
            .push(transfer(write, done, payload).boxed_local());
        Ok(())
    }

    pub(super) fn cancel_all(self) {
        if let Some((request, _)) = self.pending {
            request.reply.send(Ok(Inserted::Cancelled));
        }
        if let Some(work) = self.preparing {
            work.request.reply.send(Ok(Inserted::Cancelled));
        }
    }
}

pub(super) struct Preparing<'a> {
    request: Insertion,
    /// Whether `task` is `SetSelection`. Transfers wait until it returns, because only then does
    /// `selection` hold the new text.
    setting_selection: bool,
    pub(super) task: LocalBoxFuture<'a, anyhow::Result<Prepared>>,
}

/// The outcome of one preparation stage.
pub(super) enum Prepared {
    /// The portal clipboard offers the text; this session's ownership awaits confirmation.
    SelectionSet,
    /// The X11 clipboard holds the text under the `owner` window, or `None` when the X11 clipboard
    /// changed or the permit lapsed before either clipboard was set.
    Copied { owner: Option<u32> },
    /// Whether this session still owns the clipboard selection it set.
    Ownership { owned: bool },
    /// Whether another app has focus, and how the insertion submits.
    Target { external: bool, route: Route },
}

/// How a prepared insertion submits once focus is known.
pub(super) enum Route {
    Direct,
    Paste { owned: bool },
}

fn set_selection<'a>(
    clipboard: &'a Proxy<'static>,
    session: OwnedObjectPath,
    permit: InsertPermit,
) -> LocalBoxFuture<'a, anyhow::Result<Prepared>> {
    async move {
        if !permit.active() {
            return Ok(Prepared::Copied { owner: None });
        }
        let mime_types = Options::from([("mime_types", Value::from(PLAIN_TEXT.to_vec()))]);
        tokio::time::timeout(
            SELECTION_TIMEOUT,
            clipboard.call::<_, _, ()>("SetSelection", &(session, mime_types)),
        )
        .await??;
        Ok(Prepared::SelectionSet)
    }
    .boxed_local()
}

fn copy_with_x11(
    worker: &x11::ClipboardWorker,
    text: String,
    permit: InsertPermit,
) -> LocalBoxFuture<'_, anyhow::Result<Prepared>> {
    async move {
        Ok(Prepared::Copied {
            owner: worker.set_text(text, permit).await?,
        })
    }
    .boxed_local()
}

fn confirm_selection(
    selecting: &Sender<SelectionHandoff>,
    permit: InsertPermit,
) -> LocalBoxFuture<'_, anyhow::Result<Prepared>> {
    async move {
        let (ready, confirmed) = Reply::channel();
        selecting.send(SelectionHandoff { permit, ready }).await?;
        let owned = confirmed
            .recv()
            .await
            .context("Clipboard ownership monitoring stopped")?;
        Ok(Prepared::Ownership { owned })
    }
    .boxed_local()
}

fn confirm_owner(
    worker: &x11::ClipboardWorker,
    owner: u32,
    permit: InsertPermit,
) -> LocalBoxFuture<'_, anyhow::Result<Prepared>> {
    async move {
        Ok(Prepared::Ownership {
            owned: worker.owns(owner, permit).await?,
        })
    }
    .boxed_local()
}

fn query_focus(
    worker: &x11::ClipboardWorker,
    permit: InsertPermit,
    route: Route,
) -> LocalBoxFuture<'_, anyhow::Result<Prepared>> {
    async move {
        // A failed focus query fails closed, as if Speakeasy had focus.
        let external = worker.external_target(permit).await.unwrap_or(false);
        Ok(Prepared::Target { external, route })
    }
    .boxed_local()
}

fn insert_direct(bound: &mut Bound, request: &Insertion, external: bool) -> Inserted {
    if !request.permit.active() {
        return Inserted::Cancelled;
    }
    if let Some(outcome) = Delivery::Direct.check_focus(external) {
        return outcome;
    }
    let Some(sender) = bound.sender_mut() else {
        return text_remedy(TextBlocked::Unsupported);
    };
    let ready = match sender.ready_text(&request.text) {
        Ok(ready) => ready,
        Err(blocked) => return text_remedy(blocked),
    };
    if !request.permit.commit() {
        return Inserted::Cancelled;
    }
    ready.send();
    Inserted::Sent
}

/// How to recover when direct text input cannot run.
const fn text_remedy(blocked: TextBlocked) -> Inserted {
    Inserted::Unavailable(match blocked {
        TextBlocked::Unsupported => {
            "This desktop does not support direct text input. Choose Insert: Paste in Settings."
        },
        TextBlocked::Paused => {
            "Direct text input is paused on this desktop. Dictate again, or choose Insert: Paste in Settings."
        },
        TextBlocked::NullCharacter => {
            "Direct input cannot contain a null character. Choose Insert: Paste in Settings."
        },
    })
}

/// The portal selection was taken by another app or never confirmed as this session's, so its loss
/// revoked the paste.
const SELECTION_LOST: Inserted = Inserted::Unavailable(
    "Clipboard changed or could not be confirmed before paste. Dictate again, or choose Insert: Copy for manual paste in Settings.",
);

/// Submits a prepared paste. An unconfirmed or lost portal selection has already revoked its
/// recording, so `owned` matters only to an X11 copy.
fn finish_paste(
    bound: &mut Bound,
    request: &Insertion,
    options: &DesktopOptions,
    owned: bool,
    external: bool,
    forfeited: Option<&InsertPermit>,
) -> Inserted {
    if !request.permit.active() {
        return if forfeited.is_some_and(|lost| lost.same_recording(&request.permit)) {
            SELECTION_LOST
        } else {
            Inserted::Cancelled
        };
    }
    if !owned {
        return CLIPBOARD_CHANGED;
    }
    if let Some(outcome) = Delivery::Paste.check_focus(external) {
        return outcome;
    }
    if options.manual_paste {
        return COPIED_FOR_MANUAL_PASTE;
    }
    let Some(sender) = bound.sender_mut() else {
        return paste_remedy(PasteBlocked::NoKeyboard);
    };
    let ready = match sender.ready_paste() {
        Ok(ready) => ready,
        Err(blocked) => return paste_remedy(blocked),
    };
    if !request.permit.commit() {
        return Inserted::Cancelled;
    }
    ready.send(options.terminal_paste);
    Inserted::Sent
}

/// How to paste the copied text when automatic paste cannot run.
const fn paste_remedy(blocked: PasteBlocked) -> Inserted {
    match blocked {
        PasteBlocked::NoKeyboard => Inserted::Copied(
            "Text copied. Desktop keyboard input is unavailable or paused; paste manually.",
        ),
        PasteBlocked::NoKeyboardState => Inserted::Copied(
            "Text copied. The desktop's keyboard layout could not be read; paste manually.",
        ),
        PasteBlocked::ShortcutKeys => Inserted::Copied(
            "Text copied. This desktop could not confirm released shortcut keys; paste manually.",
        ),
        PasteBlocked::NoPasteKeys => NO_PASTE_BINDING,
    }
}

pub(super) fn apply_notice(
    notice: Notice,
    sender: Option<&mut ei::Sender>,
    selection: &mut Option<Selection>,
    forfeited: &mut Option<InsertPermit>,
) {
    match notice {
        Notice::ModifiersChanged => {
            if let Some(sender) = sender {
                sender.forget_modifiers();
            }
        },
        Notice::SelectionLost { permit, revoked } => {
            if selection
                .as_ref()
                .is_some_and(|selection| selection.serves(&permit))
            {
                *selection = None;
            }
            if revoked {
                *forfeited = Some(permit);
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use zbus::zvariant::OwnedValue;

    use super::{
        super::{CLIPBOARD, DESKTOP, Results, SelectionOwnership},
        *,
    };
    use crate::{Input, InputSender, linux::ShortcutLabels};

    /// Applies the pump's queued notices, then finishes `permit`'s paste to an external editor as
    /// the insertion loop does once focus is known.
    fn paste_after_notices(
        notices: &Receiver<Notice>,
        forfeited: &mut Option<InsertPermit>,
        permit: &InsertPermit,
        owned: bool,
    ) -> Inserted {
        while let Ok(notice) = notices.try_recv() {
            apply_notice(notice, None, &mut None, forfeited);
        }
        let (reply, _answer) = Reply::channel();
        let request = Insertion {
            text: "fixture".into(),
            permit: permit.clone(),
            delivery: Delivery::Paste,
            reply,
        };
        let mut bound = Bound {
            shortcut_session: None,
            remote: None,
            labels: ShortcutLabels::new(&DesktopOptions::default()),
        };
        let options = DesktopOptions::default();
        finish_paste(
            &mut bound,
            &request,
            &options,
            owned,
            true,
            forfeited.as_ref(),
        )
    }

    #[test]
    fn a_lost_selection_reports_its_remedy_while_cancellation_stays_silent() -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(4);
        let input = InputSender::new(events);
        let (notify, notices) = async_channel::bounded(4);
        let session: OwnedObjectPath =
            "/org/freedesktop/portal/desktop/session/fixture".try_into()?;
        let owner_event = |owned: bool| {
            let values = Results::from([("session_is_owner".into(), OwnedValue::from(owned))]);
            Ok::<_, anyhow::Error>(
                Message::signal(DESKTOP, CLIPBOARD, "SelectionOwnerChanged")?
                    .build(&(session.clone(), values))?,
            )
        };
        let mut ownership = SelectionOwnership {
            session: Some(session.clone()),
            monitored: None,
            owned: false,
        };
        let mut forfeited = None;

        let unconfirmed = input.begin().context("Unconfirmed recording")?;
        ownership.adopt(unconfirmed.clone(), &notify)?;
        assert_eq!(
            paste_after_notices(&notices, &mut forfeited, &unconfirmed, false),
            SELECTION_LOST,
            "Unconfirmed clipboard ownership looked like a cancellation"
        );

        ownership.observe(&owner_event(true)?, &notify)?;
        let taken = input.begin().context("Taken recording")?;
        ownership.adopt(taken.clone(), &notify)?;
        ownership.observe(&owner_event(false)?, &notify)?;
        assert_eq!(
            paste_after_notices(&notices, &mut forfeited, &taken, true),
            SELECTION_LOST,
            "Another app taking the clipboard looked like a cancellation"
        );

        ownership.observe(&owner_event(true)?, &notify)?;
        let escaped = input.begin().context("Escaped recording")?;
        ownership.adopt(escaped.clone(), &notify)?;
        input.deliver(Input::Cancel);
        ownership.observe(&owner_event(false)?, &notify)?;
        assert_eq!(
            paste_after_notices(&notices, &mut forfeited, &escaped, true),
            Inserted::Cancelled,
            "A selection lost after Escape reported a remedy"
        );

        let superseded = input.begin().context("Superseded recording")?;
        let _newer = input.begin().context("Newer recording")?;
        ownership.adopt(superseded.clone(), &notify)?;
        assert_eq!(
            paste_after_notices(&notices, &mut forfeited, &superseded, false),
            Inserted::Cancelled,
            "A recording a newer one replaced reported a remedy"
        );
        Ok(())
    }

    #[test]
    fn cancelled_direct_input_precedes_focus_and_capability_errors() -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let permit = input.begin().context("Recording")?;
        permit.revoke();
        let mut bound = Bound {
            shortcut_session: None,
            remote: None,
            labels: ShortcutLabels::new(&DesktopOptions::default()),
        };
        for external in [false, true] {
            for text in ["fixture", "\0"] {
                let (reply, _answer) = Reply::channel();
                let request = Insertion {
                    text: text.into(),
                    permit: permit.clone(),
                    delivery: Delivery::Direct,
                    reply,
                };
                assert!(matches!(
                    insert_direct(&mut bound, &request, external),
                    Inserted::Cancelled
                ));
            }
        }
        Ok(())
    }
}
