mod alphabet;
mod canonical;
mod invariants;
mod oracle;

use std::{collections::HashSet, env, mem, ops::ControlFlow, thread};

use self::{
    alphabet::{CONFIGS, Step, steps},
    canonical::key,
    oracle::{Abstract, Effect, Oracle},
};
use super::{Event, Halt, Machine, Now, RecognitionMode, SessionId};
use crate::gesture::{Moment, Timing};

const TIMING: Timing = Timing::new(1, 2, 3);

const RETIRING: usize = 2;

const SMOKE_DEPTH: usize = 5;

const BATCH: usize = 1 << 14;

#[derive(Clone)]
struct Explored {
    machine: Machine<Abstract>,
    clock: Moment,
    recorder: Option<(SessionId, RecognitionMode)>,
}

impl Explored {
    fn mode(&self) -> Option<RecognitionMode> {
        let session = self.machine.session.as_ref()?;
        let (id, mode) = self.recorder?;
        (session.id == id && session.is_capturing()).then_some(mode)
    }

    fn key(&self) -> u128 {
        key(&self.machine, self.clock, self.mode())
    }
}

enum Reached {
    State(Box<Explored>, u128),
    Failed(Box<Machine<Abstract>>),
    Stopped,
}

struct Successor {
    step: Step,
    reached: Reached,
}

struct Broken {
    step: Step,
    effects: Vec<Effect>,
    reason: String,
}

struct Search {
    seen: HashSet<u128>,
    parents: Vec<Option<(usize, Step)>>,
    next: Vec<(usize, Explored)>,
    transitions: u64,
    bounded: u64,
}

impl Search {
    fn reach(&mut self, source: usize, successor: Successor) {
        self.transitions = self.transitions.saturating_add(1);
        let Reached::State(explored, found) = successor.reached else {
            return;
        };
        if explored.machine.retiring.len() > RETIRING {
            self.bounded = self.bounded.saturating_add(1);
        } else if self.seen.insert(found) {
            self.next.push((self.parents.len(), *explored));
            self.parents.push(Some((source, successor.step)));
        }
    }
}

fn apply(
    state: &Explored,
    event: &Event<Abstract>,
    script: Vec<usize>,
) -> (Reached, Vec<Effect>, Option<Vec<usize>>) {
    let mut machine = state.machine.clone();
    let mut oracle = Oracle::new(script);
    let now = Now {
        moment: state.clock,
        instant: (),
    };
    let reached = match machine.step(now, event.clone(), &mut oracle) {
        ControlFlow::Continue(()) => {
            let explored = Explored {
                machine,
                clock: state.clock,
                recorder: oracle.opened.or(state.recorder),
            };
            let key = explored.key();
            Reached::State(Box::new(explored), key)
        },
        ControlFlow::Break(Halt::Failed) => Reached::Failed(Box::new(machine)),
        ControlFlow::Break(Halt::Stopped) => {
            machine.shut_down(&mut oracle);
            Reached::Stopped
        },
    };
    let next = oracle.next_script();
    (reached, oracle.effects, next)
}

fn check(
    state: &Explored,
    event: &Event<Abstract>,
    reached: &Reached,
    effects: &[Effect],
) -> Result<(), String> {
    match reached {
        Reached::State(next, _) => {
            invariants::state(&next.machine, true)?;
            invariants::transition(&state.machine, &next.machine, event, effects)
        },
        Reached::Failed(machine) => invariants::state(machine, false),
        Reached::Stopped => Ok(()),
    }
}

fn expand(state: &Explored) -> Result<Vec<Successor>, Broken> {
    let mut successors = Vec::new();
    for step in steps(&state.machine, state.clock, state.mode()) {
        let event = match step {
            Step::Elapse => {
                let explored = Explored {
                    clock: state.clock.after(1),
                    ..state.clone()
                };
                let key = explored.key();
                successors.push(Successor {
                    step: Step::Elapse,
                    reached: Reached::State(Box::new(explored), key),
                });
                continue;
            },
            Step::Event(event) => event,
        };
        let mut script = Some(Vec::new());
        while let Some(answers) = script.take() {
            let (reached, effects, next) = apply(state, &event, answers);
            if let Err(reason) = check(state, &event, &reached, &effects) {
                return Err(Broken {
                    step: Step::Event(event),
                    effects,
                    reason,
                });
            }
            successors.push(Successor {
                step: Step::Event(event.clone()),
                reached,
            });
            script = next;
        }
    }
    Ok(successors)
}

fn expand_all(level: &[(usize, Explored)]) -> Vec<Result<Vec<Successor>, Broken>> {
    let threads = thread::available_parallelism().map_or(1, usize::from);
    let chunk = level.len().div_ceil(threads).max(1);
    thread::scope(|scope| {
        #[expect(
            clippy::needless_collect,
            reason = "Every worker spawns before the first join, so the chunks run in parallel"
        )]
        let workers: Vec<_> = level
            .chunks(chunk)
            .map(|states| {
                scope.spawn(move || {
                    states
                        .iter()
                        .map(|(_, state)| expand(state))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    })
}

fn path(parents: &[Option<(usize, Step)>], mut state: usize) -> Vec<Step> {
    let mut path = Vec::new();
    while let Some((parent, step)) = &parents[state] {
        path.push(step.clone());
        state = *parent;
    }
    path.reverse();
    path
}

fn explore(depth_limit: Option<usize>) {
    let mut start = Oracle::new(Vec::new());
    let machine = Machine::new(TIMING, CONFIGS[0], false, (), None, &mut start);
    let initial = Explored {
        machine,
        clock: Moment::default(),
        recorder: None,
    };
    let mut search = Search {
        seen: HashSet::from([initial.key()]),
        parents: vec![None],
        next: Vec::new(),
        transitions: 0,
        bounded: 0,
    };
    let mut level = vec![(0_usize, initial)];
    let mut depth = 0_usize;
    while !level.is_empty() && depth_limit.is_none_or(|limit| depth < limit) {
        let expansions = level
            .chunks(BATCH)
            .flat_map(|batch| batch.iter().zip(expand_all(batch)));
        for ((source, _), expansion) in expansions {
            let successors = expansion.unwrap_or_else(|broken| {
                let mut path = path(&search.parents, *source);
                path.push(broken.step);
                panic!(
                    "{} after {path:?} with effects {:?}",
                    broken.reason, broken.effects
                )
            });
            for successor in successors {
                search.reach(*source, successor);
            }
        }
        eprintln!(
            "depth {depth}: {} states, {} transitions, {} bounded, {} next",
            search.parents.len(),
            search.transitions,
            search.bounded,
            search.next.len()
        );
        level = mem::take(&mut search.next);
        depth = depth.saturating_add(1);
    }
}

#[test]
fn every_ordering_of_events_keeps_the_machine_sound() {
    let depth = match env::var("SPEAKEASY_EXPLORE_DEPTH") {
        Ok(depth) if depth == "all" => None,
        Ok(depth) => Some(
            depth
                .parse()
                .expect("SPEAKEASY_EXPLORE_DEPTH is a depth or `all`"),
        ),
        Err(_) => Some(SMOKE_DEPTH),
    };
    explore(depth);
}
