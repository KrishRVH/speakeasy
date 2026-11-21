use std::{convert::Infallible, io, thread};

#[derive(Debug)]
pub struct OwnedThread {
    exit: async_channel::Sender<Infallible>,
    thread: thread::Thread,
    handle: Option<thread::JoinHandle<()>>,
}

impl OwnedThread {
    pub fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> io::Result<Self> {
        let (exit, alive) = async_channel::bounded::<Infallible>(1);
        let handle = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let _alive = alive;
                body();
            })?;
        Ok(Self {
            exit,
            thread: handle.thread().clone(),
            handle: Some(handle),
        })
    }

    #[must_use]
    pub const fn thread(&self) -> &thread::Thread {
        &self.thread
    }

    pub fn exited(&self) -> impl Future<Output = ()> + Send + use<> {
        let exit = self.exit.clone();
        async move { exit.closed().await }
    }

    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.exit.is_closed()
    }

    pub async fn join(self) -> io::Result<()> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_runtime_unavailable| {
            io::Error::other("Joining a worker requires an active Tokio runtime")
        })?;
        self.exited().await;
        runtime
            .spawn_blocking(move || self.join_blocking())
            .await
            .map_err(|_join_failed| io::Error::other("The worker join task did not finish"))?
    }

    fn join_blocking(mut self) -> io::Result<()> {
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_private_panic| io::Error::other("The worker thread panicked"))?;
        }
        Ok(())
    }
}

impl Drop for OwnedThread {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A panicking thread has already unwound its resources; a destructor must not resume the panic"
            )]
            let _ = handle.join();
        }
    }
}

#[derive(Debug)]
pub struct StoppableThread {
    stop: async_channel::Sender<Infallible>,
    thread: OwnedThread,
}

impl StoppableThread {
    pub fn spawn(
        name: &str,
        body: impl FnOnce(async_channel::Receiver<Infallible>) + Send + 'static,
    ) -> io::Result<Self> {
        let (stop, stopping) = async_channel::bounded(1);
        let thread = OwnedThread::spawn(name, move || body(stopping))?;
        Ok(Self { stop, thread })
    }

    pub fn request_stop(&self) {
        self.stop.close();
    }

    pub fn stopped(&self) -> impl Future<Output = ()> + Send + use<> {
        self.thread.exited()
    }

    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }
}

impl Drop for StoppableThread {
    fn drop(&mut self) {
        self.request_stop();
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use super::{OwnedThread, StoppableThread};
    use crate::PATIENCE;

    #[tokio::test]
    async fn exit_is_observed_once_the_body_returns() -> anyhow::Result<()> {
        let (release, released) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("returns", move || released.recv_blocking().unwrap())?;
        let finished_while_held = thread.is_finished();
        release.send(()).await?;
        tokio::time::timeout(PATIENCE, thread.exited()).await?;

        assert!(!finished_while_held);
        assert!(thread.is_finished());
        Ok(())
    }

    #[tokio::test]
    async fn exit_is_observed_after_the_body_panics() -> anyhow::Result<()> {
        let thread = OwnedThread::spawn("panics", || panic!("owned thread test panic"))?;
        tokio::time::timeout(PATIENCE, thread.exited()).await?;
        assert!(thread.is_finished());
        let error = thread.join().await.unwrap_err();
        assert_eq!(error.to_string(), "The worker thread panicked");
        assert!(!format!("{error:?}").contains("owned thread test panic"));
        Ok(())
    }

    #[tokio::test]
    async fn joining_resolves_only_once_the_body_returns() -> anyhow::Result<()> {
        let (release, released) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("joins later", move || released.recv_blocking().unwrap())?;
        let joined = tokio::spawn(thread.join());
        tokio::time::sleep(Duration::from_millis(30)).await;
        let early = joined.is_finished();
        release.send(()).await?;
        tokio::time::timeout(PATIENCE, joined).await???;
        assert!(!early, "Joining resolved while the body still ran");
        Ok(())
    }

    #[test]
    fn joining_without_a_runtime_reports_failure_after_cleanup() -> anyhow::Result<()> {
        use std::task::{Context, Poll, Waker};

        let (finished, observed) = mpsc::channel();
        let thread = OwnedThread::spawn("joins without runtime", move || {
            finished.send(()).unwrap();
        })?;
        let mut joining = std::pin::pin!(thread.join());
        let Poll::Ready(Err(error)) = joining
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        else {
            panic!("Joining without a runtime did not return an error");
        };
        assert_eq!(
            error.to_string(),
            "Joining a worker requires an active Tokio runtime"
        );
        assert_eq!(observed.try_recv(), Ok(()));
        Ok(())
    }

    #[tokio::test]
    async fn cancelling_a_join_keeps_ownership_until_the_body_finishes() -> anyhow::Result<()> {
        use std::task::{Context, Waker};

        let (release, released) = mpsc::channel();
        let (finished, observed) = mpsc::channel();
        let thread = OwnedThread::spawn("cancelled join", move || {
            released.recv().unwrap();
            finished.send(()).unwrap();
        })?;
        let mut joining = Box::pin(thread.join());
        assert!(
            joining
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        let (dropping, started) = mpsc::channel();
        let (dropped, done) = mpsc::channel();
        let cleanup = thread::spawn(move || {
            dropping.send(()).unwrap();
            drop(joining);
            dropped.send(observed.try_recv()).unwrap();
        });
        started.recv_timeout(PATIENCE)?;
        let early = done.recv_timeout(Duration::from_millis(30));
        release.send(())?;
        let completed = done.recv_timeout(PATIENCE);
        cleanup.join().unwrap();
        assert!(matches!(early, Err(mpsc::RecvTimeoutError::Timeout)));
        assert_eq!(completed, Ok(Ok(())));
        Ok(())
    }

    #[test]
    fn dropping_the_owner_joins_the_thread() -> anyhow::Result<()> {
        let (finished, observed) = mpsc::channel();
        let thread = OwnedThread::spawn("joins", move || {
            thread::sleep(Duration::from_millis(50));
            finished.send(()).unwrap();
        })?;
        drop(thread);
        assert_eq!(observed.try_recv(), Ok(()));
        Ok(())
    }

    #[test]
    fn dropping_a_stoppable_thread_stops_its_body_before_joining() -> anyhow::Result<()> {
        let stoppable = StoppableThread::spawn("stops", |stopping| {
            let Err(_closed) = stopping.recv_blocking();
        })?;
        let (dropped, observed) = mpsc::channel();

        thread::spawn(move || {
            drop(stoppable);
            dropped.send(()).unwrap();
        });
        assert_eq!(
            observed.recv_timeout(PATIENCE),
            Ok(()),
            "Dropping the thread did not return"
        );
        Ok(())
    }
}
