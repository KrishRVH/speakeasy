use super::*;

const SCREEN: Size = Size {
    width: 800.0,
    height: 600.0,
};

fn keyframe(index: usize, crop: Option<Rect>) -> Keyframe {
    Keyframe {
        index,
        at: Moment::from_millis(u64::try_from(index).unwrap().saturating_mul(1_000)),
        reason: Reason::SceneChange,
        event: "scene changed".to_owned(),
        said: None,
        marks: 0..0,
        action: None,
        crop,
    }
}

fn near(one: Rect, other: Rect) -> bool {
    [
        one.left - other.left,
        one.top - other.top,
        one.right - other.right,
        one.bottom - other.bottom,
    ]
    .into_iter()
    .all(|difference| difference.abs() < 1e-5)
}

#[test]
fn a_cropped_frame_is_cut_to_the_crop_it_records_and_keeps_everything_drawn() {
    let chosen = Rect::from([0.1, 0.1, 0.5, 0.5]);
    let click = Point::new(0.62, 0.3);
    let mut clicked = keyframe(2, None);
    clicked.action = Some(Action::Click {
        at: clicked.at,
        point: click,
    });
    let planned = Some(Plan {
        crop: chosen,
        within: Rect::UNIT,
    });
    let selection = Selection {
        marks: Vec::new(),
        keyframes: vec![keyframe(1, None), clicked, keyframe(3, None)],
        plans: vec![planned, planned],
    };
    let recording = Recording {
        duration: Moment::from_millis(4_000),
        surface: SCREEN,
        ..Recording::default()
    };
    let bundle = compose(
        recording,
        selection,
        |_| Image::filled(800, 600, [90; 3]),
        &typeface(),
    )
    .unwrap();
    let crops: Vec<Option<Rect>> = bundle
        .manifest
        .keyframes
        .iter()
        .map(|keyframe| keyframe.crop)
        .collect();
    let [Some(first), Some(grown), None] = crops[..] else {
        panic!("Only the first two frames are cropped: {crops:?}");
    };
    assert!(near(first, chosen), "{first:?}");
    assert!(grown.encloses(chosen) && grown.contains(click), "{grown:?}");
    let (_, files) = bundle.into_files(Path::new("show"), None).unwrap();
    for (index, crop) in crops.into_iter().enumerate() {
        let crop = crop.unwrap_or(Rect::UNIT);
        let name = format!("frames/frame-{:02}.png", index + 1);
        let image = Image::decode_png(
            &files
                .iter()
                .find(|file| file.name == name)
                .unwrap()
                .contents,
        );
        assert_eq!(
            (image.width(), image.height()),
            (
                crate::convert::pixels(crop.width() * 800.0),
                crate::convert::pixels(crop.height() * 600.0)
            ),
            "{name}"
        );
    }
}

#[test]
fn the_summary_names_cropped_frames_and_how_to_see_the_whole_screen() {
    let part = Some(Rect::from([0.1, 0.1, 0.6, 0.6]));
    let summary = |crops: &[Option<Rect>], retrieval| {
        let manifest = Manifest {
            version: Manifest::VERSION,
            recording: Recording {
                duration: Moment::from_millis(8_000),
                ..Recording::default()
            },
            marks: Vec::new(),
            keyframes: crops
                .iter()
                .enumerate()
                .map(|(position, &crop)| keyframe(position + 1, crop))
                .collect(),
        };
        Summary {
            manifest: &manifest,
            directory: Path::new("show"),
            retrieval,
        }
        .to_string()
    };
    let retrieval = Some(Retrieval {
        command: Path::new("speakeasy"),
        id: "ID",
    });
    let named = [
        (vec![None, part], "Frame #2 is cropped"),
        (vec![part, None, part], "Frames #1 and #3 are cropped"),
        (
            vec![None, part, None, part, part],
            "Frames #2, #4, and #5 are cropped",
        ),
        (vec![part, part], "Every keyframe is cropped"),
    ];
    let whole = "`show frame ID --at M:SS.mmm --out NEW.png --json`";
    for (crops, line) in named {
        let summary = summary(&crops, retrieval);
        assert!(summary.contains(line), "{summary}");
        assert!(summary.contains(whole), "{summary}");
    }
    let uncropped = summary(&[None, None], retrieval);
    assert!(!uncropped.contains("cropped"), "{uncropped}");
    assert!(!summary(&[part], None).contains("show frame"));
}
