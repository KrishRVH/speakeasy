use super::*;

#[test]
fn delayed_frames_skip_missed_deadlines_and_on_time_frames_keep_cadence() {
    let deadline = Instant::now();
    let slightly_late = deadline + Duration::from_millis(1);
    assert_eq!(
        next_frame_deadline(deadline, slightly_late),
        deadline + FRAME_INTERVAL
    );
    for delay in [FRAME_INTERVAL, Duration::from_secs(60)] {
        let late = deadline + delay;
        assert_eq!(next_frame_deadline(deadline, late), late + FRAME_INTERVAL);
    }
    let at_60_hz = deadline + Duration::from_nanos(1_000_000_000 / 60);
    assert!(
        next_frame_deadline(deadline, deadline) < at_60_hz,
        "Slower displays must remain eligible on every native frame"
    );
}

#[test]
fn feedback_deadlines_expire_and_never_apply_to_recording_or_cancellation() {
    assert_eq!(
        feedback_remaining(Phase::Done, false, Duration::ZERO),
        Some(SUBMITTED_FOR)
    );
    assert_eq!(feedback_remaining(Phase::Done, false, SUBMITTED_FOR), None);
    assert!(
        feedback_remaining(Phase::Copied, true, SUBMITTED_FOR).is_some(),
        "A copied Show stays long enough to read"
    );
    assert_eq!(feedback_remaining(Phase::Copied, true, COPIED_FOR), None);
    assert!(
        feedback_remaining(Phase::Copied, false, COPIED_FOR).is_some(),
        "A paste remedy stays longer than the Show's two words"
    );
    assert_eq!(feedback_remaining(Phase::Copied, false, NOTICE_FOR), None);
    assert_eq!(
        feedback_remaining(Phase::Recording, false, Duration::ZERO),
        None
    );
    assert_eq!(
        feedback_remaining(Phase::Recording, true, SUBMITTED_FOR),
        None
    );
    assert_eq!(
        feedback_remaining(Phase::Cancelled, false, Duration::ZERO),
        None
    );
    assert_eq!(feedback_remaining(Phase::Error, false, ERROR_FOR), None);
}

#[test]
fn a_reopened_pill_finds_every_inherited_feedback_already_spent() {
    let now = Instant::now();
    let since_phase = now.duration_since(inherited_phase_at(now));
    for phase in [
        Phase::Done,
        Phase::Copied,
        Phase::Notice,
        Phase::Empty,
        Phase::Unchosen,
        Phase::Error,
    ] {
        for labeled in [false, true] {
            assert_eq!(
                feedback_remaining(phase, labeled, since_phase),
                None,
                "A reopened pill replays {phase:?} feedback"
            );
        }
    }
}

#[test]
fn recording_bars_reach_each_level_before_the_next_meter_window() {
    let window = Duration::from_millis(32);
    let mut bar = Spring::new(0.0);
    let mut follow = |target: f32| {
        bar.target = target;
        for _ in 0..2 {
            bar.step(
                window / 2,
                meter_stiffness(Phase::Recording, bar.target > bar.value()),
            );
        }
        bar.value()
    };
    let peak = follow(1.0);
    assert!(peak > 0.9, "A syllable peaks at {peak}");
    let pause = follow(0.0);
    assert!(pause < 0.15, "A pause stays at {pause}");
}

fn recording(elapsed: Duration) -> Scene {
    Scene {
        phase: Phase::Recording,
        since_phase: Duration::ZERO,
        tray_hint: None,
        draw_hint: None,
        hands_free: true,
        show: false,
        style: Style::Prose,
        ending: Ending::Finished,
        narration_lost: None,
        clock: Clock::new(elapsed),
    }
}

#[test]
fn a_copied_show_shows_its_label_unless_it_lost_its_screen_or_narration() {
    let copied = |ending, narration_lost| Scene {
        phase: Phase::Copied,
        show: true,
        ending,
        narration_lost,
        ..recording(Duration::ZERO)
    };
    let finished = copied(Ending::Finished, None);
    assert!(finished.content() == Content::Slot);
    assert_eq!(finished.feedback(), Some(COPIED_FOR));
    for (short, footer) in [
        (copied(Ending::ScreenStopped, None), None),
        (
            copied(Ending::WindowClosed, Some(NarrationLoss::NotTranscribed)),
            Some(DETAILS),
        ),
    ] {
        assert!(
            short.content() == Content::Notice,
            "The message says what the Show lost"
        );
        assert_eq!(
            short.footer(),
            footer,
            "Only a lost narration's remedy may need Settings"
        );
        assert_eq!(short.capsule_size(), MESSAGE_SIZE);
        assert_eq!(short.feedback(), Some(NOTICE_FOR), "Long enough to read");
    }
}

#[test]
fn a_show_with_nothing_to_record_says_so_long_enough_to_read() {
    let unchosen = Scene {
        phase: Phase::Unchosen,
        show: true,
        ..recording(Duration::ZERO)
    };
    assert!(unchosen.is_shown(), "Unlike a cancel, it is explained");
    assert!(unchosen.content() == Content::Notice);
    assert_eq!(unchosen.capsule_size(), MESSAGE_SIZE);
    assert_eq!(unchosen.feedback(), Some(NOTICE_FOR));
}

#[test]
fn a_notice_reads_as_a_muted_message_rather_than_a_failure() {
    let notice = Scene {
        phase: Phase::Notice,
        ..recording(Duration::ZERO)
    };
    assert!(
        notice.content() == Content::Notice,
        "A notice needs no action"
    );
    assert_eq!(notice.capsule_size(), MESSAGE_SIZE);
    assert_eq!(notice.feedback(), Some(NOTICE_FOR), "Long enough to read");
}

#[test]
fn a_report_that_repeats_the_last_begins_new_feedback() {
    let copied = Snapshot {
        phase: Phase::Copied,
        show: true,
        message: "Show copied. Paste it into your coding agent.".into(),
        ..Snapshot::default()
    };
    assert!(
        !begins_feedback(&copied, &copied.clone()),
        "A snapshot presented again keeps its time on screen"
    );
    let copied_again = Snapshot {
        reported: 1,
        ..copied.clone()
    };
    assert!(
        begins_feedback(&copied, &copied_again),
        "Copying the last Show again went unseen"
    );
}

#[test]
fn a_new_show_says_how_to_draw_in_place_of_its_clock_until_the_hint_ends() {
    let show = |seconds, draw_hint| Scene {
        draw_hint,
        show: true,
        ..recording(Duration::from_secs(seconds))
    };
    let left = Duration::from_millis(400);
    let hinted = show(2, Some(left));
    assert!(hinted.shows_draw_hint());
    assert!(
        hinted.capsule_size().width > show(2, None).capsule_size().width,
        "The capsule widens for the hint"
    );
    assert_eq!(
        hinted.wake_after(),
        Some(left),
        "The clock returns as the hint ends"
    );
    assert!(
        !show(280, Some(left)).shows_draw_hint(),
        "The countdown to the limit outranks the hint"
    );
    let stopping = Scene {
        phase: Phase::Stopping,
        hands_free: false,
        ..show(2, Some(left))
    };
    assert!(
        !stopping.shows_draw_hint(),
        "A Show that stops drops the hint with its clock"
    );
}

#[test]
fn clock_wakes_land_on_whole_seconds_of_the_recording() {
    assert_eq!(
        recording(Duration::from_millis(2_300)).wake_after(),
        Some(Duration::from_millis(700)),
        "A frame drawn late into a second waits only for the next one"
    );
    assert_eq!(
        recording(Duration::from_secs(2)).wake_after(),
        Some(Duration::from_secs(1))
    );
}

#[test]
fn the_drawing_hint_starts_when_a_show_records_and_ends_with_its_session() {
    let snapshot = |session, phase, show| Snapshot {
        session,
        phase,
        hands_free: show,
        show,
        ..Snapshot::default()
    };
    let now = Instant::now();
    let later = now + Duration::from_secs(1);
    let full = now.checked_add(DRAW_HINT_FOR);
    let held = snapshot(1, Phase::Recording, false);
    let show = snapshot(1, Phase::Recording, true);
    assert_eq!(
        draw_hint_until(&held, &show, now, None),
        full,
        "S during a hold starts the hint"
    );
    let republished = Snapshot {
        message: "Republished".into(),
        ..show.clone()
    };
    assert_eq!(
        draw_hint_until(&show, &republished, later, full),
        full,
        "A republished Show keeps its deadline"
    );
    let opening = snapshot(1, Phase::Starting, true);
    assert_eq!(
        draw_hint_until(&snapshot(1, Phase::Starting, false), &opening, now, None),
        None,
        "The hint waits for the microphone"
    );
    assert_eq!(
        draw_hint_until(&opening, &show, later, None),
        later.checked_add(DRAW_HINT_FOR),
        "A Show that starts recording gets the full hint"
    );
    let hands_free = Snapshot {
        hands_free: true,
        ..held.clone()
    };
    assert_eq!(
        draw_hint_until(&held, &hands_free, now, None),
        None,
        "Hands-free dictation is not a Show"
    );
    assert_eq!(
        draw_hint_until(&show, &snapshot(2, Phase::Starting, false), later, full),
        None,
        "A new session drops the hint"
    );
}

#[test]
fn limit_countdown_is_clamped_and_only_appears_in_the_last_thirty_seconds() {
    let clock = |seconds| {
        let clock = Clock::new(Duration::from_secs(seconds));
        (clock.near_limit, clock.to_string())
    };
    assert_eq!(clock(269), (false, "4:29".into()));
    assert_eq!(clock(270), (true, "0:30 left".into()));
    assert_eq!(clock(305), (true, "0:00 left".into()));
}
