//! Continuous inference consumes bounded PCM chunks from the drain thread. A full lane leaves its
//! cursor for the next drain; completion hands over the unqueued tail without blocking capture.

use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

use super::{
    pcm::{bytes_in, samples_in},
    speech::Audibility,
};
use crate::runtime::ports::{CaptureEvent, LiveAudio, SessionId};

pub(super) enum Feed {
    Waiting,
    Sending {
        frames: mpsc::Sender<Zeroizing<Vec<u8>>>,
        ending: oneshot::Sender<Zeroizing<Vec<u8>>>,
        cursor: usize,
    },
    Abandoned,
}

impl Feed {
    pub(super) fn offer(
        &mut self,
        id: SessionId,
        events: &async_channel::Sender<CaptureEvent>,
        audio: &[u8],
        speech: &Audibility,
        rate: u32,
    ) {
        if matches!(self, Self::Waiting) {
            let Some(range) = speech.retained(audio.len()) else {
                return;
            };
            let (frames, receiver) = mpsc::channel(20);
            let (ending, tail) = oneshot::channel();
            if events
                .try_send(CaptureEvent::Streaming(
                    id,
                    LiveAudio {
                        rate,
                        frames: receiver,
                        tail,
                    },
                ))
                .is_err()
            {
                *self = Self::Abandoned;
                return;
            }
            *self = Self::Sending {
                frames,
                ending,
                cursor: range.start,
            };
        }
        let chunk = bytes_in(samples_in(std::time::Duration::from_millis(100), rate));
        while let Self::Sending { frames, cursor, .. } = self {
            if frames.is_closed() {
                *self = Self::Abandoned;
                break;
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
            if frames.try_send(Zeroizing::new(part.to_vec())).is_err() {
                *self = Self::Abandoned;
                break;
            }
            *cursor = cursor.saturating_add(chunk);
        }
    }

    pub(super) fn finish(self, audio: &[u8]) {
        if let Self::Sending { ending, cursor, .. } = self
            && let Some(tail) = audio.get(cursor..)
        {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A cancelled stream no longer needs its tail; the owner still receives the full sealed recording"
            )]
            let _ = ending.send(Zeroizing::new(tail.to_vec()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_starts_with_gated_padding_and_keeps_every_sample_through_finish() {
        let rate = 16_000;
        let mut speech = Audibility::new(rate);
        let (events, reports) = async_channel::bounded(1);
        let mut feed = Feed::Waiting;
        let mut audio = vec![0; 48_000];
        speech.extend(&audio);
        feed.offer(SessionId::FIRST, &events, &audio, &speech, rate);
        assert!(reports.try_recv().is_err(), "Quiet opened a stream");
        audio.extend(std::iter::repeat_n(2000_i16.to_le_bytes(), 1600).flatten());
        speech.extend(&audio);
        let start = speech.retained(audio.len()).unwrap().start;
        feed.offer(SessionId::FIRST, &events, &audio, &speech, rate);
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
            let audio: Vec<u8> = std::iter::repeat_n(2000_i16.to_le_bytes(), 48_000)
                .flatten()
                .collect();
            let mut speech = Audibility::new(16_000);
            speech.extend(&audio);
            let (events, reports) = async_channel::bounded(1);
            let mut feed = Feed::Waiting;
            feed.offer(SessionId::FIRST, &events, &audio, &speech, 16_000);
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
            feed.offer(SessionId::FIRST, &events, &audio, &speech, 16_000);
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
}
