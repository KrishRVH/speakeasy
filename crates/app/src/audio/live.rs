use std::time::Duration;

use speakeasy_core::machine::SessionId;
use tokio::sync::{mpsc, oneshot};

use super::{
    pcm::{bytes_in, samples_in},
    speech::Audibility,
};
use crate::runtime::ports::{CaptureEvent, LiveAudio};

const FRAME: Duration = Duration::from_millis(100);
const QUEUED_FRAMES: usize = 20;

pub(super) enum Feed {
    Waiting,
    Sending {
        frames: mpsc::Sender<Vec<u8>>,
        ending: oneshot::Sender<Vec<u8>>,
        cursor: usize,
    },
}

#[derive(Debug)]
pub(super) struct Abandoned;

impl Feed {
    pub(super) fn offer(
        &mut self,
        id: SessionId,
        events: &async_channel::Sender<CaptureEvent>,
        audio: &[u8],
        speech: &Audibility,
        rate: u32,
    ) -> Result<(), Abandoned> {
        if matches!(self, Self::Waiting) {
            let Some(range) = speech.retained(audio.len()) else {
                return Ok(());
            };
            let (frames, receiver) = mpsc::channel(QUEUED_FRAMES);
            let (ending, tail) = oneshot::channel();
            let live = LiveAudio {
                rate,
                frames: receiver,
                tail,
            };
            if events.try_send(CaptureEvent::Streaming(id, live)).is_err() {
                return Err(Abandoned);
            }
            *self = Self::Sending {
                frames,
                ending,
                cursor: range.start,
            };
        }
        let chunk = bytes_in(samples_in(FRAME, rate));
        while let Self::Sending { frames, cursor, .. } = self {
            if frames.is_closed() {
                return Err(Abandoned);
            }
            if frames.capacity() == 0 {
                break;
            }
            let Some(part) = audio
                .get(*cursor..cursor.saturating_add(chunk))
                .filter(|part| !part.is_empty())
            else {
                break;
            };
            if frames.try_send(part.to_vec()).is_err() {
                return Err(Abandoned);
            }
            *cursor = cursor.saturating_add(chunk);
        }
        Ok(())
    }

    pub(super) fn finish(self, audio: &[u8]) {
        if let Self::Sending { ending, cursor, .. } = self
            && let Some(tail) = audio.get(cursor..)
        {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A cancelled stream no longer needs its tail; the owner still receives the full sealed recording"
            )]
            let _ = ending.send(tail.to_vec());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::fixture::square_wave;

    fn spoken(rate: u32) -> (Vec<u8>, Audibility) {
        let audio: Vec<u8> = square_wave(2000)
            .take(48_000)
            .flat_map(i16::to_le_bytes)
            .collect();
        let mut speech = Audibility::new(rate);
        speech.extend(&audio);
        (audio, speech)
    }

    #[test]
    fn feed_starts_with_gated_padding_and_keeps_every_sample_through_finish() {
        let rate = 16_000;
        let mut speech = Audibility::new(rate);
        let (events, reports) = async_channel::bounded(1);
        let mut feed = Feed::Waiting;
        let mut audio = vec![0; 48_000];
        speech.extend(&audio);
        feed.offer(SessionId::FIRST, &events, &audio, &speech, rate)
            .unwrap();
        assert!(reports.try_recv().is_err(), "Quiet opened a stream");
        audio.extend(square_wave(2000).take(1600).flat_map(i16::to_le_bytes));
        speech.extend(&audio);
        let start = speech.retained(audio.len()).unwrap().start;
        feed.offer(SessionId::FIRST, &events, &audio, &speech, rate)
            .unwrap();
        let CaptureEvent::Streaming(_, mut live) = reports.try_recv().unwrap() else {
            panic!("Missing stream")
        };
        audio.extend([1, 0, 2, 0]);
        feed.finish(&audio);
        let mut received = Vec::new();
        while let Ok(bytes) = live.frames.try_recv() {
            received.extend_from_slice(&bytes);
        }
        received.extend_from_slice(&live.tail.try_recv().unwrap());
        assert_eq!(received, audio[start..]);
    }

    #[test]
    fn backpressure_retries_the_cursor_and_completion_preserves_the_entire_tail() {
        for retry in [false, true] {
            let (audio, speech) = spoken(16_000);
            let (events, reports) = async_channel::bounded(1);
            let mut feed = Feed::Waiting;
            feed.offer(SessionId::FIRST, &events, &audio, &speech, 16_000)
                .unwrap();
            let CaptureEvent::Streaming(_, mut live) = reports.try_recv().unwrap() else {
                panic!("Missing stream")
            };
            assert!(matches!(feed, Feed::Sending { .. }));
            let mut received = Vec::new();
            if retry {
                while let Ok(bytes) = live.frames.try_recv() {
                    received.extend_from_slice(&bytes);
                }
            }
            feed.offer(SessionId::FIRST, &events, &audio, &speech, 16_000)
                .unwrap();
            let mut more = audio.clone();
            more.extend([3, 0, 4, 0]);
            feed.finish(&more);
            while let Ok(bytes) = live.frames.try_recv() {
                received.extend_from_slice(&bytes);
            }
            received.extend_from_slice(&live.tail.try_recv().unwrap());
            assert_eq!(received, more);
        }
    }

    #[test]
    fn a_stream_nobody_can_hear_is_abandoned() {
        let (audio, speech) = spoken(16_000);
        let (closed, _receiver) = async_channel::bounded(1);
        closed.close();
        assert!(
            Feed::Waiting
                .offer(SessionId::FIRST, &closed, &audio, &speech, 16_000)
                .is_err(),
            "A stream opened on a closed event lane"
        );

        let (events, reports) = async_channel::bounded(1);
        let mut feed = Feed::Waiting;
        feed.offer(SessionId::FIRST, &events, &audio, &speech, 16_000)
            .unwrap();
        let CaptureEvent::Streaming(_, live) = reports.try_recv().unwrap() else {
            panic!("Missing stream")
        };
        drop(live);
        assert!(
            feed.offer(SessionId::FIRST, &events, &audio, &speech, 16_000)
                .is_err(),
            "A stream kept sending after its listener left"
        );
    }
}
