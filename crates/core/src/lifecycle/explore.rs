use std::collections::HashMap;

use super::{ConfigEpoch, Lifecycle, State};

type Tokens = Lifecycle<u32, u32>;

fn copy(lifecycle: &Tokens) -> Tokens {
    let state = match &lifecycle.state {
        State::Disabled => State::Disabled,
        State::Validating(epoch) => State::Validating(*epoch),
        State::Running(owners) => State::Running(*owners),
        State::Stopping { owners, pending } => State::Stopping {
            owners: *owners,
            pending: *pending,
        },
        State::Quitting(owners) => State::Quitting(*owners),
    };
    Lifecycle {
        state,
        epoch: lifecycle.epoch,
        held: lifecycle.held,
    }
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

fn key(lifecycle: &Tokens) -> String {
    let state = match &lifecycle.state {
        State::Disabled => "disabled",
        State::Validating(epoch) if *epoch == lifecycle.epoch => "validating(current)",
        State::Validating(_) => "validating(stale)",
        State::Running(_) => "running",
        State::Stopping { pending: None, .. } => "stopping",
        State::Stopping {
            pending: Some(_), ..
        } => "stopping(pending)",
        State::Quitting(None) => "quitting",
        State::Quitting(Some(_)) => "quitting(owners)",
    };
    format!("{state} held={}", lifecycle.held)
}

fn started<E>(result: Result<bool, E>) -> String {
    result.map_or_else(|_| "failed".to_owned(), |started| started.to_string())
}

type Event = (&'static str, fn(&mut Tokens) -> String);

const EVENTS: [Event; 11] = [
    ("reconfigure", |lifecycle| {
        lifecycle.reconfigure().to_string()
    }),
    ("validate", |lifecycle| {
        match lifecycle.validate() {
            Some(epoch) if epoch == lifecycle.epoch => "some",
            Some(_) => "some(other)",
            None => "none",
        }
        .to_owned()
    }),
    ("validated(checking)", |lifecycle| {
        lifecycle.validated(checking(lifecycle)).to_string()
    }),
    ("validated(other)", |lifecycle| {
        lifecycle.validated(checking(lifecycle).next()).to_string()
    }),
    ("start(launched)", |lifecycle| {
        let token = fresh(lifecycle);
        started(lifecycle.start(|| Ok::<_, ()>(token)))
    }),
    ("start(failed)", |lifecycle| {
        started(lifecycle.start(|| Err::<u32, _>(())))
    }),
    ("queue", |lifecycle| {
        lifecycle.queue(fresh(lifecycle)).to_string()
    }),
    ("pause", |lifecycle| lifecycle.pause().to_string()),
    ("start_fresh", |lifecycle| {
        lifecycle.start_fresh();
        "()".to_owned()
    }),
    ("quit", |lifecycle| lifecycle.quit().to_string()),
    ("retired", |lifecycle| {
        if lifecycle.retired().is_some() {
            "pending"
        } else {
            "none"
        }
        .to_owned()
    }),
];

fn check(before: &Tokens, event: &str, after: &Tokens, result: &str) {
    if event == "retired" {
        assert_eq!(result == "pending", pending(before).is_some());
        assert!(!before.is_quitting() || owners(after).is_none());
    } else if let Some(kept) = owners(before) {
        assert_eq!(
            owners(after),
            Some(kept),
            "{event} dropped live or retiring owners"
        );
    }
    if event.starts_with("start") && result == "true" {
        assert_eq!(owners(before), None, "a start replaced owners");
    }
    if event == "queue" && result == "true" {
        let mut retiring = copy(after);
        assert_eq!(retiring.retired(), Some(fresh(before)));
    }
    if before.is_quitting() {
        assert!(after.is_quitting(), "{event} left quitting");
        assert!(
            !["true", "some"].contains(&result),
            "{event} acted while quitting"
        );
    }
    if event == "pause" && !before.is_quitting() {
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
    while let Some(lifecycle) = states.get(expanded).map(copy) {
        for (event, apply) in EVENTS {
            let mut after = copy(&lifecycle);
            let result = apply(&mut after);
            check(&lifecycle, event, &after, &result);
            let next = states.len();
            if *index.entry(key(&after)).or_insert(next) == next {
                states.push(after);
            }
        }
        expanded += 1;
    }
    eprintln!("{} lifecycle states", states.len());
}
