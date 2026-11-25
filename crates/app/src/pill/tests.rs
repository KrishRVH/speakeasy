use super::*;

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
        note: None,
        hands_free: true,
        for_script: false,
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
    assert_eq!(finished.content(), Content::Slot);
    assert_eq!(finished.feedback(), Some(COPIED_FOR));
    for (short, footer) in [
        (copied(Ending::ScreenStopped, None), None),
        (
            copied(Ending::WindowClosed, Some(NarrationLoss::NotTranscribed)),
            Some(SHOW_DETAILS),
        ),
    ] {
        assert_eq!(
            short.content(),
            Content::Notice,
            "The message says what the Show lost"
        );
        assert_eq!(
            short.footer(),
            footer,
            "Only a lost narration points to its remedy in Shows"
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
    assert_eq!(unchosen.content(), Content::Notice);
    assert_eq!(unchosen.capsule_size(), MESSAGE_SIZE);
    assert_eq!(unchosen.feedback(), Some(NOTICE_FOR));
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
fn a_note_opens_under_the_slot_without_hiding_the_recording() {
    let left = Duration::from_millis(400);
    let noted = |phase, seconds, hands_free| Scene {
        phase,
        hands_free,
        note: Some(Showing {
            note: Note::SecureInput { holder: None },
            left,
        }),
        ..recording(Duration::from_secs(seconds))
    };
    for (seconds, hands_free) in [(2, false), (2, true), (280, true)] {
        let scene = noted(Phase::Recording, seconds, hands_free);
        let bare = Scene {
            note: None,
            ..noted(Phase::Recording, seconds, hands_free)
        };
        assert_eq!(scene.content(), Content::Slot, "The waveform stays");
        assert_eq!(
            scene.shows_clock(),
            bare.shows_clock(),
            "The lock, timer, and countdown stay"
        );
        assert_eq!(
            scene.row_size(),
            bare.row_size(),
            "The slot row keeps its size"
        );
        let (size, row) = (scene.capsule_size(), bare.capsule_size());
        assert!(
            size.height > row.height && size.width >= NOTE_WIDTH.max(row.width),
            "The capsule makes room under the slot, {size:?} around {row:?}"
        );
    }
    assert_eq!(
        noted(Phase::Recording, 2, false).wake_after(),
        Some(left),
        "The note closes as it ends"
    );
    assert!(
        noted(Phase::Processing, 2, false).shown_note().is_some(),
        "A quick hold still sees it while its words are recognized"
    );
    let done = noted(Phase::Done, 2, false);
    assert!(done.shown_note().is_none());
    assert_eq!(done.capsule_size(), done.row_size());
}

#[test]
fn secure_input_is_read_once_as_each_recording_opens_and_names_its_holder() {
    let snapshot = |session, phase| Snapshot {
        session,
        phase,
        ..Snapshot::default()
    };
    let unread =
        || -> Option<SecureInput> { panic!("Secure input was read after its recording opened") };
    let held = || {
        Some(SecureInput {
            holder: Some("Terminal".into()),
        })
    };
    let secure = Some(Note::SecureInput {
        holder: Some("Terminal".into()),
    });
    let shown = |notes: &Notes, at| notes.showing(at).map(|showing| showing.note);
    let now = Instant::now();
    let later = now + Duration::from_secs(1);
    let starting = snapshot(1, Phase::Starting);
    let recording = snapshot(1, Phase::Recording);

    let mut notes = Notes::default();
    notes.follow(&snapshot(0, Phase::Idle), &starting, now, unread, || None);
    assert_eq!(
        shown(&notes, now),
        None,
        "The note waits for the microphone"
    );
    notes.follow(&starting, &recording, now, || None, || None);
    assert_eq!(
        shown(&notes, now),
        None,
        "Without secure input the slot opens as usual"
    );

    let mut notes = Notes::default();
    notes.follow(&starting, &recording, now, held, || None);
    assert_eq!(
        shown(&notes, now),
        secure,
        "Secure input at the opening names its holder"
    );
    for next in [recording.clone(), snapshot(1, Phase::Stopping)] {
        notes.follow(&recording, &next, later, unread, || None);
    }
    assert_eq!(
        notes.showing(later).map(|showing| showing.left),
        SECURE_INPUT_FOR.checked_sub(Duration::from_secs(1)),
        "The rest of the recording keeps its deadline"
    );
    notes.follow(
        &recording,
        &snapshot(2, Phase::Starting),
        later,
        unread,
        || None,
    );
    assert_eq!(shown(&notes, later), None, "A new session drops the note");

    let mut notes = Notes::default();
    notes.follow(
        &recording,
        &snapshot(2, Phase::Recording),
        later,
        held,
        || None,
    );
    assert_eq!(
        shown(&notes, later),
        secure,
        "A session that opens at once is read too"
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
fn the_drawing_hint_starts_as_a_show_records_or_after_its_notice_and_ends_with_its_session() {
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
        "Hands-free dictation isn't a Show"
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

#[test]
fn a_dictation_for_a_script_says_so_while_it_records() {
    let script = |phase| Scene {
        phase,
        for_script: true,
        style: Style::Verbatim,
        ..recording(Duration::from_secs(3))
    };
    let captured = script(Phase::Recording);
    assert_eq!(captured.label(), Some(FOR_SCRIPT));
    assert!(captured.shows_clock());
    assert!(captured.capsule_size().width > recording(Duration::from_secs(3)).capsule_size().width);
    assert_eq!(script(Phase::Processing).label(), None);
}

#[test]
fn a_fallback_microphone_note_waits_for_the_secure_input_note_and_gets_its_full_time() {
    let now = Instant::now();
    let starting = Snapshot {
        session: 4,
        phase: Phase::Starting,
        ..Snapshot::default()
    };
    let opened = Snapshot {
        phase: Phase::Recording,
        default_microphone: true,
        ..starting.clone()
    };
    let mut notes = Notes::default();
    notes.follow(
        &starting,
        &opened,
        now,
        || Some(SecureInput { holder: None }),
        || None,
    );
    assert_eq!(
        notes.showing(now).map(|showing| showing.note),
        Some(Note::SecureInput { holder: None })
    );
    let after = now + SECURE_INPUT_FOR;
    assert_eq!(
        notes.showing(after),
        Some(Showing {
            note: Note::DefaultMicrophone,
            left: DEFAULT_MICROPHONE_FOR,
        }),
        "Each note gets its full time"
    );
    notes.follow(&opened, &opened.clone(), after, || None, || None);
    assert_eq!(
        notes.showing(after + DEFAULT_MICROPHONE_FOR),
        None,
        "Later snapshots of the session raise nothing again"
    );

    let mut notes = Notes::default();
    notes.follow(&starting, &opened, now, || None, || None);
    assert_eq!(
        notes.showing(now).map(|showing| showing.note),
        Some(Note::DefaultMicrophone)
    );
    let next = Snapshot {
        session: 5,
        ..starting
    };
    notes.follow(&opened, &next, now, || None, || None);
    assert_eq!(notes.showing(now), None, "A new session drops the note");
}

#[test]
fn an_fn_shortcut_that_macos_also_acts_on_gets_a_note_when_macos_acts() {
    let unread = || -> Option<GlobeKey> { panic!("The Fn key's action was read needlessly") };
    let now = Instant::now();
    let starting = Snapshot {
        session: 1,
        phase: Phase::Starting,
        globe_shortcut: true,
        ..Snapshot::default()
    };
    let opened = Snapshot {
        phase: Phase::Recording,
        ..starting.clone()
    };
    let mut notes = Notes::default();
    notes.follow(&starting, &opened, now, || None, || Some(GlobeKey::Emoji));
    assert_eq!(
        notes.showing(now).map(|showing| showing.note),
        Some(Note::GlobeKey(GlobeKey::Emoji))
    );
    notes.follow(&opened, &opened.clone(), now, || None, unread);

    let mut notes = Notes::default();
    notes.follow(&starting, &opened, now, || None, || None);
    assert_eq!(notes.showing(now), None, "Do Nothing raises no note");

    let another = Snapshot {
        globe_shortcut: false,
        ..opened.clone()
    };
    notes.follow(&starting, &another, now, || None, unread);
    assert_eq!(notes.showing(now), None, "Another shortcut raises no note");

    let dictation = || Some(GlobeKey::Dictation);
    let mut notes = Notes::default();
    notes.follow(&starting, &opened, now, || None, dictation);
    assert_eq!(
        notes.showing(now),
        None,
        "A hold never starts macOS Dictation"
    );
    let locked = Snapshot {
        hands_free: true,
        ..opened.clone()
    };
    notes.follow(&opened, &locked, now, || None, dictation);
    assert_eq!(
        notes.showing(now).map(|showing| showing.note),
        Some(Note::GlobeKey(GlobeKey::Dictation)),
        "The double-tap that locks hands-free also starts macOS Dictation"
    );
}
