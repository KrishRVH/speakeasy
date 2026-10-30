//! Show-only handoff on the drain thread. Capture callbacks never touch files or this queue.

use std::time::Duration;

use async_channel::{Receiver, Sender};

use super::speech::Audibility;
use crate::{
    runtime::ports::{CaptureEvent, Sealed, SessionId},
    show::{AudioLog, checkpoint::KeptAudio},
};

pub(super) struct Keep {
    pub log: AudioLog,
    pub start: Duration,
    pub language: Box<str>,
}

pub(super) struct Keeping<'a> {
    incoming: &'a Receiver<Keep>,
    log: Option<AudioLog>,
    // Cue history stays in memory until promotion so its earlier samples replay the same gate.
    cues: Vec<(usize, usize)>,
    classified: usize,
    overflowed: bool,
    warned: bool,
}

impl<'a> Keeping<'a> {
    pub(super) const fn new(incoming: &'a Receiver<Keep>) -> Self {
        Self {
            incoming,
            log: None,
            cues: Vec::new(),
            classified: 0,
            overflowed: false,
            warned: false,
        }
    }

    pub(super) fn classify(&mut self, speech: &mut Audibility, pcm: &[u8], cues: &Receiver<()>) {
        while cues.try_recv().is_ok() {
            speech.expect_cue(pcm);
            if self.cues.len() < crate::show::checkpoint::MAX_CUES {
                self.cues.push((self.classified, pcm.len()));
            } else {
                self.overflowed = true;
            }
        }
        speech.extend(pcm);
        self.classified = pcm.len();
    }

    pub(super) fn offer(&mut self, pcm: &[u8], rate: u32) -> bool {
        if let Ok(Keep {
            mut log,
            start,
            language,
        }) = self.incoming.try_recv()
        {
            self.incoming.close();
            if self.overflowed || !log.begin(rate, start, language) {
                return false;
            }
            self.log = Some(log);
        }
        if self.overflowed && self.log.take().is_some() {
            return false;
        }
        self.log
            .as_mut()
            .is_none_or(|log| log.offer(pcm, &self.cues))
    }

    pub(super) fn finish(&mut self, pcm: &[u8], rate: u32) -> bool {
        // Close before the final receive: every accepted handoff is now queued, so none can
        // arrive after the last offer and leave a producer waiting through device teardown.
        self.incoming.close();
        let offered = self.offer(pcm, rate);
        let finished = self
            .log
            .take()
            .is_none_or(|log| log.finish(pcm, &self.cues));
        offered && finished
    }

    pub(super) fn warn(&mut self, id: SessionId, events: &Sender<CaptureEvent>) {
        if !self.warned {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A full event lane cannot stall capture; later publication still preserves its live in-memory recording"
            )]
            let _ = events.try_send(CaptureEvent::CheckpointLost(id));
            self.warned = true;
        }
    }
}

/// Replays cue exclusions before sealing through the same speech gate and trimming as capture.
/// A torn log can produce only its valid prefix; no missing samples or word times are invented.
pub(crate) fn seal_kept(audio: KeptAudio) -> anyhow::Result<(Option<Sealed>, Duration, Box<str>)> {
    let KeptAudio {
        rate,
        start,
        language,
        pcm,
        cues,
        complete: _,
    } = audio;
    let pcm = super::pcm::Pcm16::from_kept(pcm);
    let mut speech = Audibility::new(rate);
    for (before, at) in cues {
        if let (Some(previous), Some(prefix)) = (pcm.audio().get(..before), pcm.audio().get(..at)) {
            speech.extend(previous);
            // During capture the cue masks the just-drained tail before that tail is classified.
            speech.expect_cue(prefix);
            speech.extend(prefix);
        }
    }
    speech.extend(pcm.audio());
    Ok((pcm.seal(speech, rate)?, start, language))
}

#[cfg(test)]
mod tests;
