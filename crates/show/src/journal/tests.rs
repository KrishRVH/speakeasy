use proptest::prelude::*;

use super::*;

fn frame(width: u32, height: u32, paint: impl Fn(usize, usize) -> [u8; 4]) -> Image {
    let pixels: Vec<u8> = (0..convert::count(height))
        .flat_map(|y| (0..convert::count(width)).map(move |x| (x, y)))
        .flat_map(|(x, y)| paint(x, y))
        .collect();
    Image::from_rgba(width, height, pixels).unwrap()
}

fn admitted(journal: &mut Journal) -> Admission<'_> {
    journal
        .admit()
        .expect("A journal without a budget admits every arrival")
}

fn quadrants(shade: [u8; 4], small: bool) -> Image {
    let (width, height) = if small { (70, 70) } else { (150, 90) };
    frame(width, height, move |x, y| {
        let rows = shade.as_chunks::<2>().0;
        let value = rows[usize::from(y >= 64)][usize::from(x >= 64)];
        [value, value, value, 255]
    })
}

fn small() -> impl Strategy<Value = bool> {
    (0_usize..3).prop_map(|draw| draw == 2)
}

#[test]
fn a_still_screen_costs_almost_nothing_and_changes_cost_their_tiles() {
    let text = |x: usize, y: usize| {
        if (x / 3 + y / 5).is_multiple_of(4) {
            [20, 20, 20, 255]
        } else {
            [240, 240, 240, 255]
        }
    };
    let mut journal = Journal::new();
    admitted(&mut journal).record(Moment::START, frame(640, 360, text));
    let first = journal.bytes();
    for millis in (200..=2_000).step_by(200) {
        admitted(&mut journal).record(Moment::from_millis(millis), frame(640, 360, text));
    }
    assert_eq!(
        journal.bytes(),
        first,
        "An unchanged screen stores nothing more"
    );

    let caret = |x: usize, y: usize| {
        if (100..102).contains(&x) && (40..60).contains(&y) {
            [0, 0, 0, 255]
        } else {
            text(x, y)
        }
    };
    admitted(&mut journal).record(Moment::from_millis(2_200), frame(640, 360, caret));
    let added = journal.bytes() - first;
    assert!(added > 0 && added < first / 20, "{added} of {first}");
}

#[test]
fn any_recorded_moment_is_rebuilt_exactly() {
    let gradient = |shift: usize| {
        move |x: usize, y: usize| {
            let value = u8::try_from((x + y + shift) % 256).unwrap();
            [value, value / 2, value.wrapping_neg(), 255]
        }
    };
    let mut journal = Journal::new();
    let frames: Vec<Image> = [0, 7, 7, 30]
        .into_iter()
        .map(|shift| frame(130, 70, gradient(shift)))
        .collect();
    for (index, image) in frames.iter().enumerate() {
        let moment = Moment::from_millis(u64::try_from(index).unwrap() * 100);
        admitted(&mut journal).record(moment, image.clone());
    }
    for (index, image) in frames.iter().enumerate() {
        let moment = Moment::from_millis(u64::try_from(index).unwrap() * 100 + 50);
        assert_eq!(
            journal.frame_at(moment).as_ref(),
            Some(image),
            "Frame {index}"
        );
    }
    assert_eq!(journal.frame_at(Moment::START).as_ref(), frames.first());

    let resized = frame(64, 64, gradient(3));
    admitted(&mut journal).record(Moment::from_millis(1_000), resized.clone());
    assert_eq!(
        journal.frame_at(Moment::from_millis(1_000)).as_ref(),
        Some(&resized)
    );
    assert_eq!(
        journal.frame_at(Moment::from_millis(150)).as_ref(),
        frames.get(1)
    );

    let whole = {
        let mut alone = Journal::new();
        admitted(&mut alone).record(Moment::START, resized.clone());
        alone.bytes()
    };
    let mut journal = journal.sealed();
    let held = journal.bytes();
    admitted(&mut journal).record(Moment::from_millis(1_100), resized.clone());
    assert_eq!(
        journal.bytes() - held,
        whole,
        "A sealed journal keeps no frame to compare the next against"
    );
    let after = frame(64, 64, gradient(9));
    admitted(&mut journal).record(Moment::from_millis(1_200), after.clone());
    assert_eq!(
        journal.frame_at(Moment::from_millis(1_100)).as_ref(),
        Some(&resized)
    );
    assert_eq!(journal.frame_at(Moment::from_millis(1_200)), Some(after));
    assert_eq!(journal.frame_at(Moment::from_millis(1_000)), Some(resized));
    assert_eq!(
        journal.frame_at(Moment::from_millis(350)).as_ref(),
        frames.get(3)
    );
    assert_eq!(Journal::new().frame_at(Moment::START), None);
}

#[test]
fn each_thinning_halves_how_often_arrivals_are_admitted() {
    let mut journal = Journal::with_budget(0);
    let mut admitted = Vec::new();
    for arrival in 0_u8..9 {
        if let Some(admission) = journal.admit() {
            admitted.push(arrival);
            let moment = Moment::from_millis(u64::from(arrival) * 200);
            admission.record(moment, quadrants([arrival; 4], true));
        }
    }
    assert_eq!(admitted, [0, 1, 2, 4, 8]);
}

#[test]
fn debugging_a_journal_never_prints_its_frames() {
    let mut journal = Journal::new();
    admitted(&mut journal).record(Moment::START, quadrants([0; 4], true));
    let one = format!("{journal:?}");
    for shade in 1_u8..20 {
        let moment = Moment::from_millis(u64::from(shade) * 200);
        admitted(&mut journal).record(moment, quadrants([shade, 0, shade, 0], false));
    }
    let many = format!("{journal:?}");
    assert!(many.len() <= one.len() + 8, "{one}\n{many}");
}

proptest! {
    #[test]
    fn thinning_keeps_every_other_frame_exact(
        changes in prop::collection::vec((0_u8..4, small()), 1..24),
        rounds in 1_usize..4,
    ) {
        let mut journal = Journal::new();
        let mut recorded = Vec::new();
        let mut shade = [0_u8; 4];
        for (index, (quadrant, small)) in changes.into_iter().enumerate() {
            if let Some(value) = shade.get_mut(usize::from(quadrant)) {
                *value = value.wrapping_add(40);
            }
            let image = quadrants(shade, small);
            let millis = u64::try_from(index).unwrap().checked_mul(100).unwrap();
            let moment = Moment::from_millis(millis);
            admitted(&mut journal).record(moment, image.clone());
            recorded.push((moment, image));
        }
        for _ in 0..rounds {
            let before = journal.bytes();
            journal.thin();
            let count = recorded.len();
            recorded = recorded
                .into_iter()
                .enumerate()
                .filter(|(index, _)| {
                    index.is_multiple_of(2) || index.checked_add(1).unwrap() == count
                })
                .map(|(_, frame)| frame)
                .collect();
            prop_assert!(journal.bytes() <= before);
        }
        let moments: Vec<Moment> = recorded.iter().map(|(moment, _)| *moment).collect();
        let held: Vec<Moment> = journal.samples().iter().map(|sample| sample.at).collect();
        prop_assert_eq!(held, moments);
        for (moment, image) in &recorded {
            prop_assert_eq!(journal.frame_at(*moment), Some(image.clone()));
        }
    }

    #[test]
    fn a_budget_thins_frames_and_their_samples_together(
        arrivals in prop::collection::vec((-300_i64..=300, 0_u8..4, small()), 1..48),
    ) {
        const BUDGET: usize = 1_500;
        let mut journal = Journal::with_budget(BUDGET);
        let (mut shade, mut millis) = ([0_u8; 4], 0_u64);
        let (mut first, mut last) = (None, None);
        for (delta, quadrant, small) in arrivals {
            millis = millis.saturating_add_signed(delta);
            if let Some(value) = shade.get_mut(usize::from(quadrant)) {
                *value = value.wrapping_add(40);
            }
            let Some(admission) = journal.admit() else {
                continue;
            };
            let moment = Moment::from_millis(millis);
            let image = quadrants(shade, small);
            first.get_or_insert_with(|| (moment, image.clone()));
            last = Some(image.clone());
            admission.record(moment, image);
            let held = journal.samples().len();
            prop_assert!(
                journal.bytes() <= BUDGET || held <= 2,
                "{} bytes in {held} frames",
                journal.bytes()
            );
        }
        let samples = journal.samples();
        prop_assert!(samples.is_sorted_by(|earlier, later| earlier.at < later.at));
        for sample in samples {
            let shown = journal.frame_at(sample.at).unwrap();
            prop_assert!(
                Signature::of(&shown) == sample.signature,
                "At {}",
                sample.at.as_millis()
            );
        }
        let (first_at, first_image) = first.unwrap();
        prop_assert_eq!(samples.first().map(|sample| sample.at), Some(first_at));
        prop_assert_eq!(journal.frame_at(first_at), Some(first_image));
        prop_assert_eq!(journal.frame_at(samples.last().unwrap().at), last);
    }

    #[test]
    fn rebuilding_is_lossless_for_any_size(
        width in 1_u32..150,
        height in 1_u32..150,
        seed in 0_u8..=255,
    ) {
        let noise = move |x: usize, y: usize| {
            let mixed = x
                .checked_mul(31)
                .unwrap()
                .checked_add(y.checked_mul(17).unwrap())
                .unwrap()
                .checked_add(usize::from(seed))
                .unwrap();
            let value = u8::try_from(mixed % 251).unwrap();
            [value, value.wrapping_mul(3), seed, 255]
        };
        let image = frame(width, height, noise);
        let mut journal = Journal::new();
        admitted(&mut journal).record(Moment::from_millis(5), image.clone());
        prop_assert_eq!(journal.frame_at(Moment::from_millis(5)), Some(image));
    }
}
