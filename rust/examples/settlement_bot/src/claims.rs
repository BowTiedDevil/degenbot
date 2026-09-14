//! At-most-once verify-claim table — the Rust/tokio twin of
//! `src/degenbot/arbitrage/_claims.py` as consumed by
//! `runner/build_paths.py::_SeatVerifyClaims` (parity-ledger row 9,
//! ergo `XFEJUG`).
//!
//! POLICY (stated once, mirroring `VerifyClaims`):
//!
//! - **claim-if-absent (leader)** — the first caller for a key registers a
//!   fresh claim under the table lock and runs the work.
//! - **await-if-present (peer)** — a caller that sees a live claim waits on
//!   the same claim and receives the leader's settled result (success or the
//!   exact failure), so a pool's verify lifecycle runs at most once per live
//!   claim window.
//! - **release-on-failure (and on success)** — the leader evicts its own
//!   claim by identity check when it finishes, so a failed lifecycle stays
//!   retriable by a LATER caller.
//!
//! The Python module notes the seam is real (asyncio.Future vs
//! threading.Event). Here the concrete primitive is tokio's `Notify` +
//! state mutex — the seat-thread analogue, since the Rust driver's per-candidate
//! units are tokio tasks. The choreography (ADR-022) stays core-owned; this
//! module is transport dedup only.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use tokio::sync::{Mutex, Notify};

/// The two-way verification failure split (mirrors the Python typed
/// `VerificationMismatchError` / `VerificationRpcError`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyErrorKind {
    /// A genuine on-chain tick-data divergence — fatal, never retried.
    Mismatch,
    /// A transient per-call transport / provider-init failure — retryable.
    Rpc,
    /// Any other lifecycle failure.
    Other,
}

/// A driver-local verification failure (the typed classification the retry
/// dance and the ledger read).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationError {
    /// The failure category.
    pub kind: VerifyErrorKind,
    /// The human-readable detail.
    pub message: String,
}

impl VerificationError {
    /// Construct a failure.
    #[must_use]
    pub fn new(kind: VerifyErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for VerificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for VerificationError {}

/// One live claim window's shared state.
#[derive(Debug)]
struct ClaimState {
    result: Mutex<Option<Result<(), VerificationError>>>,
    notify: Notify,
}

impl ClaimState {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            notify: Notify::new(),
        }
    }

    /// Park until the leader settles, returning the leader's exact result.
    async fn wait(&self) -> Result<(), VerificationError> {
        loop {
            // Create the notified future BEFORE checking so a settle between
            // the check and the await cannot be lost.
            let notified = self.notify.notified();
            if let Some(result) = self.result.lock().await.clone() {
                return result;
            }
            notified.await;
        }
    }
}

/// The at-most-once verify-claim table.
#[derive(Debug, Default)]
pub struct VerifyClaims {
    table: Mutex<HashMap<String, Arc<ClaimState>>>,
}

impl VerifyClaims {
    /// A fresh, empty claim table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `run` at most once per live claim window for `key`.
    ///
    /// Peers await the leader and receive its settled result; a failed claim
    /// is released so a later caller re-claims and retries.
    pub async fn run_exclusive<F, Fut>(&self, key: &str, run: F) -> Result<(), VerificationError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), VerificationError>>,
    {
        let (state, leader) = {
            let mut table = self.table.lock().await;
            if let Some(existing) = table.get(key) {
                (Arc::clone(existing), false)
            } else {
                let state = Arc::new(ClaimState::new());
                table.insert(key.to_string(), Arc::clone(&state));
                (state, true)
            }
        };

        if !leader {
            return state.wait().await;
        }

        let result = run().await;
        *state.result.lock().await = Some(result.clone());
        state.notify.notify_waiters();

        // Release-on-success/failure: identity-checked eviction so a newer
        // claim a retry may already have registered is never clobbered.
        {
            let mut table = self.table.lock().await;
            if let Some(existing) = table.get(key) {
                if Arc::ptr_eq(existing, &state) {
                    table.remove(key);
                }
            }
        }

        result
    }

    /// The number of live claims (test/diagnostic witness).
    pub async fn live_claim_count(&self) -> usize {
        self.table.lock().await.len()
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn leader_runs_once_and_peers_share_the_result() {
        let claims = Arc::new(VerifyClaims::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let claims = Arc::clone(&claims);
            let runs = Arc::clone(&runs);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                claims
                    .run_exclusive("v3:0xabc", || {
                        let runs = Arc::clone(&runs);
                        async move {
                            runs.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            Ok(())
                        }
                    })
                    .await
            }));
        }
        for handle in handles {
            handle.await.unwrap().unwrap();
        }
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "one lifecycle run per window"
        );
        assert_eq!(
            claims.live_claim_count().await,
            0,
            "claim released on success"
        );
    }

    #[tokio::test]
    async fn failed_claim_is_released_for_a_later_retry() {
        let claims = VerifyClaims::new();
        let first = claims
            .run_exclusive("v3:0xdef", || async {
                Err(VerificationError::new(VerifyErrorKind::Rpc, "boom"))
            })
            .await;
        assert!(first.is_err());
        assert_eq!(claims.live_claim_count().await, 0);
        let second = claims.run_exclusive("v3:0xdef", || async { Ok(()) }).await;
        assert!(second.is_ok(), "a failed claim must stay retriable");
    }

    #[tokio::test]
    async fn peers_receive_the_leaders_exact_failure() {
        let claims = Arc::new(VerifyClaims::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let error = VerificationError::new(VerifyErrorKind::Mismatch, "diverged");
        let mut handles = Vec::new();
        for _ in 0..2 {
            let claims = Arc::clone(&claims);
            let runs = Arc::clone(&runs);
            let barrier = Arc::clone(&barrier);
            let error = error.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                claims
                    .run_exclusive("v4:0x1", || {
                        let runs = Arc::clone(&runs);
                        let error = error.clone();
                        async move {
                            runs.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            Err(error)
                        }
                    })
                    .await
            }));
        }
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.unwrap());
        }
        assert!(results
            .iter()
            .all(|r| matches!(r, Err(e) if e.kind == VerifyErrorKind::Mismatch)));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }
}
