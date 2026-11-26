//! Shared helpers for the pending-tx feed integration suites.

use std::time::{Duration, Instant};

use degenbot_eventhub::PendingTx;

/// Poll `take` until `want` frames have accumulated, or fail at `timeout`
/// naming the condition and the count reached.
pub async fn drain_frames(
    mut take: impl FnMut() -> Vec<PendingTx>,
    want: usize,
    timeout: Duration,
) -> Vec<PendingTx> {
    let mut acc: Vec<PendingTx> = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        acc.extend(take());
        if acc.len() >= want {
            return acc;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {want} events (got {})",
            acc.len()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
