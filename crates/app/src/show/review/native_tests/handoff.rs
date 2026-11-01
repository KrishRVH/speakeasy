use std::collections::VecDeque;

use async_channel::Receiver;

use super::*;
use crate::show::{
    opencode::{Listing, Outcome, Receipt, Request, Session},
    review::opencode::Handoff,
};

const ENDPOINT: &str = "http://127.0.0.1:1/";
const SESSION: &str = "ses_public_selected";

struct Probe {
    lookups: VecDeque<Receiver<Result<Listing, String>>>,
    lookup_count: usize,
    sent: Vec<Request>,
    outcome: Option<Receiver<Outcome>>,
}
impl gpui::Global for Probe {}

fn lookup(cx: &mut App) -> anyhow::Result<Receipt<Result<Listing, String>>> {
    let probe = cx.global_mut::<Probe>();
    probe.lookup_count = probe.lookup_count.saturating_add(1);
    let receipt = probe
        .lookups
        .pop_front()
        .context("Unexpected synthetic lookup")?;
    Ok(Box::pin(async move {
        receipt
            .recv()
            .await
            .unwrap_or_else(|_closed| Err("Synthetic lookup closed".into()))
    }))
}

fn send(request: Request, cx: &mut App) -> anyhow::Result<Receipt> {
    let probe = cx.global_mut::<Probe>();
    let receipt = probe.outcome.take().context("Unexpected synthetic send")?;
    probe.sent.push(request);
    Ok(Box::pin(async move {
        receipt
            .recv()
            .await
            .unwrap_or_else(|_closed| Outcome::Unknown {
                reason: "Synthetic receipt closed".into(),
            })
    }))
}

fn listing() -> Listing {
    Listing {
        endpoint: ENDPOINT.into(),
        sessions: ["ses_public_other", SESSION]
            .into_iter()
            .map(|id| Session {
                id: id.into(),
                title: Some("Public fixture".into()),
                directory: "/public/project".into(),
                updated: 1000,
            })
            .collect(),
        truncated: false,
    }
}

#[expect(
    clippy::future_not_send,
    reason = "Synthetic handoff receipts drive the owned Review window on GPUI's UI thread"
)]
pub(super) async fn exercise(
    window: WindowHandle<Review>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let (stale_lookup, first_lookup) = async_channel::bounded(1);
    let (current_lookup, second_lookup) = async_channel::bounded(1);
    let (stale_outcome, outcome) = async_channel::bounded(1);
    cx.update(|cx| {
        cx.set_global(Probe {
            lookups: VecDeque::from([first_lookup, second_lookup]),
            lookup_count: 0,
            sent: Vec::new(),
            outcome: Some(outcome),
        });
    })?;
    let (original_sessions, original_send, original) = window.update(cx, |view, _, cx| {
        let original = view
            .library
            .selected
            .as_ref()
            .context("The fixture has no selected Show")?
            .directory
            .clone();
        let saved = (view.actions.sessions, view.actions.send, original);
        view.source = Source::Saved;
        view.actions.sessions = lookup;
        view.actions.send = send;
        let probe = cx.global::<Probe>();
        assert_eq!(probe.lookup_count, 0);
        assert!(
            probe.sent.is_empty(),
            "Opening Review selected a recipient automatically"
        );
        anyhow::Ok(saved)
    })??;
    window.update(cx, |view, window, cx| {
        view.act(Action::OpenCode, window, cx);
        assert!(matches!(view.handoff, Handoff::Loading { .. }));
        assert!(cx.global::<Probe>().sent.is_empty());
        view.act(Action::Show(1), window, cx);
        view.notice = Some("New selection after lookup".into());
    })?;
    wait_for(
        window,
        |view| {
            stale_lookup.is_closed()
                && view.opening.is_none()
                && matches!(view.image, ImageState::Ready(_))
        },
        cx,
    )
    .await?;
    assert!(
        stale_lookup.try_send(Ok(listing())).is_err(),
        "Switching Shows retained the old lookup receipt"
    );
    let selected = window.update(cx, |view, window, cx| {
        assert!(matches!(view.handoff, Handoff::Closed));
        assert_eq!(view.notice.as_deref(), Some("New selection after lookup"));
        let selected = view
            .library
            .selected
            .as_ref()
            .context("The replacement Show is missing")?
            .directory
            .clone();
        assert_ne!(selected, original);
        view.act(Action::OpenCode, window, cx);
        anyhow::Ok(selected)
    })??;
    current_lookup
        .try_send(Ok(listing()))
        .map_err(|_closed| anyhow::anyhow!("Synthetic chooser closed"))?;
    wait_for(
        window,
        |view| matches!(view.handoff, Handoff::Choosing { .. }),
        cx,
    )
    .await?;
    window.update(cx, |view, window, cx| {
        assert_eq!(cx.global::<Probe>().lookup_count, 2);
        assert!(
            cx.global::<Probe>().sent.is_empty(),
            "Session discovery sent a Show without selection"
        );
        view.act(
            Action::SendOpenCode("ses_not_in_listing".into()),
            window,
            cx,
        );
        assert!(
            cx.global::<Probe>().sent.is_empty(),
            "An unlisted recipient was accepted"
        );
        view.act(Action::SendOpenCode(SESSION.into()), window, cx);
        assert!(matches!(view.handoff, Handoff::Sending { .. }));
        let probe = cx.global::<Probe>();
        assert_eq!(probe.sent.len(), 1);
        let request = probe
            .sent
            .first()
            .context("The explicit send must have a captured request")?;
        assert_eq!(request.directory, selected);
        assert_eq!(request.session, SESSION);
        assert_eq!(request.expected_endpoint.as_deref(), Some(ENDPOINT));
        view.act(Action::Show(0), window, cx);
        view.notice = Some("New selection after send".into());
        anyhow::Ok(())
    })??;
    wait_for(
        window,
        |view| {
            stale_outcome.is_closed()
                && view.opening.is_none()
                && matches!(view.image, ImageState::Ready(_))
        },
        cx,
    )
    .await?;
    assert!(
        stale_outcome
            .try_send(Outcome::Queued {
                session: SESSION.into(),
                message: "msg_public_stale".into(),
                attachments: 1,
                bytes: 100,
            })
            .is_err(),
        "Switching Shows retained the old handoff receipt"
    );
    window.update(cx, |view, _, cx| {
        assert!(view.library.is_selected(&original));
        assert!(matches!(view.handoff, Handoff::Closed));
        assert_eq!(view.notice.as_deref(), Some("New selection after send"));
        assert_eq!(cx.global::<Probe>().sent.len(), 1);
        view.source = Source::Demo;
        view.actions.sessions = original_sessions;
        view.actions.send = original_send;
        view.notice = None;
        cx.notify();
    })?;
    Ok(())
}
