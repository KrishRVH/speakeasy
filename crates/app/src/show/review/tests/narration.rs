//! Imported narration states, sampling gaps, and content-free moment copies.

use speakeasy_show::{Moment, Narration as Recorded, NarrationLoss, Recording, Span, Word};

use super::super::{
    narration::Narration,
    navigation::{Origin, Relation, Request, pointer},
    *,
};

#[test]
fn navigation_reports_exact_earlier_and_first_later_samples_and_never_infers_text() {
    let root = Source::Demo.load().unwrap();
    let (rows, _) = library::list(&root.directory).unwrap();
    let directory = &rows[0].directory;
    let mut detail = Detail::open(directory, Mode::Keyframes).unwrap();
    for (frame, at) in detail.frames.iter_mut().zip([100, 700, 1_500, 30_000]) {
        frame.at = Moment::from_millis(at);
    }
    for (at, index, relation) in [
        (0, 0, Relation::Later(std::time::Duration::from_millis(100))),
        (700, 1, Relation::At),
        (
            20_000,
            2,
            Relation::Earlier(std::time::Duration::from_millis(18_500)),
        ),
        (
            40_000,
            3,
            Relation::Earlier(std::time::Duration::from_millis(10_000)),
        ),
    ] {
        assert_eq!(
            detail.locate(Moment::from_millis(at)),
            Some((index, relation))
        );
    }
    detail.summary = Ok("PRIVATE_EDIT".into());
    detail.frames[2].marker = "PRIVATE_CUE".into();
    let request = Request {
        at: Moment::from_millis(20_000),
        origin: Origin::Clause(0),
    };
    let early = Request {
        at: Moment::START,
        origin: Origin::Clause(0),
    };
    for (mode, kind) in [
        (Mode::Keyframes, "first keyframe"),
        (Mode::Source, "first retained sample"),
    ] {
        let caption = early.caption(&detail.frames[0], mode, detail.frames[0].presentation());
        assert!(caption.contains(kind) && caption.contains("100 ms later"));
        assert!(!caption.contains("first available"));
        assert!(
            !pointer(
                directory,
                &detail.frames[0],
                mode,
                Some(early),
                detail.frames[0].at.since(speakeasy_show::Moment::START)
            )
            .contains("first available")
        );
    }
    for mode in [Mode::Keyframes, Mode::Source] {
        let text = pointer(
            directory,
            &detail.frames[2],
            mode,
            Some(request),
            detail.frames[2].at.since(speakeasy_show::Moment::START),
        );
        assert!(!text.contains("PRIVATE"));
        assert!(text.contains("0:20.000") && text.contains("0:01.500"));
        assert!(text.contains("18500 ms earlier"));
        if mode == Mode::Source {
            assert!(text.contains("--at 0:01.500"));
        }
    }
    detail.frames.clear();
    assert_eq!(detail.locate(Moment::START), None);
}

#[test]
fn narration_distinguishes_silence_missing_words_and_untimed_text_and_bounds_the_preview() {
    let mut recording = Recording::default();
    assert!(matches!(
        Narration::from_recording(&recording),
        Narration::Silent
    ));
    for loss in [
        NarrationLoss::AudioStopped,
        NarrationLoss::NotTranscribed,
        NarrationLoss::Incomplete,
    ] {
        recording.narration_lost = Some(loss);
        assert!(
            matches!(Narration::from_recording(&recording), Narration::Lost(found) if found == loss)
        );
    }
    recording.narration = Recorded::Text("é".repeat(40_000));
    let Narration::Untimed {
        text,
        incomplete,
        truncated,
    } = Narration::from_recording(&recording)
    else {
        panic!("Expected untimed words");
    };
    assert_eq!(text.len(), 64 << 10);
    assert!(incomplete && truncated);
    recording.narration = Recorded::Words(
        (0_u64..600)
            .map(|n| Word {
                span: Span::at(Moment::from_millis(n.saturating_mul(2_000))),
                text: "word.".into(),
            })
            .collect(),
    );
    let Narration::Timed {
        clauses,
        incomplete,
        truncated,
    } = Narration::from_recording(&recording)
    else {
        panic!("Expected timed words");
    };
    assert_eq!(clauses.len(), 512);
    assert!(incomplete && truncated);
    recording.narration = Recorded::Words(vec![Word {
        span: Span::at(Moment::START),
        text: "é".repeat(40_000),
    }]);
    let Narration::Timed {
        clauses, truncated, ..
    } = Narration::from_recording(&recording)
    else {
        panic!("Expected timed words");
    };
    assert_eq!(clauses[0].text.len(), 64 << 10);
    assert!(truncated);
}
#[test]
fn a_video_reference_requests_inside_the_displayed_sample_instead_of_rounding_into_its_predecessor()
{
    use std::time::Duration;
    let mut frame = super::library::Frame {
        number: 2,
        at: Moment::from_millis(283),
        marker: String::new(),
        file: crate::show::preview::Content::Video {
            span: Duration::from_nanos(33_333_333)..Duration::from_nanos(66_666_666),
            annotations: Moment::from_millis(1000),
        },
    };
    let at = Duration::from_nanos(283_333_300);
    let text = pointer(
        std::path::Path::new("synthetic"),
        &frame,
        Mode::Video,
        None,
        at,
    );
    assert!(text.contains("sample at 0:00.283"));
    assert!(text.contains("--video --at 0:00.284"));
    assert!(
        !pointer(
            std::path::Path::new("synthetic"),
            &frame,
            Mode::Video,
            None,
            Duration::from_millis(300)
        )
        .contains("--at")
    );
    if let crate::show::preview::Content::Video { span, .. } = &mut frame.file {
        span.end = Duration::from_micros(33_500);
    }
    assert!(
        !pointer(
            std::path::Path::new("synthetic"),
            &frame,
            Mode::Video,
            None,
            at
        )
        .contains("--at")
    );
    frame.at = Moment::from_millis(99);
    frame.file = crate::show::preview::Content::Video {
        span: Duration::from_micros(99_500)..Duration::from_nanos(100_000_033),
        annotations: Moment::from_millis(1000),
    };
    assert!(
        !pointer(
            std::path::Path::new("synthetic"),
            &frame,
            Mode::Video,
            None,
            Duration::from_micros(99_500)
        )
        .contains("--at"),
        "A query at the decoder's next sample boundary selected the wrong frame"
    );
}
#[test]
fn video_seeking_and_request_captions_preserve_submillisecond_order() -> anyhow::Result<()> {
    use std::time::Duration;
    let root = Source::Demo.load()?;
    let (rows, _) = library::list(&root.directory)?;
    let mut detail = library::Detail::open(&rows[0].directory, Mode::Keyframes)?;
    detail.frames = [0_u64, 33_333_300]
        .into_iter()
        .enumerate()
        .map(|(index, nanos)| {
            let at = Duration::from_nanos(nanos);
            library::Frame {
                number: index + 1,
                at: Moment::from_millis(250).saturating_add(at),
                marker: String::new(),
                file: crate::show::preview::Content::Video {
                    span: at..at.saturating_add(Duration::from_nanos(33_333_300)),
                    annotations: Moment::from_millis(1000),
                },
            }
        })
        .collect();
    assert_eq!(
        detail.locate(Moment::from_millis(283)),
        Some((0, Relation::Earlier(Duration::from_millis(33))))
    );
    assert_eq!(detail.locate(Moment::from_millis(284)).unwrap().0, 1);
    let request = Request {
        at: Moment::from_millis(283),
        origin: Origin::Mark(1),
    };
    let caption = request.caption(
        &detail.frames[1],
        Mode::Video,
        detail.frames[1].presentation(),
    );
    assert!(caption.contains("0.333 ms later"));
    assert!(!caption.contains("same timestamp"));
    detail.frames[1].at = Moment::from_millis(284);
    detail.frames[1].file = crate::show::preview::Content::Video {
        span: Duration::from_nanos(34_000_033)..Duration::from_millis(50),
        annotations: Moment::from_millis(1000),
    };
    assert_eq!(
        detail.locate(Moment::from_millis(284)),
        Some((1, Relation::At)),
        "Navigation disagreed with the decoder's 100 ns clock"
    );
    Ok(())
}
