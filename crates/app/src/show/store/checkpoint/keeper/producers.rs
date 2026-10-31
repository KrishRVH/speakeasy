//! Capture-side queue budgets. A stalled audio writer leaves its backlog in the existing PCM
//! buffer; a frame gap permanently closes that stream so replay never invents missing events.

use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use speakeasy_show::{Size, TakeEvent};
use zeroize::Zeroizing;

use super::{FrameRecord, audio};

#[derive(Clone, Copy, Default)]
pub(super) struct Credit {
    pub events: usize,
    pub bytes: usize,
}

pub(super) struct Producer<T> {
    sender: Option<Sender<T>>,
    credits: Receiver<Credit>,
    complete: Receiver<bool>,
    queued: Credit,
    limit: Credit,
}

enum Offered {
    Sent,
    Busy,
    Stopped,
}

impl<T> Producer<T> {
    pub(super) fn new(
        sender: Sender<T>,
        credits: Receiver<Credit>,
        complete: Receiver<bool>,
        events: usize,
        bytes: usize,
    ) -> Self {
        Self {
            sender: Some(sender),
            credits,
            complete,
            queued: Credit::default(),
            limit: Credit { events, bytes },
        }
    }

    fn offer(&mut self, record: T, bytes: usize) -> Offered {
        while let Ok(credit) = self.credits.try_recv() {
            self.queued.events = self.queued.events.saturating_sub(credit.events);
            self.queued.bytes = self.queued.bytes.saturating_sub(credit.bytes);
        }
        let Some(sender) = &self.sender else {
            return Offered::Stopped;
        };
        if sender.is_closed() || self.credits.is_closed() {
            return Offered::Stopped;
        }
        if self.queued.events >= self.limit.events
            || self.queued.bytes.saturating_add(bytes) > self.limit.bytes
        {
            return Offered::Busy;
        }
        match sender.try_send(record) {
            Ok(()) => {
                self.queued.events = self.queued.events.saturating_add(1);
                self.queued.bytes = self.queued.bytes.saturating_add(bytes);
                Offered::Sent
            },
            Err(async_channel::TrySendError::Full(_)) => Offered::Busy,
            Err(async_channel::TrySendError::Closed(_)) => Offered::Stopped,
        }
    }

    fn stop(&mut self) {
        self.sender = None;
    }

    async fn close(mut self) -> bool {
        self.stop();
        self.complete.recv().await.unwrap_or(false)
    }
}

pub(crate) struct FrameLog {
    producer: Producer<FrameRecord>,
    complete: bool,
}

impl FrameLog {
    pub(super) fn new(producer: Producer<FrameRecord>) -> Self {
        Self {
            producer,
            complete: true,
        }
    }

    pub(crate) fn begin(&mut self, budget: usize, surface: Size) -> bool {
        self.offer(FrameRecord::Begin { budget, surface })
    }

    pub(crate) fn send(&mut self, event: TakeEvent) -> bool {
        self.offer(FrameRecord::Event(event))
    }

    pub(crate) async fn movie(
        &mut self,
        started: Instant,
    ) -> Option<speakeasy_platform::MovieTarget> {
        let (returned, target) = async_channel::bounded(1);
        if !self.offer(FrameRecord::Movie { started, returned }) {
            return None;
        }
        target.recv().await.ok()
    }

    fn offer(&mut self, record: FrameRecord) -> bool {
        let bytes = record.bytes();
        if !matches!(self.producer.offer(record, bytes), Offered::Sent) {
            self.complete = false;
            self.producer.stop();
        }
        self.complete
    }

    pub(crate) async fn close(self) -> bool {
        let flushed = self.producer.close().await;
        self.complete && flushed
    }
}

pub(crate) struct AudioLog {
    producer: Producer<audio::Record>,
    offered: usize,
    cues: usize,
    threshold: usize,
    complete: bool,
}

impl AudioLog {
    pub(super) fn new(producer: Producer<audio::Record>) -> Self {
        Self {
            producer,
            offered: 0,
            cues: 0,
            threshold: 1,
            complete: true,
        }
    }

    pub(crate) fn begin(&mut self, rate: u32, start: Duration, language: Box<str>) -> bool {
        // Half a second of mono PCM16, capped so high-rate microphones keep the queue responsive.
        self.threshold = usize::try_from(rate)
            .unwrap_or(usize::MAX)
            .clamp(2, 64 << 10);
        self.send(audio::Record::Begin {
            rate,
            start,
            language,
        })
    }

    /// False means storage stopped. A temporarily full queue returns true: the unsent samples
    /// remain in PCM and the next drain iteration retries them in order.
    pub(crate) fn offer(&mut self, pcm: &[u8], cues: &[(usize, usize)]) -> bool {
        if !self.complete {
            return false;
        }
        self.chunk(pcm, cues, false);
        self.complete
    }

    /// Offers the remaining tail within the queue's fixed budget, then closes without waiting
    /// for storage or device teardown. The session owns the keeper's eventual flush.
    pub(crate) fn finish(mut self, pcm: &[u8], cues: &[(usize, usize)]) -> bool {
        while self.complete && (self.offered < pcm.len() || self.cues < cues.len()) {
            let before = (self.offered, self.cues);
            self.chunk(pcm, cues, true);
            if before == (self.offered, self.cues) {
                self.complete = false;
            }
        }
        self.complete && self.send(audio::Record::End)
    }

    fn chunk(&mut self, pcm: &[u8], cues: &[(usize, usize)], finishing: bool) {
        let pending = pcm.len().saturating_sub(self.offered);
        let end = if finishing || pending >= self.threshold {
            self.offered.saturating_add(pending.min(audio::CHUNK_BYTES))
        } else {
            self.offered
        };
        while let Some(&(before, at)) = cues.get(self.cues) {
            if at > end {
                break;
            }
            if !self.send(audio::Record::Cue { before, at }) {
                return;
            }
            self.cues = self.cues.saturating_add(1);
        }
        if end > self.offered
            && let Some(bytes) = pcm.get(self.offered..end)
            && self.send(audio::Record::Samples(Zeroizing::new(bytes.to_vec())))
        {
            self.offered = end;
        }
    }

    fn send(&mut self, record: audio::Record) -> bool {
        let bytes = record.bytes();
        match self.producer.offer(record, bytes) {
            Offered::Sent => true,
            Offered::Busy => false,
            Offered::Stopped => {
                self.complete = false;
                self.producer.stop();
                false
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{super::Input, *};

    #[test]
    fn a_full_audio_byte_budget_resumes_after_all_inflight_credits_arrive() {
        let (producer, input) = Input::lane(64, 128 + 2 * (1_024 + 64));
        let mut log = AudioLog::new(producer);
        assert!(log.begin(100, Duration::ZERO, "en".into()));
        let pcm = vec![0; 3_072];
        assert!(log.offer(&pcm[..1_024], &[]));
        assert!(log.offer(&pcm[..2_048], &[]));
        while let Ok(record) = input.records.try_recv() {
            input.acknowledge(record.bytes());
        }
        assert!(log.offer(&pcm, &[]));
        assert_eq!(
            log.offered,
            pcm.len(),
            "Completed writes stranded the remaining PCM"
        );
    }

    #[test]
    fn a_stalled_audio_lane_keeps_its_backlog_in_pcm_and_resumes_after_credit() {
        let (producer, input) = Input::lane(2, 1_024);
        let mut log = AudioLog::new(producer);
        assert!(log.begin(100, Duration::ZERO, "en".into()));
        let pcm = [1, 0].repeat(400);
        assert!(log.offer(&pcm[..400], &[]));
        assert_eq!(log.offered, 400);
        assert!(log.offer(&pcm, &[]));
        assert_eq!(log.offered, 400, "A full queue lost its backlog");
        assert!(log.producer.queued.bytes <= 1_024);
        while let Ok(record) = input.records.try_recv() {
            input.acknowledge(record.bytes());
        }
        assert!(log.offer(&pcm, &[]));
        assert_eq!(log.offered, pcm.len());
        while let Ok(record) = input.records.try_recv() {
            input.acknowledge(record.bytes());
        }
        assert!(log.finish(&pcm, &[]));
        assert!(matches!(
            input.records.try_recv().unwrap(),
            audio::Record::End
        ));
    }
}
