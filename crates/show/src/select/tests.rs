//! Selection cases end to end through `select`, on signatures of flat gray frames.

use proptest::prelude::*;

use super::*;
use crate::{
    Ending, InkPoint, Narration, Point, Size, Stroke,
    recording::tests::words,
    signature::tests::{signature, with_patch},
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

fn recording(strokes: Vec<Stroke>, actions: Vec<Action>, narration: Narration) -> Recording {
    Recording {
        duration: Moment::from_millis(60_000),
        ending: Ending::Finished,
        surface: Size {
            width: 1000.0,
            height: 1000.0,
        },
        strokes,
        pins: Vec::new(),
        actions,
        narration,
        narration_lost: None,
        spaces: crate::Spaces::default(),
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

fn marked(selection: &Selection) -> Vec<Range<usize>> {
    selection
        .keyframes
        .iter()
        .map(|keyframe| keyframe.marks.clone())
        .collect()
}

fn reported(selection: &Selection) -> Vec<Option<Action>> {
    selection
        .keyframes
        .iter()
        .map(|keyframe| keyframe.action)
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
                .keyframes
                .iter()
                .any(|frame| frame.action == Some(action)),
            "Missing action at {}",
            action.span().end()
        );
    }
    assert_eq!(times(&shown).last(), Some(&46_250));
}

#[test]
fn changed_endings_survive_without_repeating_an_unchanged_ending() {
    // Each small step is below the scene-change threshold, but the ending is a different view.
    let gradual = samples(1_000, 50_000, |at| u8::try_from(at / 1_000).unwrap() * 4);
    let shown = select(&Recording::default(), &gradual).unwrap();
    assert_eq!(times(&shown), [25_000, 50_000]);

    // Returning to an earlier screen must still show where the recording ends.
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
    // Five minutes of clicking once a second, on a screen that never changes and on one that
    // changes with every click for a minute and then never again.
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
    assert_eq!(shown.keyframes.len(), MOST_KEYFRAMES);
    for frames in [&still, &switching] {
        for most in [12, 24, MOST_KEYFRAMES] {
            let at = times(&select_within(&crowded, frames, most).unwrap());
            let edges: Vec<u64> = std::iter::once(0)
                .chain(at.iter().copied())
                .chain([end])
                .collect();
            // Splitting the longest stretch each time leaves none over twice an even share.
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
    // Two clicks ten seconds apart on one screen, then two new screens two seconds apart.
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
fn marks_on_one_screen_share_a_keyframe() {
    let frames = [
        sample(0, 0),
        sample(1_000, 255),
        sample(2_000, 255),
        sample(4_000, 255),
    ];
    let shown = select(
        &recording(
            vec![dab(2_000), dab(4_000)],
            vec![click(750)],
            Narration::default(),
        ),
        &frames,
    )
    .unwrap();
    assert_eq!(times(&shown), [1_000, 4_000]);
    assert_eq!(shown.keyframes[0].reason, Reason::Click);
    assert_eq!(shown.keyframes[1].reason, Reason::Mark);
    assert_eq!(shown.keyframes[1].marks, 0..2);
    assert_eq!(shown.keyframes[1].file(), "frames/frame-02.png");
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
    assert_eq!(shown.keyframes.len(), 7);
    assert_eq!(marked(&shown), [0..1, 1..2, 2..3, 3..4, 4..6, 6..7, 7..8]);
    assert_eq!(shown.keyframes[4].at.as_millis(), 11_000);

    // On one unchanged screen, marks group until they span more than twenty seconds.
    let still = samples(1_000, 50_000, |_| 128);
    let strokes = [2_000, 9_000, 20_000, 33_000, 40_000].map(dab).to_vec();
    let shown = select(
        &recording(strokes, Vec::new(), Narration::default()),
        &still,
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
    let shown = select(&Recording::default(), &frames).unwrap();
    assert_eq!(times(&shown), [1_500]);
    assert_eq!(shown.keyframes[0].reason, Reason::SceneChange);
}

#[test]
fn a_click_shows_the_screen_once_its_change_settles() {
    // The pane a click opens fades in over two samples.
    let fading = samples(200, 3_000, |at| match at {
        ..1_200 => 40,
        1_200 => 120,
        _ => 200,
    });
    let clicked = recording(Vec::new(), vec![click(1_000)], Narration::default());
    let shown = select(&clicked, &fading).unwrap();
    assert_eq!(times(&shown), [1_400]);
    assert_eq!(reported(&shown), [Some(click(1_000))]);

    // A dialog lighting a corner of a dark screen is a stronger scene change than the click
    // alone scores, and its keyframe still reports the click.
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
    assert_eq!(shown.keyframes[0].reason, Reason::Click);
    assert_eq!(reported(&shown), [Some(click(1_000))]);
}

#[test]
fn a_click_waits_only_for_a_change_it_could_have_started() {
    // A click that changes nothing, then one that opens a pane: the first keeps its own screen.
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

    // A change a second after a click is something a Show does not record, such as a scroll,
    // whether the click changed nothing or only a little, as ticking a checkbox does.
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

    // A click as a video starts shows the screen just after it, not where the video stops.
    let playing = samples(200, 10_000, |at| match at {
        1_200..8_000 if at / 200 % 2 == 1 => 200,
        ..8_000 => 40,
        _ => 120,
    });
    let shown = select(&clicked, &playing).unwrap();
    assert_eq!(times(&shown), [1_200, 8_000]);
    assert_eq!(reported(&shown), [Some(click(1_000)), None]);

    // A change already under way before the click is not its result; the pane it opens next is.
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
    // A click opens a pane that fades in, and another 600 ms later opens a second pane.
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

    // A click opens a menu that fades in, and another 800 ms later changes nothing.
    let menu = samples(200, 3_000, |at| match at {
        ..1_200 => 40,
        1_200 => 120,
        _ => 200,
    });
    let clicks = vec![click(1_000), click(1_800)];
    let shown = select(&recording(Vec::new(), clicks, Narration::default()), &menu).unwrap();
    assert_eq!(times(&shown), [1_400, 2_000]);
    assert_eq!(reported(&shown), [Some(click(1_000)), Some(click(1_800))]);
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
    assert_eq!(shown.keyframes[0].event, "clicked (x 0.50, y 0.50)");

    // Clicks between two frames of a thinned recording share the one frame that shows them.
    let sparse = samples(2_000, 4_000, |_| 128);
    let actions = vec![click(1_000), click(1_800)];
    let shown = select(
        &recording(Vec::new(), actions, Narration::default()),
        &sparse,
    )
    .unwrap();
    assert_eq!(times(&shown), [2_000]);
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
    let shown = select(
        &recording(vec![dab(1_000), dab(4_000)], Vec::new(), narration),
        &frames,
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
    let shown = select(&recording(Vec::new(), Vec::new(), narration), &still).unwrap();
    assert_eq!(shown.keyframes.len(), 1);
    assert_eq!(shown.keyframes[0].reason, Reason::Cue);
    assert_eq!(said(&shown), [Some("Look at this. Now it's stuck.")]);

    // A cue over a screen a scene change already settled on adds nothing.
    let changed = samples(250, 5_000, |at| if at < 1_000 { 40 } else { 90 });
    let narration = words(&[(3_500, "Look"), (3_700, "here.")]);
    let shown = select(&recording(Vec::new(), Vec::new(), narration), &changed).unwrap();
    assert_eq!(shown.keyframes.len(), 1);
    assert_eq!(shown.keyframes[0].reason, Reason::SceneChange);
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
    assert_eq!(shown.keyframes[1].reason, Reason::Cue);
    assert_eq!(
        shown.keyframes[1].event,
        "said “watch” as the screen changed"
    );
    assert_eq!(
        said(&shown),
        [Some("Here's the list."), Some("Watch it now.")]
    );
}

#[test]
fn a_spoken_cue_never_waits_for_a_screen_that_keeps_changing() {
    // "Look" as a ten-second animation starts: the cue shows the screen as it was said, and the
    // animation's end gets a keyframe of its own.
    let animating = samples(250, 20_000, |at| match at {
        ..2_000 => 40,
        2_000..12_000 if at / 250 % 2 == 0 => 120,
        2_000..12_000 => 200,
        _ => 250,
    });
    let looked = recording(Vec::new(), Vec::new(), words(&[(2_000, "Look")]));
    let shown = select(&looked, &animating).unwrap();
    assert_eq!(times(&shown), [2_250, 12_000]);
    assert_eq!(shown.keyframes[0].event, "said “look”");
    assert_eq!(shown.keyframes[1].reason, Reason::SceneChange);
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

    // Five strokes a mark: two marks fit a group, a third would carry too much ink.
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
fn same_screen_merges_stay_balanced() {
    let still = samples(250, 15_000, |_| 128);
    let strokes = every(1_000, 2_000, 6).map(dab).collect();
    let clicks = every(2_000, 2_000, 5).map(click).collect();
    let shown =
        select_within(&recording(strokes, clicks, Narration::default()), &still, 3).unwrap();
    assert_eq!(marked(&shown), [0..2, 2..4, 4..6]);
}

#[test]
fn only_a_different_screen_cuts_into_a_drawing() {
    let still = samples(250, 10_000, |_| 128);
    let narration = words(&[(4_000, "look")]);
    let shown = select(
        &recording(vec![dab(2_000), dab(6_000)], Vec::new(), narration),
        &still,
    )
    .unwrap();
    assert_eq!(shown.keyframes.len(), 1);
    assert_eq!(shown.keyframes[0].reason, Reason::Mark);

    // Nor does a click between the strokes of one mark, far from the frame that shows it: the
    // mark's keyframe reports it.
    let strokes = || vec![dab(2_000), dab(3_100), dab(4_200)];
    let clicked = recording(strokes(), vec![click(2_500)], Narration::default());
    let shown = select(&clicked, &still).unwrap();
    assert_eq!(times(&shown), [4_250]);
    assert_eq!(shown.keyframes[0].marks, 0..1);
    assert_eq!(shown.keyframes[0].action, Some(click(2_500)));
    assert!(
        shown.keyframes[0]
            .event
            .ends_with("; clicked (x 0.50, y 0.50)"),
        "{}",
        shown.keyframes[0].event
    );
    // Of two such clicks, it reports the later.
    let twice = recording(
        strokes(),
        vec![click(2_500), click(3_500)],
        Narration::default(),
    );
    let shown = select(&twice, &still).unwrap();
    assert_eq!(times(&shown), [4_250]);
    assert_eq!(shown.keyframes[0].action, Some(click(3_500)));
    // So does a click made just before the drawing whose result shows while it is drawn.
    let before = recording(strokes(), vec![click(1_800)], Narration::default());
    let shown = select(&before, &still).unwrap();
    assert_eq!(times(&shown), [4_250]);
    assert_eq!(shown.keyframes[0].action, Some(click(1_800)));
    // And so does a click right after it, too close to the mark's frame for one of its own.
    let after = recording(strokes(), vec![click(4_300)], Narration::default());
    let shown = select(&after, &still).unwrap();
    assert_eq!(times(&shown), [4_250]);
    assert_eq!(shown.keyframes[0].action, Some(click(4_300)));
    // Unless that click opens a different screen, which then gets a keyframe of its own.
    let opened = samples(250, 10_000, |at| if at >= 4_500 { 255 } else { 128 });
    let shown = select(&after, &opened).unwrap();
    assert_eq!(times(&shown), [4_250, 4_500]);
    assert_eq!(shown.keyframes[0].action, None);
    assert_eq!(shown.keyframes[1].action, Some(click(4_300)));
    // With no room for that keyframe, the mark's keyframe still reports the click.
    let shown = select_within(&after, &opened, 1).unwrap();
    assert_eq!(times(&shown), [4_250]);
    assert_eq!(shown.keyframes[0].action, Some(click(4_300)));

    // A click whose result the mark's frame no longer shows keeps a keyframe of its own.
    let flash = samples(250, 10_000, |at| {
        if (2_600..3_400).contains(&at) {
            255
        } else {
            128
        }
    });
    let shown = select(&clicked, &flash).unwrap();
    assert_eq!(times(&shown), [2_750, 4_250]);
    assert_eq!(shown.keyframes[1].action, None);
    // With no room for it, the mark reports that click although its result was another screen.
    let shown = select_within(&clicked, &flash, 1).unwrap();
    assert_eq!(times(&shown), [4_250]);
    assert_eq!(shown.keyframes[0].action, Some(click(2_500)));
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

    // Both marks cover this click's result too, but the later one is nearer.
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
    // Two minutes of steady talk. Small changes at 0:30 and 1:00, then bigger ones near the
    // end that outrank them: all settled views still get represented.
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
    let shown = select(&recording(Vec::new(), Vec::new(), words(&talk)), &still).unwrap();
    assert_eq!(shown.keyframes.len(), 1);
}

#[test]
fn each_event_says_why_its_frame_was_chosen() {
    let event = |shown: Option<Selection>| shown.unwrap().keyframes[0].event.clone();
    let still = samples(250, 10_000, |_| 128);
    let drawn = recording(
        vec![dab(2_000), dab(4_000)],
        Vec::new(),
        Narration::default(),
    );
    assert_eq!(
        event(select(&drawn, &still)),
        "① tapped x 0.50, y 0.50; ② tapped x 0.50, y 0.50"
    );
    let looked = recording(Vec::new(), Vec::new(), words(&[(3_000, "Look")]));
    assert_eq!(event(select(&looked, &still)), "said “look”");
    let changed = [
        sample(0, 0),
        sample(1_000, 80),
        sample(1_250, 160),
        sample(1_500, 255),
    ];
    assert_eq!(
        event(select(&Recording::default(), &changed)),
        "scene changed"
    );
    let quiet = [sample(0, 0), sample(1_000, 0), sample(2_000, 0)];
    assert_eq!(
        event(select(&Recording::default(), &quiet)),
        "the screen midway through"
    );
}

#[test]
fn a_quiet_show_falls_back_to_its_middle() {
    let frames = [sample(0, 0), sample(1_000, 0), sample(2_000, 0)];
    let shown = select(&Recording::default(), &frames).unwrap();
    assert_eq!(times(&shown), [1_000]);
    assert_eq!(shown.keyframes[0].reason, Reason::Middle);
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
        let keyframes = &shown.keyframes;
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
        let mut said: Vec<&str> = keyframes
            .iter()
            .filter_map(|keyframe| keyframe.said.as_deref())
            .flat_map(str::split_whitespace)
            .collect();
        let mut expected: Vec<&str> = narration.iter().map(|(_, word)| *word).collect();
        said.sort_unstable();
        expected.sort_unstable();
        prop_assert_eq!(said, expected);
    }
}

#[test]
fn debugging_a_keyframe_never_prints_what_was_said() {
    let shown = |event: &str, said: &str| {
        let keyframe = Keyframe {
            index: 1,
            at: Moment::from_millis(0),
            reason: Reason::Middle,
            event: event.to_owned(),
            said: Some(said.to_owned()),
            marks: 0..0,
            action: None,
        };
        format!("{keyframe:?}")
    };
    let baseline = shown("said “look”", "alpha");
    assert_eq!(shown("said “look”", "omega"), baseline);
    assert_eq!(shown("said “here”", "alpha"), baseline);
}
