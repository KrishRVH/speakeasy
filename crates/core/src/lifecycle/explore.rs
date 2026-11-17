use std::collections::HashMap;

use super::{ConfigEpoch, Lifecycle, State};

type Tokens = Lifecycle<u32, u32>;

#[derive(Clone, Copy, Debug)]
enum Operation {
    Reconfigure,
    Validate,
    Validated { current: bool },
    Start { launches: bool },
    Queue,
    Pause,
    StartFresh,
    Quit,
    Retired,
}

const OPERATIONS: [Operation; 11] = [
    Operation::Reconfigure,
    Operation::Validate,
    Operation::Validated { current: true },
    Operation::Validated { current: false },
    Operation::Start { launches: true },
    Operation::Start { launches: false },
    Operation::Queue,
    Operation::Pause,
    Operation::StartFresh,
    Operation::Quit,
    Operation::Retired,
];

impl Operation {
    fn apply(self, lifecycle: &mut Tokens) -> bool {
        match self {
            Self::Reconfigure => lifecycle.reconfigure(),
            Self::Validate => lifecycle.validate().is_some(),
            Self::Validated { current } => {
                let checking = checking(lifecycle);
                lifecycle.validated(if current { checking } else { checking.next() })
            },
            Self::Start { launches } => {
                let token = fresh(lifecycle);
                lifecycle.start(|| launches.then_some(token).ok_or(())) == Ok(true)
            },
            Self::Queue => {
                let token = fresh(lifecycle);
                lifecycle.queue(token)
            },
            Self::Pause => lifecycle.pause(),
            Self::StartFresh => {
                lifecycle.start_fresh();
                false
            },
            Self::Quit => lifecycle.quit(),
            Self::Retired => lifecycle.retired().is_some(),
        }
    }
}

#[derive(PartialEq, Eq, Hash)]
enum Shape {
    Disabled,
    Validating { current: bool },
    Running,
    Stopping { pending: bool },
    Quitting { owners: bool },
}

fn key(lifecycle: &Tokens) -> (Shape, bool) {
    let shape = match &lifecycle.state {
        State::Disabled => Shape::Disabled,
        State::Validating(epoch) => Shape::Validating {
            current: *epoch == lifecycle.epoch,
        },
        State::Running(_) => Shape::Running,
        State::Stopping { pending, .. } => Shape::Stopping {
            pending: pending.is_some(),
        },
        State::Quitting(owners) => Shape::Quitting {
            owners: owners.is_some(),
        },
    };
    (shape, lifecycle.held)
}

const fn owners(lifecycle: &Tokens) -> Option<u32> {
    match lifecycle.state {
        State::Running(owners) | State::Stopping { owners, .. } | State::Quitting(Some(owners)) => {
            Some(owners)
        },
        State::Disabled | State::Validating(_) | State::Quitting(None) => None,
    }
}

const fn pending(lifecycle: &Tokens) -> Option<u32> {
    match lifecycle.state {
        State::Stopping { pending, .. } => pending,
        State::Disabled | State::Validating(_) | State::Running(_) | State::Quitting(_) => None,
    }
}

const fn checking(lifecycle: &Tokens) -> ConfigEpoch {
    match lifecycle.state {
        State::Validating(epoch) => epoch,
        State::Disabled | State::Running(_) | State::Stopping { .. } | State::Quitting(_) => {
            lifecycle.epoch
        },
    }
}

fn fresh(lifecycle: &Tokens) -> u32 {
    owners(lifecycle)
        .into_iter()
        .chain(pending(lifecycle))
        .max()
        .map_or(1, |token| token.saturating_add(1))
}

fn check(before: &Tokens, operation: Operation, after: &Tokens, acted: bool) {
    if matches!(operation, Operation::Retired) {
        assert_eq!(acted, pending(before).is_some());
        assert!(!before.is_quitting() || owners(after).is_none());
    } else if let Some(kept) = owners(before) {
        assert_eq!(
            owners(after),
            Some(kept),
            "{operation:?} dropped live or retiring owners"
        );
    }
    if matches!(operation, Operation::Start { .. }) && acted {
        assert_eq!(owners(before), None, "a start replaced owners");
    }
    if matches!(operation, Operation::Queue) && acted {
        assert_eq!(after.clone().retired(), Some(fresh(before)));
    }
    if before.is_quitting() {
        assert!(after.is_quitting(), "{operation:?} left quitting");
        assert!(!acted, "{operation:?} acted while quitting");
    }
    if matches!(operation, Operation::Pause) && !before.is_quitting() {
        for saved in [before.epoch, after.epoch] {
            for live in [false, true] {
                assert!(
                    !after.save_may_enable(saved, |_| live),
                    "a save after Pause would enable dictation"
                );
            }
        }
    }
}

#[test]
fn every_ordering_of_lifecycle_events_keeps_owners_until_they_retire() {
    let mut states = vec![Tokens::default()];
    let mut index = HashMap::from([(key(&states[0]), 0_usize)]);
    let mut expanded = 0;
    while let Some(lifecycle) = states.get(expanded).cloned() {
        for operation in OPERATIONS {
            let mut after = lifecycle.clone();
            let acted = operation.apply(&mut after);
            check(&lifecycle, operation, &after, acted);
            let next = states.len();
            if *index.entry(key(&after)).or_insert(next) == next {
                states.push(after);
            }
        }
        expanded += 1;
    }
    eprintln!("{} lifecycle states", states.len());
}
