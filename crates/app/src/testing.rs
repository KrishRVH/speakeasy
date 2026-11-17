use std::{io, time::Duration};

use tokio::time::timeout;

pub(crate) const PATIENCE: Duration = Duration::from_secs(2);

pub(crate) async fn still_pending(future: impl Future) -> bool {
    timeout(Duration::from_millis(30), future).await.is_err()
}

pub(crate) struct Closed;

impl io::Write for Closed {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
