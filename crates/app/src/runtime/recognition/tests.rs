//! Whole PCM remains available after a tail failure, missing piece, or overloaded request lane.

use super::*;
use crate::{audio::silent_wav, runtime::ports::Partition};

fn sealed() -> Sealed {
    Sealed {
        wav: silent_wav(16_000, Duration::from_secs(2)).unwrap(),
        trimmed: Duration::from_millis(500),
        partition: Some(Partition {
            pieces: 1,
            tail: 32_000..64_000,
        }),
    }
}

#[test]
fn stale_tickets_cannot_complete_or_fail_an_active_piece() {
    let mut recognition = Recognition::new(SessionId::FIRST.next().unwrap());
    recognition.piece(0, Sealed::default());
    let current = recognition.request().unwrap().ticket;
    let stale = Ticket {
        session: SessionId::FIRST,
        part: current.part,
    };
    assert!(
        recognition
            .heard(stale, Transcript::Text("stale".into()))
            .is_none()
    );
    assert!(recognition.failed(stale));
    assert!(recognition.accepts(current));
    assert!(
        recognition
            .heard(current, Transcript::Text("current".into()))
            .is_none()
    );
    assert!(!recognition.accepts(current));
}

#[test]
fn failed_tail_retries_original_audio_and_language_once() {
    let mut recognition = Recognition::new(SessionId::FIRST);
    recognition.piece(0, Sealed::default());
    let piece = recognition.request().unwrap();
    assert!(
        recognition
            .heard(piece.ticket, Transcript::Text("prefix".into()))
            .is_none()
    );
    let whole = sealed();
    let original = whole.wav.clone();
    recognition.seal(
        whole,
        Duration::from_secs(3),
        Some("fr".into()),
        Detail::Text,
    );
    let tail = recognition.request().unwrap();
    assert_eq!(&tail.audio.wav[44..], &original[32_044..]);
    assert_eq!(tail.language.as_deref(), Some("fr"));
    assert!(recognition.failed(tail.ticket));
    let retry = recognition.request().unwrap();
    assert_eq!(retry.audio.wav, original);
    assert_eq!(retry.language.as_deref(), Some("fr"));
    assert!(!recognition.failed(retry.ticket));
    assert!(recognition.request().is_none());
}

#[test]
fn missing_pieces_and_backpressure_fall_back_to_the_exact_whole_wav() {
    for overload in [false, true] {
        let mut recognition = Recognition::new(SessionId::FIRST);
        if overload {
            recognition.piece(0, Sealed::default());
        }
        recognition.piece(1, Sealed::default());
        let whole = sealed();
        let original = whole.wav.clone();
        recognition.seal(whole, Duration::ZERO, None, Detail::Text);
        let request = recognition.request().unwrap();
        assert_eq!(request.ticket.part, Part::Final);
        assert_eq!(request.audio.wav, original);
    }
}

#[test]
fn promotion_invalidates_inflight_piece_and_keeps_full_audio_and_timeline() {
    let mut recognition = Recognition::new(SessionId::FIRST);
    recognition.piece(0, Sealed::default());
    let piece = recognition.request().unwrap();
    recognition.whole();
    assert!(!recognition.accepts(piece.ticket));
    assert!(
        recognition
            .heard(piece.ticket, Transcript::Text("stale".into()))
            .is_none()
    );
    let whole = sealed();
    let original = whole.wav.clone();
    recognition.seal(
        whole,
        Duration::from_millis(1700),
        Some("en".into()),
        Detail::Words,
    );
    let request = recognition.request().unwrap();
    assert_eq!(request.detail, Detail::Words);
    assert_eq!(request.audio.wav, original);
    assert_eq!(recognition.start(), Some(Duration::from_millis(1700)));
}
