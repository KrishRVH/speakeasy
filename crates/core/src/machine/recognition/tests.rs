use std::time::Duration;

use super::*;
use crate::machine::fixture::{Sample, cut};

const FIRST: SessionId = SessionId::FIRST;

fn recording() -> Vec<u8> {
    (0..64).collect()
}

const fn partition() -> Partition<Sample> {
    Partition {
        pieces: 1,
        tail: Some(32..64),
    }
}

fn request(recognition: &mut Recognition<Sample>) -> Option<Request<Sample>> {
    recognition.request(|audio, range| cut(audio, range))
}

fn audio(request: &Request<Sample>) -> &[u8] {
    let (Request::Piece { audio, .. } | Request::Final { audio, .. }) = request else {
        panic!("Expected recorded audio")
    };
    audio
}

fn detail(request: &Request<Sample>) -> Detail {
    let Request::Final { detail, .. } = *request else {
        panic!("Expected the final request")
    };
    detail
}

#[test]
fn stale_tickets_cannot_complete_or_fail_an_active_piece() {
    let mut recognition = Recognition::<Sample>::new(FIRST.next().unwrap());
    recognition.piece(0, Vec::new());
    let current = request(&mut recognition).unwrap().ticket();
    let stale = Ticket {
        session: FIRST,
        part: current.part,
    };
    assert!(
        recognition
            .heard(stale, Transcript::Text("stale".into()))
            .is_none()
    );
    assert!(recognition.absorb_failure(stale));
    assert!(recognition.accepts(current));
    assert!(
        recognition
            .heard(current, Transcript::Text("current".into()))
            .is_none()
    );
    assert!(!recognition.accepts(current));
}

#[test]
fn a_failed_tail_retries_the_whole_recording_once() {
    let mut recognition = Recognition::<Sample>::new(FIRST);
    recognition.piece(0, Vec::new());
    let piece = request(&mut recognition).unwrap();
    assert!(
        recognition
            .heard(piece.ticket(), Transcript::Text("prefix".into()))
            .is_none()
    );
    recognition.seal(
        recording(),
        Some(partition()),
        Duration::from_secs(3),
        Detail::Text,
    );
    let tail = request(&mut recognition).unwrap();
    assert_eq!(audio(&tail), &recording()[32..]);
    assert!(recognition.absorb_failure(tail.ticket()));
    let retry = request(&mut recognition).unwrap();
    assert_eq!(audio(&retry), recording());
    assert!(!recognition.absorb_failure(retry.ticket()));
    assert!(request(&mut recognition).is_none());
}

#[test]
fn a_tail_that_cannot_be_cut_falls_back_to_the_whole_recording() {
    let mut recognition = Recognition::<Sample>::new(FIRST);
    recognition.piece(0, Vec::new());
    let piece = request(&mut recognition).unwrap();
    assert!(
        recognition
            .heard(piece.ticket(), Transcript::Text("prefix".into()))
            .is_none()
    );
    recognition.seal(
        recording(),
        Some(Partition {
            pieces: 1,
            tail: Some(32..99),
        }),
        Duration::ZERO,
        Detail::Text,
    );
    let whole = request(&mut recognition).unwrap();
    assert_eq!(audio(&whole), recording());
    assert!(
        matches!(
            recognition.heard(whole.ticket(), Transcript::Text("all of it".into())),
            Some(Transcript::Text(text)) if text == "all of it"
        ),
        "A whole-recording transcript repeated the pieces it already covers"
    );
}

#[test]
fn missing_pieces_and_backpressure_fall_back_to_the_exact_whole_recording() {
    for overload in [false, true] {
        let mut recognition = Recognition::<Sample>::new(FIRST);
        if overload {
            recognition.piece(0, Vec::new());
        }
        recognition.piece(1, Vec::new());
        recognition.seal(recording(), Some(partition()), Duration::ZERO, Detail::Text);
        let whole = request(&mut recognition).unwrap();
        assert_eq!(whole.ticket().part, Part::Final);
        assert_eq!(audio(&whole), recording());
    }
}

#[test]
fn promotion_invalidates_an_inflight_piece_and_keeps_full_audio_and_timeline() {
    let mut recognition = Recognition::<Sample>::new(FIRST);
    recognition.piece(0, Vec::new());
    let piece = request(&mut recognition).unwrap();
    recognition.whole();
    assert!(!recognition.accepts(piece.ticket()));
    assert!(
        recognition
            .heard(piece.ticket(), Transcript::Text("stale".into()))
            .is_none()
    );
    recognition.seal(
        recording(),
        Some(partition()),
        Duration::from_millis(1700),
        Detail::Words,
    );
    let words = request(&mut recognition).unwrap();
    assert_eq!(detail(&words), Detail::Words);
    assert_eq!(audio(&words), recording());
    assert_eq!(recognition.start(), Some(Duration::from_millis(1700)));
}

#[test]
fn a_streaming_final_waits_for_capture_and_is_delivered_only_once() {
    for sealed_first in [false, true] {
        let mut recognition = Recognition::<Sample>::new(FIRST);
        recognition.stream(());
        let stream = request(&mut recognition).unwrap();
        assert!(matches!(stream, Request::Stream { .. }));
        assert!(request(&mut recognition).is_none());
        if sealed_first {
            recognition.seal(recording(), None, Duration::ZERO, Detail::Text);
        }
        let result = recognition.heard(stream.ticket(), Transcript::Text("complete".into()));
        if sealed_first {
            assert!(matches!(result, Some(Transcript::Text(text)) if text == "complete"));
        } else {
            assert!(result.is_none());
            recognition.seal(recording(), None, Duration::ZERO, Detail::Text);
            assert_eq!(recognition.complete().as_deref(), Some("complete"));
        }
        assert!(recognition.complete().is_none());
        assert!(request(&mut recognition).is_none());
        assert!(!recognition.accepts(stream.ticket()));
    }
}

#[test]
fn streaming_failure_and_show_promotion_keep_the_full_recording() {
    for promotion in [false, true] {
        let mut recognition = Recognition::<Sample>::new(FIRST);
        recognition.stream(());
        let streaming = request(&mut recognition).unwrap();
        if promotion {
            recognition.whole();
        } else {
            assert!(recognition.absorb_failure(streaming.ticket()));
        }
        assert!(!recognition.accepts(streaming.ticket()));
        assert!(
            recognition
                .heard(streaming.ticket(), Transcript::Text("stale".into()))
                .is_none()
        );
        let asked = if promotion {
            Detail::Words
        } else {
            Detail::Text
        };
        recognition.seal(recording(), None, Duration::from_secs(2), asked);
        let fallback = request(&mut recognition).unwrap();
        assert_eq!(audio(&fallback), recording());
        assert_eq!(detail(&fallback), asked);
        assert_eq!(recognition.start(), Some(Duration::from_secs(2)));
    }
}

#[test]
fn pieces_and_an_empty_tail_join_without_another_transcription() {
    let mut recognition = Recognition::<Sample>::new(FIRST);
    recognition.piece(0, Vec::new());
    let first = request(&mut recognition).unwrap();
    assert!(
        recognition
            .heard(first.ticket(), Transcript::Text(" hello ".into()))
            .is_none()
    );
    recognition.piece(1, Vec::new());
    let second = request(&mut recognition).unwrap();
    recognition.seal(
        recording(),
        Some(Partition {
            pieces: 2,
            tail: None,
        }),
        Duration::ZERO,
        Detail::Text,
    );
    assert!(matches!(
        recognition.heard(second.ticket(), Transcript::Text("world".into())),
        Some(Transcript::Text(text)) if text == "hello world"
    ));
    assert!(request(&mut recognition).is_none());
}
