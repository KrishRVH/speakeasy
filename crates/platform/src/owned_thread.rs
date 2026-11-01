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

    pub async fn join(self) {
        self.exited().await;
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Dropping the thread discards its panic, and a runtime shutting down drops the closure, which joins in place"
        )]
        let _ = tokio::task::spawn_blocking(move || drop(self)).await;
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
        tokio::time::timeout(PATIENCE, joined).await??;
        assert!(!early, "Joining resolved while the body still ran");
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
