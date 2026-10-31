//! Pins `show.json`, notes a recording that stopped early and narration that was lost in both
//! files, keeps file contents out of debug output, checks that the summary says how to read the
//! Show with every path whole and set apart, and that contact sheets show every keyframe once and
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
            ending: Ending::Finished,
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
            narration_lost: None,
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
        Manifest::parse(&serde_json::to_vec(&golden).unwrap()).unwrap(),
        manifest,
        "{changed}"
    );
}

#[test]
fn the_summary_says_how_to_read_every_sheet_and_sets_each_full_path_apart() {
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
        retrieval: None,
        manifest: &manifest,
        directory: &directory,
    }
    .to_string();
    let lines: Vec<&str> = summary.lines().collect();
    let file = |parts: &[&str]| {
        let path = parts
            .iter()
            .fold(directory.clone(), |path, part| path.join(part));
        format!("`{}`", path.display())
    };
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
                file(&["contact-sheet.png"])
            ),
            format!(
                "Contact sheet 2 (frames #5–#7): {}",
                file(&["contact-sheet-02.png"])
            ),
        ]
    );
    assert_eq!(
        lines.last().copied(),
        Some(
            format!(
                "Full-size keyframes: {} to {}. Timeline: {}.",
                file(&["frames", "frame-01.png"]),
                file(&["frames", "frame-07.png"]),
                file(&["show.json"])
            )
            .as_str()
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

/// The summary and parsed `show.json` of a one-frame Show of `recording`.
fn summary_and_manifest(recording: Recording) -> (String, serde_json::Value) {
    let samples = [Sample {
        at: Moment::START,
        signature: signature(128),
    }];
    let selection = select(&recording, &samples).unwrap();
    let bundle = compose(
        recording,
        selection,
        |_| Image::filled(160, 120, [90; 3]),
        &typeface(),
    )
    .unwrap();
    let (summary, files) = bundle.into_files(Path::new("show"), None).unwrap();
    let manifest = files
        .iter()
        .find(|file| file.name == Manifest::FILE)
        .unwrap();
    (summary, serde_json::from_slice(&manifest.contents).unwrap())
}

/// A 42-second recording of an 800 × 600 surface that ended as `ending`.
fn ended(ending: Ending) -> Recording {
    Recording {
        duration: Moment::from_millis(42_000),
        ending,
        surface: Size {
            width: 800.0,
            height: 600.0,
        },
        ..Recording::default()
    }
}

/// The line a 42-second Show whose screen stopped adds after the header.
const SCREEN_STOPPED: &str = "The screen stopped recording at 0:42, before the user finished the \
                              Show; later steps are missing.";

#[test]
fn a_show_whose_recording_stopped_early_says_why_and_when_in_both_files() {
    for (ending, why, field) in [
        (
            Ending::ScreenStopped,
            "The screen stopped recording",
            "screenStopped",
        ),
        (
            Ending::WindowClosed,
            "The recorded window closed",
            "windowClosed",
        ),
        (
            Ending::WindowOffScreen,
            "The recorded window went off screen",
            "windowOffScreen",
        ),
    ] {
        let (summary, manifest) = summary_and_manifest(ended(ending));
        assert_eq!(
            summary.lines().nth(1),
            Some(
                format!(
                    "{why} at 0:42, before the user finished the Show; later steps are missing."
                )
                .as_str()
            ),
            "Right after the header"
        );
        assert_eq!(manifest["ending"], field);
    }
}

#[test]
fn a_show_whose_narration_was_lost_says_why_in_both_files() {
    let lost = |ending, loss| Recording {
        narration_lost: Some(loss),
        ..ended(ending)
    };
    let (summary, manifest) =
        summary_and_manifest(lost(Ending::Finished, NarrationLoss::AudioStopped));
    assert_eq!(
        summary.lines().nth(1),
        Some(
            "Audio capture stopped at 0:42 and the Show ended there; what the user said and any \
             later steps are missing."
        )
    );
    assert_eq!(manifest["narrationLost"], "audioStopped");
    let (summary, _) =
        summary_and_manifest(lost(Ending::ScreenStopped, NarrationLoss::AudioStopped));
    assert_eq!(
        summary.lines().skip(1).take(2).collect::<Vec<_>>(),
        [
            SCREEN_STOPPED,
            "Audio capture stopped as well; what the user said is missing."
        ],
        "Only the screen's ending ended the Show"
    );
    let (summary, manifest) =
        summary_and_manifest(lost(Ending::ScreenStopped, NarrationLoss::NotTranscribed));
    assert_eq!(
        summary.lines().skip(1).take(2).collect::<Vec<_>>(),
        [
            SCREEN_STOPPED,
            "The user's speech could not be transcribed; what the user said is missing."
        ],
        "Both losses survive"
    );
    assert_eq!(manifest["narrationLost"], "notTranscribed");
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
        let (summary, files) = bundle.into_files(directory, None).unwrap();
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
