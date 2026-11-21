use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender};
use speakeasy_show::{Framing, Size, TakeEvent};
use zeroize::Zeroizing;

use super::{FrameRecord, audio};

const FINISH_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Default)]
pub(super) struct Credit {
    pub events: usize,
    pub bytes: usize,
}

pub(super) struct Producer<T> {
    sender: Option<Sender<T>>,
    credits: mpsc::Receiver<Credit>,
    complete: Receiver<bool>,
    queued: Credit,
    limit: Credit,
}

enum Offered<T> {
    Sent,
    Busy(T),
    Stopped,
}

impl<T> Producer<T> {
    pub(super) fn new(
        sender: Sender<T>,
        credits: mpsc::Receiver<Credit>,
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

    fn offer(&mut self, record: T, bytes: usize) -> Offered<T> {
        loop {
            match self.credits.try_recv() {
                Ok(credit) => self.settle(credit),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Offered::Stopped,
            }
        }
        let Some(sender) = &self.sender else {
            return Offered::Stopped;
        };
        if sender.is_closed() || bytes > self.limit.bytes {
            return Offered::Stopped;
        }
        if self.queued.events >= self.limit.events
            || self.queued.bytes.saturating_add(bytes) > self.limit.bytes
        {
            return Offered::Busy(record);
        }
        match sender.try_send(record) {
            Ok(()) => {
                self.queued.events = self.queued.events.saturating_add(1);
                self.queued.bytes = self.queued.bytes.saturating_add(bytes);
                Offered::Sent
            },
            Err(async_channel::TrySendError::Full(record)) => Offered::Busy(record),
            Err(async_channel::TrySendError::Closed(_)) => Offered::Stopped,
        }
    }

    fn offer_until(&mut self, mut record: T, bytes: usize, deadline: Instant) -> Offered<T> {
        loop {
            match self.offer(record, bytes) {
                Offered::Busy(pending) => {
                    let Ok(credit) = self
                        .credits
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    else {
                        return Offered::Stopped;
                    };
                    self.settle(credit);
                    record = pending;
                },
                offered => return offered,
            }
        }
    }

    fn settle(&mut self, credit: Credit) {
        self.queued.events = self.queued.events.saturating_sub(credit.events);
        self.queued.bytes = self.queued.bytes.saturating_sub(credit.bytes);
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

    pub(crate) fn begin(&mut self, budget: usize, surface: Size, framing: Framing) -> bool {
        self.offer(FrameRecord::Begin {
            budget,
            surface,
            framing,
        })
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

#[derive(Clone, Copy)]
pub(crate) enum AudioEnding {
    Complete,
    Interrupted,
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
        self.threshold = usize::try_from(rate)
            .unwrap_or(usize::MAX)
            .clamp(2, 64 << 10);
        self.send(
            audio::Record::Begin {
                rate,
                start,
                language,
            },
            None,
        )
    }

    pub(crate) fn offer(&mut self, pcm: &[u8], cues: &[(usize, usize)]) -> bool {
        if !self.complete {
            return false;
        }
        self.chunk(pcm, cues, None);
        self.complete
    }

    pub(crate) fn finish(
        mut self,
        pcm: &[u8],
        cues: &[(usize, usize)],
        ending: AudioEnding,
    ) -> bool {
        let now = Instant::now();
        let deadline = now.checked_add(FINISH_WAIT).unwrap_or(now);
        while self.complete && (self.offered < pcm.len() || self.cues < cues.len()) {
            let before = (self.offered, self.cues);
            self.chunk(pcm, cues, Some(deadline));
            if before == (self.offered, self.cues) {
                self.complete = false;
            }
        }
        self.complete
            && match ending {
                AudioEnding::Complete => self.send(audio::Record::End, Some(deadline)),
                AudioEnding::Interrupted => true,
            }
    }

    fn chunk(&mut self, pcm: &[u8], cues: &[(usize, usize)], deadline: Option<Instant>) {
        let pending = pcm.len().saturating_sub(self.offered);
        let end = if deadline.is_some() || pending >= self.threshold {
            self.offered.saturating_add(pending.min(audio::CHUNK_BYTES))
        } else {
            self.offered
        };
        while let Some(&(before, at)) = cues.get(self.cues) {
            if at > end {
                break;
            }
            if !self.send(audio::Record::Cue { before, at }, deadline) {
                return;
            }
            self.cues = self.cues.saturating_add(1);
        }
        if end > self.offered
            && let Some(bytes) = pcm.get(self.offered..end)
            && self.send(
                audio::Record::Samples(Zeroizing::new(bytes.to_vec())),
                deadline,
            )
        {
            self.offered = end;
        }
    }

    fn send(&mut self, record: audio::Record, deadline: Option<Instant>) -> bool {
        let bytes = record.bytes();
        let offered = match deadline {
            Some(deadline) => self.producer.offer_until(record, bytes, deadline),
            None => self.producer.offer(record, bytes),
        };
        match offered {
            Offered::Sent => true,
            Offered::Busy(_) => false,
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
        assert!(log.finish(&pcm, &[], AudioEnding::Complete));
        assert!(matches!(
            input.records.try_recv().unwrap(),
            audio::Record::End
        ));
    }
}
