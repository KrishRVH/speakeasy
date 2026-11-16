use std::hash::{DefaultHasher, Hash, Hasher};

use super::{
    super::{Failure, Machine, Outcome, Part, Phase, RecognitionMode, SessionId, Ticket, Worker},
    Abstract, TIMING,
};
use crate::gesture::Moment;

pub(super) const STALE: SessionId = SessionId::new(0);

#[derive(Clone, Copy)]
pub(super) struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xCBF2_9CE4_8422_2325)
    }
}

impl Fnv {
    pub(super) fn line(&mut self, line: &str) {
        for byte in line.bytes().chain(*b"\n") {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3);
        }
    }

    pub(super) const fn finish(self) -> u64 {
        self.0
    }
}

pub(super) fn ids(machine: &Machine<Abstract>) -> Vec<SessionId> {
    ids_in(&format!("{:?}", shown(machine)))
}

fn ids_in(render: &str) -> Vec<SessionId> {
    let mut ids: Vec<SessionId> = render
        .split("SessionId(")
        .skip(1)
        .filter_map(|after| after.split_once(')')?.0.parse().ok())
        .map(SessionId::new)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

pub(super) fn name(ids: &[SessionId], id: SessionId) -> String {
    ids.binary_search(&id)
        .map_or_else(|_| "new".into(), |rank| rank.to_string())
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

pub(super) fn render(
    machine: &Machine<Abstract>,
    clock: Moment,
    mode: Option<RecognitionMode>,
) -> String {
    let render = format!("{:?} recorder: {mode:?}", shown(machine));
    let ids = ids_in(&render);
    let renamed = replace(&render, "SessionId(", |id| name(&ids, SessionId::new(id)));
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
