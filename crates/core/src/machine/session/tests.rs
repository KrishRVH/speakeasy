use std::time::Duration;

use super::*;
use crate::machine::{
    Part,
    fixture::{Effect, Log, Sample},
};

fn opened(log: &mut Log, show: bool) -> Session<Sample> {
    let mut session =
        Session::<Sample>::new(SessionId::FIRST, 1, Duration::ZERO, Purpose::Dictation, ());
    if show {
        assert!(session.turn_into_show((), log));
    }
    session.opened();
    session
}

fn sealed() -> Sealed<Sample> {
    Sealed {
        audio: vec![1, 2],
        trimmed: Duration::from_millis(500),
        partition: None,
    }
}

const fn loss(session: &Session<Sample>) -> Option<NarrationLoss> {
    match &session.stage {
        Stage::AwaitingFootage { loss, .. } => *loss,
        _ => None,
    }
}

#[test]
fn publication_closes_producers_before_it_hands_over_the_checkpoint() {
    let mut log = Log::default();
    let mut session = opened(&mut log, true);
    let kept = Kept {
        checkpoint: "checkpoint",
        audio: log.producer(),
        frames: log.producer(),
    };
    session.keep(kept);
    session.audio_ready(State::Held, Duration::ZERO);
    session.screen_recorded("footage");
    assert!(session.captured(None));
    session.publish(&mut log);
    assert_eq!(
        log.effects,
        [Effect::Publish {
            producers_closed: true
        }]
    );
    let publication = log.published.unwrap();
    assert_eq!(publication.checkpoint, Some("checkpoint"));
    assert_eq!(publication.footage, "footage");
}

#[test]
fn narration_lands_where_it_was_spoken_on_the_show_timeline() {
    let mut log = Log::default();
    let mut session = opened(&mut log, true);
    session.audio_ready(State::Held, Duration::from_millis(1_200));
    session.finish();
    assert!(session.captured(Some(sealed())));
    let request = session.request(&mut log).unwrap();
    assert_eq!(
        session.heard(
            request.ticket(),
            Transcript::Words(vec![Duration::from_millis(300)])
        ),
        None
    );
    let Stage::AwaitingFootage { narration, .. } = &session.stage else {
        panic!("Narration did not move the Show on to its footage");
    };
    assert_eq!(narration, &[Duration::from_millis(2_000)]);
}

#[test]
fn a_show_goes_on_without_narration_only_from_the_stages_its_loss_can_reach() {
    let mut log = Log::default();
    let mut dictation = opened(&mut log, false);
    dictation.audio_ready(State::Held, Duration::ZERO);
    assert!(!dictation.audio_failed(false), "Dictation has nothing left");

    let mut show = opened(&mut log, true);
    assert!(
        !show.audio_failed(false),
        "A microphone that never listened fails with its remedy"
    );
    show.audio_ready(State::Held, Duration::ZERO);
    assert!(
        !show.not_transcribed(),
        "A Show still recording has no speech to transcribe"
    );
    show.finish();
    assert!(
        show.audio_failed(false),
        "Audio can still stop while the capture seals it"
    );
    assert_eq!(loss(&show), Some(NarrationLoss::AudioStopped));

    let mut show = opened(&mut log, true);
    show.audio_ready(State::Held, Duration::ZERO);
    show.finish();
    assert!(show.audio_failed(true));
    assert_eq!(loss(&show), Some(NarrationLoss::AudioSilent));

    let mut show = opened(&mut log, true);
    show.audio_ready(State::Held, Duration::ZERO);
    show.finish();
    assert!(show.captured(Some(sealed())));
    assert!(!show.audio_failed(false), "Sealed audio can no longer stop");
    assert!(show.request(&mut log).is_some());
    assert!(!show.not_transcribed(), "Speech keeps its one retry");
    assert!(show.failed(Ticket {
        session: show.id,
        part: Part::Final
    }));
    assert!(show.request(&mut log).is_some());
    assert!(show.not_transcribed());
    assert_eq!(loss(&show), Some(NarrationLoss::NotTranscribed));
}
