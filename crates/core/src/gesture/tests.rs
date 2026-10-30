//! Drives the gesture with explicit instants: tap and hands-free timing, Shows, cancellation, and
//! generated event sequences that must keep every capture within the recording limit.

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
    Cancel,
    Abandon,
    Complete,
    Finish,
}

impl Event {
    const ALL: &[Self] = &[
        Self::Press,
        Self::Release,
        Self::Lock,
        Self::Show,
        Self::Toggle,
        Self::Escape,
        Self::Cancel,
        Self::Abandon,
        Self::Complete,
        Self::Finish,
    ];

    fn apply(self, gesture: &mut Gesture, now: Instant) {
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
        }
    }
}

fn capture_start(gesture: &Gesture) -> Option<Instant> {
    match gesture.phase {
        Phase::Held { started }
        | Phase::PendingTap { started, .. }
        | Phase::HandsFree { started }
        | Phase::Show { started }
        | Phase::ShowArmed { started } => Some(started),
        Phase::Idle | Phase::Processing | Phase::Saving => None,
    }
}

#[test]
fn unrepresentable_recording_deadline_expires_immediately() {
    let latest = (0..u64::BITS).rev().fold(Instant::now(), |latest, shift| {
        latest
            .checked_add(Duration::from_secs(1_u64 << shift))
            .unwrap_or(latest)
    });
    assert!(latest.checked_add(RECORDING_LIMIT).is_none());
    let mut gesture = Gesture::default();
    assert_eq!(gesture.press(latest), Some(Action::Start));
    assert_eq!(gesture.deadline(), Some(latest));
    assert_eq!(gesture.tick(latest), Some(Action::Finish));
}

proptest! {
    #[test]
    fn arbitrary_event_sequences_keep_capture_bounded_and_a_saving_show_saved(
        events in prop::collection::vec(
            (prop::sample::select(Event::ALL), 0_u32..600_000),
            0..128,
        )
    ) {
        let mut gesture = Gesture::default();
        let mut now = Instant::now();
        for (event, elapsed) in events {
            now = now
                .checked_add(Duration::from_millis(u64::from(elapsed)))
                .expect("The generated sequence spans less than one day");
            let saving = gesture.phase == Phase::Saving;
            event.apply(&mut gesture, now);
            // Stands in for the owner's timer, which ticks once a deadline passes.
            gesture.tick(now);
            if saving && !matches!(event, Event::Abandon | Event::Complete) {
                prop_assert_eq!(gesture.phase, Phase::Saving, "{:?} ended a Show being saved", event);
            }
            if let Some(started) = capture_start(&gesture) {
                let limit = started.checked_add(RECORDING_LIMIT).expect("Bounded test clock");
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
    let start = Instant::now();
    let mut gesture = Gesture::default();
    assert_eq!(gesture.press(start), Some(Action::Start));
    gesture.release(start + TAP);
    assert_eq!(gesture.tick(start + TAP + DOUBLE_TAP), Some(Action::Finish));
    gesture.complete();
    let start = start + Duration::from_secs(1);
    gesture.press(start);
    gesture.release(start + TAP);
    let just_before_expiry = (start + TAP + DOUBLE_TAP)
        .checked_sub(Duration::from_millis(1))
        .unwrap();
    assert_eq!(gesture.press(just_before_expiry), None);
    assert_eq!(gesture.phase, Phase::HandsFree { started: start });
}

#[test]
fn locking_a_hold_keeps_its_capture_until_the_next_press() {
    let start = Instant::now();
    let mut gesture = Gesture::default();
    assert_eq!(gesture.press(start), Some(Action::Start));
    gesture.lock();
    assert_eq!(gesture.release(start + Duration::from_secs(1)), None);
    assert_eq!(gesture.phase, Phase::HandsFree { started: start });
    gesture.lock();
    assert_eq!(
        gesture.press(start + Duration::from_secs(2)),
        Some(Action::Finish)
    );
}

#[test]
fn expiration_wins_over_second_press_and_capture_is_capped() {
    let start = Instant::now();
    let mut gesture = Gesture::default();
    gesture.press(start);
    gesture.release(start + TAP);
    assert_eq!(
        gesture.press(start + TAP + DOUBLE_TAP),
        Some(Action::Finish)
    );
    assert_eq!(gesture.state(), State::Processing);
    gesture.complete();
    assert_eq!(gesture.press(start + Duration::from_secs(1)), None);
    gesture.release(start + Duration::from_secs(1));
    let second_press = start + Duration::from_secs(2);
    gesture.press(second_press);
    let limit = second_press + RECORDING_LIMIT;
    assert_eq!(gesture.tick(limit), Some(Action::Finish));
    assert_eq!(gesture.release(limit + Duration::from_secs(1)), None);
}

#[test]
fn cancel_requires_release_and_late_completion_cannot_end_new_capture() {
    let start = Instant::now();
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
    let start = Instant::now();
    let at = |seconds| start + Duration::from_secs(seconds);
    let mut gesture = Gesture::default();
    assert_eq!(gesture.press(start), Some(Action::Start));
    gesture.show();
    assert_eq!(gesture.press(at(1)), None, "Autorepeat armed the Show");
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
    let start = Instant::now();
    let limit = start + RECORDING_LIMIT;
    let mut gesture = Gesture::default();
    gesture.press(start);
    gesture.show();
    assert_eq!(gesture.deadline(), Some(limit));
    gesture.release(start + Duration::from_secs(1));
    gesture.press(start + Duration::from_secs(2));
    assert_eq!(gesture.deadline(), Some(limit));
    assert_eq!(gesture.tick(limit), Some(Action::Finish));
    assert_eq!(
        gesture.release(limit + Duration::from_secs(1)),
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
    let start = Instant::now();
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
    let start = Instant::now();
    let mut gesture = Gesture::default();
    gesture.press(start);
    gesture.show();
    assert_eq!(gesture.toggle(start), Some(Action::Finish));
    assert_eq!(gesture.escape(), None);
    assert_eq!(gesture.cancel(), None);
    assert_eq!(gesture.state(), State::Processing);
    assert_eq!(gesture.abandon(), Some(Action::Cancel));
    assert_eq!(gesture.state(), State::Idle);
}

#[test]
fn escape_and_cancel_discard_dictation_recording_or_processing() {
    let start = Instant::now();
    let mut gesture = Gesture::default();
    let discards: [fn(&mut Gesture) -> Option<Action>; 2] = [Gesture::escape, Gesture::cancel];
    for discard in discards {
        assert_eq!(gesture.toggle(start), Some(Action::Start));
        assert_eq!(discard(&mut gesture), Some(Action::Cancel), "Hands-free");
        assert_eq!(gesture.toggle(start), Some(Action::Start));
        assert_eq!(gesture.toggle(start), Some(Action::Finish));
        assert_eq!(discard(&mut gesture), Some(Action::Cancel), "Processing");
    }
}
