//! Selection cases end to end through `select_within`, on signatures of flat gray frames.

use super::*;
use crate::{
    InkPoint, Narration, Point, Stroke, Word, recording::Size, signature::tests::signature,
};

fn sample(at: u64, level: u8) -> Sample {
    Sample {
        at: Moment::from_millis(at),
        signature: signature(level),
    }
}

/// Samples every `step` milliseconds through `end`, gray at the level `level_at` gives.
fn samples(step: usize, end: u64, level_at: impl Fn(u64) -> u8) -> Vec<Sample> {
    (0..=end)
        .step_by(step)
        .map(|at| sample(at, level_at(at)))
        .collect()
}

/// `count` moments `step` milliseconds apart from `first`.
fn every(first: u64, step: usize, count: usize) -> impl Iterator<Item = u64> {
    (first..).step_by(step).take(count)
}

/// A dab of ink lifted at `at`.
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

fn words(list: &[(u64, &str)]) -> Narration {
    Narration::Words(
        list.iter()
            .map(|&(at, text)| Word {
                span: Span::new(
                    Moment::from_millis(at),
                    Moment::from_millis(at.saturating_add(200)),
                ),
                text: text.to_owned(),
            })
            .collect(),
    )
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
    }
}

fn times(selection: &Selection) -> Vec<u64> {
    selection
        .keyframes
        .iter()
        .map(|keyframe| keyframe.at.as_millis())
        .collect()
}

fn said(selection: &Selection) -> Vec<Option<&str>> {
    selection
        .keyframes
        .iter()
        .map(|keyframe| keyframe.said.as_deref())
        .collect()
}

#[test]
fn marks_on_one_screen_share_a_keyframe() {
    let frames = [
        sample(0, 0),
        sample(1_000, 255),
        sample(2_000, 255),
        sample(4_000, 255),
    ];
    let shown = select_within(
        &recording(
            vec![dab(2_000), dab(4_000)],
            vec![click(750)],
            Narration::default(),
        ),
        &frames,
        2,
    )
    .unwrap();
    assert_eq!(times(&shown), [1_000, 4_000]);
    assert_eq!(shown.keyframes()[0].reason, Reason::Click);
    assert_eq!(shown.keyframes()[1].reason, Reason::Mark);
    assert_eq!(shown.keyframes()[1].marks, 0..2);
    assert_eq!(shown.keyframes()[1].file(), "frames/frame-02.png");
}

#[test]
fn every_mark_gets_a_keyframe_within_the_budget() {
    let levels = [0, 40, 80, 120, 160, 164, 210, 250];
    let frames: Vec<Sample> = every(1_000, 2_000, 8)
        .zip(levels)
        .map(|(at, level)| sample(at, level))
        .collect();
    let strokes = every(1_000, 2_000, 8).map(dab).collect();
    let shown = select_within(
        &recording(strokes, Vec::new(), Narration::default()),
        &frames,
        7,
    )
    .unwrap();
    assert_eq!(shown.keyframes().len(), 7);
    let covered: Vec<Range<usize>> = shown.keyframes().iter().map(|k| k.marks.clone()).collect();
    assert_eq!(covered, [0..1, 1..2, 2..3, 3..4, 4..6, 6..7, 7..8]);
    assert_eq!(shown.keyframes()[4].at.as_millis(), 11_000);

    // On one unchanged screen, marks group until they span more than twenty seconds.
    let still = samples(1_000, 50_000, |_| 128);
    let strokes = [2_000, 9_000, 20_000, 33_000, 40_000].map(dab).to_vec();
    let shown = select_within(
        &recording(strokes, Vec::new(), Narration::default()),
        &still,
        12,
    )
    .unwrap();
    assert_eq!(times(&shown), [20_000, 40_000]);
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
    let shown = select_within(&Recording::default(), &frames, 12).unwrap();
    assert_eq!(times(&shown), [1_500]);
    assert_eq!(shown.keyframes()[0].reason, Reason::SceneChange);
}

#[test]
fn double_clicks_merge_and_keyframes_keep_apart() {
    let frames = samples(250, 3_000, |_| 128);
    let actions = vec![click(1_000), click(1_100), click(2_300)];
    let shown = select_within(
        &recording(Vec::new(), actions, Narration::default()),
        &frames,
        12,
    )
    .unwrap();
    assert_eq!(times(&shown), [1_250, 2_500]);
    assert_eq!(shown.keyframes()[0].event, "clicked (x 0.50, y 0.50)");
}

#[test]
fn narration_divides_among_marks() {
    let frames = [sample(1_000, 0), sample(4_000, 255)];
    let narration = words(&[
        (0, "Make"),
        (1_000, "this"),
        (2_000, "smaller,"),
        (3_000, "and"),
        (4_000, "move"),
        (7_000, "that."),
    ]);
    let shown = select_within(
        &recording(vec![dab(1_000), dab(4_000)], Vec::new(), narration),
        &frames,
        12,
    )
    .unwrap();
    assert_eq!(
        said(&shown),
        [Some("Make this smaller,"), Some("and move that.")]
    );
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
    let shown = select_within(&recording(Vec::new(), Vec::new(), narration), &still, 12).unwrap();
    assert_eq!(shown.keyframes().len(), 1);
    assert_eq!(shown.keyframes()[0].reason, Reason::Cue);
    assert_eq!(said(&shown), [Some("Look at this. Now it's stuck.")]);

    // A cue over a screen a scene change already settled on adds nothing.
    let changed = samples(250, 5_000, |at| if at < 1_000 { 40 } else { 90 });
    let narration = words(&[(3_500, "Look"), (3_700, "here.")]);
    let shown = select_within(&recording(Vec::new(), Vec::new(), narration), &changed, 12).unwrap();
    assert_eq!(shown.keyframes().len(), 1);
    assert_eq!(shown.keyframes()[0].reason, Reason::SceneChange);
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
    let shown = select_within(
        &recording(Vec::new(), vec![click(500)], narration),
        &frames,
        12,
    )
    .unwrap();
    assert_eq!(times(&shown), [750, 3_250]);
    assert_eq!(shown.keyframes()[1].reason, Reason::Cue);
    assert_eq!(
        shown.keyframes()[1].event,
        "said “watch” as the screen changed"
    );
    assert_eq!(
        said(&shown),
        [Some("Here's the list."), Some("Watch it now.")]
    );
}

#[test]
fn groups_close_at_a_click_or_too_much_ink() {
    let still = samples(250, 10_000, |_| 128);
    let shown = select_within(
        &recording(
            vec![dab(2_000), dab(4_000)],
            vec![click(3_000)],
            Narration::default(),
        ),
        &still,
        12,
    )
    .unwrap();
    let marks: Vec<Range<usize>> = shown.keyframes().iter().map(|k| k.marks.clone()).collect();
    assert!(
        marks.contains(&(0..1)) && marks.contains(&(1..2)),
        "{marks:?}"
    );

    // Five strokes a mark: two marks fit a group, a third would carry too much ink.
    let heavy = |at: u64| every(at.saturating_sub(400), 100, 5).map(dab);
    let strokes = heavy(2_000)
        .chain(heavy(4_000))
        .chain(heavy(6_000))
        .collect();
    let shown = select_within(
        &recording(strokes, Vec::new(), Narration::default()),
        &still,
        12,
    )
    .unwrap();
    let marks: Vec<Range<usize>> = shown.keyframes().iter().map(|k| k.marks.clone()).collect();
    assert_eq!(marks, [0..2, 2..3]);
}

#[test]
fn same_screen_merges_stay_balanced() {
    let still = samples(250, 15_000, |_| 128);
    let strokes = every(1_000, 2_000, 6).map(dab).collect();
    let clicks = every(2_000, 2_000, 5).map(click).collect();
    let shown =
        select_within(&recording(strokes, clicks, Narration::default()), &still, 3).unwrap();
    let marks: Vec<Range<usize>> = shown.keyframes().iter().map(|k| k.marks.clone()).collect();
    assert_eq!(marks, [0..2, 2..4, 4..6]);
}

#[test]
fn only_a_different_screen_cuts_into_a_drawing() {
    let still = samples(250, 10_000, |_| 128);
    let narration = words(&[(4_000, "look")]);
    let shown = select_within(
        &recording(vec![dab(2_000), dab(6_000)], Vec::new(), narration),
        &still,
        12,
    )
    .unwrap();
    assert_eq!(shown.keyframes().len(), 1);
    assert_eq!(shown.keyframes()[0].reason, Reason::Mark);

    // Nor does a click between the strokes of one mark, far from the frame that shows it: the
    // mark's keyframe reports it.
    let strokes = || vec![dab(2_000), dab(3_100), dab(4_200)];
    let clicked = recording(strokes(), vec![click(2_500)], Narration::default());
    let shown = select_within(&clicked, &still, 12).unwrap();
    assert_eq!(times(&shown), [4_250]);
    assert_eq!(shown.keyframes()[0].marks, 0..1);
    assert_eq!(shown.keyframes()[0].action, Some(click(2_500)));
    assert!(
        shown.keyframes()[0]
            .event
            .ends_with("; clicked (x 0.50, y 0.50)"),
        "{}",
        shown.keyframes()[0].event
    );
    // Of two such clicks, it reports the later.
    let twice = recording(
        strokes(),
        vec![click(2_500), click(3_500)],
        Narration::default(),
    );
    let shown = select_within(&twice, &still, 12).unwrap();
    assert_eq!(times(&shown), [4_250]);
    assert_eq!(shown.keyframes()[0].action, Some(click(3_500)));
    // So does a click made just before the drawing whose result shows while it is drawn.
    let before = recording(strokes(), vec![click(1_800)], Narration::default());
    let shown = select_within(&before, &still, 12).unwrap();
    assert_eq!(times(&shown), [4_250]);
    assert_eq!(shown.keyframes()[0].action, Some(click(1_800)));
    // And so does a click right after it, too close to the mark's frame for one of its own.
    let after = recording(strokes(), vec![click(4_300)], Narration::default());
    let shown = select_within(&after, &still, 12).unwrap();
    assert_eq!(times(&shown), [4_250]);
    assert_eq!(shown.keyframes()[0].action, Some(click(4_300)));
    // Unless that click opens a different screen, which then gets a keyframe of its own.
    let opened = samples(250, 10_000, |at| if at >= 4_500 { 255 } else { 128 });
    let shown = select_within(&after, &opened, 12).unwrap();
    assert_eq!(times(&shown), [4_250, 4_500]);
    assert_eq!(shown.keyframes()[0].action, None);
    assert_eq!(shown.keyframes()[1].action, Some(click(4_300)));
    // With no room for that keyframe, the mark's keyframe still reports the click.
    let shown = select_within(&after, &opened, 1).unwrap();
    assert_eq!(times(&shown), [4_250]);
    assert_eq!(shown.keyframes()[0].action, Some(click(4_300)));

    // A click whose result the mark's frame no longer shows keeps a keyframe of its own.
    let flash = samples(250, 10_000, |at| {
        if (2_600..3_400).contains(&at) {
            255
        } else {
            128
        }
    });
    let shown = select_within(&clicked, &flash, 12).unwrap();
    assert_eq!(times(&shown), [2_750, 4_250]);
    assert_eq!(shown.keyframes()[1].action, None);
    // With no room for it, the mark reports that click although its result was another screen.
    let shown = select_within(&clicked, &flash, 1).unwrap();
    assert_eq!(times(&shown), [4_250]);
    assert_eq!(shown.keyframes()[0].action, Some(click(2_500)));
}

#[test]
fn a_click_between_two_marks_is_reported_by_the_nearer() {
    let still = samples(100, 10_000, |_| 128);
    let shown = select_within(
        &recording(
            vec![dab(2_000), dab(3_350)],
            vec![click(2_400)],
            Narration::default(),
        ),
        &still,
        12,
    )
    .unwrap();
    assert_eq!(times(&shown), [2_000, 3_300]);
    let actions: Vec<Option<Action>> = shown.keyframes().iter().map(|k| k.action).collect();
    assert_eq!(actions, [Some(click(2_400)), None]);

    // Both marks cover this click's result too, but the later one is nearer.
    let shown = select_within(
        &recording(
            vec![dab(2_000), dab(3_350)],
            vec![click(2_450)],
            Narration::default(),
        ),
        &still,
        12,
    )
    .unwrap();
    let actions: Vec<Option<Action>> = shown.keyframes().iter().map(|k| k.action).collect();
    assert_eq!(actions, [None, Some(click(2_450))]);
}

#[test]
fn long_talk_over_a_changing_screen_gets_more_than_one_frame() {
    // Two minutes of steady talk. Small changes at 0:30 and 1:00, then bigger ones near the
    // end that outrank them: ranked alone, the first keyframe would own a minute and a half.
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
    let shown =
        select_within(&recording(Vec::new(), Vec::new(), words(&talk)), &frames, 4).unwrap();
    let at = times(&shown);
    assert_eq!(at.len(), 4, "{at:?}");
    assert!(at[0] <= 30_000 && at[1] <= 60_000, "{at:?}");
    let most = shown
        .keyframes
        .iter()
        .map(|keyframe| {
            keyframe
                .said
                .as_deref()
                .unwrap_or("")
                .split_whitespace()
                .count()
        })
        .max();
    assert!(most <= Some(120), "{most:?}");

    // The same talk over an unchanged screen stays one frame.
    let still = samples(500, 120_000, |_| 90);
    let shown =
        select_within(&recording(Vec::new(), Vec::new(), words(&talk)), &still, 12).unwrap();
    assert_eq!(shown.keyframes().len(), 1);
}

#[test]
fn a_long_lead_in_is_split_near_its_middle() {
    // One strong late change ranks first; of the small earlier ones, coverage takes the one
    // nearest the middle of the lead-in, not the one beside the late frame.
    let level = |at: u64| match at {
        0..30_000 => 20,
        30_000..70_000 => 50,
        70_000..130_000 => 80,
        130_000..138_000 => 110,
        _ => 210,
    };
    let frames = samples(500, 150_000, level);
    let shown = select_within(&Recording::default(), &frames, 2).unwrap();
    assert_eq!(times(&shown), [70_000, 138_000]);
}

#[test]
fn the_budget_grows_with_duration() {
    let budgets =
        [8, 39, 40, 67, 140, 268, u64::MAX].map(|seconds| budget(Duration::from_secs(seconds)));
    assert_eq!(budgets, [6, 6, 7, 8, 12, 12, 12]);
}

#[test]
fn each_event_says_why_its_frame_was_chosen() {
    let event = |shown: Option<Selection>| shown.unwrap().keyframes()[0].event.clone();
    let still = samples(250, 10_000, |_| 128);
    let drawn = recording(
        vec![dab(2_000), dab(4_000)],
        Vec::new(),
        Narration::default(),
    );
    assert_eq!(
        event(select_within(&drawn, &still, 12)),
        "tapped x 0.50, y 0.50; tapped x 0.50, y 0.50"
    );
    let looked = recording(Vec::new(), Vec::new(), words(&[(3_000, "Look")]));
    assert_eq!(event(select_within(&looked, &still, 12)), "said “look”");
    let changed = [
        sample(0, 0),
        sample(1_000, 80),
        sample(1_250, 160),
        sample(1_500, 255),
    ];
    assert_eq!(
        event(select_within(&Recording::default(), &changed, 12)),
        "scene changed"
    );
    let quiet = [sample(0, 0), sample(1_000, 0), sample(2_000, 0)];
    assert_eq!(
        event(select_within(&Recording::default(), &quiet, 12)),
        "the screen midway through"
    );
}

#[test]
fn a_quiet_show_falls_back_to_its_middle() {
    let frames = [sample(0, 0), sample(1_000, 0), sample(2_000, 0)];
    let shown = select_within(&Recording::default(), &frames, 12).unwrap();
    assert_eq!(times(&shown), [1_000]);
    assert_eq!(shown.keyframes()[0].reason, Reason::Middle);
    assert_eq!(select_within(&Recording::default(), &frames, 0), None);
    assert_eq!(select_within(&Recording::default(), &[], 12), None);
}

proptest::proptest! {
    #[test]
    fn selection_keeps_its_invariants(
        gaps in proptest::collection::vec((50_u64..2_000, 0_u8..=255), 1..40),
        dabs in proptest::collection::vec(100_u64..60_000, 0..10),
        clicks in proptest::collection::vec(0_u64..60_000, 0..6),
        spoken in proptest::collection::vec((0_u64..60_000, 0_usize..6), 0..30),
        budget in 0_usize..16,
    ) {
        let frames: Vec<Sample> = gaps
            .iter()
            .scan(0_u64, |at, &(gap, level)| {
                *at = at.saturating_add(gap);
                Some(sample(*at, level))
            })
            .collect();
        let mut dabs = dabs;
        dabs.sort_unstable();
        let mut spoken = spoken;
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
        let shown = select_within(&recording, &frames, budget);
        proptest::prop_assert_eq!(shown.is_some(), budget > 0);
        let Some(shown) = shown else {
            return Ok(());
        };
        let keyframes = shown.keyframes();
        proptest::prop_assert!(keyframes.len() <= budget.min(MOST_KEYFRAMES));
        proptest::prop_assert!(keyframes.iter().map(|k| k.index).eq(1..=keyframes.len()));
        proptest::prop_assert!(keyframes.windows(2).all(|pair| pair[0].at <= pair[1].at));
        let covered: Vec<usize> = keyframes.iter().flat_map(|k| k.marks.clone()).collect();
        proptest::prop_assert_eq!(covered, (0..shown.marks.len()).collect::<Vec<_>>());
        let mut said: Vec<&str> = keyframes
            .iter()
            .filter_map(|k| k.said.as_deref())
            .flat_map(str::split_whitespace)
            .collect();
        let mut expected: Vec<&str> = narration.iter().map(|(_, word)| *word).collect();
        said.sort_unstable();
        expected.sort_unstable();
        proptest::prop_assert_eq!(said, expected);
    }
}
