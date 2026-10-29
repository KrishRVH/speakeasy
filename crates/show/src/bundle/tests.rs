//! Pins `show.json`, keeps file contents out of debug output, checks that the summary says how to
//! read the Show with every path set apart, and that contact sheets show every keyframe once and
//! stop loading at a failed frame.

use serde_json::json;

use super::*;
use crate::{
    Action, InkPoint, Pin, Point, Reason, Rect, Sample, Shape, Size, Span, Stroke,
    recording::tests::words, select, signature::tests::signature, typeface::tests::typeface,
};

#[test]
fn show_json_keeps_its_wire_format() {
    let click = Action::Click {
        at: Moment::from_millis(400),
        point: Point::new(0.5, 0.25),
    };
    let ink = |x: f32, millis: u64| InkPoint {
        point: Point::new(x, 0.5),
        at: Moment::from_millis(millis),
    };
    let manifest = Manifest {
        version: Manifest::VERSION,
        recording: Recording {
            duration: Moment::from_millis(2_000),
            surface: Size {
                width: 800.0,
                height: 600.0,
            },
            strokes: vec![Stroke::new(vec![ink(0.25, 100), ink(0.75, 300)]).unwrap()],
            pins: vec![Pin {
                at: Moment::from_millis(1_200),
                point: Point::new(0.5, 0.25),
            }],
            actions: vec![
                click,
                Action::Drag {
                    span: Span::new(Moment::from_millis(600), Moment::from_millis(900)),
                    from: Point::new(0.25, 0.75),
                    to: Point::new(0.75, 0.75),
                },
            ],
            narration: words(&[(100, "Look"), (300, "here.")]),
        },
        marks: vec![
            Mark {
                number: 1,
                shape: Shape::Line {
                    from: Point::new(0.25, 0.5),
                    to: Point::new(0.75, 0.5),
                },
                strokes: 0..1,
                span: Span::new(Moment::from_millis(100), Moment::from_millis(300)),
                bounds: Rect {
                    left: 0.25,
                    top: 0.5,
                    right: 0.75,
                    bottom: 0.5,
                },
            },
            Mark {
                number: 2,
                shape: Shape::Pin {
                    point: Point::new(0.5, 0.25),
                },
                strokes: 1..1,
                span: Span::at(Moment::from_millis(1_200)),
                bounds: Rect::from_origin(Point::new(0.5, 0.25), 0.0, 0.0),
            },
        ],
        keyframes: vec![
            Keyframe {
                index: 1,
                at: Moment::from_millis(300),
                reason: Reason::Mark,
                event: "drew a line".to_owned(),
                said: Some("Look here.".to_owned()),
                marks: 0..1,
                action: None,
            },
            Keyframe {
                index: 2,
                at: Moment::from_millis(1_000),
                reason: Reason::Click,
                event: "clicked".to_owned(),
                said: None,
                marks: 0..0,
                action: Some(click),
            },
            Keyframe {
                index: 3,
                at: Moment::from_millis(1_500),
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
        "version": 2,
        "duration": 2000,
        "surface": {"width": 800.0, "height": 600.0},
        "strokes": [[[0.25, 0.5, 100], [0.75, 0.5, 300]]],
        "pins": [{"at": 1200, "point": [0.5, 0.25]}],
        "actions": [
            click,
            {"kind": "drag", "span": [600, 900], "from": [0.25, 0.75], "to": [0.75, 0.75]},
        ],
        "narration": {"words": [
            {"span": [100, 300], "text": "Look"},
            {"span": [300, 500], "text": "here."},
        ]},
        "marks": [
            {
                "number": 1,
                "shape": "line",
                "from": [0.25, 0.5],
                "to": [0.75, 0.5],
                "strokes": [0, 1],
                "span": [100, 300],
                "bounds": [0.25, 0.5, 0.75, 0.5],
            },
            {
                "number": 2,
                "shape": "pin",
                "point": [0.5, 0.25],
                "strokes": [1, 1],
                "span": [1200, 1200],
                "bounds": [0.5, 0.25, 0.5, 0.25],
            },
        ],
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
            {
                "index": 3,
                "at": 1500,
                "reason": "sceneChange",
                "event": "scene changed",
                "marks": [0, 0],
            },
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
fn the_summary_says_how_to_read_every_sheet_and_sets_each_path_apart() {
    let manifest = Manifest {
        version: Manifest::VERSION,
        recording: Recording {
            duration: Moment::from_millis(8_000),
            ..Recording::default()
        },
        marks: Vec::new(),
        keyframes: (1..=7)
            .map(|index| Keyframe {
                index,
                at: Moment::from_millis(index as u64 * 1_000),
                reason: Reason::SceneChange,
                event: "scene changed".to_owned(),
                said: None,
                marks: 0..0,
                action: None,
            })
            .collect(),
    };
    let directory = Path::new("My Shows").join("show 1");
    let summary = Summary {
        manifest: &manifest,
        directory: &directory,
    }
    .to_string();
    let lines: Vec<&str> = summary.lines().collect();
    let sheet = |name| format!("`{}`", directory.join(name).display());
    assert_eq!(
        lines[0],
        format!(
            "Screen recording (0:08, 7 keyframes): `{}`",
            directory.display()
        )
    );
    assert!(
        lines[1].starts_with("Before acting, open the contact sheets, then the full-size frames"),
        "{}",
        lines[1]
    );
    assert_eq!(
        lines[2..4],
        [
            format!(
                "Contact sheet 1 (frames #1–#4): {}",
                sheet("contact-sheet.png")
            ),
            format!(
                "Contact sheet 2 (frames #5–#7): {}",
                sheet("contact-sheet-02.png")
            ),
        ]
    );
    assert_eq!(
        summary.lines().last(),
        Some(
            "Full-size keyframes: `frames/frame-01.png` to `frames/frame-07.png`. Timeline: `show.json`."
        )
    );
}

#[test]
fn a_code_span_holds_any_path_whole() {
    let span = |text: &str| Code(text).to_string();
    assert_eq!(span("My Shows/show 1"), "`My Shows/show 1`");
    assert_eq!(span("a`b``c"), "```a`b``c```");
    assert_eq!(span("`a"), "`` `a ``");
    assert_eq!(span(" a"), "`  a `");
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
fn a_failed_frame_stops_loading_before_the_next_sheet() {
    let selection = Selection {
        marks: Vec::new(),
        keyframes: (1..=13)
            .map(|index| Keyframe {
                index,
                at: Moment::from_millis(index as u64),
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
                "Loaded the sheet after a failed sheet"
            );
            if moment.as_millis() == 7 {
                Err(Error::unsampled())
            } else {
                Image::filled(160, 100, [90; 3])
            }
        },
        &typeface(),
    );
    assert_eq!(
        result.unwrap_err().to_string(),
        Error::unsampled().to_string()
    );
}

#[test]
fn contact_sheets_cover_every_frame_once_in_readable_pages() {
    for (count, sizes) in [
        (6_usize, &[6][..]),
        (7, &[4, 3]),
        (12, &[6, 6]),
        (13, &[5, 4, 4]),
    ] {
        let recording = Recording {
            duration: Moment::from_millis(30_000),
            actions: (0..count)
                .map(|index| Action::Click {
                    at: Moment::from_millis(index as u64 * 2_000),
                    point: Point::new(0.1, 0.1),
                })
                .collect(),
            ..Recording::default()
        };
        let samples: Vec<Sample> = (0..count)
            .map(|index| Sample {
                at: Moment::from_millis(index as u64 * 2_000 + 250),
                signature: signature(128),
            })
            .collect();
        let selection = select(&recording, &samples).unwrap();
        assert_eq!(selection.keyframes.len(), count);
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
        let frame_files: Vec<_> = bundle
            .manifest
            .keyframes
            .iter()
            .map(Keyframe::file)
            .collect();
        let (summary, files) = bundle.into_files(directory).unwrap();
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
                    "Frame {} on {} of {count} frames",
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
