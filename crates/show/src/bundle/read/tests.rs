//! Broken references and private decoder failures are rejected before inspection.

use proptest::prelude::*;
use serde_json::{Value, json};

use super::*;

fn fixture() -> Value {
    json!({
        "version": 2,
        "duration": 3000,
        "surface": {"width": 800, "height": 600},
        "strokes": [[[0.2, 0.3, 100], [0.4, 0.3, 200]]],
        "pins": [], "actions": [],
        "narration": {"words": [{"span": [100, 200], "text": "PRIVATE_SPEECH"}]},
        "marks": [{"number": 1, "shape": "underline", "strokes": [0, 1],
            "span": [100, 200], "bounds": [0.2, 0.3, 0.4, 0.3]}],
        "keyframes": [
            {"index": 1, "at": 200, "reason": "mark", "event": "PRIVATE_EVENT",
                "said": "PRIVATE_SPEECH", "marks": [0, 1]},
            {"index": 2, "at": 2500, "reason": "sceneChange", "event": "changed", "marks": [0, 0]}
        ]
    })
}

fn parse(value: &Value) -> Result<Manifest, Unreadable> {
    Manifest::parse(&serde_json::to_vec(value).unwrap())
}

#[test]
fn future_fields_are_compatible_but_future_formats_are_not_guessed() {
    let mut value = fixture();
    value["extra"] = json!({"anything": [1, 2]});
    let manifest = parse(&value).unwrap();
    assert_eq!(manifest.keyframes().len(), 2);
    assert_eq!(
        parse(&json!({"version": 3, "keyframes": "changed format"})),
        Err(Unreadable::Unsupported(3))
    );
    assert_eq!(
        parse(&json!({"version": 1})),
        Err(Unreadable::Unsupported(1))
    );
}

#[test]
fn broken_references_and_order_are_rejected() {
    let oversized: Vec<_> = (1..=MOST_KEYFRAMES.saturating_add(1))
        .map(|index| {
            let mut frame = fixture()["keyframes"][1].clone();
            frame["index"] = json!(index);
            frame
        })
        .collect();
    let faults = [
        ("/keyframes", json!([])),
        ("/keyframes", json!(oversized)),
        ("/keyframes/0/index", json!(0)),
        ("/keyframes/1/index", json!(1)),
        ("/keyframes/1/at", json!(100)),
        ("/keyframes/1/at", json!(3001)),
        ("/keyframes/0/marks", json!([0, 2])),
        ("/keyframes/0/marks", json!([2, 2])),
        ("/keyframes/0/reason", json!("click")),
        ("/marks/0/number", json!(2)),
        ("/marks/0/strokes", json!([0, 2])),
    ];
    for (pointer, fault) in faults {
        let mut value = fixture();
        *value.pointer_mut(pointer).unwrap() = fault;
        assert!(
            matches!(parse(&value), Err(Unreadable::Inconsistent(_))),
            "accepted {pointer}"
        );
    }
    let mut value = fixture();
    value["keyframes"][1]["reason"] = json!("mark");
    value["keyframes"][1]["marks"] = json!([0, 1]);
    assert!(matches!(parse(&value), Err(Unreadable::Inconsistent(_))));
}

#[test]
fn a_mark_without_a_saved_frame_remains_inspectable() {
    let mut value = fixture();
    value["keyframes"][0]["marks"] = json!([0, 0]);
    let manifest = parse(&value).unwrap();
    assert_eq!(manifest.marks().len(), 1);
    assert!(
        manifest
            .keyframes()
            .iter()
            .all(|frame| frame.marks.is_empty())
    );
}

#[test]
fn errors_and_debug_output_never_quote_manifest_content() {
    let manifest = parse(&fixture()).unwrap();
    assert!(!format!("{manifest:?}").contains("PRIVATE_"));
    let mut value = fixture();
    value["marks"][0]["shape"] = json!("PRIVATE_SCREEN_TEXT");
    let error = parse(&value).unwrap_err();
    assert!(matches!(error, Unreadable::Malformed { .. }));
    assert!(!format!("{error} {error:?}").contains("PRIVATE_"));
    assert!(std::error::Error::source(&error).is_none());
}

proptest! {
    #[test]
    fn sheets_and_paths_cover_each_saved_frame_once(count in 1..=MOST_KEYFRAMES) {
        let mut value = fixture();
        value["keyframes"] = Value::Array((1..=count).map(|index| json!({
            "index": index, "at": index, "reason": "sceneChange", "event": "", "marks": [0, 0]
        })).collect());
        let manifest = parse(&value).unwrap();
        let numbers: Vec<_> = manifest.sheets().flat_map(|(_, frames)| frames.iter().map(|frame| frame.index)).collect();
        prop_assert_eq!(numbers, (1..=count).collect::<Vec<_>>());
        let paths: Vec<_> = manifest.files().collect();
        prop_assert_eq!(paths.len(), count.saturating_add(count.div_ceil(6)).saturating_add(2));
        prop_assert!(paths.iter().all(|path| !path.contains("..")));
    }

    #[test]
    fn truncated_or_mutated_manifests_do_not_panic(cut in 0_usize..2048, at in 0_usize..2048, byte in any::<u8>()) {
        let mut bytes = serde_json::to_vec(&fixture()).unwrap();
        if let Some(target) = bytes.get_mut(at) { *target = byte; }
        bytes.truncate(cut);
        if let Ok(manifest) = Manifest::parse(&bytes) {
            prop_assert!(!manifest.keyframes().is_empty());
        }
    }
}
