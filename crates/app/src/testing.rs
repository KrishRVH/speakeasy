use std::time::Duration;

use tokio::time::timeout;

pub(crate) const PATIENCE: Duration = Duration::from_secs(2);

pub(crate) async fn still_pending(future: impl Future) -> bool {
    timeout(Duration::from_millis(30), future).await.is_err()
}
