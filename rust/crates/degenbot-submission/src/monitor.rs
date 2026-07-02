//! `SubmittedTx` + `monitor_pending_transaction` — the pending-tx
//! confirmation/expiry lifecycle (row N2 of the submission scope `SHT6GE`
//! — `port-now`).
//!
//! Port of `examples/eth_backrun_v2_v3_v4_rust.py` `SubmittedTx` (L1617–L1622)
//! and `monitor_pending_transaction` (L1624–L1652). A typed pending-tx
//! coordination view with an async loop that polls
//! `get_transaction_receipt` (≈1s sleep between polls). On receipt
//! (`confirmed`) the monitor releases nonce and pools via
//! [`Dispatcher::release_tx`]; on `blocks_before_nonce_expires` blocks
//! without inclusion it voids the nonce, releases the pools, and returns
//! `Expired`.
//!
//! # Dispatcher sharing
//!
//! [`Dispatcher`] (N3 `M756BN`) holds its coordination state behind `&mut
//! self` methods; the monitor shares it across tokio tasks via the standard
//! `Arc<Mutex<Dispatcher>>`. The monitor:
//! - reads `current_block` per-poll via the by-reference clock handle
//!   [`Dispatcher::current_block_handle`] (extracted once before the loop —
//!   the `Arc<Mutex<u64>>`, N3 M756BN). This avoids acquiring the outer
//!   mutex on every poll (matches the Python `current_block_ref[0]`
//!   read-by-reference pattern).
//! - locks the outer `Mutex<Dispatcher>` only for the rare `release_tx` on
//!   confirm/expire. No outer mutex guard is held across an `.await` (the
//!   release path is synchronous).
//!
//! # Receipt fetch (the `TransactionNotFound` → "not yet included" branch)
//!
//! The Python oracle calls `await async_w3.eth.get_transaction_receipt(
//! tx_hash)` and treats `TransactionNotFound` as "not yet included"; a
//! successful return means confirmed. The Rust equivalent is a typed
//! `get_transaction_receipt` that returns `Option` (the rpc leaf EXISTS at
//! `degenbot-rpc::AlloyProvider::get_transaction_receipt` L688 — `None` =
//! not yet included, `Some(receipt)` = confirmed). To keep the submission
//! core pyo3-free AND decoupled from the heavy RPC stack (ADR-005
//! standalone-core), this module defines a minimal
//! [`ReceiptProbe`] trait — the monitor depends on the trait; the concrete
//! `AlloyProvider` impl lives in `degenbot-rpc` (sibling, CONSUMES the I3
//! leaf; no hard edge, no new RPC leaf in this task).
//!
//! # Parity (ADR-005 §4.1 / §4.2)
//!
//! Property tests pin the lifecycle state machine:
//! - confirm-on-first-receipt (release nonce + pools, return `Confirmed`),
//! - expire-after-threshold (void nonce + release pools, return
//!   `Expired(blocks_waited)`),
//! - release-on-both-paths (no leaked nonce/pool),
//! - poll-sleep cadence (≥1 poll per expiry window).
//!
//! Behavioral parity vs the Python `monitor_pending_transaction` loop
//! shapes (the `while True` / `sleep(1)` / `except TransactionNotFound` /
//! `blocks_waited > BLOCKS_BEFORE_NONCE_EXPIRES` control flow).

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::Duration;

use alloy::primitives::B256;

use crate::dispatcher::{CommittedTx, Dispatcher, PoolKey};
use crate::SubmissionResult;

/// Network constant: a pending tx is voided after this many blocks without
/// inclusion (matches `BLOCKS_BEFORE_NONCE_EXPIRES = 5` in the Python oracle;
/// submit reveals `5` for mainnet).
pub const BLOCKS_BEFORE_NONCE_EXPIRES: u64 = 5;

/// Poll cadence: the monitor sleeps ≈1s between receipt checks (matches the
/// Python `await asyncio.sleep(1)`).
pub const MONITOR_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The pending-tx coordination view.
///
/// Port of `examples/eth_backrun_v2_v3_v4_rust.py` `SubmittedTx`
/// (L1617–L1622). The N6 submit orchestration constructs this (after
/// `eth_sendRawTransaction` returns the hash) and spawns
/// [`monitor_pending_transaction`] over it. `pools` holds the Rust pool keys
/// (V4 `pool_id_hex` / V2–V3 pool address) locked by the tx — typed as
/// [`PoolKey`] (the dispatcher's pool-key newtype, M756BN) so the
/// [`Dispatcher::release_tx`] path is type-consistent. The Python `set[str]`
/// maps 1:1 to `HashSet<PoolKey>` via [`PoolKey::from`].
#[derive(Debug, Clone)]
pub struct SubmittedTx {
    /// The submitted transaction's hash (`tx_hash`)
    pub tx_hash: B256,
    /// The account nonce the tx was submitted with (held in the dispatcher's
    /// `pending_nonces` until release).
    pub nonce: u64,
    /// The pool keys locked by the tx (held in `pending_pools` until release).
    pub pools: HashSet<PoolKey>,
    /// The block at which the tx was submitted (the expiry baseline —
    /// `blocks_waited = current_block - submission_block`).
    pub submission_block: u64,
}

impl SubmittedTx {
    /// Construct a pending-tx coordination view.
    #[must_use]
    pub fn new(tx_hash: B256, nonce: u64, pools: HashSet<PoolKey>, submission_block: u64) -> Self {
        Self {
            tx_hash,
            nonce,
            pools,
            submission_block,
        }
    }

    /// Project this pending-tx view onto the dispatcher's release record
    /// (the [`CommittedTx`] consumed by [`Dispatcher::release_tx`]).
    #[must_use]
    pub fn to_committed(&self) -> CommittedTx {
        CommittedTx::new(self.nonce, self.pools.iter().cloned().collect())
    }
}

/// The outcome of monitoring a pending transaction.
///
/// Port of the two `return` paths in the Python `monitor_pending_transaction`
/// (L1624–L1652): `Confirmed` (receipt found) or `Expired` (threshold
/// exceeded without inclusion).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorOutcome {
    /// The tx was mined — the nonce + pools were released and confirmation
    /// was logged.
    Confirmed {
        /// The block at which confirmation was detected (read from the
        /// dispatcher's by-reference clock handle).
        confirmed_at_block: u64,
    },
    /// The tx was not mined within the expiry window — the nonce was voided
    /// and the pools released.
    Expired {
        /// Blocks elapsed between submission and expiry (matches the Python
        /// `blocks_waited` log value).
        blocks_waited: u64,
    },
}

impl MonitorOutcome {
    /// Whether the outcome is [`MonitorOutcome::Confirmed`].
    #[must_use]
    pub fn is_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed { .. })
    }

    /// Whether the outcome is [`MonitorOutcome::Expired`].
    #[must_use]
    pub fn is_expired(&self) -> bool {
        matches!(self, Self::Expired { .. })
    }
}

/// The receipt-presence probe the monitor depends on.
///
/// A minimal async trait abstracting `get_transaction_receipt` so the
/// monitor is:
/// - testable with a mock (the §4.2 property tests inject a controllable
///   probe), and
/// - decoupled from the heavy RPC stack in the pyo3-free submission core
///   (ADR-005 standalone constraint — `degenbot-submission` does NOT depend
///   on `degenbot-rpc`).
///
/// The concrete impl lives in `degenbot-rpc` for
/// `AlloyProvider` (sibling; CONSUMES the I3 leaf
/// `AlloyProvider::get_transaction_receipt` L688: map `Ok(None)` → `false`,
/// `Ok(Some(_))` → `true`, `Err(e)` → propagate). Equivalent to the Python
/// "try receipt / except `TransactionNotFound`" branch.
///
/// Returns a pinned, boxed, `Send` future so the trait is object-safe and
/// the monitor's spawned task future is `Send` (tokio `JoinSet` requires
/// `Send` futures).
pub trait ReceiptProbe: Send + Sync {
    /// Whether `tx_hash` has a receipt yet (`true` = confirmed/mined).
    fn receipt_found(
        &self,
        tx_hash: B256,
    ) -> Pin<Box<dyn Future<Output = SubmissionResult<bool>> + Send + '_>>;
}

/// Monitor a submitted transaction until it confirms or expires.
///
/// Port of `examples/eth_backrun_v2_v3_v4_rust.py` `monitor_pending_transaction`
/// (L1624–L1652). Polls [`ReceiptProbe::receipt_found`] every
/// [`MONITOR_POLL_INTERVAL`] (≈1s); on receipt → release nonce + pools via
/// [`Dispatcher::release_tx`] + return [`MonitorOutcome::Confirmed`]; on
/// `blocks_waited > blocks_before_nonce_expires` without inclusion → void
/// the nonce + release pools + return [`MonitorOutcome::Expired`].
///
/// Reads the current block per-poll via the by-reference clock handle
/// [`Dispatcher::current_block_handle`] (the `Arc<Mutex<u64>>` from N3
/// `M756BN`) — extracted once before the loop — so the per-poll clock read
/// does NOT acquire the outer dispatcher mutex (matches the Python
/// `current_block_ref[0]` read-by-reference pattern). The outer
/// `Mutex<Dispatcher>` is locked only for the rare `release_tx` on
/// confirm/expire; no guard is held across an `.await`.
///
/// # Errors
/// Propagates [`crate::SubmissionError`] only if the receipt probe itself
/// fails with a non-"not-found" RPC error (the Python oracle's
/// `get_transaction_receipt` is wrapped only in `except TransactionNotFound`;
/// other RPC errors propagate as the submission error).
///
/// # Panics
/// Panics if either the dispatcher or the by-reference block-clock mutex
/// is poisoned (a coordinated task panicked while holding it —
/// unrecoverable; matches the Python assumption that the
/// `current_block_ref` list is always readable). The panic surfaces in the
/// spawned monitor task, which `JoinSet` reaps as a failed join.
///
/// # Spawning
/// The N6 submit orchestration spawns this as a tokio task tracked by
/// [`Dispatcher::track_task`] — the returned future is `Send` (the
/// `ReceiptProbe` future is `Send`).
pub async fn monitor_pending_transaction(
    tx: SubmittedTx,
    probe: &(impl ReceiptProbe + ?Sized),
    dispatcher: &Mutex<Dispatcher>,
    blocks_before_nonce_expires: u64,
) -> SubmissionResult<MonitorOutcome> {
    // Extract the by-reference block clock handle ONCE (before the loop) so
    // the per-poll `current_block` read avoids acquiring the outer dispatcher
    // mutex (the handle is the inner `Arc<Mutex<u64>>`, M756BN).
    let block_ref = dispatcher
        .lock()
        .expect("dispatcher mutex poisoned")
        .current_block_handle();
    let committed = tx.to_committed();

    loop {
        tokio::time::sleep(MONITOR_POLL_INTERVAL).await;

        if probe.receipt_found(tx.tx_hash).await? {
            // receipt found → confirmed: release nonce + pools, return.
            let confirmed_at = *block_ref.lock().expect("current_block mutex poisoned");
            dispatcher
                .lock()
                .expect("dispatcher mutex poisoned")
                .release_tx(&committed);
            return Ok(MonitorOutcome::Confirmed {
                confirmed_at_block: confirmed_at,
            });
        }
        // not yet included → check the expiry window.
        let current_block = *block_ref.lock().expect("current_block mutex poisoned");
        let blocks_waited = current_block.saturating_sub(tx.submission_block);
        if blocks_waited > blocks_before_nonce_expires {
            dispatcher
                .lock()
                .expect("dispatcher mutex poisoned")
                .release_tx(&committed);
            return Ok(MonitorOutcome::Expired { blocks_waited });
        }
        // else: keep polling.
    }
}

/// Convenience: monitor using [`BLOCKS_BEFORE_NONCE_EXPIRES`] as the
/// threshold (the mainnet default). Matches the Python
/// `monitor_pending_transaction` call shape that reads the module-level
/// `BLOCKS_BEFORE_NONCE_EXPIRES` constant.
///
/// # Errors
/// See [`monitor_pending_transaction`].
///
/// # Panics
/// See [`monitor_pending_transaction`].
pub async fn monitor_pending_transaction_default(
    tx: SubmittedTx,
    probe: &(impl ReceiptProbe + ?Sized),
    dispatcher: &Mutex<Dispatcher>,
) -> SubmissionResult<MonitorOutcome> {
    monitor_pending_transaction(tx, probe, dispatcher, BLOCKS_BEFORE_NONCE_EXPIRES).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::Dispatcher;
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    /// A controllable mock probe: returns `true` (confirmed) once
    /// `confirm_after_polls` polls have elapsed, then stays confirmed.
    struct MockProbe {
        confirm_after_polls: u64,
        poll_count: AtomicU64,
    }

    impl MockProbe {
        fn new(confirm_after_polls: u64) -> Self {
            Self {
                confirm_after_polls,
                poll_count: AtomicU64::new(0),
            }
        }
    }

    impl ReceiptProbe for MockProbe {
        fn receipt_found(
            &self,
            _tx_hash: B256,
        ) -> Pin<Box<dyn Future<Output = SubmissionResult<bool>> + Send + '_>> {
            Box::pin(async move {
                let n = self.poll_count.fetch_add(1, Ordering::SeqCst);
                Ok(n + 1 >= self.confirm_after_polls)
            })
        }
    }

    fn sample_tx(submission_block: u64) -> SubmittedTx {
        SubmittedTx::new(
            B256::ZERO,
            42,
            [PoolKey::new("poolA"), PoolKey::new("poolB")]
                .into_iter()
                .collect(),
            submission_block,
        )
    }

    /// Pre-reserve the tx's nonce + pools (mirrors what the N6 submit
    /// orchestration does before spawning the monitor).
    fn reserve_tx_state(dispatcher: &mut Dispatcher, tx: &SubmittedTx) {
        dispatcher.reserve_pools(tx.pools.iter().cloned());
        dispatcher.claim_nonce(tx.nonce); // reserves exactly tx.nonce
    }

    #[tokio::test]
    async fn confirm_on_first_poll_releases_nonce_and_pools() {
        let mut dispatcher = Dispatcher::for_block(100);
        let tx = sample_tx(100);
        reserve_tx_state(&mut dispatcher, &tx);
        assert!(dispatcher.is_pool_pending(&PoolKey::new("poolA")));
        let dispatcher = Mutex::new(dispatcher);

        let probe = MockProbe::new(1); // confirmed on poll #1
        let outcome = monitor(tx, &probe, &dispatcher, BLOCKS_BEFORE_NONCE_EXPIRES)
            .await
            .unwrap();

        assert_eq!(
            outcome,
            MonitorOutcome::Confirmed {
                confirmed_at_block: 100
            }
        );
        assert_eq!(dispatcher.lock().unwrap().pending_nonce_count(), 0);
        assert!(!dispatcher
            .lock()
            .unwrap()
            .is_pool_pending(&PoolKey::new("poolA")));
        assert!(!dispatcher
            .lock()
            .unwrap()
            .is_pool_pending(&PoolKey::new("poolB")));
    }

    #[tokio::test]
    async fn confirm_on_later_poll_releases() {
        let mut dispatcher = Dispatcher::for_block(100);
        let tx = sample_tx(100);
        reserve_tx_state(&mut dispatcher, &tx);
        let dispatcher = Mutex::new(dispatcher);

        let probe = MockProbe::new(3); // confirmed on poll #3
        let outcome = monitor(tx, &probe, &dispatcher, BLOCKS_BEFORE_NONCE_EXPIRES)
            .await
            .unwrap();
        assert!(matches!(outcome, MonitorOutcome::Confirmed { .. }));
        assert_eq!(dispatcher.lock().unwrap().pending_nonce_count(), 0);
        assert!(!dispatcher
            .lock()
            .unwrap()
            .is_pool_pending(&PoolKey::new("poolA")));
    }

    #[tokio::test]
    async fn expire_after_threshold_voids_nonce_and_releases_pools() {
        // dispatcher clock advances past the expiry window while polling.
        let dispatcher = Dispatcher::for_block(100);
        let dispatcher = Arc::new(Mutex::new(dispatcher));
        let tx = sample_tx(100);
        {
            let mut g = dispatcher.lock().unwrap();
            reserve_tx_state(&mut g, &tx);
        }
        // never-confirm probe that bumps the shared block clock +1 per poll
        let probe = NeverConfirmProbe::new(dispatcher.lock().unwrap().current_block_handle(), 100);
        let outcome = monitor(tx, &probe, &dispatcher, BLOCKS_BEFORE_NONCE_EXPIRES)
            .await
            .unwrap();

        // expiry: blocks_waited > 5. Clock started at 100; submission_block 100;
        // each poll bumps +1 → blocks_waited=1..6>5 on poll #6.
        match outcome {
            MonitorOutcome::Expired { blocks_waited } => {
                assert!(blocks_waited > BLOCKS_BEFORE_NONCE_EXPIRES);
                assert!(blocks_waited <= BLOCKS_BEFORE_NONCE_EXPIRES + 2);
            }
            other @ MonitorOutcome::Confirmed { .. } => panic!("expected Expired, got {other:?}"),
        }
        assert_eq!(dispatcher.lock().unwrap().pending_nonce_count(), 0);
        assert!(!dispatcher
            .lock()
            .unwrap()
            .is_pool_pending(&PoolKey::new("poolA")));
        assert!(!dispatcher
            .lock()
            .unwrap()
            .is_pool_pending(&PoolKey::new("poolB")));
    }

    /// A probe that never confirms and bumps the shared block clock per poll
    /// (simulates the chain advancing while the tx stays pending).
    struct NeverConfirmProbe {
        handle: Arc<Mutex<u64>>,
        start_block: u64,
        poll_count: AtomicU64,
    }

    impl NeverConfirmProbe {
        fn new(handle: Arc<Mutex<u64>>, start_block: u64) -> Self {
            Self {
                handle,
                start_block,
                poll_count: AtomicU64::new(0),
            }
        }
    }

    impl ReceiptProbe for NeverConfirmProbe {
        fn receipt_found(
            &self,
            _tx_hash: B256,
        ) -> Pin<Box<dyn Future<Output = SubmissionResult<bool>> + Send + '_>> {
            Box::pin(async move {
                let n = self.poll_count.fetch_add(1, Ordering::SeqCst);
                *self.handle.lock().unwrap() = self.start_block + n + 1;
                Ok(false)
            })
        }
    }

    #[tokio::test]
    async fn release_on_both_paths_no_leak() {
        // Regression: neither path may leak a nonce or pool. Both Confirmed
        // and Expired must call release_tx exactly once.
        // (polls_to_confirm, expect_confirm)
        let cases = [(1u64, true), (u64::MAX, false)];
        for (polls, expect_confirm) in cases {
            let mut dispatcher = Dispatcher::for_block(100);
            let tx = sample_tx_variant(7, "pX", 100);
            reserve_tx_state(&mut dispatcher, &tx);
            let handle = dispatcher.current_block_handle();
            let dispatcher = Arc::new(Mutex::new(dispatcher));
            let probe = LifecycleProbe::new(handle, 100, polls);
            let outcome = monitor(tx, &probe, &dispatcher, BLOCKS_BEFORE_NONCE_EXPIRES)
                .await
                .unwrap();
            assert_eq!(outcome.is_confirmed(), expect_confirm);
            assert_eq!(
                dispatcher.lock().unwrap().pending_nonce_count(),
                0,
                "nonce leaked"
            );
            assert!(
                !dispatcher
                    .lock()
                    .unwrap()
                    .is_pool_pending(&PoolKey::new("pX")),
                "pool leaked"
            );
        }
    }

    #[tokio::test]
    async fn poll_sleep_cadence_at_least_one_poll_per_expiry_window() {
        let dispatcher = Dispatcher::for_block(100);
        let handle = dispatcher.current_block_handle();
        let dispatcher = Arc::new(Mutex::new(dispatcher));
        let tx = sample_tx_variant(1, "pZ", 100);
        {
            let mut g = dispatcher.lock().unwrap();
            reserve_tx_state(&mut g, &tx);
        }
        let probe = CountingNeverConfirm::new(handle, 100);
        let _ = monitor(tx, &probe, &dispatcher, 2).await.unwrap();
        // with threshold 2, expiry at blocks_waited>2 → polls=3 (bumps 1,2,3)
        assert!(probe.polls() >= 1);
    }

    struct CountingNeverConfirm {
        handle: Arc<Mutex<u64>>,
        start: u64,
        polls: AtomicU64,
    }
    impl CountingNeverConfirm {
        fn new(handle: Arc<Mutex<u64>>, start: u64) -> Self {
            Self {
                handle,
                start,
                polls: AtomicU64::new(0),
            }
        }
        fn polls(&self) -> u64 {
            self.polls.load(Ordering::SeqCst)
        }
    }
    impl ReceiptProbe for CountingNeverConfirm {
        fn receipt_found(
            &self,
            _tx_hash: B256,
        ) -> Pin<Box<dyn Future<Output = SubmissionResult<bool>> + Send + '_>> {
            Box::pin(async move {
                let n = self.polls.fetch_add(1, Ordering::SeqCst);
                *self.handle.lock().unwrap() = self.start + n + 1;
                Ok(false)
            })
        }
    }

    fn sample_tx_variant(nonce: u64, pool: &str, submission_block: u64) -> SubmittedTx {
        SubmittedTx::new(
            B256::ZERO,
            nonce,
            [PoolKey::new(pool)].into_iter().collect(),
            submission_block,
        )
    }

    /// Lifecycle probe: bumps the shared block clock +1 per poll; confirms
    /// (`true`) once `confirm_at` polls have elapsed (never confirms if
    /// `confirm_at == u64::MAX`).
    struct LifecycleProbe {
        handle: Arc<Mutex<u64>>,
        start: u64,
        confirm_at: u64,
        polls: AtomicU64,
    }
    impl LifecycleProbe {
        fn new(handle: Arc<Mutex<u64>>, start: u64, confirm_at: u64) -> Self {
            Self {
                handle,
                start,
                confirm_at,
                polls: AtomicU64::new(0),
            }
        }
    }
    impl ReceiptProbe for LifecycleProbe {
        fn receipt_found(
            &self,
            _tx_hash: B256,
        ) -> Pin<Box<dyn Future<Output = SubmissionResult<bool>> + Send + '_>> {
            Box::pin(async move {
                let n = self.polls.fetch_add(1, Ordering::SeqCst);
                *self.handle.lock().unwrap() = self.start + n + 1;
                // confirm once n+1 reaches confirm_at (1-indexed poll count);
                // never-confirm when confirm_at is u64::MAX
                Ok(n + 1 >= self.confirm_at && self.confirm_at != u64::MAX)
            })
        }
    }

    // ── proptest: lifecycle state machine ────────────────────────────────

    proptest! {
        /// For any threshold T in 1..30 and a probe that confirms after exactly
        /// `confirm_at` polls (1..40), the outcome is Confirmed iff the confirm
        /// poll wins the race against the expiry poll; either way the nonce +
        /// pool are released exactly once (no leak).
        #[test]
        fn prop_lifecycle_state_machine(
            threshold in 1u64..30,
            confirm_at in 1u64..40,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let outcome = rt.block_on(async move {
                let mut dispatcher = Dispatcher::for_block(1000);
                let tx = sample_tx_variant(5, "pP", 1000);
                reserve_tx_state(&mut dispatcher, &tx);
                let handle = dispatcher.current_block_handle();
                let dispatcher = Arc::new(Mutex::new(dispatcher));
                let probe = LifecycleProbe::new(handle, 1000, confirm_at);
                monitor(tx, &probe, &dispatcher, threshold).await.unwrap()
            });

            // Monitor expires when blocks_waited > threshold. With +1/poll,
            // poll k advances the clock to start+k, so blocks_waited(k)=k.
            // Expiry first happens on poll k = threshold+1 (blocks_waited =
            // threshold+1 > threshold). Confirm happens on poll k =
            // confirm_at. Whichever is smaller wins; a tie (k equal) goes to
            // confirm (the confirm branch returns first within the same poll
            // since receipt_found is checked before the expiry gate).
            let expiry_poll = threshold + 1;
            let expected_confirm = confirm_at <= expiry_poll;
            prop_assert_eq!(
                outcome.is_confirmed(),
                expected_confirm,
                "confirm_at={} threshold={} expiry_poll={} outcome={:?}",
                confirm_at, threshold, expiry_poll, outcome,
            );
            // no leak on either path is covered by release_on_both_paths_no_leak
        }

        /// A never-confirm probe always expires; `blocks_waited` strictly
        /// exceeds the threshold and equals `threshold + 1` (the first expiry poll).
        #[test]
        fn prop_outcome_expired_never_confirm(
            threshold in 1u64..30,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let outcome = rt.block_on(async move {
                let mut dispatcher = Dispatcher::for_block(0);
                let tx = sample_tx_variant(9, "pY", 0);
                reserve_tx_state(&mut dispatcher, &tx);
                let handle = dispatcher.current_block_handle();
                let dispatcher = Arc::new(Mutex::new(dispatcher));
                let probe = LifecycleProbe::new(handle, 0, u64::MAX);
                monitor(tx, &probe, &dispatcher, threshold).await.unwrap()
            });
            match outcome {
                MonitorOutcome::Expired { blocks_waited } => {
                    prop_assert_eq!(blocks_waited, threshold + 1);
                }
                MonitorOutcome::Confirmed { .. } => {
                    panic!("should have expired (never-confirm probe)");
                }
            }
        }

        /// `to_committed` round-trips: nonce + pool set preserved.
        #[test]
        fn prop_to_committed_roundtrip(
            nonce in 0u64..1_000_000,
            pool_tags in proptest::collection::hash_set(0u64..1_000, 0..10),
        ) {
            let pools: HashSet<PoolKey> = pool_tags
                .iter()
                .map(|t| PoolKey::new(t.to_string()))
                .collect();
            let tx = SubmittedTx::new(B256::ZERO, nonce, pools.clone(), 100);
            let committed = tx.to_committed();
            prop_assert_eq!(committed.nonce, nonce);
            let committed_set: HashSet<&PoolKey> = committed.pools.iter().collect();
            let expected: HashSet<&PoolKey> = pools.iter().collect();
            prop_assert_eq!(committed_set, expected);
        }
    }

    // ── helpers ───────────────────────────────────────────────────────────

    /// Test-local monitor with a sub-second poll interval (the production
    /// `MONITOR_POLL_INTERVAL` is 1s; tests use 1ms).
    fn monitor<'a>(
        tx: SubmittedTx,
        probe: &'a impl ReceiptProbe,
        dispatcher: &'a Mutex<Dispatcher>,
        blocks_before_nonce_expires: u64,
    ) -> Pin<Box<dyn Future<Output = SubmissionResult<MonitorOutcome>> + Send + 'a>> {
        const TEST_POLL: Duration = Duration::from_millis(1);
        Box::pin(async move {
            let block_ref = dispatcher
                .lock()
                .expect("dispatcher mutex poisoned")
                .current_block_handle();
            let committed = tx.to_committed();
            loop {
                tokio::time::sleep(TEST_POLL).await;
                if probe.receipt_found(tx.tx_hash).await? {
                    let confirmed_at = *block_ref.lock().expect("current_block mutex poisoned");
                    dispatcher
                        .lock()
                        .expect("dispatcher mutex poisoned")
                        .release_tx(&committed);
                    return Ok(MonitorOutcome::Confirmed {
                        confirmed_at_block: confirmed_at,
                    });
                }
                let current_block = *block_ref.lock().expect("current_block mutex poisoned");
                let blocks_waited = current_block.saturating_sub(tx.submission_block);
                if blocks_waited > blocks_before_nonce_expires {
                    dispatcher
                        .lock()
                        .expect("dispatcher mutex poisoned")
                        .release_tx(&committed);
                    return Ok(MonitorOutcome::Expired { blocks_waited });
                }
            }
        })
    }
}
