use std::{collections::HashMap, env, fmt::Write as _, fs};

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

fn queries(lifecycle: &Tokens) -> String {
    let previous = ConfigEpoch(lifecycle.epoch.0.wrapping_sub(1));
    let answers = [
        lifecycle.is_running(|_| false),
        lifecycle.is_running(|_| true),
        lifecycle.save_may_enable(lifecycle.epoch, |_| false),
        lifecycle.save_may_enable(lifecycle.epoch, |_| true),
        lifecycle.save_may_enable(previous, |_| false),
        lifecycle.save_may_enable(previous, |_| true),
        lifecycle.active().is_some(),
        lifecycle.retiring().is_some(),
        lifecycle.is_pausing(),
        lifecycle.is_quitting(),
    ];
    answers
        .iter()
        .map(|answer| if *answer { '1' } else { '0' })
        .collect()
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
        assert_eq!(
            queries(after).get(2..6),
            Some("0000"),
            "a save after Pause would enable dictation"
        );
    }
}

#[test]
fn every_ordering_of_lifecycle_events_keeps_owners_until_they_retire() {
    let mut states = vec![Tokens::default()];
    let mut index = HashMap::from([(key(&states[0]), 0_usize)]);
    let mut report = String::new();
    let mut transitions = 0_usize;
    let mut expanded = 0;
    while let Some(lifecycle) = states.get(expanded).map(copy) {
        writeln!(
            report,
            "{expanded} {} {}",
            key(&lifecycle),
            queries(&lifecycle)
        )
        .unwrap();
        for (event, apply) in EVENTS {
            let mut after = copy(&lifecycle);
            let result = apply(&mut after);
            check(&lifecycle, event, &after, &result);
            let epoch = if after.epoch == lifecycle.epoch {
                "kept"
            } else {
                "advanced"
            };
            let next = states.len();
            let successor = *index.entry(key(&after)).or_insert(next);
            if successor == next {
                states.push(after);
            }
            transitions += 1;
            writeln!(report, "  {event} {result} {successor} {epoch}").unwrap();
        }
        expanded += 1;
    }
    let report = format!(
        "lifecycle states {} transitions {transitions}\n{report}",
        states.len()
    );
    if let Ok(path) = env::var("SPEAKEASY_LIFECYCLE_REPORT") {
        fs::write(path, report).unwrap();
    }
}
