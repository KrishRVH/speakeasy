use serde_json::json;

use super::*;
use crate::{Action, InkPoint, Moment, Narration, Reason, Shape, Size, Span, Stroke, Word};

fn at(millis: u64) -> Moment {
    Moment::from_millis(millis)
}

#[test]
fn show_json_keeps_its_wire_format() {
    let click = Action::Click {
        at: at(400),
        point: Point::new(0.5, 0.25),
    };
    let word = |start: u64, text: &str| Word {
        span: Span::new(at(start), at(start + 200)),
        text: text.to_owned(),
    };
    let ink = |x: f32, millis: u64| InkPoint {
        point: Point::new(x, 0.5),
        at: at(millis),
    };
    let manifest = Manifest {
        version: Manifest::VERSION,
        recording: Recording {
            duration: at(2_000),
            surface: Size {
                width: 800.0,
                height: 600.0,
            },
            strokes: vec![Stroke::new(vec![ink(0.25, 100), ink(0.75, 300)]).unwrap()],
            actions: vec![
                click,
                Action::Drag {
                    span: Span::new(at(600), at(900)),
                    from: Point::new(0.25, 0.75),
                    to: Point::new(0.75, 0.75),
                },
            ],
            narration: Narration::Words(vec![word(100, "Look"), word(300, "here.")]),
        },
        marks: vec![Mark {
            shape: Shape::Line {
                from: Point::new(0.25, 0.5),
                to: Point::new(0.75, 0.5),
            },
            strokes: 0..1,
            span: Span::new(at(100), at(300)),
            bounds: Rect {
                left: 0.25,
                top: 0.5,
                right: 0.75,
                bottom: 0.5,
            },
        }],
        keyframes: vec![
            Keyframe {
                index: 1,
                at: at(300),
                reason: Reason::Mark,
                event: "drew a line".to_owned(),
                said: Some("Look here.".to_owned()),
                marks: 0..1,
                action: None,
            },
            Keyframe {
                index: 2,
                at: at(1_000),
                reason: Reason::Click,
                event: "clicked".to_owned(),
                said: None,
                marks: 0..0,
                action: Some(click),
            },
            Keyframe {
                index: 3,
                at: at(1_500),
                reason: Reason::SceneChange,
                event: "scene changed".to_owned(),
                said: None,
                marks: 0..0,
                action: None,
            },
        ],
    };
    let click = json!({"kind": "click", "at": 400, "point": [0.5, 0.25]});
    let golden = json!({
        "version": 1,
        "duration": 2000,
        "surface": {"width": 800.0, "height": 600.0},
        "strokes": [[[0.25, 0.5, 100], [0.75, 0.5, 300]]],
        "actions": [
            click,
            {"kind": "drag", "span": [600, 900], "from": [0.25, 0.75], "to": [0.75, 0.75]},
        ],
        "narration": {"words": [
            {"span": [100, 300], "text": "Look"},
            {"span": [300, 500], "text": "here."},
        ]},
        "marks": [{
            "shape": "line",
            "from": [0.25, 0.5],
            "to": [0.75, 0.5],
            "strokes": [0, 1],
            "span": [100, 300],
            "bounds": [0.25, 0.5, 0.75, 0.5],
        }],
        "keyframes": [
            {
                "index": 1,
                "at": 300,
                "reason": "mark",
                "event": "drew a line",
                "said": "Look here.",
                "marks": [0, 1],
            },
            {
                "index": 2,
                "at": 1000,
                "reason": "click",
                "event": "clicked",
                "marks": [0, 0],
                "action": click,
            },
            {"index": 3, "at": 1500, "reason": "sceneChange", "event": "scene changed", "marks": [0, 0]},
        ],
    });
    let changed =
        "show.json changed; bump Manifest::VERSION if an existing reader would misread it";
    assert_eq!(
        serde_json::to_value(&manifest).unwrap(),
        golden,
        "{changed}"
    );
    assert_eq!(
        serde_json::from_value::<Manifest>(golden).unwrap(),
        manifest,
        "{changed}"
    );
}

#[test]
fn debugging_a_file_never_prints_its_contents() {
    let file = File {
        name: "frames/frame-01.png".to_owned(),
        contents: vec![0x5A; 4_096],
    };
    let shown = format!("{file:?}");
    assert!(shown.contains("frame-01") && shown.len() < 80, "{shown}");
}

#[test]
fn a_provider_failure_stops_before_loading_the_next_page() {
    let selection = Selection {
        marks: Vec::new(),
        keyframes: (1..=13)
            .map(|index| Keyframe {
                index,
                at: at(index as u64),
                reason: Reason::Middle,
                event: String::new(),
                said: None,
                marks: 0..0,
                action: None,
            })
            .collect(),
    };
    let result = compose(
        Recording::default(),
        selection,
        |moment| {
            // Thirteen keyframes share three sheets as #1–#5, #6–#9, and #10–#13.
            assert!(
                moment.as_millis() <= 9,
                "loaded the sheet after a failed sheet"
            );
            if moment.as_millis() == 7 {
                Err(Error::unsampled())
            } else {
                Image::filled(160, 100, [90; 3])
            }
        },
        &crate::typeface::tests::typeface(),
    );
    assert_eq!(
        result.unwrap_err().to_string(),
        Error::unsampled().to_string()
    );
}

#[test]
fn contact_sheets_cover_every_frame_once_in_readable_pages() {
    use crate::{Sample, select, signature::tests::signature, typeface::tests::typeface};

    for (count, sizes) in [
        (6_usize, &[6][..]),
        (7, &[4, 3]),
        (12, &[6, 6]),
        (13, &[5, 4, 4]),
    ] {
        let recording = Recording {
            duration: at(30_000),
            actions: (0..count)
                .map(|index| Action::Click {
                    at: at(index as u64 * 2_000),
                    point: Point::new(0.1, 0.1),
                })
                .collect(),
            ..Recording::default()
        };
        let samples: Vec<Sample> = (0..count)
            .map(|index| Sample {
                at: at(index as u64 * 2_000 + 250),
                signature: signature(128),
            })
            .collect();
        let selection = select(&recording, &samples).unwrap();
        assert_eq!(selection.keyframes().len(), count);
        let colors: Vec<[u8; 3]> = (0..count)
            .map(|index| [40 + u8::try_from(index).unwrap() * 10, 90, 150])
            .collect();
        let bundle = compose(
            recording,
            selection,
            |moment| {
                let index = usize::try_from(moment.as_millis() / 2_000).unwrap();
                Image::filled(320, 200, colors[index])
            },
            &typeface(),
        )
        .unwrap();
        let directory = Path::new("show");
        let summary = bundle.summary(directory);
        let frame_files: Vec<_> = bundle
            .manifest()
            .keyframes
            .iter()
            .map(Keyframe::file)
            .collect();
        let files = bundle.into_files(directory).unwrap();
        let sheets: Vec<&File> = files
            .iter()
            .filter(|file| file.name.starts_with("contact-sheet"))
            .collect();
        assert_eq!(sheets.len(), sizes.len());
        let mut first = 0;
        for (file, &size) in sheets.iter().zip(sizes) {
            let page = first..first + size;
            first += size;
            assert!(summary.contains(&directory.join(&file.name).display().to_string()));
            let image = Image::decode_png(&file.contents);
            assert!(image.width().max(image.height()) <= 1_600);
            for (index, color) in colors.iter().enumerate() {
                let present = image
                    .as_ref()
                    .data()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[..3] == *color);
                assert_eq!(
                    present,
                    page.contains(&index),
                    "frame {} on {} of {count} frames",
                    index + 1,
                    file.name
                );
            }
        }
        for name in frame_files {
            assert!(files.iter().any(|file| file.name == name));
        }
    }
}
