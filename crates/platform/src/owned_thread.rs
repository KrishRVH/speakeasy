//! Named OS threads whose exit an async owner can await and whose drop joins them.

use std::{convert::Infallible, io, thread};

/// An OS thread that belongs to exactly one value.
///
/// Dropping it joins the thread, so the thread's captures are released before any field declared
/// after it in the containing value. [`exited`](Self::exited) lets an async owner wait for the body
/// to finish without blocking its executor.
pub struct OwnedThread {
    /// Never sends; it closes once the thread drops the receiving end.
    exit: async_channel::Sender<Infallible>,
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
            handle: Some(handle),
        })
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
}
