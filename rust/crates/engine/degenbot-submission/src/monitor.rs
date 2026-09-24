//! `SubmittedTx` + `monitor_pending_transaction` — the pending-tx
//! confirmation/expiry lifecycle (row N2 of the submission scope `SHT6GE`
//! — `port-now`).
//!
//! Port of `examples/eth_backrun_v2_v3_v4_rust.py` `SubmittedTx` (L1617–L1622)
//! and `monitor_pending_transaction` (L1624–L1652). A typed pending-tx
//! coordination view with an async loop that waits on real head events and
//! probes `get_transaction_receipt` once per event. On receipt
//! (`confirmed`) the monitor releases the tx's pools via
//! [`Dispatcher::release_tx`]; on `blocks_before_nonce_expires` blocks
//! without inclusion it releases the pools and returns `Expired`. Nonce
//! lifecycle — land, release, tombstone — belongs to the `NonceAuthority`:
//! the owning lane lands a confirmed broadcast at the next head and releases
//! an expired one through the authority's expiry entry point.
//!
//! # Dispatcher sharing
//!
//! [`Dispatcher`] (N3 `M756BN`) holds its pool/task coordination state behind
//! `&mut self` methods; the monitor shares it across tokio tasks via the
//! standard `Arc<Mutex<Dispatcher>>`. The monitor:
//! - waits on the head-event broadcast [`Dispatcher::block_events`] (a
//!   `tokio::sync::watch<u64>` fed by [`Dispatcher::advance_block`]) so the
//!   loop is event-driven, not a fixed-interval sleep. The `watch` channel
//!   never loses a head: a height change between the receipt/expiry checks
//!   and the `changed().await` is already buffered.
//! - reads `current_block` after each head event via the by-reference clock
//!   handle [`Dispatcher::current_block_handle`] (extracted once before the
//!   loop — the `Arc<Mutex<u64>>`, N3 M756BN). This avoids acquiring the
//!   outer mutex on every event (matches the Python `current_block_ref[0]`
//!   read-by-reference pattern).
//! - locks the outer `Mutex<Dispatcher>` only for the rare pool `release_tx`
//!   on confirm/expire. No outer mutex guard is held across an `.await` (the
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
//! impl (`PyReceiptProbe`) lives in `degenbot-python` (`submission/submit.rs`)
//! and consumes the `AlloyProvider` `get_transaction_receipt` leaf.
//!
//! # Parity (ADR-005 §4.1 / §4.2)
//!
//! Property tests pin the lifecycle state machine:
//! - confirm-on-first-receipt (release nonce + pools, return `Confirmed`),
//! - expire-after-threshold (void nonce + release pools, return
//!   `Expired(blocks_waited)`),
//! - release-on-both-paths (no leaked nonce/pool),
//! - head-event cadence (one receipt probe per head event, plus the initial
//!   probe — no fixed-interval timer).
//!
//! Behavioral parity vs the Python `monitor_pending_transaction` loop
//! shapes (the `while True` / `sleep(1)` / `except TransactionNotFound` /
//! `blocks_waited > BLOCKS_BEFORE_NONCE_EXPIRES` control flow). The numeric
//! policy is IDENTICAL (inclusion = first receipt; expiry =
//! `blocks_waited > blocks_before_nonce_expires` read from the shared clock);
//! only the wait mechanism changed — from a 1s timer to the real head event
//! published by [`Dispatcher::advance_block`].

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use alloy::primitives::B256;

use crate::dispatcher::{CommittedTx, Dispatcher, PoolKey};
use crate::{SubmissionError, SubmissionResult};

/// Network constant: a pending tx is voided after this many blocks without
/// inclusion (matches `BLOCKS_BEFORE_NONCE_EXPIRES = 5` in the Python oracle;
/// submit reveals `5` for mainnet).
pub const BLOCKS_BEFORE_NONCE_EXPIRES: u64 = 5;

/// The pending-tx coordination view.
///
/// Port of `examples/eth_backrun_v2_v3_v4_rust.py` `SubmittedTx`
/// (L1617–L1622). The N6 submit orchestration constructs this (after
/// `eth_sendRawTransaction` returns the hash) and spawns
/// [`monitor_pending_transaction`] over it. `pools` holds the Rust pool keys
/// (V4 `pool_id_hex` / V2–V3 pool address) locked by the tx — typed as
/// [`PoolKey`] (the dispatcher's pool-key newtype, M756BN) so the
/// [`Dispatcher::release_tx`] path is type-consistent. The Python `set[str]`
/// maps 1:1 to `HashSet<PoolKey>` via [`PoolKey::from`]. `nonce` names the
/// authority slot the tx was signed against; the monitor carries it for
/// tracing and surfaces it to the owning lane, which releases an expired
/// broadcast through the authority.
#[derive(Debug, Clone)]
pub struct SubmittedTx {
    /// The submitted transaction's hash (`tx_hash`)
    pub tx_hash: B256,
    /// The account nonce the tx was submitted with. The dispatcher no longer
    /// holds it — nonce lifecycle is the `NonceAuthority`'s — but the monitor
    /// carries it for tracing and for the owning lane's expiry release.
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

    /// Project this pending-tx view onto the dispatcher's pool-release record
    /// (the [`CommittedTx`] consumed by [`Dispatcher::release_tx`]).
    #[must_use]
    pub fn to_committed(&self) -> CommittedTx {
        CommittedTx::new(self.pools.iter().cloned().collect())
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
/// (L1624–L1652). Probes [`ReceiptProbe::receipt_found`] once immediately and
/// then once per real head event — awaited via [`Dispatcher::block_events`]
/// (a `tokio::sync::watch<u64>` fed by [`Dispatcher::advance_block`]) — so the
/// monitor reacts at head granularity, not at a fixed timer interval. On
/// receipt → release pools via [`Dispatcher::release_tx`] + return
/// [`MonitorOutcome::Confirmed`]; on `blocks_waited > blocks_before_nonce_expires`
/// without inclusion → release pools + return
/// [`MonitorOutcome::Expired`]. The numeric policy is unchanged. Nonce release
/// is the authority's: a confirmed broadcast lands at the next head, and the
/// owning lane releases an expired one.
///
/// Reads the current block after each head event via the by-reference clock
/// handle [`Dispatcher::current_block_handle`] (the `Arc<Mutex<u64>>` from N3
/// `M756BN`) — extracted once before the loop — so the per-event clock read
/// does NOT acquire the outer dispatcher mutex (matches the Python
/// `current_block_ref[0]` read-by-reference pattern). The outer
/// `Mutex<Dispatcher>` is locked only for the rare `release_tx` on
/// confirm/expire; no guard is held across an `.await`.
///
/// # Errors
/// Propagates [`crate::SubmissionError`] if the receipt probe itself fails
/// with a non-"not-found" RPC error (the Python oracle's
/// `get_transaction_receipt` is wrapped only in `except TransactionNotFound`;
/// other RPC errors propagate as the submission error), or if the dispatcher
/// head-event source is dropped while the tx is pending (no further head can
/// arrive, so the tx can never confirm or expire).
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
    // Extract the by-reference block clock handle + the head-event receiver
    // ONCE (before the loop) so the per-event `current_block` read avoids
    // acquiring the outer dispatcher mutex (the handle is the inner
    // `Arc<Mutex<u64>>`, M756BN) and the receiver is registered before the
    // first probe (no head event can slip between subscription and the loop).
    // RMHQAR : OTel tier-1 - one Jaeger node per awaited receipt
    // (degenbot.bundle.monitor); parents under the block/solve spans when
    // pump-driven. Inert without a subscriber.
    let span = tracing::info_span!(
        "degenbot.bundle.monitor",
        nonce = tx.nonce,
        submission_block = tx.submission_block,
        monitor.result = tracing::field::Empty,
        monitor.confirmed_at_block = tracing::field::Empty,
        monitor.blocks_waited = tracing::field::Empty,
    );
    let _guard = span.enter();
    #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
    let (block_ref, mut block_events) = {
        let dispatcher = dispatcher.lock().expect("dispatcher mutex poisoned");
        (dispatcher.current_block_handle(), dispatcher.block_events())
    };
    let committed = tx.to_committed();

    loop {
        // Receipt check: once immediately, then once per head event. The
        // monitor is woken by `advance_block`, never by a fixed-interval
        // timer.
        let found = match probe.receipt_found(tx.tx_hash).await {
            Ok(found) => found,
            Err(e) => {
                span.record("monitor.result", "error");
                return Err(e);
            }
        };
        if found {
            // receipt found → confirmed: release nonce + pools, return.
            #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
            let confirmed_at = *block_ref.lock().expect("current_block mutex poisoned");
            #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
            {
                dispatcher
                    .lock()
                    .expect("dispatcher mutex poisoned")
                    .release_tx(&committed);
            }
            span.record("monitor.result", "confirmed");
            span.record("monitor.confirmed_at_block", confirmed_at);
            return Ok(MonitorOutcome::Confirmed {
                confirmed_at_block: confirmed_at,
            });
        }
        // not yet included → check the expiry window against the shared clock.
        #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
        let current_block = *block_ref.lock().expect("current_block mutex poisoned");
        let blocks_waited = current_block.saturating_sub(tx.submission_block);
        if blocks_waited > blocks_before_nonce_expires {
            span.record("monitor.result", "expired");
            span.record("monitor.blocks_waited", blocks_waited);
            #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
            {
                dispatcher
                    .lock()
                    .expect("dispatcher mutex poisoned")
                    .release_tx(&committed);
            }
            return Ok(MonitorOutcome::Expired { blocks_waited });
        }
        // Neither included nor expired → park until the next head event. The
        // `watch` receiver buffers the latest height, so a head that advanced
        // while the checks above ran already resolves `changed()`.
        if block_events.changed().await.is_err() {
            // The head source is gone: no further event can arrive, so the tx
            // can never confirm or expire. Release the nonce + pools and
            // surface the broken coordination state rather than park forever.
            #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
            {
                dispatcher
                    .lock()
                    .expect("dispatcher mutex poisoned")
                    .release_tx(&committed);
            }
            span.record("monitor.result", "dispatcher_gone");
            return Err(SubmissionError::MonitorProbe(
                "dispatcher dropped while monitoring pending tx".to_string(),
            ));
        }
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

#[expect(clippy::unwrap_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::Dispatcher;
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// A deterministic head-event-driven mock probe.
    ///
    /// Each call is one monitor receipt probe. While the tx is pending
    /// (call # < `confirm_at`) the probe advances the shared block clock by
    /// one block via [`Dispatcher::advance_block`] — which publishes the head
    /// event that wakes the monitor — and returns `false`. On the confirming
    /// call it returns `true` without advancing. `confirm_at == u64::MAX`
    /// never confirms (and advances on every call).
    struct StepProbe {
        dispatcher: Arc<Mutex<Dispatcher>>,
        confirm_at: u64,
        calls: AtomicU64,
    }

    impl StepProbe {
        fn new(dispatcher: Arc<Mutex<Dispatcher>>, confirm_at: u64) -> Self {
            Self {
                dispatcher,
                confirm_at,
                calls: AtomicU64::new(0),
            }
        }

        fn calls(&self) -> u64 {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl ReceiptProbe for StepProbe {
        fn receipt_found(
            &self,
            _tx_hash: B256,
        ) -> Pin<Box<dyn Future<Output = SubmissionResult<bool>> + Send + '_>> {
            Box::pin(async move {
                let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
                if self.confirm_at != u64::MAX && n >= self.confirm_at {
                    return Ok(true);
                }
                // Still pending: emulate the chain reaching the next head.
                // `advance_block` publishes the event that wakes the monitor.
                let dispatcher = self.dispatcher.lock().unwrap();
                let next = dispatcher.current_block() + 1;
                dispatcher.advance_block(next);
                Ok(false)
            })
        }
    }

    /// A probe that confirms once the shared clock reaches `confirm_at_block`
    /// (i.e. after a real head event), with no timer involvement.
    struct ClockProbe {
        dispatcher: Arc<Mutex<Dispatcher>>,
        confirm_at_block: u64,
        calls: AtomicU64,
    }

    impl ClockProbe {
        fn new(dispatcher: Arc<Mutex<Dispatcher>>, confirm_at_block: u64) -> Self {
            Self {
                dispatcher,
                confirm_at_block,
                calls: AtomicU64::new(0),
            }
        }

        fn calls(&self) -> u64 {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl ReceiptProbe for ClockProbe {
        fn receipt_found(
            &self,
            _tx_hash: B256,
        ) -> Pin<Box<dyn Future<Output = SubmissionResult<bool>> + Send + '_>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let block = self.dispatcher.lock().unwrap().current_block();
                Ok(block >= self.confirm_at_block)
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

    fn sample_tx_variant(nonce: u64, pool: &str, submission_block: u64) -> SubmittedTx {
        SubmittedTx::new(
            B256::ZERO,
            nonce,
            [PoolKey::new(pool)].into_iter().collect(),
            submission_block,
        )
    }

    /// Pre-reserve the tx's pools (mirrors what the N6 submit orchestration
    /// does before spawning the monitor).
    fn reserve_tx_state(dispatcher: &mut Dispatcher, tx: &SubmittedTx) {
        dispatcher.reserve_pools(tx.pools.iter().cloned());
    }

    #[tokio::test]
    async fn confirm_on_first_probe_releases_pools() {
        let mut dispatcher = Dispatcher::for_block(100);
        let tx = sample_tx(100);
        reserve_tx_state(&mut dispatcher, &tx);
        assert!(dispatcher.is_pool_pending(&PoolKey::new("poolA")));
        let dispatcher = Arc::new(Mutex::new(dispatcher));

        let probe = StepProbe::new(Arc::clone(&dispatcher), 1); // confirms on probe #1
        let outcome = monitor(tx, &probe, &dispatcher, BLOCKS_BEFORE_NONCE_EXPIRES)
            .await
            .unwrap();

        assert_eq!(
            outcome,
            MonitorOutcome::Confirmed {
                confirmed_at_block: 100
            }
        );
        assert_eq!(probe.calls(), 1);
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
    async fn confirm_on_later_head_event_releases() {
        let mut dispatcher = Dispatcher::for_block(100);
        let tx = sample_tx(100);
        reserve_tx_state(&mut dispatcher, &tx);
        let dispatcher = Arc::new(Mutex::new(dispatcher));

        let probe = StepProbe::new(Arc::clone(&dispatcher), 3); // confirms on probe #3
        let outcome = monitor(tx, &probe, &dispatcher, BLOCKS_BEFORE_NONCE_EXPIRES)
            .await
            .unwrap();
        assert!(matches!(outcome, MonitorOutcome::Confirmed { .. }));
        assert_eq!(probe.calls(), 3);
        assert!(!dispatcher
            .lock()
            .unwrap()
            .is_pool_pending(&PoolKey::new("poolA")));
    }

    #[tokio::test]
    async fn expire_after_threshold_releases_pools() {
        // The chain advances one block per head event; the tx never confirms.
        let mut dispatcher = Dispatcher::for_block(100);
        let tx = sample_tx(100);
        reserve_tx_state(&mut dispatcher, &tx);
        let dispatcher = Arc::new(Mutex::new(dispatcher));
        let probe = StepProbe::new(Arc::clone(&dispatcher), u64::MAX);

        let outcome = monitor(tx, &probe, &dispatcher, BLOCKS_BEFORE_NONCE_EXPIRES)
            .await
            .unwrap();

        // expiry: blocks_waited > 5. Clock starts at 100; submission_block 100;
        // one +1 advance per probe → blocks_waited=1..6>5 on probe #6.
        match outcome {
            MonitorOutcome::Expired { blocks_waited } => {
                assert!(blocks_waited > BLOCKS_BEFORE_NONCE_EXPIRES);
                assert!(blocks_waited <= BLOCKS_BEFORE_NONCE_EXPIRES + 2);
            }
            other @ MonitorOutcome::Confirmed { .. } => panic!("expected Expired, got {other:?}"),
        }
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
    async fn release_on_both_paths_no_leak() {
        // Regression: neither path may leak a pool. Both Confirmed and
        // Expired must call release_tx exactly once.
        // (confirm_at, expect_confirm)
        let cases = [(1u64, true), (u64::MAX, false)];
        for (confirm_at, expect_confirm) in cases {
            let mut dispatcher = Dispatcher::for_block(100);
            let tx = sample_tx_variant(7, "pX", 100);
            reserve_tx_state(&mut dispatcher, &tx);
            let dispatcher = Arc::new(Mutex::new(dispatcher));
            let probe = StepProbe::new(Arc::clone(&dispatcher), confirm_at);
            let outcome = monitor(tx, &probe, &dispatcher, BLOCKS_BEFORE_NONCE_EXPIRES)
                .await
                .unwrap();
            assert_eq!(outcome.is_confirmed(), expect_confirm);
            assert!(
                !dispatcher
                    .lock()
                    .unwrap()
                    .is_pool_pending(&PoolKey::new("pX")),
                "pool leaked"
            );
        }
    }

    // ── head-event behaviour (replaces the old poll-sleep cadence test) ──

    /// Inclusion is gated on a real head event, not a timer: with the tx
    /// pending and no head event emitted, the monitor stays parked no matter
    /// how much wall-clock time elapses; a single `advance_block` (the head
    /// event) wakes it and resolves `Confirmed`.
    #[tokio::test]
    async fn inclusion_resolves_on_head_event_not_timer() {
        let mut dispatcher = Dispatcher::for_block(100);
        let tx = sample_tx_variant(0xEE, "eventPool", 100);
        reserve_tx_state(&mut dispatcher, &tx);
        let dispatcher = Arc::new(Mutex::new(dispatcher));

        // Confirms only once the shared clock reaches 101 (i.e. after a head
        // event). The initial probe sees 100 → false, so it parks.
        let probe = ClockProbe::new(Arc::clone(&dispatcher), 101);
        let mut fut = Box::pin(monitor_pending_transaction(
            tx,
            &probe,
            &dispatcher,
            BLOCKS_BEFORE_NONCE_EXPIRES,
        ));

        // No head event yet: real wall-clock time elapses and the monitor
        // still parks — there is no sleep-poll driving it.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut fut)
                .await
                .is_err(),
            "monitor resolved without a head event"
        );
        assert_eq!(probe.calls(), 1, "only the initial probe ran");

        // Emit exactly one head event.
        dispatcher.lock().unwrap().advance_block(101);

        let outcome = fut.await.unwrap();
        assert_eq!(
            outcome,
            MonitorOutcome::Confirmed {
                confirmed_at_block: 101
            }
        );
        assert_eq!(probe.calls(), 2, "one receipt probe per head event");
        assert!(!dispatcher
            .lock()
            .unwrap()
            .is_pool_pending(&PoolKey::new("eventPool")));
    }

    // ── proptest: lifecycle state machine ────────────────────────────────

    proptest! {
        /// For any threshold T in 1..30 and a probe that confirms on call
        /// `confirm_at` (1..40), the outcome is Confirmed iff the confirm call
        /// wins the race against the expiry call; either way the pools are
        /// released exactly once (no leak).
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
                let dispatcher = Arc::new(Mutex::new(dispatcher));
                let probe = StepProbe::new(Arc::clone(&dispatcher), confirm_at);
                monitor(tx, &probe, &dispatcher, threshold).await.unwrap()
            });

            // StepProbe advances +1/block per pending probe, so on probe #k
            // either the confirm returns true (before any advance) or the
            // monitor reads blocks_waited = k. Expiry first happens on probe
            // #(threshold+1); the receipt gate is evaluated before the expiry
            // gate, so a tie on that probe goes to confirm.
            let expiry_call = threshold + 1;
            let expected_confirm = confirm_at <= expiry_call;
            prop_assert_eq!(
                outcome.is_confirmed(),
                expected_confirm,
                "confirm_at={} threshold={} expiry_call={} outcome={:?}",
                confirm_at, threshold, expiry_call, outcome,
            );
        }

        /// A never-confirm probe always expires; `blocks_waited` strictly
        /// exceeds the threshold and equals `threshold + 1`.
        #[test]
        fn prop_outcome_expired_never_confirm(
            threshold in 1u64..30,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let outcome = rt.block_on(async move {
                let mut dispatcher = Dispatcher::for_block(0);
                let tx = sample_tx_variant(9, "pY", 0);
                reserve_tx_state(&mut dispatcher, &tx);
                let dispatcher = Arc::new(Mutex::new(dispatcher));
                let probe = StepProbe::new(Arc::clone(&dispatcher), u64::MAX);
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

        /// `to_committed` round-trips the pool set.
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
            let committed_set: HashSet<&PoolKey> = committed.pools.iter().collect();
            let expected: HashSet<&PoolKey> = pools.iter().collect();
            prop_assert_eq!(committed_set, expected);
        }
    }

    // ── helpers ───────────────────────────────────────────────────────────

    /// Test alias for the production monitor (no timer): the loop is driven
    /// by the head events the probes publish via `Dispatcher::advance_block`.
    fn monitor<'a>(
        tx: SubmittedTx,
        probe: &'a impl ReceiptProbe,
        dispatcher: &'a Mutex<Dispatcher>,
        blocks_before_nonce_expires: u64,
    ) -> Pin<Box<dyn Future<Output = SubmissionResult<MonitorOutcome>> + Send + 'a>> {
        Box::pin(monitor_pending_transaction(
            tx,
            probe,
            dispatcher,
            blocks_before_nonce_expires,
        ))
    }

    /// RMHQAR : the monitor span records "monitor.result" on every
    /// terminal path. Unique nonces filter this test's spans from the shared global
    /// capture (MQUKB6 unique-identifier rule).
    #[tokio::test]
    async fn monitor_span_records_terminal_outcomes() {
        const CONFIRM_NONCE: u64 = 0xC0FF_EE01;
        const EXPIRE_NONCE: u64 = 0xC0FF_EE02;
        let cap = crate::span_capture::global();

        // Confirmed path: StepProbe confirms on probe #1 (no head event
        // needed — inclusion is observed by the initial probe).
        let mut dispatcher = Dispatcher::for_block(100);
        let tx = sample_tx_variant(CONFIRM_NONCE, "capPoolA", 100);
        reserve_tx_state(&mut dispatcher, &tx);
        let dispatcher = Arc::new(Mutex::new(dispatcher));
        let probe = StepProbe::new(Arc::clone(&dispatcher), 1);
        let outcome =
            monitor_pending_transaction(tx, &probe, &dispatcher, BLOCKS_BEFORE_NONCE_EXPIRES)
                .await
                .unwrap();
        assert!(outcome.is_confirmed());

        // Expired path: never-confirm; each pending probe publishes a head
        // event and advances the clock. Threshold 1 → expiry on probe #2.
        let mut dispatcher = Dispatcher::for_block(100);
        let tx = sample_tx_variant(EXPIRE_NONCE, "capPoolB", 100);
        reserve_tx_state(&mut dispatcher, &tx);
        let dispatcher = Arc::new(Mutex::new(dispatcher));
        let probe = StepProbe::new(Arc::clone(&dispatcher), u64::MAX);
        let outcome = monitor_pending_transaction(tx, &probe, &dispatcher, 1)
            .await
            .unwrap();
        assert!(!outcome.is_confirmed());

        let mut confirmed = 0;
        let mut expired = 0;
        for (name, fields) in cap.snapshot() {
            if name != "degenbot.bundle.monitor" {
                continue;
            }
            match fields.get("nonce").map(String::as_str) {
                Some(v) if v == CONFIRM_NONCE.to_string().as_str() => {
                    assert_eq!(
                        fields.get("monitor.result").map(String::as_str),
                        Some("confirmed")
                    );
                    confirmed += 1;
                }
                Some(v) if v == EXPIRE_NONCE.to_string().as_str() => {
                    assert_eq!(
                        fields.get("monitor.result").map(String::as_str),
                        Some("expired")
                    );
                    expired += 1;
                }
                _ => {}
            }
        }
        assert_eq!(
            confirmed, 1,
            "exactly one confirmed monitor span for {CONFIRM_NONCE}"
        );
        assert_eq!(
            expired, 1,
            "exactly one expired monitor span for {EXPIRE_NONCE}"
        );
    }
}
