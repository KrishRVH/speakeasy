use std::{
    fs::{File, OpenOptions},
    io::Write as _,
    os::unix::fs::OpenOptionsExt as _,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::{Mutex, MutexGuard, PoisonError},
};

use anyhow::{Context as _, ensure};
use objc2::{AllocAnyThread as _, DefinedClass as _, define_class, msg_send, rc::Retained};
use objc2_av_foundation::{AVAssetSegmentType, AVAssetWriter, AVAssetWriterDelegate};
use objc2_foundation::{NSData, NSObject, NSObjectProtocol};

struct Sink {
    file: Option<File>,
    bytes: u64,
    limit: u64,
    progress: Progress,
}

#[derive(PartialEq, Eq)]
enum Progress {
    Uninitialized,
    Initialized,
    Failed,
}

pub(super) struct State {
    #[expect(
        clippy::disallowed_types,
        reason = "Native segment callbacks and owner teardown share one file lease; the lock serializes writes and acknowledges closure"
    )]
    sink: Mutex<Sink>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[ivars = State]
    pub(super) struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl AVAssetWriterDelegate for Output {
        #[unsafe(method(assetWriter:didOutputSegmentData:segmentType:))]
        fn segment(&self, _: &AVAssetWriter, data: &NSData, _: AVAssetSegmentType) {
            let mut sink = self.sink();
            if !matches!(
                catch_unwind(AssertUnwindSafe(|| sink.write(data))),
                Ok(Ok(()))
            ) {
                sink.progress = Progress::Failed;
            }
        }
    }
);

impl Output {
    #[expect(
        clippy::disallowed_types,
        reason = "The synchronized file lease serializes native callbacks with explicit owner closure"
    )]
    pub(super) fn new(path: &Path, limit: u64) -> anyhow::Result<Retained<Self>> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        let this = Self::alloc().set_ivars(State {
            sink: Mutex::new(Sink {
                file: Some(file),
                bytes: 0,
                limit,
                progress: Progress::Uninitialized,
            }),
        });

        Ok(unsafe { msg_send![super(this), init] })
    }

    pub(super) fn check(&self) -> anyhow::Result<()> {
        let sink = self.sink();
        ensure!(
            sink.progress != Progress::Failed && sink.file.is_some(),
            "Movie segment output failed"
        );
        drop(sink);
        Ok(())
    }

    pub(super) fn close(&self) -> anyhow::Result<()> {
        let mut sink = self.sink();
        sink.file.take().context("Movie output already closed")?;
        ensure!(
            sink.progress == Progress::Initialized,
            "Movie segment output failed"
        );
        drop(sink);
        Ok(())
    }

    pub(super) fn retire(&self) {
        self.sink().file = None;
    }

    fn sink(&self) -> MutexGuard<'_, Sink> {
        self.ivars()
            .sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(super) fn file_identity(&self) -> (std::os::fd::RawFd, std::fs::Metadata) {
        use std::os::fd::AsRawFd as _;
        let sink = self.sink();
        let file = sink.file.as_ref().unwrap();
        let identity = (file.as_raw_fd(), file.metadata().unwrap());
        drop(sink);
        identity
    }
}

impl Sink {
    fn write(&mut self, data: &NSData) -> anyhow::Result<()> {
        ensure!(self.progress != Progress::Failed, "Movie output has failed");
        let end = self
            .bytes
            .checked_add(u64::try_from(data.length())?)
            .filter(|end| *end <= self.limit)
            .context("The movie reached its storage limit")?;
        let file = self.file.as_mut().context("Movie output has closed")?;

        file.write_all(unsafe { data.as_bytes_unchecked() })?;
        self.bytes = end;
        self.progress = Progress::Initialized;
        Ok(())
    }
}
