use proptest::prelude::*;

use super::*;
use crate::{
    InkPoint, Narration, Pin, Point, Size, Stroke,
    recording::tests::words,
    signature::tests::{signature, with_patch},
};

pub(crate) fn keyframe(index: usize, at: u64) -> Keyframe {
    Keyframe {
        index,
        at: Moment::from_millis(at),
        reason: Reason::Middle,
        event: String::new(),
        said: None,
        marks: 0..0,
        action: None,
        crop: None,
    }
}

fn sample(at: u64, level: u8) -> Sample {
    Sample {
        at: Moment::from_millis(at),
        signature: signature(level),
    }
}

fn samples(step: usize, end: u64, level_at: impl Fn(u64) -> u8) -> Vec<Sample> {
    (0..=end)
        .step_by(step)
        .map(|at| sample(at, level_at(at)))
        .collect()
}

fn every(first: u64, step: usize, count: usize) -> impl Iterator<Item = u64> {
    (first..).step_by(step).take(count)
}

fn dab(at: u64) -> Stroke {
    Stroke::new(
        [at.saturating_sub(100), at]
            .map(|at| InkPoint {
                point: Point::new(0.5, 0.5),
                at: Moment::from_millis(at),
            })
            .to_vec(),
    )
    .unwrap()
}

fn click(at: u64) -> Action {
    Action::Click {
        at: Moment::from_millis(at),
        point: Point::new(0.5, 0.5),
    }
}

fn recording(strokes: Vec<Stroke>, actions: Vec<Action>, narration: Narration) -> Recording {
    Recording {
        duration: Moment::from_millis(60_000),
        surface: Size {
            width: 1000.0,
            height: 1000.0,
        },
        strokes,
        actions,
        narration,
        ..Recording::default()
    }
}

fn times(selection: &Selection) -> Vec<u64> {
    selection
        .shots
        .iter()
        .map(|shot| shot.keyframe.at.as_millis())
        .collect()
}

fn said(selection: &Selection) -> Vec<Option<&str>> {
    selection
        .shots
        .iter()
        .map(|shot| shot.keyframe.said.as_deref())
        .collect()
}

fn marked(selection: &Selection) -> Vec<Range<usize>> {
    selection
        .shots
        .iter()
        .map(|shot| shot.keyframe.marks.clone())
        .collect()
}

fn reported(selection: &Selection) -> Vec<Option<Action>> {
    selection
        .shots
        .iter()
        .map(|shot| shot.keyframe.action)
        .collect()
}

#[test]
fn later_actions_survive_a_full_first_sheet() {
    let frames = samples(250, 51_500, |_| 128);
    let actions: Vec<Action> = every(18_000, 2_000, 15).map(click).collect();
    let recording = recording(
        [4_000, 8_000, 12_000, 16_000].map(dab).to_vec(),
        actions.clone(),
        Narration::default(),
    );
    let shown = select(&recording, &frames).unwrap();
    for action in actions {
        assert!(
            shown
                .shots
                .iter()
                .any(|shot| shot.keyframe.action == Some(action)),
            "Missing action at {}",
            action.span().end()
        );
    }
    assert_eq!(times(&shown).last(), Some(&46_250));
}

#[test]
fn changed_endings_survive_without_repeating_an_unchanged_ending() {
    let gradual = samples(1_000, 50_000, |at| u8::try_from(at / 1_000).unwrap() * 4);
    let shown = select(&Recording::default(), &gradual).unwrap();
    assert_eq!(times(&shown), [25_000, 50_000]);

    let returned = samples(250, 51_000, |at| {
        if (20_000..30_000).contains(&at) {
            255
        } else {
            0
        }
    });
    let recording = recording(vec![dab(1_250)], Vec::new(), Narration::default());
    let shown = select(&recording, &returned).unwrap();
    assert_eq!(times(&shown), [1_250, 20_000, 51_000]);

    let still = samples(250, 51_000, |_| 0);
    let shown = select(&recording, &still).unwrap();
    assert_eq!(times(&shown), [1_250]);
}

#[test]
fn a_crowded_show_keeps_its_most_keyframes_spread_out() {
    let end = 300_000;
    let clicks = every(1_000, 1_000, 299).map(click).collect();
    let crowded = Recording {
        duration: Moment::from_millis(end),
        ..recording(Vec::new(), clicks, Narration::default())
    };
    let still = samples(200, end, |_| 128);
    let switching = samples(200, end, |at| match at {
        ..60_000 if at / 1_000 % 2 == 0 => 40,
        ..60_000 => 200,
        _ => 128,
    });
    let shown = select(&crowded, &still).unwrap();
    assert_eq!(shown.shots.len(), MOST_KEYFRAMES);
    for frames in [&still, &switching] {
        for most in [12, 24, MOST_KEYFRAMES] {
            let at = times(&select_within(&crowded, frames, most).unwrap());
            let edges: Vec<u64> = std::iter::once(0)
                .chain(at.iter().copied())
                .chain([end])
                .collect();

            let even = end / (most as u64 + 1);
            assert_eq!(at.len(), most, "{at:?}");
            assert!(
                edges
                    .array_windows()
                    .all(|[one, next]| next - one <= 2 * even),
                "{most}: {at:?}"
            );
        }
    }
}

#[test]
fn thinning_spares_new_screens_before_repeated_ones() {
    let level = |at: u64| match at {
        ..22_000 => 40,
        22_000..24_000 => 120,
        _ => 200,
    };
    let frames = samples(250, 40_000, level);
    let clicks = [10_000, 20_000, 22_000, 24_000].map(click).to_vec();
    let narration = words(&[(10_000, "First"), (20_000, "again.")]);
    let shown = select_within(&recording(Vec::new(), clicks, narration), &frames, 3).unwrap();
    assert_eq!(times(&shown), [10_250, 22_250, 24_250]);
    assert_eq!(said(&shown), [Some("First again."), None, None]);
}

#[test]
fn marks_group_only_while_they_share_a_screen() {
    let levels = [0, 40, 80, 120, 160, 164, 210, 250];
    let frames: Vec<Sample> = every(1_000, 2_000, 8)
        .zip(levels)
        .map(|(at, level)| sample(at, level))
        .collect();
    let strokes = every(1_000, 2_000, 8).map(dab).collect();
    let shown = select(
        &recording(strokes, Vec::new(), Narration::default()),
        &frames,
    )
    .unwrap();
    assert_eq!(shown.shots.len(), 7);
    assert_eq!(marked(&shown), [0..1, 1..2, 2..3, 3..4, 4..6, 6..7, 7..8]);
    assert_eq!(shown.shots[4].keyframe.at.as_millis(), 11_000);
}

#[test]
fn a_scene_change_shows_the_settled_view() {
    let frames = [
        sample(0, 0),
        sample(1_000, 80),
        sample(1_250, 160),
        sample(1_500, 255),
        sample(1_750, 255),
    ];
    let shown = select(&Recording::default(), &frames).unwrap();
    assert_eq!(times(&shown), [1_500]);
    assert_eq!(shown.shots[0].keyframe.reason, Reason::SceneChange);
}

#[test]
fn a_click_shows_the_screen_once_its_change_settles() {
    let fading = samples(200, 3_000, |at| match at {
        ..1_200 => 40,
        1_200 => 120,
        _ => 200,
    });
    let clicked = recording(Vec::new(), vec![click(1_000)], Narration::default());
    let shown = select(&clicked, &fading).unwrap();
    assert_eq!(times(&shown), [1_400]);
    assert_eq!(reported(&shown), [Some(click(1_000))]);

    let dialog: Vec<Sample> = (0..=3_000)
        .step_by(200)
        .map(|at| Sample {
            at: Moment::from_millis(at),
            signature: if at < 1_200 {
                signature(40)
            } else {
                with_patch(640, 360, [0.0, 0.0, 320.0, 120.0])
            },
        })
        .collect();
    let shown = select(&clicked, &dialog).unwrap();
    assert_eq!(times(&shown), [1_200]);
    assert_eq!(shown.shots[0].keyframe.reason, Reason::Click);
    assert_eq!(reported(&shown), [Some(click(1_000))]);
}

#[test]
fn a_click_waits_only_for_a_change_it_could_have_started() {
    let opening = samples(200, 4_000, |at| match at {
        ..2_200 => 40,
        2_200 => 120,
        _ => 200,
    });
    let clicks = vec![click(1_000), click(2_000)];
    let shown = select(
        &recording(Vec::new(), clicks, Narration::default()),
        &opening,
    )
    .unwrap();
    assert_eq!(times(&shown), [1_200, 2_400]);
    assert_eq!(reported(&shown), [Some(click(1_000)), Some(click(2_000))]);

    let clicked = recording(Vec::new(), vec![click(1_000)], Narration::default());
    for answer in [signature(40), with_patch(640, 360, [0.0, 0.0, 240.0, 60.0])] {
        let scrolled: Vec<Sample> = (0..=4_000)
            .step_by(200)
            .map(|at| Sample {
                at: Moment::from_millis(at),
                signature: match at {
                    ..1_200 => signature(40),
                    1_200..2_200 => answer.clone(),
                    _ => signature(200),
                },
            })
            .collect();
        let shown = select(&clicked, &scrolled).unwrap();
        assert_eq!(times(&shown), [1_200, 2_200]);
        assert_eq!(reported(&shown), [Some(click(1_000)), None]);
    }

    let playing = samples(200, 10_000, |at| match at {
        1_200..8_000 if at / 200 % 2 == 1 => 200,
        ..8_000 => 40,
        _ => 120,
    });
    let shown = select(&clicked, &playing).unwrap();
    assert_eq!(times(&shown), [1_200, 8_000]);
    assert_eq!(reported(&shown), [Some(click(1_000)), None]);

    let animated = samples(200, 4_000, |at| match at {
        ..1_200 if at / 200 % 2 == 1 => 200,
        ..1_200 => 40,
        1_200..1_600 => 120,
        _ => 250,
    });
    let shown = select(&clicked, &animated).unwrap();
    assert_eq!(times(&shown), [1_600]);
    assert_eq!(reported(&shown), [Some(click(1_000))]);
}

#[test]
fn a_click_soon_after_another_keeps_its_keyframe() {
    let panes = samples(200, 3_000, |at| match at {
        ..1_200 => 40,
        1_200 => 120,
        1_400..1_800 => 200,
        _ => 90,
    });
    let second = Action::Click {
        at: Moment::from_millis(1_600),
        point: Point::new(0.2, 0.5),
    };
    let clicks = vec![click(1_000), second];
    let shown = select(&recording(Vec::new(), clicks, Narration::default()), &panes).unwrap();
    assert_eq!(times(&shown), [1_400, 1_800]);
    assert_eq!(reported(&shown), [Some(click(1_000)), Some(second)]);
}

#[test]
fn double_clicks_merge_and_keyframes_keep_apart() {
    let frames = samples(250, 3_000, |_| 128);
    let actions = vec![click(1_000), click(1_100), click(2_300)];
    let shown = select(
        &recording(Vec::new(), actions, Narration::default()),
        &frames,
    )
    .unwrap();
    assert_eq!(times(&shown), [1_250, 2_500]);
    assert_eq!(shown.shots[0].keyframe.event, "clicked (x 0.50, y 0.50)");
}

#[test]
fn spoken_cues_never_repeat_an_unchanged_screen() {
    let still = samples(250, 5_000, |_| 90);
    let narration = words(&[
        (500, "Look"),
        (700, "at"),
        (900, "this."),
        (3_000, "Now"),
        (3_200, "it's"),
        (3_400, "stuck."),
    ]);
    let shown = select(&recording(Vec::new(), Vec::new(), narration), &still).unwrap();
    assert_eq!(shown.shots.len(), 1);
    assert_eq!(shown.shots[0].keyframe.reason, Reason::Cue);
    assert_eq!(said(&shown), [Some("Look at this. Now it's stuck.")]);

    let changed = samples(250, 5_000, |at| if at < 1_000 { 40 } else { 90 });
    let narration = words(&[(3_500, "Look"), (3_700, "here.")]);
    let shown = select(&recording(Vec::new(), Vec::new(), narration), &changed).unwrap();
    assert_eq!(shown.shots.len(), 1);
    assert_eq!(shown.shots[0].keyframe.reason, Reason::SceneChange);
}

#[test]
fn a_spoken_cue_finds_the_change_it_points_at() {
    let frames = samples(250, 6_000, |at| if at < 3_250 { 40 } else { 220 });
    let narration = words(&[
        (200, "Here's"),
        (400, "the"),
        (600, "list."),
        (2_400, "Watch"),
        (2_600, "it"),
        (2_800, "now."),
    ]);
    let shown = select(&recording(Vec::new(), vec![click(500)], narration), &frames).unwrap();
    assert_eq!(times(&shown), [750, 3_250]);
    assert_eq!(shown.shots[1].keyframe.reason, Reason::Cue);
    assert_eq!(
        shown.shots[1].keyframe.event,
        "said “watch” as the screen changed"
    );
    assert_eq!(
        said(&shown),
        [Some("Here's the list."), Some("Watch it now.")]
    );
}

#[test]
fn a_spoken_cue_never_waits_for_a_screen_that_keeps_changing() {
    let animating = samples(250, 20_000, |at| match at {
        ..2_000 => 40,
        2_000..12_000 if at / 250 % 2 == 0 => 120,
        2_000..12_000 => 200,
        _ => 250,
    });
    let looked = recording(Vec::new(), Vec::new(), words(&[(2_000, "Look")]));
    let shown = select(&looked, &animating).unwrap();
    assert_eq!(times(&shown), [2_250, 12_000]);
    assert_eq!(shown.shots[0].keyframe.event, "said “look”");
    assert_eq!(shown.shots[1].keyframe.reason, Reason::SceneChange);
}

#[test]
fn narration_before_the_first_change_keeps_the_screen_it_describes() {
    let opened = samples(250, 6_000, |at| if at < 3_250 { 40 } else { 220 });
    let introduced = words(&[
        (200, "Here's"),
        (400, "the"),
        (600, "list."),
        (2_800, "When"),
        (3_000, "I"),
        (3_200, "open"),
        (3_400, "it."),
    ]);
    let clicked = |narration| recording(Vec::new(), vec![click(3_000)], narration);
    let shown = select(&clicked(introduced.clone()), &opened).unwrap();
    assert_eq!(times(&shown), [2_750, 3_250]);
    assert_eq!(shown.shots[0].keyframe.reason, Reason::Opening);
    assert_eq!(
        shown.shots[0].keyframe.event,
        "the screen before the first change"
    );
    assert_eq!(
        said(&shown),
        [Some("Here's the list."), Some("When I open it.")]
    );

    let still = samples(250, 6_000, |_| 40);
    assert_eq!(
        times(&select(&clicked(introduced), &still).unwrap()),
        [3_250]
    );
    for untold in [
        Narration::default(),
        Narration::Text("Here's the list.".to_owned()),
    ] {
        assert_eq!(times(&select(&clicked(untold), &opened).unwrap()), [3_250]);
    }

    let ongoing = words(&[(0, "So"), (300, "I"), (600, "open"), (900, "it.")]);
    let changed = samples(250, 6_000, |at| if at < 750 { 40 } else { 220 });
    let early = recording(Vec::new(), vec![click(600)], ongoing);
    let shown = select(&early, &changed).unwrap();
    assert_eq!(times(&shown), [750]);
    assert_eq!(said(&shown), [Some("So I open it.")]);
}

#[test]
fn groups_close_at_a_click_or_too_much_ink() {
    let still = samples(250, 10_000, |_| 128);
    let shown = select(
        &recording(
            vec![dab(2_000), dab(4_000)],
            vec![click(3_000)],
            Narration::default(),
        ),
        &still,
    )
    .unwrap();
    let groups = marked(&shown);
    assert!(
        groups.contains(&(0..1)) && groups.contains(&(1..2)),
        "{groups:?}"
    );

    let heavy = |at: u64| every(at.saturating_sub(400), 100, 5).map(dab);
    let strokes = heavy(2_000)
        .chain(heavy(4_000))
        .chain(heavy(6_000))
        .collect();
    let shown = select(
        &recording(strokes, Vec::new(), Narration::default()),
        &still,
    )
    .unwrap();
    assert_eq!(marked(&shown), [0..2, 2..3]);
}

#[test]
fn overlapping_marks_group_by_the_span_of_the_whole_group_and_the_screen_it_shows() {
    let at = Moment::from_millis;
    let held_until = |end| {
        Stroke::new(vec![
            InkPoint {
                point: Point::new(0.3, 0.5),
                at: at(1_000),
            },
            InkPoint {
                point: Point::new(0.7, 0.5),
                at: at(end),
            },
        ])
        .unwrap()
    };
    let pinned = |times: [u64; 2]| {
        times
            .map(|millis| Pin {
                at: at(millis),
                point: Point::new(0.5, 0.5),
            })
            .to_vec()
    };
    let groups = |recording: &Recording, samples: &[Sample]| -> Vec<Vec<usize>> {
        marked(&select(recording, samples).unwrap())
            .into_iter()
            .filter(|marks| !marks.is_empty())
            .map(Iterator::collect)
            .collect()
    };

    let clicked_while_drawing = Recording {
        pins: pinned([1_040, 3_500]),
        ..recording(
            vec![held_until(3_000)],
            vec![click(2_000)],
            Narration::default(),
        )
    };
    assert_eq!(
        groups(&clicked_while_drawing, &samples(250, 10_000, |_| 128)),
        [[0, 1, 2]],
        "A click inside the group's span closed it"
    );

    let patched = |at_millis, left: f32| Sample {
        at: at(at_millis),
        signature: with_patch(128, 72, [left, 31.0, left + 6.0, 37.0]),
    };
    let settling = [
        patched(1_040, 30.0),
        patched(2_000, 40.0),
        Sample {
            at: at(5_000),
            signature: signature(40),
        },
    ];
    let drawn_while_settling = Recording {
        pins: pinned([1_040, 2_000]),
        ..recording(vec![held_until(5_000)], Vec::new(), Narration::default())
    };
    assert_eq!(
        groups(&drawn_while_settling, &settling),
        [[0, 1, 2]],
        "Each mark's screen matches the one the group shows, yet they split"
    );

    let outlasting = Recording {
        pins: pinned([1_040, 1_080]),
        ..recording(vec![held_until(22_000)], Vec::new(), Narration::default())
    };
    assert_eq!(
        groups(&outlasting, &samples(250, 25_000, |_| 128)),
        [vec![0], vec![1, 2]],
        "Pins joined a group whose stroke already spans more than twenty seconds"
    );
}

#[test]
fn same_screen_merges_stay_balanced() {
    let still = samples(250, 15_000, |_| 128);
    let strokes = every(1_000, 2_000, 6).map(dab).collect();
    let clicks = every(2_000, 2_000, 5).map(click).collect();
    let shown =
        select_within(&recording(strokes, clicks, Narration::default()), &still, 3).unwrap();
    assert_eq!(marked(&shown), [0..2, 2..4, 4..6]);
}

#[test]
fn a_spoken_cue_never_cuts_into_a_drawing_on_the_same_screen() {
    let still = samples(250, 10_000, |_| 128);
    let narration = words(&[(4_000, "look")]);
    let shown = select(
        &recording(vec![dab(2_000), dab(6_000)], Vec::new(), narration),
        &still,
    )
    .unwrap();
    assert_eq!(shown.shots.len(), 1);
    assert_eq!(shown.shots[0].keyframe.reason, Reason::Mark);
}

fn drawn(clicks: Vec<Action>) -> Recording {
    recording(
        vec![dab(2_000), dab(3_100), dab(4_200)],
        clicks,
        Narration::default(),
    )
}

#[test]
fn a_drawing_reports_the_last_click_on_its_screen() {
    let still = samples(250, 10_000, |_| 128);
    for (clicks, last) in [
        (vec![click(2_500)], click(2_500)),
        (vec![click(2_500), click(3_500)], click(3_500)),
        (vec![click(1_800)], click(1_800)),
        (vec![click(4_300)], click(4_300)),
    ] {
        let shown = select(&drawn(clicks), &still).unwrap();
        assert_eq!(times(&shown), [4_250]);
        assert_eq!(shown.shots[0].keyframe.marks, 0..1);
        assert_eq!(reported(&shown), [Some(last)]);
        assert!(
            shown.shots[0]
                .keyframe
                .event
                .ends_with("; clicked (x 0.50, y 0.50)"),
            "{}",
            shown.shots[0].keyframe.event
        );
    }
}

#[test]
fn a_click_that_changes_the_screen_keeps_its_own_keyframe_until_thinning_folds_it_in() {
    let opened = samples(250, 10_000, |at| if at >= 4_500 { 255 } else { 128 });
    let flash = samples(250, 10_000, |at| {
        if (2_600..3_400).contains(&at) {
            255
        } else {
            128
        }
    });
    for (clicked, frames, apart, reports) in [
        (
            click(4_300),
            &opened,
            [4_250, 4_500],
            [None, Some(click(4_300))],
        ),
        (
            click(2_500),
            &flash,
            [2_750, 4_250],
            [Some(click(2_500)), None],
        ),
    ] {
        let recording = drawn(vec![clicked]);
        let shown = select(&recording, frames).unwrap();
        assert_eq!(times(&shown), apart);
        assert_eq!(reported(&shown), reports);

        let shown = select_within(&recording, frames, 1).unwrap();
        assert_eq!(times(&shown), [4_250]);
        assert_eq!(reported(&shown), [Some(clicked)]);
    }
}

#[test]
fn a_click_between_two_marks_is_reported_by_the_nearer() {
    let still = samples(100, 10_000, |_| 128);
    let shown = select(
        &recording(
            vec![dab(2_000), dab(3_350)],
            vec![click(2_400)],
            Narration::default(),
        ),
        &still,
    )
    .unwrap();
    assert_eq!(times(&shown), [2_000, 3_300]);
    assert_eq!(reported(&shown), [Some(click(2_400)), None]);

    let shown = select(
        &recording(
            vec![dab(2_000), dab(3_350)],
            vec![click(2_450)],
            Narration::default(),
        ),
        &still,
    )
    .unwrap();
    assert_eq!(reported(&shown), [None, Some(click(2_450))]);
}

#[test]
fn long_talk_over_a_changing_screen_gets_more_than_one_frame() {
    let level = |at: u64| match at {
        0..30_000 => 20,
        30_000..60_000 => 50,
        60_000..90_000 => 80,
        90_000..100_000 => 180,
        100_000..110_000 => 250,
        110_000..115_000 => 140,
        _ => 220,
    };
    let frames = samples(500, 120_000, level);
    let talk: Vec<(u64, &str)> = every(0, 500, 240).map(|at| (at, "word")).collect();
    let shown = select(&recording(Vec::new(), Vec::new(), words(&talk)), &frames).unwrap();
    let at = times(&shown);
    assert_eq!(at.len(), 6, "{at:?}");
    assert!(at[0] <= 30_000 && at[1] <= 60_000, "{at:?}");
    let most = shown
        .shots
        .iter()
        .map(|shot| {
            shot.keyframe
                .said
                .as_deref()
                .unwrap_or("")
                .split_whitespace()
                .count()
        })
        .max();
    assert!(most <= Some(120), "{most:?}");
}

#[test]
fn a_quiet_show_falls_back_to_its_middle() {
    let frames = [sample(0, 0), sample(1_000, 0), sample(2_000, 0)];
    let shown = select(&Recording::default(), &frames).unwrap();
    assert_eq!(times(&shown), [1_000]);
    assert_eq!(shown.shots[0].keyframe.reason, Reason::Middle);
    assert_eq!(select(&Recording::default(), &[]), None);
}

proptest! {
    #[test]
    fn selection_keeps_its_invariants(
        gaps in prop::collection::vec((50_u64..2_000, 0_u8..=255), 1..40),
        mut dabs in prop::collection::vec(100_u64..60_000, 0..10),
        clicks in prop::collection::vec(0_u64..60_000, 0..6),
        mut spoken in prop::collection::vec((0_u64..60_000, 0_usize..6), 0..30),
        most in 0_usize..16,
    ) {
        let frames: Vec<Sample> = gaps
            .iter()
            .scan(0_u64, |at, &(gap, level)| {
                *at = at.saturating_add(gap);
                Some(sample(*at, level))
            })
            .collect();
        dabs.sort_unstable();
        spoken.sort_unstable();
        let vocabulary = ["look", "now", "this", "works", "fine.", "here,"];
        let narration: Vec<(u64, &str)> = spoken
            .iter()
            .map(|&(at, word)| (at, vocabulary[word]))
            .collect();
        let recording = recording(
            dabs.into_iter().map(dab).collect(),
            clicks.into_iter().map(click).collect(),
            words(&narration),
        );
        let shown = select_within(&recording, &frames, most);
        prop_assert_eq!(shown.is_some(), most > 0);
        let Some(shown) = shown else {
            return Ok(());
        };
        let keyframes: Vec<&Keyframe> = shown.shots.iter().map(|shot| &shot.keyframe).collect();
        prop_assert!(keyframes.len() <= most);
        let view = Frames::new(&frames).unwrap();
        let with_marks = keyframes.iter().filter(|keyframe| !keyframe.marks.is_empty()).count();
        if let Some(last) = keyframes.last()
            && with_marks < most
        {
            prop_assert!(!view.novel(view.nearest(last.at), view.last()));
        }
        prop_assert!(keyframes.iter().map(|keyframe| keyframe.index).eq(1..=keyframes.len()));
        prop_assert!(keyframes.is_sorted_by_key(|keyframe| keyframe.at));
        let covered: Vec<usize> =
            keyframes.iter().flat_map(|keyframe| keyframe.marks.clone()).collect();
        prop_assert_eq!(covered, (0..shown.marks.len()).collect::<Vec<_>>());
        let said: Vec<&str> = keyframes
            .iter()
            .filter_map(|keyframe| keyframe.said.as_deref())
            .flat_map(str::split_whitespace)
            .collect();
        let spoken: Vec<&str> = narration.iter().map(|&(_, word)| word).collect();
        prop_assert_eq!(said, spoken);
    }
}
