//! A bounded writer owned by the recording thread. Byte credits cross a channel, so queue
//! accounting stays with the recorder and never needs an atomic, lock, or callback-side wait.

use std::{
    fs::File,
    io::{BufWriter, Write as _},
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender};
use speakeasy_platform::OwnedThread;
use speakeasy_show::{Log, TakeEvent};

const QUEUED_BYTES: usize = 64 << 20;
const QUEUED_EVENTS: usize = 512;
const SYNC_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) struct Writer {
    // Closing this lane precedes joining, including when the recorder unwinds.
    lane: Option<Lane>,
    result: Receiver<anyhow::Result<()>>,
    thread: OwnedThread,
    complete: bool,
}

struct Lane {
    events: Sender<TakeEvent>,
    written: Receiver<usize>,
    queued: usize,
}

impl Writer {
    pub(in crate::show) fn start(file: File) -> anyhow::Result<Self> {
        Self::spawn(move |incoming, written| write(file, &incoming, &written))
    }

    fn spawn(
        work: impl FnOnce(Receiver<TakeEvent>, Sender<usize>) -> anyhow::Result<()> + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (events, incoming) = async_channel::bounded(QUEUED_EVENTS);
        let (written, credits) = async_channel::bounded(1);
        let (done, result) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("show-checkpoint", move || {
            let result = work(incoming, written);
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The receiver belongs to the recording that owns this thread; it can leave during unwinding"
            )]
            let _ = done.send_blocking(result);
        })?;
        Ok(Self {
            lane: Some(Lane {
                events,
                written: credits,
                queued: 0,
            }),
            result,
            thread,
            complete: true,
        })
    }

    /// False means checkpointing stopped at a complete prefix. The caller shows one notice and
    /// continues recording in memory; it must never resume this log after an omitted event.
    pub(crate) fn send(&mut self, event: TakeEvent) -> bool {
        let Some(lane) = &mut self.lane else {
            return false;
        };
        while let Ok(bytes) = lane.written.try_recv() {
            lane.queued = lane.queued.saturating_sub(bytes);
        }
        let bytes = event.bytes();
        if lane.queued.saturating_add(bytes) > QUEUED_BYTES || lane.events.try_send(event).is_err()
        {
            self.lane = None;
            self.complete = false;
            return false;
        }
        lane.queued = lane.queued.saturating_add(bytes);
        true
    }

    pub(crate) async fn close(mut self) -> bool {
        self.lane = None;
        self.thread.join().await;
        self.complete && matches!(self.result.try_recv(), Ok(Ok(())))
    }
}

fn write(
    file: File,
    incoming: &Receiver<TakeEvent>,
    written: &Sender<usize>,
) -> anyhow::Result<()> {
    let mut file = BufWriter::new(file);
    let mut size = u64::try_from(Log::HEADER_BYTES)?;
    let mut events = 0_usize;
    let mut done = 0_usize;
    let mut synced = Instant::now();
    let result = (|| {
        while let Ok(event) = incoming.recv_blocking() {
            let record = Log::encode(&event)?;
            size = size.saturating_add(u64::try_from(record.len())?);
            events = events.saturating_add(1);
            anyhow::ensure!(
                size <= super::FILE_LIMIT && events <= super::EVENT_LIMIT,
                "Show checkpoint reached its storage limit"
            );
            file.write_all(&record)?;
            done = done.saturating_add(event.bytes());
            if written.try_send(done).is_ok() {
                done = 0;
            }
            if incoming.is_empty() {
                file.flush()?;
            }
            if synced.elapsed() >= SYNC_INTERVAL {
                file.flush()?;
                file.get_ref().sync_data()?;
                synced = Instant::now();
            }
        }
        anyhow::Ok(())
    })();
    // Even a limit or failed record write leaves the preceding complete prefix worth recovering.
    file.flush()?;
    file.get_ref().sync_data()?;
    result
}

#[cfg(test)]
mod tests {
    use speakeasy_show::{Moment, Point};

    use super::*;

    fn pointer() -> TakeEvent {
        TakeEvent::Pointer {
            at: Moment::START,
            point: Point::new(0.5, 0.5),
            drawing: false,
            pressed: false,
        }
    }

    #[tokio::test]
    async fn a_blocked_writer_keeps_only_a_bounded_prefix_and_never_resumes_after_a_gap()
    -> anyhow::Result<()> {
        let (release, held) = async_channel::bounded(1);
        let (counted, count) = async_channel::bounded(1);
        let mut writer = Writer::spawn(move |incoming, _written| {
            held.recv_blocking()?;
            let mut received = 0_usize;
            while incoming.recv_blocking().is_ok() {
                received = received.saturating_add(1);
            }
            counted.try_send(received)?;
            Ok(())
        })?;
        for _ in 0..QUEUED_EVENTS {
            assert!(writer.send(pointer()));
        }
        assert!(!writer.send(pointer()));
        release.try_send(())?;
        writer.thread.exited().await;
        assert!(
            !writer.send(pointer()),
            "A recovered queue must not append beyond an omitted event"
        );
        assert!(!writer.close().await);
        assert_eq!(count.recv().await?, QUEUED_EVENTS);
        Ok(())
    }

    #[tokio::test]
    async fn closing_a_writer_waits_for_its_last_storage_acknowledgement() -> anyhow::Result<()> {
        let (release, held) = async_channel::bounded(1);
        let writer = Writer::spawn(move |_, _| {
            held.recv_blocking()?;
            Ok(())
        })?;
        let closing = writer.close();
        tokio::pin!(closing);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut closing)
                .await
                .is_err()
        );
        release.try_send(())?;
        assert!(closing.await);
        Ok(())
    }
}
