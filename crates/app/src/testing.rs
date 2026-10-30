//! The waits the app's tests share.

use std::time::Duration;

use tokio::time::timeout;

/// How long a test waits for work that should finish before it fails.
pub(crate) const PATIENCE: Duration = Duration::from_secs(2);

/// Whether `future` is still pending once every task has gone idle on the paused clock, or after
/// 30 ms on the real one.
pub(crate) async fn still_pending(future: impl Future) -> bool {
    timeout(Duration::from_millis(30), future).await.is_err()
}
