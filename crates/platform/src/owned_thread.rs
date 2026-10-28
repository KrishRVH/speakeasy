//! Named OS threads whose exit an async owner can await and whose drop joins them, asking a
//! stoppable one to stop first.

use std::{convert::Infallible, io, thread};

/// An OS thread that belongs to exactly one value.
///
/// Dropping it joins the thread, so the thread's captures are released before any field declared
/// after it in the containing value. [`exited`](Self::exited) lets an async owner wait for the body
/// to finish, and [`join`](Self::join) for the thread to end, without blocking its executor.
#[derive(Debug)]
pub struct OwnedThread {
    /// Never sends; it closes once the thread drops the receiving end.
    exit: async_channel::Sender<Infallible>,
    thread: thread::Thread,
    handle: Option<thread::JoinHandle<()>>,
}

impl OwnedThread {
    /// Runs `body` on a new thread called `name`.
    ///
    /// # Errors
    /// Returns the operating system's refusal to create the thread.
    pub fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> io::Result<Self> {
        let (exit, alive) = async_channel::bounded::<Infallible>(1);
        let handle = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                body();
                // Naming `alive` moves it into the closure, so exit closes only once body returns
                // or unwinds.
                drop(alive);
            })?;
        Ok(Self {
            exit,
            thread: handle.thread().clone(),
            handle: Some(handle),
        })
    }

    /// Runs `body` on a new thread called `name`, handing it a lane that never carries a value and
    /// closes once the returned thread is asked to stop or dropped.
    ///
    /// # Errors
    /// Returns the operating system's refusal to create the thread.
    pub fn spawn_stoppable(
        name: &str,
        body: impl FnOnce(async_channel::Receiver<Infallible>) + Send + 'static,
    ) -> io::Result<StoppableThread> {
        let (stop, stopping) = async_channel::bounded(1);
        let thread = Self::spawn(name, move || body(stopping))?;
        Ok(StoppableThread { stop, thread })
    }

    /// The thread, for unparking a body that parks.
    #[must_use]
    pub const fn thread(&self) -> &thread::Thread {
        &self.thread
    }

    /// Resolves once the body has returned or unwound.
    pub fn exited(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        let exit = self.exit.clone();
        async move { exit.closed().await }
    }

    /// Whether the body has returned or unwound.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.exit.is_closed()
    }

    /// Resolves once the body has returned or unwound and the thread has ended. A thread's end
    /// still runs its thread-local destructors, such as a COM apartment's teardown, which can
    /// block, so the join runs on Tokio's blocking pool rather than the awaiting executor.
    ///
    /// # Panics
    /// Panics when awaited outside a Tokio runtime.
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

/// An [`OwnedThread`] whose body watches a stop lane.
///
/// Dropping it closes the lane before joining, so the body is asked to stop before the drop waits
/// for it, however the containing value orders its fields.
#[derive(Debug)]
pub struct StoppableThread {
    /// Never sends; closing it asks the body to stop.
    stop: async_channel::Sender<Infallible>,
    thread: OwnedThread,
}

impl StoppableThread {
    /// Asks the body to stop, without joining it.
    pub fn request_stop(&self) {
        self.stop.close();
    }

    /// Resolves once the body has returned or unwound.
    pub fn stopped(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        self.thread.exited()
    }

    /// Whether the body has returned or unwound.
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

    use super::OwnedThread;

    const PATIENCE: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn exit_is_observed_once_the_body_returns() -> anyhow::Result<()> {
        let (release, released) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("returns", move || released.recv_blocking().unwrap())?;
        let finished_while_held = thread.is_finished();
        release.send(()).await?;
        tokio::time::timeout(PATIENCE, thread.exited()).await?;
        // Asserting before release would hang on failure: unwinding joins the blocked thread before
        // dropping `release`.
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
        // The lane never carries a value, so receiving returns only once it closes.
        let stoppable = OwnedThread::spawn_stoppable("stops", |stop| {
            let Err(_closed) = stop.recv_blocking();
        })?;
        let (dropped, observed) = mpsc::channel();
        // A drop that joined without closing the lane would block forever, so it runs off the test
        // thread.
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
