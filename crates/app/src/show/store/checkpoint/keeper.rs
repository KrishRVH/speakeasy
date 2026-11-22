mod movie;
mod producers;

use std::{
    convert::Infallible,
    fs::File,
    io::{BufWriter, Write as _},
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

use anyhow::Context as _;
use async_channel::{Receiver, Sender};
use movie::Writing;
pub(crate) use producers::{AudioEnding, AudioLog, AudioTail, FrameLog};
use speakeasy_platform::{MovieTarget, StoppableThread};
use speakeasy_show::{Framing, Size, TakeEvent, log};

use super::{Checkpoint, audio};

const FRAME_EVENTS: usize = 512;
const AUDIO_EVENTS: usize = 64;

pub(crate) struct Kept {
    pub keeper: Keeper,
    pub frames: FrameLog,
    pub audio: AudioLog,
}

pub(crate) struct Keeper {
    thread: StoppableThread,
    closed: Receiver<Option<Checkpoint>>,
}

impl Keeper {
    #[cfg(test)]
    pub(crate) fn fixture(
        parent: &Path,
        recorded: SystemTime,
        budget: usize,
        surface: Size,
        framing: Framing,
    ) -> anyhow::Result<(Self, FrameLog)> {
        let parent = parent.to_owned();
        let Kept {
            keeper, mut frames, ..
        } = Self::start(move || Ok(parent), recorded)?;
        anyhow::ensure!(
            frames.begin(budget, surface, framing),
            "Synthetic frame log did not begin"
        );
        Ok((keeper, frames))
    }

    pub(crate) fn start(
        parent: impl FnOnce() -> anyhow::Result<PathBuf> + Send + 'static,
        recorded: SystemTime,
    ) -> anyhow::Result<Kept> {
        let (frames, frame_records) = async_channel::bounded(FRAME_EVENTS);
        let (audio, audio_records) = async_channel::bounded(AUDIO_EVENTS);
        let (returned, closed) = async_channel::bounded(1);
        let thread = StoppableThread::spawn("show-checkpoint", move |stopping| {
            let frames = Lane::new(frame_records);
            let audio = Lane::new(audio_records);
            let checkpoint = parent()
                .and_then(|parent| Checkpoint::create(&parent, recorded))
                .ok();
            if let Some(checkpoint) = &checkpoint
                && let Ok(runtime) = tokio::runtime::Builder::new_current_thread().build()
            {
                runtime.block_on(write(&checkpoint.directory, frames, audio, &stopping));
            }
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The single-slot lane is empty; a dropped keeper unlocks the checkpoint by dropping it"
            )]
            let _ = returned.try_send(checkpoint);
        })?;
        Ok(Kept {
            keeper: Self { thread, closed },
            frames: FrameLog::new(frames),
            audio: AudioLog::new(audio),
        })
    }

    pub(crate) fn discard(&self) {
        self.thread.request_stop();
    }

    pub(crate) async fn close(self) -> anyhow::Result<Option<Checkpoint>> {
        let returned = self.closed.recv().await;
        returned.context("Show checkpoint stopped before returning its files")
    }
}

enum FrameRecord {
    Begin {
        budget: usize,
        surface: Size,
        framing: Framing,
    },
    Movie {
        started: Instant,
        returned: Sender<MovieTarget>,
    },
    Event(TakeEvent),
}

struct Lane<T> {
    records: Receiver<T>,
    file: Option<BufWriter<File>>,
}

impl<T> Lane<T> {
    const fn new(records: Receiver<T>) -> Self {
        Self {
            records,
            file: None,
        }
    }

    fn begin(&mut self, directory: &Path, name: &str, header: &[u8]) -> anyhow::Result<()> {
        let mut file = BufWriter::new(File::create_new(directory.join(name))?);
        file.write_all(header)?;
        self.file = Some(file);
        Ok(())
    }

    fn append(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.file
            .as_mut()
            .context("Show checkpoint stream has no header")?
            .write_all(bytes)?;
        Ok(())
    }

    fn settle(&mut self) -> anyhow::Result<()> {
        if let Some(file) = &mut self.file
            && self.records.is_empty()
        {
            file.flush()?;
        }
        Ok(())
    }
}

impl<T> Drop for Lane<T> {
    fn drop(&mut self) {
        self.records.close();
        while self.records.try_recv().is_ok() {}
    }
}

async fn write(
    directory: &Path,
    frames: Lane<FrameRecord>,
    audio: Lane<audio::Record>,
    stopping: &Receiver<Infallible>,
) {
    let mut frames = Some(frames);
    let mut audio = Some(audio);
    let mut movie: Option<Writing> = None;
    let mut stopped = false;
    while frames.is_some() || audio.is_some() || movie.is_some() {
        if !stopped && stopping.is_closed() {
            stopped = true;
            frames = None;
            audio = None;
            if let Some(movie) = &movie {
                movie.stop();
            }
            continue;
        }
        tokio::select! {
            _stop = stopping.recv(), if !stopped => {},
            event = movie::next(movie.as_ref()) => match event {
                Some(event) => if let Some(movie) = &mut movie { movie.event(directory, event); },
                None => movie = None,
            },
            record = receive(frames.as_ref()) => {
                consume(&mut frames, record, |lane, record| frame(directory, lane, record, &mut movie));
            },
            record = receive(audio.as_ref()) => {
                consume(&mut audio, record, |lane, record| sound(directory, lane, record));
            },
        }
    }
}

fn consume<T>(
    lane: &mut Option<Lane<T>>,
    record: Result<T, async_channel::RecvError>,
    write: impl FnOnce(&mut Lane<T>, T) -> anyhow::Result<()>,
) {
    let Some(live) = lane.as_mut() else {
        return;
    };
    let kept = record.is_ok_and(|record| write(live, record).and_then(|()| live.settle()).is_ok());
    if !kept {
        *lane = None;
    }
}

async fn receive<T>(lane: Option<&Lane<T>>) -> Result<T, async_channel::RecvError> {
    match lane {
        Some(lane) => lane.records.recv().await,
        None => std::future::pending().await,
    }
}

fn frame(
    directory: &Path,
    lane: &mut Lane<FrameRecord>,
    record: FrameRecord,
    movie: &mut Option<Writing>,
) -> anyhow::Result<()> {
    match record {
        FrameRecord::Begin {
            budget,
            surface,
            framing,
        } => lane.begin(
            directory,
            super::LOG,
            &log::header(budget, surface, framing)?,
        ),
        FrameRecord::Movie { started, returned } => {
            let (writing, target) = Writing::new(directory, started)?;
            *movie = Some(writing);
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A dropped recorder refuses this target and closes its native lease"
            )]
            let _ = returned.try_send(target);
            Ok(())
        },
        FrameRecord::Event(event) => lane.append(&log::encode(&event)?),
    }
}

fn sound(
    directory: &Path,
    lane: &mut Lane<audio::Record>,
    record: audio::Record,
) -> anyhow::Result<()> {
    if matches!(record, audio::Record::Begin { .. }) {
        lane.begin(directory, audio::FILE, audio::MAGIC)?;
    }
    lane.append(&record.encode()?)
}

#[cfg(test)]
mod tests;
