use super::{
    super::{Capture, Device, Event, Machine, Outcome, Payload, Worker, session::Session},
    Abstract,
    canonical::STALE,
    oracle::{Effect, Query},
};
use crate::gesture::State;

pub(super) fn state(machine: &Machine<Abstract>, settled: bool) -> Result<(), String> {
    let session = machine.session.as_ref();
    let gesture = machine.gesture.state();
    if (gesture == State::Idle) != session.is_none() {
        return Err(format!(
            "The gesture is {gesture:?} while a session {}",
            if session.is_some() {
                "exists"
            } else {
                "is absent"
            }
        ));
    }
    if let Some(session) = session {
        if session.id != machine.latest.id {
            return Err("The session is not the latest one".into());
        }
        if session.is_capturing()
            && gesture != State::Processing
            && machine.gesture.deadline().is_none()
        {
            return Err("A capturing session has no recording deadline".into());
        }
        if session.awaits_delivery() && gesture != State::Processing {
            return Err("A delivery runs while the gesture still records".into());
        }
        if session.is_practice() && machine.practice.is_none() {
            return Err("A practice session outlived its practice".into());
        }
        if machine.retiring.contains(&session.id) {
            return Err("The live session is also retiring".into());
        }
    }
    if machine.microphone == Device::Open && !session.is_some_and(Session::is_capturing) {
        return Err("The microphone is open without a capturing session".into());
    }
    if machine.screen == Device::Open && !session.is_some_and(Session::records_screen) {
        return Err("The screen is open without a Show recording it".into());
    }
    if let Worker::Transcribing(ticket) = machine.worker
        && ticket.session > machine.latest.id
    {
        return Err("The worker transcribes for a session that never started".into());
    }
    if machine.retiring.iter().any(|&id| id > machine.latest.id) {
        return Err("A cleanup belongs to a session that never started".into());
    }
    let mut retiring = machine.retiring.clone();
    retiring.sort_unstable();
    retiring.dedup();
    if retiring.len() != machine.retiring.len() {
        return Err("One session retires twice".into());
    }
    if machine.remembered.is_some_and(|id| id > machine.latest.id) {
        return Err("A remembered Show belongs to a session that never started".into());
    }
    if settled
        && machine.cues_held
            != (Abstract::settings(&machine.config).play_sounds && session.is_some())
    {
        return Err("Held cues disagree with the session and sound settings".into());
    }
    Ok(())
}

pub(super) fn transition(
    before: &Machine<Abstract>,
    after: &Machine<Abstract>,
    event: &Event<Abstract>,
    effects: &[Effect],
) -> Result<(), String> {
    let deliveries: Vec<&Effect> = effects
        .iter()
        .filter(|effect| {
            matches!(
                effect,
                Effect::Insert | Effect::Publish { .. } | Effect::Reopen
            )
        })
        .collect();
    let lost = before.session.as_ref().is_some_and(|session| {
        session.lost_its_microphone()
            || matches!(event, Event::Capture(id, Capture::Lost(..)) if *id == session.id)
    });
    match deliveries[..] {
        [] => {},
        [delivery] => {
            let Some(session) = after.session.as_ref() else {
                return Err("A delivery started without a session to own it".into());
            };
            if !session.awaits_delivery() {
                return Err("A delivery started but its session does not await it".into());
            }
            let settled = matches!(event, Event::Delivered(_))
                || effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::RetireDelivery(_)));
            if before
                .session
                .as_ref()
                .is_some_and(Session::awaits_delivery)
                && !settled
            {
                return Err("A second delivery started while the first was in flight".into());
            }
            if matches!(delivery, Effect::Insert) && (session.is_show() || session.is_practice()) {
                return Err("A Show or practice inserted text".into());
            }
            if matches!(delivery, Effect::Insert) && lost {
                return Err("A dictation that lost its microphone inserted text".into());
            }
            if matches!(delivery, Effect::Reopen) && before.session.is_some() {
                return Err("Recovery started while a session was active".into());
            }
        },
        _ => return Err("One step started two deliveries".into()),
    }
    if effects
        .iter()
        .any(|effect| matches!(effect, Effect::Answered(Query::Practise, _)))
    {
        if !before.session.as_ref().is_some_and(Session::is_practice) {
            return Err("Practice text came from a session that was not practising".into());
        }
        if after.session.is_some() {
            return Err("A practice session survived handing over its text".into());
        }
    }
    if matches!(after.outcome, Outcome::Withheld(())) && after.last_dictation.is_none() {
        return Err("A withheld dictation is not kept for Paste last".into());
    }
    if let (Some(earlier), Some(later)) = (before.remembered, after.remembered)
        && later < earlier
    {
        return Err("An earlier Show replaced a later remembered one".into());
    }
    if matches!(event, Event::Capture(STALE, _) | Event::Show(STALE, _))
        && let (Some(was), Some(is)) = (&before.session, &after.session)
        && was.id == is.id
        && was.phase() != is.phase()
        && !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Answered(..)))
    {
        return Err("An event for an earlier session moved the current one".into());
    }
    Ok(())
}
