use std::ops::Range;

use speakeasy_show::Moment;

use super::super::{
    narration::Narration,
    navigation::{Origin, Relation, Request, pointer},
    *,
};

#[test]
fn navigation_reports_exact_earlier_and_first_later_samples() -> anyhow::Result<()> {
    let directory = Path::new("synthetic");
    let mut detail = detail(
        [100, 700, 1_500, 30_000]
            .into_iter()
            .zip(1..)
            .map(|(at, number)| frame(number, at, keyframe(number)))
            .collect(),
    );
    for (at, index, relation) in [
        (0, 0, Relation::Later(Duration::from_millis(100))),
        (700, 1, Relation::At),
        (20_000, 2, Relation::Earlier(Duration::from_millis(18_500))),
        (40_000, 3, Relation::Earlier(Duration::from_millis(10_000))),
    ] {
        assert_eq!(
            detail.locate(Moment::from_millis(at)),
            Some((index, relation))
        );
    }
    let request = Request {
        at: Moment::from_millis(20_000),
        origin: Origin::Clause(0),
    };
    let early = Request {
        at: Moment::START,
        origin: Origin::Clause(0),
    };
    let source = retained()?;
    for (kind, [first, third]) in [
        ("first keyframe", [keyframe(1), keyframe(3)]),
        ("first frame", [source.clone(), source]),
    ] {
        let first = frame(1, 100, first);
        let caption = early.caption(&first, first.presentation);
        assert!(caption.contains(kind) && caption.contains("100 ms later"));
        assert!(!caption.contains("first available"));
        assert!(
            !pointer(
                directory,
                &first,
                Some(early),
                first.presentation,
                movie::AnnotationState::Off,
            )
            .contains("first available")
        );
        let third = frame(3, 1_500, third);
        let text = pointer(
            directory,
            &third,
            Some(request),
            third.presentation,
            movie::AnnotationState::Off,
        );
        assert!(text.contains("0:20.000") && text.contains("0:01.500"));
        assert!(text.contains("18.5 s earlier"));
        if matches!(third.content, preview::Content::Source { .. }) {
            assert!(text.contains("--at 0:01.500"));
        }
    }
    detail.frames.clear();
    assert_eq!(detail.locate(Moment::START), None);
    Ok(())
}

#[test]
fn a_video_reference_requests_inside_the_displayed_sample_instead_of_rounding_into_its_predecessor()
{
    let directory = Path::new("synthetic");
    let reference = |frame: &library::Frame, sampled| {
        pointer(directory, frame, None, sampled, movie::AnnotationState::Off)
    };
    let mut sample = video_frame(
        2,
        250,
        Duration::from_nanos(33_333_333)..Duration::from_nanos(66_666_666),
    );
    let at = Duration::from_nanos(if cfg!(target_os = "macos") {
        283_333_333
    } else {
        283_333_300
    });
    let text = reference(&sample, at);
    assert!(text.contains("frame at 0:00.283"));
    assert!(text.contains("--video --at 0:00.284"));
    assert!(!reference(&sample, Duration::from_millis(300)).contains("--at"));
    sample.content = video(Duration::from_nanos(33_333_333)..Duration::from_micros(33_500));
    assert!(!reference(&sample, at).contains("--at"));
    let sample = video_frame(
        2,
        0,
        Duration::from_micros(99_500)..Duration::from_nanos(100_000_033),
    );
    let boundary = reference(&sample, Duration::from_micros(99_500));
    assert_eq!(
        boundary.contains("--at"),
        cfg!(target_os = "macos"),
        "Only the nanosecond clock keeps the next sample strictly beyond 100 ms"
    );
}

#[test]
fn video_seeking_and_request_captions_preserve_submillisecond_order() {
    let mut detail = detail(
        [0_u64, 33_333_300]
            .into_iter()
            .zip(1..)
            .map(|(nanos, number)| {
                let at = Duration::from_nanos(nanos);
                video_frame(
                    number,
                    250,
                    at..at.saturating_add(Duration::from_nanos(33_333_300)),
                )
            })
            .collect(),
    );
    assert_eq!(
        detail.locate(Moment::from_millis(283)),
        Some((0, Relation::Earlier(Duration::from_millis(33))))
    );
    assert_eq!(detail.locate(Moment::from_millis(284)).unwrap().0, 1);
    let request = Request {
        at: Moment::from_millis(283),
        origin: Origin::Mark(1),
    };
    let caption = request.caption(&detail.frames[1], detail.frames[1].presentation);
    assert!(caption.contains("0.333 ms later"));
    assert!(!caption.contains("same timestamp"));
    detail.frames[1] = video_frame(
        2,
        250,
        Duration::from_nanos(34_000_033)..Duration::from_millis(50),
    );
    assert_eq!(
        detail.locate(Moment::from_millis(284)),
        if cfg!(target_os = "macos") {
            Some((0, Relation::Earlier(Duration::from_millis(34))))
        } else {
            Some((1, Relation::At))
        },
        "Navigation disagreed with the platform decoder's clock"
    );
}

fn detail(frames: Vec<library::Frame>) -> Detail {
    Detail {
        frames,
        marks: Vec::new(),
        warnings: Vec::new(),
        summary: Ok(String::new()),
        narration: Narration::Silent,
        source_available: false,
        coverage: String::new(),
        movie: None,
    }
}

fn frame(number: usize, at: u64, content: preview::Content) -> library::Frame {
    library::Frame {
        number,
        presentation: Duration::from_millis(at),
        marker: String::new(),
        cropped: false,
        content,
    }
}

fn video_frame(number: usize, origin: u64, span: Range<Duration>) -> library::Frame {
    library::Frame {
        presentation: Duration::from_millis(origin).saturating_add(span.start),
        ..frame(number, 0, video(span))
    }
}

fn keyframe(number: usize) -> preview::Content {
    preview::Content::Png(format!("frames/frame-{number:02}.png"))
}

fn video(span: Range<Duration>) -> preview::Content {
    preview::Content::Video {
        span,
        annotations: Moment::from_millis(1000),
        scene: None,
    }
}

fn retained() -> anyhow::Result<preview::Content> {
    let bytes = super::source_bytes()?;
    let index = speakeasy_show::Source::read(u64::try_from(bytes.len())?, |range| {
        let range = usize::try_from(range.start)?..usize::try_from(range.end)?;
        bytes
            .get(range)
            .map(<[u8]>::to_vec)
            .context("Synthetic source read past its end")
    })?;
    Ok(preview::Content::Source {
        index: Arc::new(index),
    })
}
