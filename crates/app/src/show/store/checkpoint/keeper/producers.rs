use std::time::{Duration, Instant};

use async_channel::{Sender, TrySendError};
use speakeasy_show::{Framing, Size, TakeEvent};

use super::{FrameRecord, audio};

pub(crate) struct FrameLog {
    lane: Option<Sender<FrameRecord>>,
}

impl FrameLog {
    pub(super) const fn new(lane: Sender<FrameRecord>) -> Self {
        Self { lane: Some(lane) }
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
        let sent = self
            .lane
            .as_ref()
            .is_some_and(|lane| lane.try_send(record).is_ok());
        if !sent {
            self.lane = None;
        }
        sent
    }
}

#[derive(Clone, Copy)]
pub(crate) enum AudioEnding {
    Complete,
    Interrupted,
}

pub(crate) struct AudioLog {
    lane: Option<Sender<audio::Record>>,
    offered: usize,
    cues: usize,
    threshold: usize,
}

impl AudioLog {
    pub(super) const fn new(lane: Sender<audio::Record>) -> Self {
        Self {
            lane: Some(lane),
            offered: 0,
            cues: 0,
            threshold: 1,
        }
    }

    pub(crate) fn begin(&mut self, rate: u32, start: Duration, language: Box<str>) -> bool {
        self.threshold = usize::try_from(rate)
            .unwrap_or(usize::MAX)
            .clamp(2, 64 << 10);
        self.send(audio::Record::Begin {
            rate,
            start,
            language,
        })
    }

    pub(crate) fn offer(&mut self, pcm: &[u8], cues: &[(usize, usize)]) -> bool {
        if self.lane.is_some() {
            self.chunk(pcm, cues);
        }
        self.lane.is_some()
    }

    pub(crate) fn close(
        self,
        pcm: &[u8],
        cues: &[(usize, usize)],
        ending: AudioEnding,
    ) -> AudioTail {
        let cues = cues
            .get(self.cues..)
            .unwrap_or_default()
            .iter()
            .map(|&(before, at)| audio::Record::Cue { before, at });
        let samples = pcm
            .get(self.offered..)
            .unwrap_or_default()
            .chunks(audio::CHUNK_BYTES)
            .map(|chunk| audio::Record::Samples(chunk.to_vec()));
        let end = matches!(ending, AudioEnding::Complete).then_some(audio::Record::End);
        AudioTail {
            lane: self.lane,
            records: cues.chain(samples).chain(end).collect(),
        }
    }

    #[cfg(test)]
    pub(crate) fn finish(self, pcm: &[u8], cues: &[(usize, usize)], ending: AudioEnding) {
        self.close(pcm, cues, ending).flush();
    }

    fn chunk(&mut self, pcm: &[u8], cues: &[(usize, usize)]) {
        let pending = pcm.len().saturating_sub(self.offered);
        let end = if pending >= self.threshold {
            self.offered.saturating_add(pending.min(audio::CHUNK_BYTES))
        } else {
            self.offered
        };
        while let Some(&(before, at)) = cues.get(self.cues).filter(|&&(_, at)| at <= end) {
            if !self.send(audio::Record::Cue { before, at }) {
                return;
            }
            self.cues = self.cues.saturating_add(1);
        }
        if end > self.offered
            && let Some(bytes) = pcm.get(self.offered..end)
            && self.send(audio::Record::Samples(bytes.to_vec()))
        {
            self.offered = end;
        }
    }

    fn send(&mut self, record: audio::Record) -> bool {
        match self.lane.as_ref().map(|lane| lane.try_send(record)) {
            Some(Ok(())) => true,
            Some(Err(TrySendError::Full(_))) => false,
            Some(Err(TrySendError::Closed(_))) | None => {
                self.lane = None;
                false
            },
        }
    }
}

pub(crate) struct AudioTail {
    lane: Option<Sender<audio::Record>>,
    records: Vec<audio::Record>,
}

impl AudioTail {
    pub(crate) fn flush(self) {
        let Some(lane) = self.lane else {
            return;
        };
        for record in self.records {
            if lane.send_blocking(record).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_audio_lane_keeps_its_backlog_in_pcm_and_resumes() {
        let (lane, records) = async_channel::bounded(2);
        let mut log = AudioLog::new(lane);
        assert!(log.begin(100, Duration::ZERO, "en".into()));
        let pcm = [1, 0].repeat(400);
        assert!(log.offer(&pcm[..400], &[]));
        assert_eq!(log.offered, 400);
        assert!(log.offer(&pcm, &[]));
        assert_eq!(log.offered, 400, "A full lane lost its backlog");
        while records.try_recv().is_ok() {}
        assert!(log.offer(&pcm, &[]));
        assert_eq!(log.offered, pcm.len());
        while records.try_recv().is_ok() {}
        log.finish(&pcm, &[], AudioEnding::Complete);
        assert!(matches!(records.try_recv(), Ok(audio::Record::End)));
    }
}
