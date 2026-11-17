use speakeasy_show::{Moment, Narration as Recorded, NarrationLoss, Recording, Span, Word};

use super::super::narration::Narration;

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
