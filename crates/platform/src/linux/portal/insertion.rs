use std::{
    convert::Infallible,
    os::fd::OwnedFd,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use async_channel::{Receiver, Sender};
use futures_util::{FutureExt as _, StreamExt as _, future::LocalBoxFuture};
use x11rb::protocol::xproto::Window;
use zbus::{
    Message, MessageStream, Proxy,
    zvariant::{OwnedObjectPath, Value},
};

use super::{
    CLIPBOARD_DISCONNECTED, Notice, Options, Remote, SelectionHandoff, deadline, or_pending,
    transfer::{Selection, Transfers, transfer},
};
use crate::{
    Delivery, InputSender, InsertPermit, Inserted, LinuxPaste,
    insertion::{Insertion, MODIFIER_WAIT, Reply},
    linux::{
        CLIPBOARD_CHANGED, COPIED_FOR_MANUAL_PASTE, NO_PASTE_BINDING,
        ei::{self, PasteBlocked, TextBlocked},
        stop_requested, x11,
    },
};

const PLAIN_TEXT: [&str; 2] = ["text/plain;charset=utf-8", "text/plain"];
const MAX_TRANSFERS: usize = 4;
const SELECTION_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) struct Insertions<'a> {
    transfers: Transfers<'a>,
    in_flight: Option<InFlight<'a>>,
    remote: Option<Remote>,
    selection: Option<Selection>,
    forfeited: Option<InsertPermit>,
    notices: &'a Receiver<Notice>,
    selecting: &'a Sender<SelectionHandoff>,
    worker: &'a x11::ClipboardWorker,
    clipboard: &'a Proxy<'static>,
    paste: LinuxPaste,
}

impl<'a> Insertions<'a> {
    pub(super) fn new(
        remote: Option<Remote>,
        notices: &'a Receiver<Notice>,
        selecting: &'a Sender<SelectionHandoff>,
        worker: &'a x11::ClipboardWorker,
        clipboard: &'a Proxy<'static>,
        paste: LinuxPaste,
    ) -> Self {
        Self {
            transfers: Transfers::new(),
            in_flight: None,
            remote,
            selection: None,
            forfeited: None,
            notices,
            selecting,
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
        mut transfer_requests: Option<MessageStream>,
        requests: Receiver<Insertion>,
        input: &InputSender,
        stopping: &Receiver<Infallible>,
    ) -> anyhow::Result<()> {
        loop {
            self.advance_waiting()?;
            let idle = self.in_flight.is_none();
            let modifier_wait = or_pending(self.modifier_wait());
            let prepared = or_pending(
                self.in_flight
                    .as_mut()
                    .and_then(|in_flight| in_flight.phase.preparation()),
            );
            let readable = or_pending(
                self.remote
                    .as_ref()
                    .map(|remote| remote.sender.readiness().readable()),
            );
            tokio::select! {
                biased;
                () = stop_requested(stopping, input) => break,
                notice = self.notices.recv() => {
                    self.apply(notice.context("Desktop monitoring stopped")?);
                },
                prepared = prepared => self.advance_preparation(prepared)?,
                _ = self.transfers.next(), if !self.transfers.is_empty() => {},
                event = or_pending(transfer_requests.as_mut().map(|stream| stream.next())) => {
                    self.serve_transfer(&event.context(CLIPBOARD_DISCONNECTED)??).await?;
                },
                request = requests.recv(), if idle => match request {
                    Ok(request) => {
                        self.in_flight = Some(InFlight {
                            request,
                            phase: Phase::AwaitingModifiers(Instant::now()),
                        });
                    },
                    Err(_) => break,
                },
                readable = readable => readable?.clear_ready(),
                () = modifier_wait => {},
            }
        }
        self.cancel_all();
        Ok(())
    }

    fn clipboard_session(&self) -> Option<&OwnedObjectPath> {
        self.remote.as_ref()?.clipboard_session.as_ref()
    }

    fn dispatch(&mut self) -> anyhow::Result<()> {
        match &mut self.remote {
            Some(remote) => remote.sender.dispatch(),
            None => Ok(()),
        }
    }

    fn accepts_transfers(&self) -> bool {
        self.transfers.len() < MAX_TRANSFERS
            && !matches!(
                self.in_flight,
                Some(InFlight {
                    phase: Phase::SettingSelection(_),
                    ..
                })
            )
    }

    fn apply_queued_notices(&mut self) {
        while let Ok(notice) = self.notices.try_recv() {
            self.apply(notice);
        }
    }

    fn apply(&mut self, notice: Notice) {
        apply_notice(notice, &mut self.selection, &mut self.forfeited);
    }

    fn modifier_wait(&self) -> Option<tokio::time::Sleep> {
        match self.in_flight.as_ref()?.phase {
            Phase::AwaitingModifiers(started) => Some(deadline(started, MODIFIER_WAIT)),
            Phase::SettingSelection(_) | Phase::Preparing(_) => None,
        }
    }

    fn advance_waiting(&mut self) -> anyhow::Result<()> {
        self.apply_queued_notices();
        self.dispatch()?;
        let Some(InFlight { request, .. }) = self.in_flight.take_if(|in_flight| {
            let Phase::AwaitingModifiers(started) = in_flight.phase else {
                return false;
            };
            let request = &in_flight.request;
            !request.permit.active()
                || self.paste == LinuxPaste::Manual
                || request.delivery == Delivery::Direct
                || self
                    .remote
                    .as_ref()
                    .is_some_and(|remote| remote.sender.can_paste())
                || started.elapsed() >= MODIFIER_WAIT
        }) else {
            return Ok(());
        };
        if !request.permit.active() {
            request.reply.send(Ok(Inserted::Cancelled));
            return Ok(());
        }
        let permit = request.permit.clone();
        let phase = match (request.delivery, self.clipboard_session()) {
            (Delivery::Direct, _) => {
                Phase::Preparing(query_focus(self.worker, permit, Route::Direct).boxed_local())
            },
            (Delivery::Paste, Some(session)) => Phase::SettingSelection(
                set_selection(self.clipboard, session.clone(), permit).boxed_local(),
            ),
            (Delivery::Paste, None) => Phase::Preparing(
                copy_with_x11(self.worker, request.text.clone(), permit).boxed_local(),
            ),
        };
        self.in_flight = Some(InFlight { request, phase });
        Ok(())
    }

    fn advance_preparation(&mut self, prepared: anyhow::Result<Prepared>) -> anyhow::Result<()> {
        let InFlight { request, phase } = self
            .in_flight
            .take()
            .context("Missing clipboard preparation")?;
        let permit = request.permit.clone();
        let task = match prepared {
            Ok(Prepared::SelectionSet) => {
                self.selection = None;
                if !permit.active() {
                    request.reply.send(Ok(Inserted::Cancelled));
                    return Ok(());
                }
                confirm_selection(self.selecting, permit).boxed_local()
            },
            Ok(Prepared::Copied { owner }) => {
                prepare_x11_paste(self.worker, owner, request.text.clone(), permit).boxed_local()
            },
            Ok(Prepared::Ownership { owned }) => {
                query_focus(self.worker, permit, Route::PortalPaste { owned }).boxed_local()
            },
            Ok(Prepared::NotCopied) => {
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
                if matches!(phase, Phase::SettingSelection(_)) {
                    self.selection = None;
                }
                request.reply.send(if permit.active() {
                    Err(error)
                } else {
                    Ok(Inserted::Cancelled)
                });
                return Ok(());
            },
        };
        self.in_flight = Some(InFlight {
            request,
            phase: Phase::Preparing(task),
        });
        Ok(())
    }

    fn finish(&mut self, request: Insertion, external: bool, route: Route) -> anyhow::Result<()> {
        self.apply_queued_notices();
        self.dispatch()?;
        let sender = self.remote.as_mut().map(|remote| &mut remote.sender);
        let outcome = match route {
            Route::Direct => insert_direct(sender, &request, external),
            Route::X11Paste { owned } | Route::PortalPaste { owned } => finish_paste(
                sender,
                &request,
                self.paste,
                owned,
                external,
                self.forfeited.as_ref(),
            ),
        };
        let outcome = if matches!(route, Route::PortalPaste { .. }) {
            publish_portal_selection(&mut self.selection, &request, outcome)
        } else {
            outcome
        };
        request.reply.send(Ok(outcome));
        Ok(())
    }

    #[expect(
        clippy::future_not_send,
        reason = "The desktop thread keeps the native sender and its transfer futures on one LocalSet"
    )]
    async fn serve_transfer(&self, event: &Message) -> anyhow::Result<()> {
        let (path, mime, serial): (OwnedObjectPath, String, u32) = event.body().deserialize()?;
        if self.clipboard_session() != Some(&path) {
            return Ok(());
        }
        let payload = self
            .selection
            .as_ref()
            .filter(|_| self.accepts_transfers() && PLAIN_TEXT.contains(&mime.as_str()))
            .map(Selection::payload);
        let clipboard = self.clipboard;
        let Some(payload) = payload else {
            tokio::time::timeout(
                SELECTION_TIMEOUT,
                clipboard.call_noreply("SelectionWriteDone", &(path, serial, false)),
            )
            .await??;
            return Ok(());
        };
        let session = path.clone();
        let write = async move {
            let fd: zbus::zvariant::OwnedFd =
                clipboard.call("SelectionWrite", &(session, serial)).await?;
            Ok(OwnedFd::from(fd))
        };
        let done = async move |success: bool| {
            clipboard
                .call::<_, _, ()>("SelectionWriteDone", &(path, serial, success))
                .await?;
            Ok(())
        };
        self.transfers
            .push(transfer(write, done, payload).boxed_local());
        Ok(())
    }

    fn cancel_all(self) {
        if let Some(in_flight) = self.in_flight {
            in_flight.request.reply.send(Ok(Inserted::Cancelled));
        }
    }
}

struct InFlight<'a> {
    request: Insertion,
    phase: Phase<'a>,
}

enum Phase<'a> {
    AwaitingModifiers(Instant),
    SettingSelection(Preparation<'a>),
    Preparing(Preparation<'a>),
}

impl<'a> Phase<'a> {
    fn preparation(&mut self) -> Option<&mut Preparation<'a>> {
        match self {
            Self::AwaitingModifiers(_) => None,
            Self::SettingSelection(task) | Self::Preparing(task) => Some(task),
        }
    }
}

type Preparation<'a> = LocalBoxFuture<'a, anyhow::Result<Prepared>>;

enum Prepared {
    SelectionSet,
    Copied { owner: Window },
    NotCopied,
    Ownership { owned: bool },
    Target { external: bool, route: Route },
}

enum Route {
    Direct,
    X11Paste { owned: bool },
    PortalPaste { owned: bool },
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
    tokio::time::timeout(
        SELECTION_TIMEOUT,
        clipboard.call::<_, _, ()>("SetSelection", &(session, mime_types)),
    )
    .await??;
    Ok(Prepared::SelectionSet)
}

async fn copy_with_x11(
    worker: &x11::ClipboardWorker,
    text: String,
    permit: InsertPermit,
) -> anyhow::Result<Prepared> {
    Ok(worker
        .set_text(text, permit)
        .await?
        .map_or(Prepared::NotCopied, |owner| Prepared::Copied { owner }))
}

async fn confirm_selection(
    selecting: &Sender<SelectionHandoff>,
    permit: InsertPermit,
) -> anyhow::Result<Prepared> {
    let (ready, confirmed) = Reply::channel();
    selecting.send(SelectionHandoff { permit, ready }).await?;
    let owned = confirmed
        .recv()
        .await
        .context("Clipboard ownership monitoring stopped")?;
    Ok(Prepared::Ownership { owned })
}

async fn prepare_x11_paste(
    worker: &x11::ClipboardWorker,
    selection_window: Window,
    text: String,
    permit: InsertPermit,
) -> anyhow::Result<Prepared> {
    let (external, owned) = worker.paste_target(selection_window, text, permit).await?;
    Ok(Prepared::Target {
        external,
        route: Route::X11Paste { owned },
    })
}

async fn query_focus(
    worker: &x11::ClipboardWorker,
    permit: InsertPermit,
    route: Route,
) -> anyhow::Result<Prepared> {
    let external = worker.external_target(permit).await.unwrap_or(false);
    Ok(Prepared::Target { external, route })
}

fn insert_direct(sender: Option<&mut ei::Sender>, request: &Insertion, external: bool) -> Inserted {
    if !request.permit.active() {
        return Inserted::Cancelled;
    }
    if let Some(outcome) = Delivery::Direct.check_focus(external && !request.own_focus.held()) {
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
            "Direct input cannot contain a null character. Set “Insert text by” to a Paste option in Settings."
        },
    })
}

const SELECTION_LOST: Inserted = Inserted::Unavailable(
    "The clipboard changed or could not be confirmed before pasting. Set “Insert text by” to Copy for manual paste in Settings.",
);

fn finish_paste(
    sender: Option<&mut ei::Sender>,
    request: &Insertion,
    paste: LinuxPaste,
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
    if let Some(outcome) = Delivery::Paste.check_focus(external && !request.own_focus.held()) {
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
    selection: &mut Option<Selection>,
    request: &Insertion,
    outcome: Inserted,
) -> Inserted {
    match outcome {
        Inserted::Sent => {},
        Inserted::Copied(_) if request.permit.commit() => {},
        Inserted::Copied(_) => return Inserted::Cancelled,
        Inserted::Cancelled | Inserted::Unavailable(_) => return outcome,
    }
    *selection = Some(Selection::new(&request.text, request.permit.clone()));
    outcome
}

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
    selection: &mut Option<Selection>,
    forfeited: &mut Option<InsertPermit>,
) {
    let Notice { permit, revoked } = notice;
    if selection
        .as_ref()
        .is_some_and(|selection| selection.serves(&permit))
    {
        *selection = None;
    }
    if revoked {
        *forfeited = Some(permit);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        super::{
            SelectionOwnership,
            tests::{fixture_session, owner_changed},
        },
        *,
    };
    use crate::{FocusReport, Input, InputSender, OwnFocus};

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

    fn paste_after_notices(
        notices: &Receiver<Notice>,
        forfeited: &mut Option<InsertPermit>,
        permit: &InsertPermit,
        owned: bool,
    ) -> Inserted {
        while let Ok(notice) = notices.try_recv() {
            apply_notice(notice, &mut None, forfeited);
        }
        let request = insertion(
            "fixture",
            permit,
            Delivery::Paste,
            FocusReport::default().subscribe(),
        );
        finish_paste(
            None,
            &request,
            LinuxPaste::Standard,
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
        let session = fixture_session()?;
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

        ownership.observe(&owner_changed(&session, true)?, &notify)?;
        let taken = input.begin().context("Taken recording")?;
        ownership.adopt(taken.clone(), &notify)?;
        ownership.observe(&owner_changed(&session, false)?, &notify)?;
        assert_eq!(
            paste_after_notices(&notices, &mut forfeited, &taken, true),
            SELECTION_LOST,
            "Another app taking the clipboard looked like a cancellation"
        );

        ownership.observe(&owner_changed(&session, true)?, &notify)?;
        let escaped = input.begin().context("Escaped recording")?;
        ownership.adopt(escaped.clone(), &notify)?;
        input.deliver(Input::Cancel);
        ownership.observe(&owner_changed(&session, false)?, &notify)?;
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
    fn a_focused_speakeasy_window_never_receives_portal_input() -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let permit = input.begin().context("Recording")?;
        let report = FocusReport::default();
        report.settle(7, true);
        let request = |delivery| insertion("fixture", &permit, delivery, report.subscribe());
        assert_eq!(
            Some(insert_direct(None, &request(Delivery::Direct), true)),
            Delivery::Direct.check_focus(false),
            "Direct input ignored Speakeasy's own focus"
        );
        assert_eq!(
            Some(finish_paste(
                None,
                &request(Delivery::Paste),
                LinuxPaste::Standard,
                true,
                true,
                None,
            )),
            Delivery::Paste.check_focus(false),
            "Paste ignored Speakeasy's own focus"
        );
        assert!(permit.commit(), "Refused input spent its authority");
        Ok(())
    }

    #[test]
    fn cancelled_direct_input_precedes_focus_and_capability_errors() -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let permit = input.begin().context("Recording")?;
        permit.revoke();
        for external in [false, true] {
            for text in ["fixture", "\0"] {
                let request = insertion(
                    text,
                    &permit,
                    Delivery::Direct,
                    FocusReport::default().subscribe(),
                );
                assert_eq!(
                    insert_direct(None, &request, external),
                    Inserted::Cancelled,
                    "external={external}, text={text:?}"
                );
            }
        }
        Ok(())
    }
}