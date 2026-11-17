use proptest::prelude::*;

use super::*;

#[derive(Clone, Copy, Debug)]
enum Event {
    Press,
    Release,
    Lock,
    Show,
    Toggle,
    Escape,
    Interrupt,
    Cancel,
    Abandon,
    Complete,
    Finish,
    Deliver,
    Save,
}

impl Event {
    const ALL: &[Self] = &[
        Self::Press,
        Self::Release,
        Self::Lock,
        Self::Show,
        Self::Toggle,
        Self::Escape,
        Self::Interrupt,
        Self::Cancel,
        Self::Abandon,
        Self::Complete,
        Self::Finish,
        Self::Deliver,
        Self::Save,
    ];

    fn apply(self, gesture: &mut Gesture, now: Moment) {
        match self {
            Self::Press => {
                gesture.press(now);
            },
            Self::Release => {
                gesture.release(now);
            },
            Self::Lock => gesture.lock(),
            Self::Show => gesture.show(),
            Self::Toggle => {
                gesture.toggle(now);
            },
            Self::Escape => {
                gesture.escape();
            },
            Self::Interrupt => {
                gesture.interrupt();
            },
            Self::Cancel => {
                gesture.cancel();
            },
            Self::Abandon => {
                gesture.abandon();
            },
            Self::Complete => gesture.complete(),
            Self::Finish => {
                gesture.finish();
            },
            Self::Deliver => {
                gesture.deliver();
            },
            Self::Save => {
                gesture.save();
            },
        }
    }
}

fn seconds(count: u64) -> u64 {
    nanos(Duration::from_secs(count))
}

fn capture_start(gesture: &Gesture) -> Option<Moment> {
    match gesture.phase {
        Phase::Held { started }
        | Phase::PendingTap { started, .. }
        | Phase::HandsFree { started }
        | Phase::Show { started }
        | Phase::ShowArmed { started } => Some(started),
        Phase::Idle | Phase::Processing | Phase::Saving => None,
    }
}

proptest! {
    #[test]
    fn arbitrary_event_sequences_never_restart_or_overrun_capture_and_keep_a_saving_show(
        events in prop::collection::vec(
            (prop::sample::select(Event::ALL), 0_u32..600_000),
            0..128,
        )
    ) {
        let mut gesture = Gesture::default();
        let mut now = Moment::default();
        for (event, elapsed) in events {
            now = now.after(nanos(Duration::from_millis(u64::from(elapsed))));
            let saving = gesture.phase == Phase::Saving;
            let before = capture_start(&gesture);
            event.apply(&mut gesture, now);

            gesture.tick(now);
            if let (Some(before), Some(after)) = (before, capture_start(&gesture)) {
                prop_assert_eq!(after, before, "{:?} restarted the recording clock", event);
            }
            if saving && !matches!(event, Event::Abandon | Event::Complete) {
                prop_assert_eq!(
                    gesture.phase,
                    Phase::Saving,
                    "{:?} ended a Show being saved",
                    event
                );
            }
            if let Some(started) = capture_start(&gesture) {
                let limit = started.after(nanos(RECORDING_LIMIT));
                let deadline = gesture.deadline().expect("Active capture retains its deadline");
                prop_assert!(started <= now && now < deadline && deadline <= limit);
            } else {
                prop_assert!(gesture.deadline().is_none());
            }
        }
    }
}

#[test]
fn single_tap_finishes_but_double_tap_keeps_the_original_capture() {
    let start = Moment::default();
    let mut gesture = Gesture::default();
    assert_eq!(gesture.press(start), Some(Action::Start));
    let released = start.after(nanos(TAP));
    gesture.release(released);
    assert_eq!(
        gesture.tick(released.after(nanos(DOUBLE_TAP))),
        Some(Action::Finish)
    );
    gesture.complete();
    let start = start.after(seconds(1));
    gesture.press(start);
    let released = start.after(nanos(TAP));
    gesture.release(released);
    let just_before_expiry =
        released.after(nanos(DOUBLE_TAP.saturating_sub(Duration::from_millis(1))));
    assert_eq!(gesture.press(just_before_expiry), None);
    assert_eq!(gesture.phase, Phase::HandsFree { started: start });
}

#[test]
fn locking_a_hold_keeps_its_capture_until_the_next_press() {
    let start = Moment::default();
    let mut gesture = Gesture::default();
    assert_eq!(gesture.press(start), Some(Action::Start));
    gesture.lock();
    assert_eq!(gesture.release(start.after(seconds(1))), None);
    assert_eq!(gesture.phase, Phase::HandsFree { started: start });
    gesture.lock();
    assert_eq!(gesture.press(start.after(seconds(2))), Some(Action::Finish));
}

#[test]
fn a_press_meeting_tap_expiry_finishes_and_must_be_released_before_restarting() {
    let start = Moment::default();
    let mut gesture = Gesture::default();
    gesture.press(start);
    let released = start.after(nanos(TAP));
    gesture.release(released);
    assert_eq!(
        gesture.press(released.after(nanos(DOUBLE_TAP))),
        Some(Action::Finish)
    );
    assert_eq!(gesture.state(), State::Processing);
    gesture.complete();
    assert_eq!(gesture.press(start.after(seconds(1))), None);
    gesture.release(start.after(seconds(1)));
    assert_eq!(gesture.press(start.after(seconds(2))), Some(Action::Start));
}

#[test]
fn a_hold_finishes_at_the_recording_limit_and_its_late_release_does_nothing() {
    let start = Moment::default();
    let limit = start.after(nanos(RECORDING_LIMIT));
    let mut gesture = Gesture::default();
    assert_eq!(gesture.press(start), Some(Action::Start));
    assert_eq!(gesture.tick(limit), Some(Action::Finish));
    assert_eq!(gesture.release(limit.after(seconds(1))), None);
}

#[test]
fn cancel_requires_release_and_late_completion_cannot_end_new_capture() {
    let start = Moment::default();
    let mut gesture = Gesture::default();
    gesture.press(start);
    assert_eq!(gesture.cancel(), Some(Action::Cancel));
    assert_eq!(gesture.press(start), None);
    gesture.release(start);
    assert_eq!(gesture.press(start), Some(Action::Start));
    gesture.complete();
    assert_eq!(gesture.state(), State::Held);
}

#[test]
fn a_show_finishes_on_the_release_after_a_fresh_press() {
    let start = Moment::default();
    let at = |count| start.after(seconds(count));
    let mut gesture = Gesture::default();
    assert_eq!(gesture.press(start), Some(Action::Start));
    gesture.show();
    gesture.press(at(1));
    assert_eq!(
        gesture.phase,
        Phase::Show { started: start },
        "Autorepeat armed the Show"
    );
    assert_eq!(
        gesture.release(at(2)),
        None,
        "Letting go of the hold that started the Show finished it"
    );
    assert_eq!(gesture.state(), State::HandsFree);
    assert_eq!(gesture.press(at(3)), None, "A press finished the Show");
    assert_eq!(gesture.press(at(4)), None);
    assert_eq!(gesture.release(at(5)), Some(Action::Finish));
    assert_eq!(gesture.state(), State::Processing);
    assert_eq!(
        gesture.press(at(6)),
        None,
        "A press reached a Show being saved"
    );
    assert_eq!(gesture.release(at(7)), None);
}

#[test]
fn a_show_keeps_its_capture_start_and_finishes_at_the_limit_even_while_armed() {
    let start = Moment::default();
    let limit = start.after(nanos(RECORDING_LIMIT));
    let mut gesture = Gesture::default();
    gesture.press(start);
    gesture.show();
    assert_eq!(gesture.deadline(), Some(limit));
    gesture.release(start.after(seconds(1)));
    gesture.press(start.after(seconds(2)));
    assert_eq!(gesture.deadline(), Some(limit));
    assert_eq!(gesture.tick(limit), Some(Action::Finish));
    assert_eq!(
        gesture.release(limit.after(seconds(1))),
        None,
        "The release finished the Show a second time"
    );
    assert_eq!(
        gesture.cancel(),
        None,
        "The limit's Show was not being saved"
    );
}

#[test]
fn escape_cancels_a_show_only_while_the_shortcut_is_held() {
    let start = Moment::default();
    let mut gesture = Gesture::default();
    gesture.press(start);
    gesture.show();
    assert_eq!(
        gesture.escape(),
        Some(Action::Cancel),
        "Escape during the opening hold"
    );
    gesture.release(start);
    gesture.press(start);
    gesture.show();
    gesture.release(start);
    assert_eq!(
        gesture.escape(),
        None,
        "Escape alone cancelled a hands-free Show"
    );
    gesture.press(start);
    assert_eq!(
        gesture.escape(),
        Some(Action::Cancel),
        "Escape during the finishing press"
    );
    assert_eq!(gesture.release(start), None, "A cancelled Show finished");
    assert_eq!(gesture.state(), State::Idle);
}

#[test]
fn only_abandoning_discards_a_show_being_saved() {
    let start = Moment::default();
    let mut gesture = Gesture::default();
    gesture.press(start);
    gesture.show();
    assert_eq!(gesture.toggle(start), Some(Action::Finish));
    assert_eq!(gesture.escape(), None);
    assert_eq!(gesture.interrupt(), None);
    assert_eq!(gesture.cancel(), None);
    assert_eq!(gesture.state(), State::Processing);
    assert_eq!(gesture.abandon(), Some(Action::Cancel));
    assert_eq!(gesture.state(), State::Idle);
}

#[test]
fn an_interruption_finishes_a_recording_show_however_the_shortcut_stands() {
    let start = Moment::default();
    let finishes = |gesture: &mut Gesture, stance: &str| {
        assert_eq!(gesture.interrupt(), Some(Action::Finish), "{stance}");
        assert_eq!(gesture.phase, Phase::Saving, "{stance}");
        assert_eq!(
            gesture.release(start),
            None,
            "{stance}: the release finished the Show again"
        );
        gesture.complete();
    };
    let mut gesture = Gesture::default();
    gesture.press(start);
    gesture.show();
    finishes(&mut gesture, "During the opening hold");
    gesture.press(start);
    gesture.show();
    gesture.release(start);
    finishes(&mut gesture, "Hands-free");
    gesture.press(start);
    gesture.show();
    gesture.release(start);
    gesture.press(start);
    finishes(&mut gesture, "During the finishing press");
}

#[test]
fn escape_cancel_and_interruptions_discard_dictation_recording_or_processing() {
    type Discard = fn(&mut Gesture) -> Option<Action>;
    let start = Moment::default();
    let mut gesture = Gesture::default();
    let discards: [(&str, Discard); 3] = [
        ("Escape", Gesture::escape),
        ("Cancel", Gesture::cancel),
        ("Interrupt", Gesture::interrupt),
    ];
    for (name, discard) in discards {
        assert_eq!(gesture.toggle(start), Some(Action::Start));
        assert_eq!(
            discard(&mut gesture),
            Some(Action::Cancel),
            "{name} while hands-free"
        );
        assert_eq!(gesture.toggle(start), Some(Action::Start));
        assert_eq!(gesture.toggle(start), Some(Action::Finish));
        assert_eq!(
            discard(&mut gesture),
            Some(Action::Cancel),
            "{name} while processing"
        );
    }
}

#[test]
fn recovery_owns_saving_until_completion_or_explicit_abandonment() {
    let mut gesture = Gesture::default();
    let now = Moment::default();
    assert!(gesture.save());
    assert!(!gesture.save());
    assert_eq!(gesture.state(), State::Processing);
    assert_eq!(
        [
            gesture.press(now),
            gesture.escape(),
            gesture.cancel(),
            gesture.interrupt(),
            gesture.release(now),
        ],
        [None; 5]
    );
    assert_eq!(gesture.state(), State::Processing);
    assert_eq!(gesture.abandon(), Some(Action::Cancel));
    assert_eq!(gesture.state(), State::Idle);
    assert!(gesture.save());
    gesture.complete();
    assert_eq!(gesture.press(now), Some(Action::Start));
    assert!(!gesture.save(), "Recovery cannot take over a recording");
}

#[test]
fn explicit_delivery_excludes_capture_and_remains_cancellable() {
    let mut gesture = Gesture::default();
    let now = Moment::default();
    assert!(gesture.deliver());
    assert!(!gesture.deliver());
    assert_eq!(gesture.press(now), None);
    assert_eq!(gesture.release(now), None);
    assert_eq!(gesture.state(), State::Processing);
    assert_eq!(gesture.escape(), Some(Action::Cancel));
    assert_eq!(gesture.state(), State::Idle);
    assert_eq!(gesture.press(now), Some(Action::Start));
    assert!(!gesture.deliver());
}
