use speakeasy_show::{Moment, Narration as Recorded, NarrationLoss, Recording, Span, Word};

use super::super::narration::Narration;

#[test]
fn narration_distinguishes_silence_missing_words_and_untimed_text() {
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
    recording.narration = Recorded::Text("Untimed words.".into());
    assert!(matches!(
        Narration::from_recording(&recording),
        Narration::Untimed {
            incomplete: true,
            ..
        }
    ));
    recording.narration = Recorded::Words(vec![Word {
        span: Span::at(Moment::START),
        text: "Timed words.".into(),
    }]);
    let narration = Narration::from_recording(&recording);
    assert!(narration.incomplete());
    assert_eq!(narration.clauses()[0].text, "Timed words.");
}
