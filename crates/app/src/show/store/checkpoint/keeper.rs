//! One owner retains the checkpoint lock until its streams and native movie writer close.
//! Separate bounded lanes isolate storage failures; credits keep queue accounting with producers.

mod movie;
mod producers;

use std::{
    convert::Infallible,
    fs::File,
    io::{BufWriter, Write as _},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use anyhow::Context as _;
use async_channel::{Receiver, Sender};
use movie::Writing;
pub(crate) use producers::{AudioLog, FrameLog};
use producers::{Credit, Producer};
use speakeasy_platform::{MovieTarget, StoppableThread};
use speakeasy_show::{Log, Size, TakeEvent};

use super::{Checkpoint, audio};

const FRAME_EVENTS: usize = 512;
const FRAME_BYTES: usize = 64 << 20;
const AUDIO_EVENTS: usize = 64;
const AUDIO_BYTES: usize = 8 << 20;
const SYNC_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) struct Kept {
    pub keeper: Keeper,
    pub frames: FrameLog,
    pub audio: AudioLog,
}

pub(crate) struct Keeper {
    // Emergency drop stops and joins before the returned checkpoint's receiver can release its lock.
    thread: StoppableThread,
    closed: Receiver<anyhow::Result<Checkpoint>>,
}

impl Keeper {
    #[cfg(test)]
    pub(crate) fn fixture(
        parent: &Path,
        recorded: SystemTime,
        budget: usize,
        surface: Size,
    ) -> anyhow::Result<(Self, FrameLog)> {
        let parent = parent.to_owned();
        let Kept {
            keeper,
            mut frames,
            audio,
        } = Self::start(move || Ok(parent), recorded)?;
        drop(audio);
        anyhow::ensure!(
            frames.begin(budget, surface),
            "Synthetic frame log did not begin"
        );
        Ok((keeper, frames))
    }

    pub(crate) fn start(
        parent: impl FnOnce() -> anyhow::Result<PathBuf> + Send + 'static,
        recorded: SystemTime,
    ) -> anyhow::Result<Kept> {
        let (frames, frame_input) = Input::lane(FRAME_EVENTS, FRAME_BYTES);
        let (audio, audio_input) = Input::lane(AUDIO_EVENTS, AUDIO_BYTES);
        let (returned, closed) = async_channel::bounded(1);
        let thread = StoppableThread::spawn("show-checkpoint", move |stopping| {
            let outcome = (|| {
                let checkpoint = Checkpoint::create(&parent()?, recorded)?;
                tokio::runtime::Builder::new_current_thread()
                    .build()?
                    .block_on(write(
                        &checkpoint.directory,
                        frame_input,
                        audio_input,
                        stopping,
                    ));
                anyhow::Ok(checkpoint)
            })();
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Emergency teardown may drop the receiver; dropping this result unlocks the retained checkpoint only after all writes stop"
            )]
            let _ = returned.try_send(outcome);
        })?;
        Ok(Kept {
            keeper: Self { thread, closed },
            frames: FrameLog::new(frames),
            audio: AudioLog::new(audio),
        })
    }

    /// Resolves after both producers close and their last writes are flushed. The returned value
    /// is the sole authority that can publish, retain, or discard these files.
    pub(crate) async fn close(self) -> anyhow::Result<Checkpoint> {
        self.thread.stopped().await;
        tokio::task::spawn_blocking(move || {
            let Self { thread, closed } = self;
            drop(thread);
            closed
                .try_recv()
                .context("Show checkpoint stopped before returning its files")?
        })
        .await
        .unwrap_or_else(|_private_panic| {
            Err(anyhow::anyhow!("Show checkpoint stopped unexpectedly"))
        })
    }
}

enum FrameRecord {
    Begin {
        budget: usize,
        surface: Size,
    },
    Movie {
        started: Instant,
        returned: Sender<MovieTarget>,
    },
    Event(TakeEvent),
}

impl FrameRecord {
    fn bytes(&self) -> usize {
        match self {
            Self::Begin { .. } | Self::Movie { .. } => 64,
            Self::Event(event) => event.bytes(),
        }
    }
}

struct Input<T> {
    records: Receiver<T>,
    credits: Sender<Credit>,
    flushed: Sender<bool>,
    storage: Storage,
}

impl<T> Input<T> {
    fn lane(events: usize, bytes: usize) -> (Producer<T>, Self) {
        let (outgoing, records) = async_channel::bounded(events);
        let (credits, available) = async_channel::bounded(events);
        let (flushed, complete) = async_channel::bounded(1);
        (
            Producer::new(outgoing, available, complete, events, bytes),
            Self {
                records,
                credits,
                flushed,
                storage: Storage::default(),
            },
        )
    }

    fn acknowledge(&self, bytes: usize) {
        // Every outstanding record owns one credit slot. Sending each credit separately lets a
        // producer recover its entire byte budget even when no further record can be enqueued.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The event budget guarantees a credit slot; a closed lane means its producer has left"
        )]
        let _ = self.credits.try_send(Credit { events: 1, bytes });
    }

    fn finish(mut self, complete: bool) {
        let synced = self.storage.flush();
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Only a producer explicitly closing its lane waits for a flush receipt"
        )]
        let _ = self.flushed.try_send(complete && synced.is_ok());
    }
}

async fn write(
    directory: &Path,
    frames: Input<FrameRecord>,
    audio: Input<audio::Record>,
    stopping: Receiver<Infallible>,
) {
    let mut frames = Some(frames);
    let mut audio = Some(audio);
    let mut movie: Option<Writing> = None;
    let mut stopped = false;
    while frames.is_some() || audio.is_some() || movie.is_some() {
        tokio::select! {
            _stop = stopping.recv(), if !stopped => {
                stopped = true;
                if let Some(input) = frames.take() { input.finish(false); }
                if let Some(input) = audio.take() { input.finish(false); }
                if let Some(movie) = &movie { movie.stop(); }
            },
            event = movie::next(movie.as_ref()) => {
                match event {
                    Some(event) => if let Some(movie) = &mut movie { movie.event(directory, event); },
                    None => movie = None,
                }
            },
            record = receive(frames.as_ref()) => {
                consume(&mut frames, record, |input, record| frame(directory, input, record, &mut movie));
            },
            record = receive(audio.as_ref()) => {
                consume(&mut audio, record, |input, record| sound(directory, input, record));
            },
        }
    }
}

fn consume<T>(
    input: &mut Option<Input<T>>,
    record: Result<T, async_channel::RecvError>,
    write: impl FnOnce(&mut Input<T>, T) -> anyhow::Result<()>,
) {
    let Some(live) = input.as_mut() else {
        return;
    };
    let finished = match record {
        Ok(record) => write(live, record).err().map(|_| false),
        Err(_) => Some(true),
    };
    if let Some(complete) = finished
        && let Some(input) = input.take()
    {
        input.finish(complete);
    }
}

async fn receive<T>(input: Option<&Input<T>>) -> Result<T, async_channel::RecvError> {
    match input {
        Some(input) => input.records.recv().await,
        None => std::future::pending().await,
    }
}

fn frame(
    directory: &Path,
    input: &mut Input<FrameRecord>,
    record: FrameRecord,
    movie: &mut Option<Writing>,
) -> anyhow::Result<()> {
    let bytes = record.bytes();
    match record {
        FrameRecord::Begin { budget, surface } => {
            input
                .storage
                .begin(directory, super::LOG, &Log::header(budget, surface)?)?;
        },
        FrameRecord::Movie { started, returned } => {
            anyhow::ensure!(
                input.storage.file.is_some() && movie.is_none(),
                "Movie needs a new checkpoint stream"
            );
            let (writing, target) = Writing::new(directory, started)?;
            *movie = Some(writing);
            // A cancelled recorder drops the returned target; the independent lane still waits
            // for that release before the checkpoint can unlock.
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A dropped recorder refuses this target and closes its native lease"
            )]
            let _ = returned.try_send(target);
        },
        FrameRecord::Event(event) => {
            input
                .storage
                .append(&Log::encode(&event)?, super::FILE_LIMIT, super::EVENT_LIMIT)?;
        },
    }
    input.storage.drain(input.records.is_empty())?;
    input.acknowledge(bytes);
    Ok(())
}

fn sound(
    directory: &Path,
    input: &mut Input<audio::Record>,
    record: audio::Record,
) -> anyhow::Result<()> {
    if matches!(record, audio::Record::Begin { .. }) {
        input.storage.begin(directory, audio::FILE, audio::MAGIC)?;
    }
    input
        .storage
        .append(&record.encode()?, audio::FILE_LIMIT, audio::MAX_RECORDS)?;
    input.storage.drain(input.records.is_empty())?;
    input.acknowledge(record.bytes());
    Ok(())
}

struct Storage {
    file: Option<BufWriter<File>>,
    size: u64,
    records: usize,
    synced: Instant,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            file: None,
            size: 0,
            records: 0,
            synced: Instant::now(),
        }
    }
}

impl Storage {
    fn begin(&mut self, directory: &Path, name: &str, header: &[u8]) -> anyhow::Result<()> {
        anyhow::ensure!(self.file.is_none(), "Show checkpoint stream began twice");
        let mut file = File::create_new(directory.join(name))?;
        file.write_all(header)?;
        file.sync_all()?;
        super::sync_directory(directory)?;
        self.size = u64::try_from(header.len())?;
        self.file = Some(BufWriter::new(file));
        Ok(())
    }

    fn append(&mut self, bytes: &[u8], limit: u64, records: usize) -> anyhow::Result<()> {
        self.size = self.size.saturating_add(u64::try_from(bytes.len())?);
        self.records = self.records.saturating_add(1);
        anyhow::ensure!(
            self.size <= limit && self.records <= records,
            "Show checkpoint reached its storage limit"
        );
        self.file
            .as_mut()
            .context("Show checkpoint stream has no header")?
            .write_all(bytes)?;
        Ok(())
    }

    fn drain(&mut self, empty: bool) -> anyhow::Result<()> {
        if self.synced.elapsed() >= SYNC_INTERVAL {
            self.flush()?;
            self.synced = Instant::now();
        } else if empty && let Some(file) = &mut self.file {
            file.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> anyhow::Result<()> {
        if let Some(file) = &mut self.file {
            file.flush()?;
            file.get_ref().sync_data()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
