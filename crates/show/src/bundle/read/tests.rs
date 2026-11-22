use serde_json::{Value, json};

use super::*;

fn fixture() -> Value {
    json!({
        "version": 3,
        "duration": 3000,
        "surface": {"width": 800, "height": 600},
        "strokes": [[[0.2, 0.3, 100], [0.4, 0.3, 200]]],
        "pins": [], "actions": [],
        "spaces":{"strokes":[{"generation":1,"part":[0,0,1,1]}],"pins":[],"actions":[]},
        "narration": {"words": [{"span": [100, 200], "text": "Look here."}]},
        "marks": [{"number": 1, "shape": "underline", "strokes": [0, 1],
            "span": [100, 200], "bounds": [0.2, 0.3, 0.4, 0.3]}],
        "keyframes": [
            {"index": 1, "at": 200, "reason": "mark", "event": "drew a line",
                "said": "Look here.", "marks": [0, 1]},
            {"index": 2, "at": 2500, "reason": "sceneChange", "event": "changed", "marks": [0, 0],
                "crop": [0.1, 0.2, 0.6, 0.7]}
        ]
    })
}

fn parse(value: &Value) -> Result<Manifest, Unreadable> {
    Manifest::parse(&serde_json::to_vec(value).unwrap())
}

#[test]
fn unsupported_formats_are_rejected_before_their_contents() {
    let error = parse(&json!({"version": 4, "keyframes": "changed format"})).unwrap_err();
    assert!(error.to_string().contains("update Speakeasy"));
    assert!(matches!(error, Unreadable::Unsupported(4)));
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
fn a_malformed_manifest_reports_the_decoder_error() {
    let error = Manifest::parse(br#"{"version":"three"}"#).unwrap_err();
    assert!(error.to_string().contains("invalid type"));
}
