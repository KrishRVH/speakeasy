use std::hash::{DefaultHasher, Hash, Hasher};

use super::{
    super::{Failure, Machine, Outcome, Part, Phase, RecognitionMode, SessionId, Ticket, Worker},
    Abstract, TIMING,
};
use crate::gesture::Moment;

pub(super) const STALE: SessionId = SessionId::new(0);

fn ids(render: &str) -> Vec<u64> {
    let mut ids: Vec<u64> = render
        .split("SessionId(")
        .skip(1)
        .filter_map(|after| after.split_once(')')?.0.parse().ok())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn replace(render: &str, tag: &str, value: impl Fn(u64) -> String) -> String {
    let mut parts = render.split(tag);
    let mut replaced = parts.next().unwrap_or_default().to_owned();
    for part in parts {
        replaced.push_str(tag);
        let number = part
            .split_once(')')
            .and_then(|(digits, after)| Some((digits.parse().ok()?, after)));
        if let Some((number, after)) = number {
            replaced.push_str(&value(number));
            replaced.push(')');
            replaced.push_str(after);
        } else {
            replaced.push_str(part);
        }
    }
    replaced
}

fn shown(machine: &Machine<Abstract>) -> Machine<Abstract> {
    let mut shown = machine.clone();
    shown.outcome = observed(&machine.outcome);
    if let Worker::Transcribing(ticket) = shown.worker
        && shown
            .session
            .as_ref()
            .is_none_or(|session| session.id != ticket.session)
    {
        shown.worker = Worker::Transcribing(Ticket {
            session: STALE,
            part: Part::Final,
        });
    }
    shown
}

fn observed(outcome: &Outcome<Abstract>) -> Outcome<Abstract> {
    match outcome {
        Outcome::ShowCopied(()) => Outcome::ShowCopied(()),
        outcome => match outcome.phase() {
            Phase::Idle
            | Phase::Starting
            | Phase::Recording
            | Phase::Stopping
            | Phase::Processing => Outcome::Idle,
            Phase::Done => Outcome::Done,
            Phase::Copied => Outcome::Copied(()),
            Phase::Notice => Outcome::ShowGone,
            Phase::Empty => Outcome::Empty,
            Phase::Cancelled => Outcome::Cancelled,
            Phase::Unchosen => Outcome::Unchosen(()),
            Phase::Error => Outcome::Failed(Failure::InputLost),
        },
    }
}

fn render(machine: &Machine<Abstract>, clock: Moment, mode: Option<RecognitionMode>) -> String {
    let render = format!("{:?} recorder: {mode:?}", shown(machine));
    let ids = ids(&render);
    let renamed = replace(&render, "SessionId(", |id| {
        ids.partition_point(|&earlier| earlier < id).to_string()
    });
    replace(&renamed, "Moment(", |moment| {
        clock
            .nanos()
            .saturating_sub(moment)
            .min(TIMING.limit())
            .to_string()
    })
}

pub(super) fn key(
    machine: &Machine<Abstract>,
    clock: Moment,
    mode: Option<RecognitionMode>,
) -> u128 {
    let canonical = render(machine, clock, mode);
    (u128::from(half(&canonical, 0)) << 64) | u128::from(half(&canonical, 1))
}

fn half(canonical: &str, salt: u8) -> u64 {
    let mut hasher = DefaultHasher::new();
    salt.hash(&mut hasher);
    canonical.hash(&mut hasher);
    hasher.finish()
}
