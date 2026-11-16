use super::{
    super::{
        Device, Machine, Payload, Worker,
        session::{Purpose, Session},
    },
    Abstract,
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
        if (session.is_delivering() || session.is_reopening()) && gesture != State::Processing {
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
    if machine.screen == Device::Open
        && !session.is_some_and(|session| session.is_show() && session.records_screen())
    {
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
    label: &str,
    effects: &str,
) -> Result<(), String> {
    let effects: Vec<&str> = effects.split(' ').collect();
    let delivering = |machine: &Machine<Abstract>| {
        machine
            .session
            .as_ref()
            .is_some_and(|session| session.is_delivering() || session.is_reopening())
    };
    let starts = effects
        .iter()
        .filter(|effect| {
            **effect == "insert" || effect.starts_with("publish(") || **effect == "reopen"
        })
        .count();
    if starts > 1 {
        return Err("One step started two deliveries".into());
    }
    if starts == 1 {
        let Some(session) = after.session.as_ref() else {
            return Err("A delivery started without a session to own it".into());
        };
        if !(session.is_delivering() || session.is_reopening()) {
            return Err("A delivery started but its session does not await it".into());
        }
        let settled = label.starts_with("delivered(")
            || effects
                .iter()
                .any(|effect| effect.starts_with("retire_delivery("));
        if delivering(before) && !settled {
            return Err("A second delivery started while the first was in flight".into());
        }
        if effects.contains(&"insert") && (session.is_show() || session.is_practice()) {
            return Err("A Show or practice inserted text".into());
        }
        if effects.contains(&"reopen") && before.session.is_some() {
            return Err("Recovery started while a session was active".into());
        }
    }
    if effects.iter().any(|effect| effect.starts_with("practise=")) {
        if !before
            .session
            .as_ref()
            .is_some_and(|session| matches!(session.purpose, Purpose::Practice(())))
        {
            return Err("Practice text came from a session that was not practising".into());
        }
        if after.session.is_some() {
            return Err("A practice session survived handing over its text".into());
        }
    }
    if let (Some(earlier), Some(later)) = (before.remembered, after.remembered)
        && later < earlier
    {
        return Err("An earlier Show replaced a later remembered one".into());
    }
    if label.contains("stale")
        && let (Some(was), Some(is)) = (&before.session, &after.session)
        && was.id == is.id
        && was.phase() != is.phase()
        && !effects.iter().any(|effect| effect.contains('='))
    {
        return Err("An event for an earlier session moved the current one".into());
    }
    Ok(())
}
