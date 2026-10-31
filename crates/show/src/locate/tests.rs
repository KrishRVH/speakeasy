//! Locations remain explicit and source sampling never invents coverage.

use proptest::prelude::*;
use serde_json::json;

use super::*;
use crate::{Image, SourceFrame};

fn manifest() -> Manifest {
    Manifest::parse(&serde_json::to_vec(&json!({
        "version": 2, "duration": 4000, "surface": {"width": 800, "height": 600},
        "strokes": [], "pins": [], "actions": [],
        "narration": {"words": [
            {"span": [100,200], "text": "SAVE, the"}, {"span": [250,400], "text": "café!"},
            {"span": [2000,2100], "text": "save"}, {"span": [2100,2300], "text": "the café"}
        ]},
        "marks": [{"number": 1, "shape": "pin", "point": [0.5,0.5], "bounds": [0.5,0.5,0.5,0.5], "span": [150,150], "strokes": [0,0]}],
        "keyframes": [{"index": 1, "at": 150, "reason": "mark", "event": "private", "marks": [0,1]}]
    })).unwrap()).unwrap()
}

#[test]
fn phrase_queries_return_every_location_without_private_text() {
    let found = manifest().locate(Focus::Phrase("save THE café")).unwrap();
    assert_eq!(
        found,
        vec![
            Span::from([Moment::from_millis(100), Moment::from_millis(400)]),
            Span::from([Moment::from_millis(2000), Moment::from_millis(2300)])
        ]
    );
    assert_eq!(manifest().locate(Focus::Phrase("ave")).unwrap(), []);
    assert!(manifest().locate(Focus::Phrase("---")).is_err());
    assert!(!format!("{:?}", Focus::Phrase("private")).contains("private"));
    assert!(manifest().locate(Focus::Mark(2)).is_err());
    assert!(
        manifest()
            .locate(Focus::At(Moment::from_millis(4001)))
            .is_err()
    );
    let mut manifest = manifest();
    manifest.recording.narration = Narration::Text("private".into());
    assert!(
        manifest
            .locate(Focus::Phrase("private"))
            .unwrap_err()
            .to_string()
            .contains("no word timings")
    );
    manifest.recording.narration = Narration::default();
    assert!(manifest.locate(Focus::Phrase("private")).is_err());
}

#[test]
fn window_edges_and_carry_in_expose_missing_coverage() {
    let at = Moment::from_millis;
    let span = window(Span::new(at(100), at(300)), Duration::from_secs(1), at(800));
    assert_eq!(span, Span::new(at(0), at(800)));
    let samples = pick(
        &[at(100), at(900), at(3500)],
        Span::new(at(1000), at(4000)),
        at(1800),
        2,
    );
    assert_eq!(samples.times, [at(900), at(3500)]);
    assert_eq!(samples.carried_in, Some(at(900)));
    assert_eq!(samples.longest_gap, Duration::from_millis(2500));
    assert_eq!(
        pick(&[at(2000)], Span::new(at(0), at(1000)), at(100), 6).times,
        []
    );
    let samples = pick(
        &[at(900), at(1000), at(1500)],
        Span::new(at(1000), at(2000)),
        at(1200),
        2,
    );
    assert_eq!(samples.carried_in, None);
    assert_eq!(samples.times, [at(1000), at(1500)]);
}

proptest! {
    #[test]
    fn selection_is_bounded_distinct_and_keeps_the_anchor(
        mut millis in prop::collection::vec(0_u64..300_000, 0..100),
        start in 0_u64..300_000, end in 0_u64..300_000, most in 1_usize..13,
    ) {
        millis.sort_unstable(); millis.dedup();
        let moments: Vec<_> = millis.into_iter().map(Moment::from_millis).collect();
        let window = Span::new(Moment::from_millis(start), Moment::from_millis(end));
        let anchor = window.start();
        let samples = pick(&moments, window, anchor, most);
        prop_assert!(samples.times.len() <= most);
        prop_assert!(samples.times.windows(2).all(|pair| pair[0] < pair[1]));
        prop_assert!(samples.times.iter().all(|at| moments.contains(at) && *at <= window.end()));
        if let Some(at) = moments.iter().rev().find(|at| **at <= anchor) {
            prop_assert!(samples.times.contains(at));
        }
        prop_assert!(samples.longest_gap <= window.end().since(window.start()));
    }
}

#[test]
fn cropped_evidence_preserves_pixels_times_and_bounded_loads() {
    let typeface = crate::typeface::tests::typeface();
    let moments = [Moment::from_millis(100), Moment::from_millis(2500)];
    let mut loaded = 0;
    let evidence = Evidence::compose::<crate::Error>(
        &moments,
        Some(manifest().marks()[0].bounds),
        |at| {
            loaded += 1;
            Ok(SourceFrame {
                at,
                image: Image::filled(100, 100, [1, 2, 3])?,
            })
        },
        &typeface,
    )
    .unwrap();
    assert_eq!(loaded, 2);
    assert_eq!(
        evidence
            .cells
            .iter()
            .map(|cell| cell.sampled_ms)
            .collect::<Vec<_>>(),
        [100, 2500]
    );
    assert_eq!(
        evidence.cells[0].crop_px,
        Some(PixelRect {
            left: 40,
            top: 40,
            width: 21,
            height: 21
        })
    );
    let image = Image::filled(3, 2, [11, 22, 33]).unwrap();
    assert_eq!(
        image.cropped(1, 0, 2, 2).unwrap(),
        Image::filled(2, 2, [11, 22, 33]).unwrap()
    );
    assert!(image.cropped(2, 0, 2, 2).is_err());
    assert!(
        Evidence::compose::<crate::Error>(
            &moments,
            Some(manifest().marks()[0].bounds),
            |at| {
                Ok(SourceFrame {
                    at,
                    image: Image::filled(if at == moments[0] { 100 } else { 200 }, 100, [1, 2, 3])?,
                })
            },
            &typeface
        )
        .is_err()
    );
}
