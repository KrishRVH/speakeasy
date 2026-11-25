use std::{convert::Infallible, sync::Arc, time::Duration};

use anyhow::Context as _;
use async_channel::Receiver;
use futures_util::{
    FutureExt as _, StreamExt as _, future::LocalBoxFuture, stream::FuturesUnordered,
};
use tokio::time::{Instant, sleep_until};
use zbus::{
    Message, MessageStream, Proxy,
    message::Sequence,
    names::MemberName,
    zvariant::{OwnedObjectPath, Value},
};

use super::{Options, Results, or_pending, transfer::transfer};
use crate::{
    Delivery, InputSender, InsertPermit, Inserted, LinuxPaste,
    insertion::{Insertion, MODIFIER_WAIT},
    linux::{
        COPIED_FOR_MANUAL_PASTE, NO_PASTE_BINDING,
        ei::{self, PasteBlocked, TextBlocked},
        focus, stop_requested, x11,
    },
};

const PLAIN_TEXT: [&str; 2] = ["text/plain;charset=utf-8", "text/plain"];
const SELECTION_TIMEOUT: Duration = Duration::from_secs(2);
const OWNERSHIP_SETTLE: Duration = Duration::from_millis(500);
const CLIPBOARD_DISCONNECTED: &str = "Clipboard portal disconnected";

pub(super) struct Insertions<'a> {
    transfers: FuturesUnordered<LocalBoxFuture<'a, ()>>,
    in_flight: Option<InFlight<'a>>,
    sender: Option<ei::Sender>,
    clipboard_session: Option<OwnedObjectPath>,
    selection: Option<Arc<str>>,
    owned_since: Option<Sequence>,
    worker: &'a x11::ClipboardWorker,
    clipboard: &'a Proxy<'static>,
    paste: LinuxPaste,
}

impl<'a> Insertions<'a> {
    pub(super) fn new(
        sender: Option<ei::Sender>,
        clipboard_session: Option<OwnedObjectPath>,
        worker: &'a x11::ClipboardWorker,
        clipboard: &'a Proxy<'static>,
        paste: LinuxPaste,
    ) -> Self {
        Self {
            transfers: FuturesUnordered::new(),
            in_flight: None,
            sender,
            clipboard_session,
            selection: None,
            owned_since: None,
            worker,
            clipboard,
            paste,
        }
    }

    #[expect(
        clippy::future_not_send,
        reason = "libei resources remain on the owned desktop thread and its LocalSet; native pointers must never move between threads"
    )]
    pub(super) async fn serve(
        mut self,
        mut clipboard_signals: Option<MessageStream>,
        requests: Receiver<Insertion>,
        input: &InputSender,
        stopping: &Receiver<Infallible>,
    ) -> anyhow::Result<()> {
        loop {
            self.dispatch()?;
            self.advance_waiting();
            let idle = self.in_flight.is_none();
            let deadline = self
                .in_flight
                .as_ref()
                .and_then(|pending| pending.phase.deadline());
            let prepared = self
                .in_flight
                .as_mut()
                .and_then(|pending| pending.phase.preparation());
            let readable = self
                .sender
                .as_ref()
                .map(|sender| sender.readiness().readable());
            tokio::select! {
                () = stop_requested(stopping, input) => break,
                prepared = or_pending(prepared) => self.advance_preparation(prepared)?,
                request = requests.recv(), if idle => match request {
                    Ok(request) => {
                        self.in_flight = Some(InFlight {
                            request,
                            phase: Phase::AwaitingModifiers(deadline_after(MODIFIER_WAIT)),
                        });
                    },
                    Err(_) => break,
                },
                _ = self.transfers.next(), if !self.transfers.is_empty() => {},
                event = or_pending(clipboard_signals.as_mut().map(|stream| stream.next())) => {
                    let event = event.context(CLIPBOARD_DISCONNECTED)??;
                    match event.header().member().map(MemberName::as_str) {
                        Some("SelectionOwnerChanged") => self.observe_owner(&event)?,
                        Some("SelectionTransfer") => self.serve_transfer(&event)?,
                        _ => {},
                    }
                },
                readable = or_pending(readable) => readable?.clear_ready(),
                () = or_pending(deadline.map(sleep_until)) => {},
            }
        }
        if let Some(pending) = self.in_flight {
            pending.request.reply.send(Ok(Inserted::Cancelled));
        }
        Ok(())
    }

    fn dispatch(&mut self) -> anyhow::Result<()> {
        if let Some(sender) = &mut self.sender {
            sender.dispatch()?;
        }
        Ok(())
    }

    fn advance_waiting(&mut self) {
        let Some(pending) = &mut self.in_flight else {
            return;
        };
        let request = &pending.request;
        match pending.phase {
            Phase::AwaitingModifiers(until) => {
                if !request.permit.active() {
                    self.reply(Ok(Inserted::Cancelled));
                    return;
                }
                if self.paste != LinuxPaste::Manual
                    && request.delivery == Delivery::Paste
                    && !self.sender.as_ref().is_some_and(ei::Sender::can_paste)
                    && Instant::now() < until
                {
                    return;
                }
                let permit = request.permit.clone();
                pending.phase = match (request.delivery, &self.clipboard_session) {
                    (Delivery::Direct, _) => Phase::Preparing(
                        query_focus(self.worker, permit, Route::Direct).boxed_local(),
                    ),
                    (Delivery::Paste, Some(session)) => Phase::SettingSelection(
                        set_selection(self.clipboard, session.clone(), permit).boxed_local(),
                    ),
                    (Delivery::Paste, None) => Phase::Preparing(
                        copy_with_x11(self.worker, request.text.clone(), permit).boxed_local(),
                    ),
                };
            },
            Phase::AwaitingOwnership { replied, until } => {
                if !request.permit.active() {
                    self.reply(Ok(Inserted::Cancelled));
                } else if owns_selection(self.owned_since, replied) {
                    pending.phase = Phase::Preparing(
                        query_focus(
                            self.worker,
                            request.permit.clone(),
                            Route::PortalPaste { replied },
                        )
                        .boxed_local(),
                    );
                } else if Instant::now() >= until {
                    self.reply(Ok(SELECTION_LOST));
                }
            },
            Phase::SettingSelection(_) | Phase::Preparing(_) => {},
        }
    }

    fn advance_preparation(&mut self, prepared: anyhow::Result<Prepared>) -> anyhow::Result<()> {
        let pending = self
            .in_flight
            .as_mut()
            .context("Missing clipboard preparation")?;
        // Before the permit check: a replacement the portal accepted, or never answered, withdraws
        // the old copy even when cancelled. A refusal or a skipped call keeps it served.
        if matches!(pending.phase, Phase::SettingSelection(_)) {
            match &prepared {
                Ok(Prepared::NotCopied) => {},
                Err(error)
                    if matches!(error.downcast_ref(), Some(zbus::Error::MethodError(..))) => {},
                _ => self.selection = None,
            }
        }
        if !pending.request.permit.active() {
            self.reply(Ok(Inserted::Cancelled));
            return Ok(());
        }
        match prepared {
            Ok(Prepared::SelectionSet { replied }) => {
                pending.phase = Phase::AwaitingOwnership {
                    replied,
                    until: deadline_after(OWNERSHIP_SETTLE),
                };
            },
            Ok(Prepared::NotCopied) => self.reply(Ok(Inserted::Cancelled)),
            Ok(Prepared::Target { external, route }) => {
                let pending = self
                    .in_flight
                    .take()
                    .context("Missing clipboard preparation")?;
                self.finish(pending.request, external, route)?;
            },
            Err(error) => self.reply(Err(error)),
        }
        Ok(())
    }

    fn reply(&mut self, outcome: anyhow::Result<Inserted>) {
        if let Some(pending) = self.in_flight.take() {
            pending.request.reply.send(outcome);
        }
    }

    fn finish(&mut self, request: Insertion, external: bool, route: Route) -> anyhow::Result<()> {
        self.dispatch()?;
        let sender = self.sender.as_mut();
        let outcome = match route {
            Route::Direct => insert_direct(sender, &request, external),
            Route::X11Paste => finish_paste(sender, &request, self.paste, external),
            Route::PortalPaste { replied } => {
                if owns_selection(self.owned_since, replied) {
                    publish_portal_selection(
                        &mut self.selection,
                        &request,
                        finish_paste(sender, &request, self.paste, external),
                    )
                } else if request.permit.active() {
                    SELECTION_LOST
                } else {
                    Inserted::Cancelled
                }
            },
        };
        request.reply.send(Ok(outcome));
        Ok(())
    }

    fn observe_owner(&mut self, event: &Message) -> anyhow::Result<()> {
        let (_, values): (OwnedObjectPath, Results) = event.body().deserialize()?;
        let owned = values
            .get("session_is_owner")
            .and_then(|value| bool::try_from(value).ok())
            .unwrap_or(false);
        self.owned_since = owned.then(|| event.recv_position());
        if !owned {
            self.selection = None;
        }
        Ok(())
    }

    fn serve_transfer(&self, event: &Message) -> anyhow::Result<()> {
        let (session, mime, serial): (OwnedObjectPath, String, u32) = event.body().deserialize()?;
        let setting = self
            .in_flight
            .as_ref()
            .is_some_and(|pending| matches!(pending.phase, Phase::SettingSelection(_)));
        // Cloned now, so a started transfer keeps its text. Nothing is served during
        // `SetSelection`, since the desktop may already offer the replacement.
        let text = self
            .selection
            .clone()
            .filter(|_| !setting && PLAIN_TEXT.contains(&mime.as_str()));
        self.transfers
            .push(transfer(self.clipboard, session, serial, text).boxed_local());
        Ok(())
    }
}

fn deadline_after(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

/// A replacement is owned only once an ownership change arrives after its reply.
///
/// The portal frontend answers `SetSelection` before its backend installs the selection, and
/// ownership changes do not name the selection they confirm.
fn owns_selection(owned_since: Option<Sequence>, replied: Sequence) -> bool {
    owned_since.is_some_and(|since| since > replied)
}

struct InFlight<'a> {
    request: Insertion,
    phase: Phase<'a>,
}

enum Phase<'a> {
    AwaitingModifiers(Instant),
    SettingSelection(Preparation<'a>),
    AwaitingOwnership { replied: Sequence, until: Instant },
    Preparing(Preparation<'a>),
}

impl<'a> Phase<'a> {
    const fn deadline(&self) -> Option<Instant> {
        match *self {
            Self::AwaitingModifiers(until) | Self::AwaitingOwnership { until, .. } => Some(until),
            Self::SettingSelection(_) | Self::Preparing(_) => None,
        }
    }

    const fn preparation(&mut self) -> Option<&mut Preparation<'a>> {
        match self {
            Self::SettingSelection(task) | Self::Preparing(task) => Some(task),
            Self::AwaitingModifiers(_) | Self::AwaitingOwnership { .. } => None,
        }
    }
}

type Preparation<'a> = LocalBoxFuture<'a, anyhow::Result<Prepared>>;

enum Prepared {
    SelectionSet { replied: Sequence },
    NotCopied,
    Target { external: bool, route: Route },
}

enum Route {
    Direct,
    X11Paste,
    PortalPaste { replied: Sequence },
}

async fn set_selection(
    clipboard: &Proxy<'static>,
    session: OwnedObjectPath,
    permit: InsertPermit,
) -> anyhow::Result<Prepared> {
    if !permit.active() {
        return Ok(Prepared::NotCopied);
    }
    let mime_types = Options::from([("mime_types", Value::from(PLAIN_TEXT.to_vec()))]);
    let reply = tokio::time::timeout(
        SELECTION_TIMEOUT,
        clipboard.call_method("SetSelection", &(session, mime_types)),
    )
    .await??;
    Ok(Prepared::SelectionSet {
        replied: reply.recv_position(),
    })
}

async fn copy_with_x11(
    worker: &x11::ClipboardWorker,
    text: String,
    permit: InsertPermit,
) -> anyhow::Result<Prepared> {
    if !worker.set_text(text, permit.clone()).await? {
        return Ok(Prepared::NotCopied);
    }
    query_focus(worker, permit, Route::X11Paste).await
}

async fn query_focus(
    worker: &x11::ClipboardWorker,
    permit: InsertPermit,
    route: Route,
) -> anyhow::Result<Prepared> {
    // A failed focus query fails closed, as if Speakeasy had focus.
    let external = worker.external_target(permit).await.unwrap_or(false);
    Ok(Prepared::Target { external, route })
}

fn insert_direct(sender: Option<&mut ei::Sender>, request: &Insertion, external: bool) -> Inserted {
    if !request.permit.active() {
        return Inserted::Cancelled;
    }
    if let Some(outcome) = Delivery::Direct.check_focus(focus(request, external)) {
        return outcome;
    }
    let ready = match sender
        .ok_or(TextBlocked::Unsupported)
        .and_then(|sender| sender.ready_text(&request.text))
    {
        Ok(ready) => ready,
        Err(blocked) => return text_remedy(blocked),
    };
    if !request.permit.commit() {
        return Inserted::Cancelled;
    }
    ready.send();
    Inserted::Sent
}

const fn text_remedy(blocked: TextBlocked) -> Inserted {
    Inserted::Unavailable(match blocked {
        TextBlocked::Unsupported => {
            "This desktop does not support direct text input. Set “Insert text by” to a Paste option in Settings."
        },
        TextBlocked::Paused => {
            "Direct text input is paused on this desktop. Set “Insert text by” to a Paste option in Settings."
        },
        TextBlocked::NullCharacter => {
            "Direct input can't contain a null character. Set “Insert text by” to a Paste option in Settings."
        },
    })
}

const SELECTION_LOST: Inserted = Inserted::Unavailable(
    "The clipboard changed or couldn't be confirmed before pasting. Set “Insert text by” to Copy for manual paste in Settings.",
);

fn finish_paste(
    sender: Option<&mut ei::Sender>,
    request: &Insertion,
    paste: LinuxPaste,
    external: bool,
) -> Inserted {
    if !request.permit.active() {
        return Inserted::Cancelled;
    }
    if let Some(outcome) = Delivery::Paste.check_focus(focus(request, external)) {
        return outcome;
    }
    if paste == LinuxPaste::Manual {
        return COPIED_FOR_MANUAL_PASTE;
    }
    let ready = match sender
        .ok_or(PasteBlocked::NoKeyboard)
        .and_then(ei::Sender::ready_paste)
    {
        Ok(ready) => ready,
        Err(blocked) => return paste_remedy(blocked),
    };
    if !request.permit.commit() {
        return Inserted::Cancelled;
    }
    ready.send();
    Inserted::Sent
}

fn publish_portal_selection(
    selection: &mut Option<Arc<str>>,
    request: &Insertion,
    outcome: Inserted,
) -> Inserted {
    let outcome = match outcome {
        Inserted::Copied(_) if !request.permit.commit() => Inserted::Cancelled,
        outcome => outcome,
    };
    if matches!(outcome, Inserted::Sent | Inserted::Copied(_)) {
        *selection = Some(Arc::from(request.text.as_str()));
    }
    outcome
}

const fn paste_remedy(blocked: PasteBlocked) -> Inserted {
    match blocked {
        PasteBlocked::NoKeyboard => Inserted::Copied(
            "Text copied. Desktop keyboard input is unavailable or paused; paste manually.",
        ),
        PasteBlocked::NoKeyboardState => Inserted::Copied(
            "Text copied. The desktop's keyboard layout couldn't be read; paste manually.",
        ),
        PasteBlocked::ShortcutKeys => Inserted::Copied(
            "Text copied. This desktop couldn't confirm released shortcut keys; paste manually.",
        ),
        PasteBlocked::NoPasteKeys => NO_PASTE_BINDING,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        FocusReport, OwnFocus,
        insertion::{Focus, Reply},
    };

    fn insertion(
        text: &str,
        permit: &InsertPermit,
        delivery: Delivery,
        own_focus: OwnFocus,
    ) -> Insertion {
        let (reply, _answer) = Reply::channel();
        Insertion {
            text: text.into(),
            permit: permit.clone(),
            delivery,
            enter: crate::Enter::Withheld,
            own_focus,
            reply,
        }
    }

    #[test]
    fn portal_selection_needs_committed_copy_authority() -> anyhow::Result<()> {
        for active in [true, false] {
            let (events, _receiver) = async_channel::bounded(1);
            let input = InputSender::new(events);
            let permit = input.begin().context("Recording")?;
            let request = insertion(
                "fixture",
                &permit,
                Delivery::Paste,
                FocusReport::default().subscribe(),
            );
            if !active {
                permit.revoke();
            }
            let mut selection = None;
            let outcome =
                publish_portal_selection(&mut selection, &request, COPIED_FOR_MANUAL_PASTE);
            assert_eq!(selection.is_some(), active);
            assert_eq!(
                outcome,
                if active {
                    COPIED_FOR_MANUAL_PASTE
                } else {
                    Inserted::Cancelled
                }
            );
            assert!(!permit.active());
        }
        Ok(())
    }

    #[test]
    fn a_focused_speakeasy_window_never_receives_portal_input() -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let permit = input.begin().context("Recording")?;
        let report = FocusReport::default();
        report.settle(7, true);
        let request = |delivery| insertion("fixture", &permit, delivery, report.subscribe());
        assert_eq!(
            Some(insert_direct(None, &request(Delivery::Direct), true)),
            Delivery::Direct.check_focus(Focus::Own),
            "Direct input ignored Speakeasy's own focus"
        );
        assert_eq!(
            Some(finish_paste(
                None,
                &request(Delivery::Paste),
                LinuxPaste::Standard,
                true,
            )),
            Delivery::Paste.check_focus(Focus::Own),
            "Paste ignored Speakeasy's own focus"
        );
        assert!(permit.commit(), "Refused input spent its authority");
        Ok(())
    }
}
