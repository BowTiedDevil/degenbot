//! The at-most-once verification claim — the ONE owner of the claim policy.
//!
//! A pool's registration verify lifecycle (ADR-022) is expensive and not
//! safe to repeat inside one window: two concurrent callers both seeding a
//! verify can move the drain pin under each other and false-trip the
//! mismatch tripwire. The invariant is therefore **at most one lifecycle run
//! per live claim window per pool**, and this module is where that invariant
//! lives: a claim table plus the whole policy that governs it. It is an
//! invariant, not a cache — a caller's own memo may skip a later window, but
//! only this table decides that concurrent callers share one run.
//!
//! # The policy (stated once, here)
//!
//! * **claim-if-absent (leader)** — the first caller for a claim key
//!   registers a fresh claim under the table lock and runs the work.
//! * **wait-if-present (peer)** — a caller that sees a live claim waits on
//!   THAT claim instead of re-running the work, then receives the leader's
//!   settled outcome (success, or the leader's failure re-issued to the
//!   peer), so the work runs at most once per window.
//! * **release-on-settlement (and on failure)** — the leader evicts its own
//!   entry by identity check when it finishes, so a failed lifecycle stays
//!   retriable by a LATER caller and a stale leader can never clobber a
//!   newer claim a retry already registered.
//! * **abandon-reclaims (cancellation safety)** — a leader whose future is
//!   dropped mid-window (task cancel, panic) never settles, so the window is
//!   released as abandoned and a waiting peer re-claims and runs the work. A
//!   peer never waits forever on a leader that will never answer, and
//!   at-most-once still holds for every window that settles.
//!
//! # Why the wake is a `watch` channel
//!
//! Peers must not miss a settlement racing their first poll, and a peer that
//! subscribes after settlement must read the outcome rather than block.
//! `tokio::sync::watch` gives both: the value IS the settlement, and its
//! version counter makes a missed update impossible. The outcome travels with
//! the primitive instead of being reconstructed by the waiter.
//!
//! # Adapters, and why the seam is real
//!
//! One policy, genuinely different callers: the async driver entry point
//! awaits it, the seat-thread `_sync` entry point parks a plain thread inside
//! `runtime.block_on`, the `PyO3` operator surface awaits it through
//! `future_into_py`, and the fleet seats reach it over the blocking bridge.
//! Same type, same table, one policy; no caller re-implements any of it.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::watch;

use crate::bot_core::registration_ledger::RegistrationLedger;

/// The shared claim table, held behind an `Arc` so a leader guard can evict
/// its own entry without borrowing the table's owner.
type ClaimTable<E> = Arc<Mutex<HashMap<String, Arc<VerifyClaim<E>>>>>;

/// The settlement state of one claim window.
#[derive(Debug)]
enum Settlement<E> {
    /// The leader is still running the work.
    Pending,
    /// The leader finished; every peer receives this outcome.
    Settled(Result<(), E>),
    /// The leader's future was dropped without settling. A waiting peer
    /// re-claims rather than inheriting an answer nobody will give.
    Abandoned,
}

/// One live claim window: the settlement slot peers read.
#[derive(Debug)]
struct VerifyClaim<E> {
    settlement: watch::Sender<Settlement<E>>,
}

impl<E: Clone> VerifyClaim<E> {
    fn new() -> Self {
        Self {
            settlement: watch::Sender::new(Settlement::Pending),
        }
    }

    /// The leader's publish. Replaces any prior value, so a subscriber that
    /// arrives after settlement reads it directly.
    fn settle(&self, outcome: Result<(), E>) {
        self.settlement.send_replace(Settlement::Settled(outcome));
    }

    fn abandon(&self) {
        self.settlement.send_replace(Settlement::Abandoned);
    }

    /// The peer's wait. `None` means the window was abandoned and the caller
    /// must re-claim.
    async fn wait(&self) -> Option<Result<(), E>> {
        let mut settlement = self.settlement.subscribe();
        loop {
            let outcome = match &*settlement.borrow_and_update() {
                Settlement::Pending => None,
                Settlement::Settled(outcome) => Some(Some(outcome.clone())),
                Settlement::Abandoned => Some(None),
            };
            if let Some(outcome) = outcome {
                return outcome;
            }
            if settlement.changed().await.is_err() {
                // The leader's sender lives in the table entry this window
                // owns, so a dropped sender means the entry is already gone.
                return None;
            }
        }
    }
}

/// The leader half of a claim window, armed only when the caller won it.
///
/// The eviction is identity-checked, so only the leader that owns the live
/// entry removes it. `Drop` also covers a dropped or panicking future: the
/// window is marked abandoned (peers re-claim) and then released.
struct LeaderGuard<E: Clone> {
    table: ClaimTable<E>,
    key: String,
    claim: Arc<VerifyClaim<E>>,
    armed: bool,
}

impl<E: Clone> LeaderGuard<E> {
    fn settle(mut self, outcome: Result<(), E>) -> Result<(), E> {
        self.claim.settle(outcome.clone());
        self.release();
        outcome
    }

    fn release(&mut self) {
        if self.armed {
            self.armed = false;
            let mut table = self.table.lock();
            if table
                .get(&self.key)
                .is_some_and(|existing| Arc::ptr_eq(existing, &self.claim))
            {
                table.remove(&self.key);
            }
        }
    }
}

impl<E: Clone> Drop for LeaderGuard<E> {
    fn drop(&mut self) {
        // Only an UNSETTLED window is abandoned. `settle` disarms the guard
        // before this runs, so a settled window's published outcome is never
        // overwritten by the drop path.
        if self.armed {
            self.claim.abandon();
            self.release();
        }
    }
}

/// The at-most-once claim table: the one owner of the claim policy.
#[derive(Debug)]
pub struct VerifyClaims<E> {
    table: ClaimTable<E>,
}

impl<E> Default for VerifyClaims<E> {
    fn default() -> Self {
        Self {
            table: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl<E: Clone + Send + Sync + 'static> VerifyClaims<E> {
    /// An empty claim table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim-if-absent under the table lock: the check and the insert are
    /// contiguous, so two callers racing one key cannot both win. The guard
    /// is armed only for the winner; a peer's is inert.
    fn acquire(&self, key: &str) -> (Arc<VerifyClaim<E>>, LeaderGuard<E>) {
        let (claim, armed) = {
            let mut table = self.table.lock();
            if let Some(existing) = table.get(key) {
                (Arc::clone(existing), false)
            } else {
                let claim = Arc::new(VerifyClaim::new());
                table.insert(key.to_string(), Arc::clone(&claim));
                (claim, true)
            }
        };
        (
            Arc::clone(&claim),
            LeaderGuard {
                table: Arc::clone(&self.table),
                key: key.to_string(),
                claim,
                armed,
            },
        )
    }

    /// Run `run` at most once per live claim window for `key`.
    ///
    /// A peer that finds a live claim waits for the leader and receives the
    /// leader's outcome; a window whose leader was abandoned is re-claimed and
    /// re-run, so at-most-once holds for every window that settles while a
    /// cancelled leader can never strand a waiter.
    ///
    /// # Errors
    ///
    /// The leader's own error, or a re-issued copy of it when this caller
    /// was a peer. A failed window is released, so a later caller retries.
    pub async fn run_exclusive<F, Fut>(&self, key: &str, run: F) -> Result<(), E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), E>>,
    {
        loop {
            let (claim, guard) = self.acquire(key);
            if guard.armed {
                let outcome = run().await;
                return guard.settle(outcome);
            }
            // Peer: wait for the settlement. A settled window hands back the
            // leader's outcome; an abandoned one loops back to claim it.
            if let Some(outcome) = claim.wait().await {
                return outcome;
            }
        }
    }

    /// How many claim windows are live (test/diagnostic witness).
    #[must_use]
    pub fn live_claim_count(&self) -> usize {
        self.table.lock().len()
    }

    /// Whether `key` currently holds a live claim window.
    #[must_use]
    pub fn has_live_claim(&self, key: &str) -> bool {
        self.table.lock().contains_key(key)
    }
}

/// The per-pool registration-verify entry: the live-window claim policy
/// ([`VerifyClaims`]) plus the durable **verified pool** fact the registration
/// ledger owns.
///
/// The claim alone dedups callers that overlap in time, then releases its
/// window on settlement — by design, so a failure stays retriable and a
/// cancelled leader cannot strand a peer. A registration path can therefore
/// reach the same pool's lifecycle again in a later window. This owner is what
/// makes that a no-op once a lifecycle has COMPLETED, without teaching the
/// claim table to hold a settled success: a success records a pool fact in the
/// ledger, a failure or an abandoned window records nothing.
///
/// The key is the caller's identity string (for the driver, the family-scoped
/// claim key). A fact is key-scoped, never address-scoped, so two families at
/// one address and two `(PoolManager, pool_id)` pools under one manager are
/// independent entries.
#[derive(Debug)]
pub struct PoolVerifications<E> {
    claims: VerifyClaims<E>,
    ledger: Mutex<RegistrationLedger>,
}

impl<E: Clone + Send + Sync + 'static> Default for PoolVerifications<E> {
    fn default() -> Self {
        Self {
            claims: VerifyClaims::new(),
            ledger: Mutex::new(RegistrationLedger::new()),
        }
    }
}

impl<E: Clone + Send + Sync + 'static> PoolVerifications<E> {
    /// An empty per-pool verification owner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this pool's verify lifecycle already COMPLETED.
    #[must_use]
    pub fn is_verified(&self, key: &str) -> bool {
        self.ledger.lock().pool_verified(key)
    }

    /// Record a completed verify lifecycle (a pool fact).
    pub fn mark_verified(&self, key: &str) {
        self.ledger.lock().memoize_verified_pool(key.to_string());
    }

    /// Run `run` for `key` at most once per live claim window, and never once
    /// the pool's lifecycle has completed.
    ///
    /// A verified pool returns `Ok` without invoking `run`. Otherwise the run
    /// enters [`VerifyClaims`]: concurrent callers share the leader's run and
    /// its outcome, and a failed or abandoned window is released so a later
    /// caller retries. Success records the durable fact.
    ///
    /// # Errors
    ///
    /// The leader's own error, or a re-issued copy of it when this caller was
    /// a peer. A failed window is released and stays retriable.
    pub async fn run_exclusive<F, Fut>(&self, key: &str, run: F) -> Result<(), E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), E>>,
    {
        if self.is_verified(key) {
            return Ok(());
        }
        let outcome = self.claims.run_exclusive(key, run).await;
        if outcome.is_ok() {
            self.mark_verified(key);
        }
        outcome
    }

    /// How many claim windows are live (test/diagnostic witness).
    #[must_use]
    pub fn live_claim_count(&self) -> usize {
        self.claims.live_claim_count()
    }

    /// How many pools have a durable completed-lifecycle fact
    /// (test/diagnostic witness).
    #[must_use]
    pub fn verified_pool_count(&self) -> usize {
        self.ledger.lock().verified_pool_count()
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// A stand-in lifecycle failure that carries the two-way transient /
    /// fatal split every claim consumer classifies on.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeVerifyError {
        kind: &'static str,
    }

    type Claims = VerifyClaims<FakeVerifyError>;

    /// The success value every policy test settles with.
    const OK: Result<(), FakeVerifyError> = Ok(());

    /// Leader success: N concurrent callers, one run, every caller sees the
    /// settled outcome, and the window is released.
    #[tokio::test]
    async fn async_caller_leader_runs_once_and_peers_share_the_outcome() {
        let claims = Arc::new(Claims::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(8));
        let mut handles = Vec::new();
        for _ in 0..8 {
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
                            OK
                        }
                    })
                    .await
            }));
        }
        for handle in handles {
            assert_eq!(handle.await.unwrap(), Ok(()));
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "one run per window");
        assert_eq!(claims.live_claim_count(), 0, "released on settlement");
    }

    /// Leader failure: peers receive the leader's outcome, the failed window
    /// is released, and a LATER caller re-claims and retries.
    #[tokio::test]
    async fn async_caller_failure_is_shared_and_stays_retriable() {
        let claims = Claims::new();
        let first = claims
            .run_exclusive("v3:0xdef", || async {
                Err(FakeVerifyError { kind: "rpc" })
            })
            .await;
        assert_eq!(first, Err(FakeVerifyError { kind: "rpc" }));
        assert_eq!(claims.live_claim_count(), 0, "released on failure");
        assert!(
            claims
                .run_exclusive("v3:0xdef", || async { OK })
                .await
                .is_ok(),
            "a failed claim must stay retriable"
        );
    }

    /// A peer that arrives while the leader is failing receives the failure
    /// rather than re-running the lifecycle.
    #[tokio::test]
    async fn async_caller_peer_inherits_the_leaders_failure_without_re_running() {
        let claims = Arc::new(Claims::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let claims = Arc::clone(&claims);
            let runs = Arc::clone(&runs);
            let started = Arc::clone(&started);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                claims
                    .run_exclusive("v4:0x1", || {
                        let runs = Arc::clone(&runs);
                        let started = Arc::clone(&started);
                        async move {
                            runs.fetch_add(1, Ordering::SeqCst);
                            started.notify_one();
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            Err(FakeVerifyError { kind: "mismatch" })
                        }
                    })
                    .await
            }));
        }
        for handle in handles {
            assert_eq!(
                handle.await.unwrap(),
                Err(FakeVerifyError { kind: "mismatch" }),
                "every caller observes the same settled failure"
            );
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "the peer never re-ran");
    }

    /// Cancellation safety: a leader whose task is dropped before settling
    /// does not strand its peer — the peer re-claims and runs the work.
    #[tokio::test]
    async fn abandoned_leader_window_is_reclaimed_by_the_peer() {
        let claims = Arc::new(Claims::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let claimed = Arc::new(tokio::sync::Notify::new());
        let leader = {
            let claims = Arc::clone(&claims);
            let claimed = Arc::clone(&claimed);
            tokio::spawn(async move {
                claims
                    .run_exclusive("v3:0xcancel", || async {
                        claimed.notify_one();
                        std::future::pending::<()>().await;
                        OK
                    })
                    .await
            })
        };
        claimed.notified().await;
        assert!(claims.has_live_claim("v3:0xcancel"));
        leader.abort();
        let _ = leader.await;

        assert!(
            claims
                .run_exclusive("v3:0xcancel", || async {
                    runs.fetch_add(1, Ordering::SeqCst);
                    OK
                })
                .await
                .is_ok(),
            "an abandoned window must be re-claimable"
        );
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(claims.live_claim_count(), 0);
    }

    /// The blocking-caller adapter: a plain thread (a fleet seat, no
    /// `Send`-bound driver task) parks on the same table through
    /// `block_on` and gets the same policy — one run, shared outcome.
    #[test]
    fn blocking_caller_shares_the_same_policy_matrix() {
        let claims = Arc::new(Claims::new());
        let runs = Arc::new(AtomicUsize::new(0));
        // Every seat thread reaches the claim together; whichever one wins
        // holds the window open long enough for the rest to park on it.
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let claims = Arc::clone(&claims);
            let runs = Arc::clone(&runs);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                degenbot_core::runtime::get_runtime().block_on(async move {
                    barrier.wait();
                    claims
                        .run_exclusive("v3:0xseat", || {
                            let runs = Arc::clone(&runs);
                            async move {
                                runs.fetch_add(1, Ordering::SeqCst);
                                tokio::time::sleep(Duration::from_millis(50)).await;
                                OK
                            }
                        })
                        .await
                })
            }));
        }
        for handle in handles {
            assert_eq!(handle.join().unwrap(), Ok(()));
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "one run per window");
        assert_eq!(claims.live_claim_count(), 0);
    }

    /// A blocking caller inherits a failure and a later caller retries.
    #[test]
    fn blocking_caller_failure_is_shared_and_stays_retriable() {
        let claims = Claims::new();
        let failure = degenbot_core::runtime::get_runtime().block_on(
            claims.run_exclusive("v3:0xseat-fail", || async {
                Err(FakeVerifyError { kind: "rpc" })
            }),
        );
        assert_eq!(failure, Err(FakeVerifyError { kind: "rpc" }));
        assert_eq!(claims.live_claim_count(), 0);
        assert!(degenbot_core::runtime::get_runtime()
            .block_on(claims.run_exclusive("v3:0xseat-fail", || async { OK }))
            .is_ok());
    }

    /// The release is identity-checked: a leader that has already released
    /// its window can never evict the newer claim a retry registered in its
    /// place, and the live entry survives the stale leader going out of
    /// scope.
    #[test]
    fn stale_release_never_clobbers_a_newer_claim() {
        let claims = Claims::new();
        let (first, mut first_guard) = claims.acquire("v3:0xidentity");
        assert!(first_guard.armed, "the first caller claims an empty key");
        first_guard.release();

        let (second, mut second_guard) = claims.acquire("v3:0xidentity");
        assert!(second_guard.armed, "the retry re-claims the released key");
        assert!(!Arc::ptr_eq(&first, &second), "a re-claim is a new window");

        // A stale leader leaving scope must not touch the live entry.
        drop(first_guard);
        drop(first);
        assert_eq!(claims.live_claim_count(), 1, "the newer claim is intact");

        second_guard.release();
        assert_eq!(claims.live_claim_count(), 0);
    }

    // ── PoolVerifications: the durable verified-pool fact ────────

    /// The registration entry the driver holds: a completed lifecycle records
    /// a pool fact and a later caller for the same identity does NOT re-run
    /// the work, while the live window is still released.
    #[tokio::test]
    async fn pool_verifications_skip_a_completed_lifecycle() {
        let verifications = PoolVerifications::<FakeVerifyError>::new();
        let runs = Arc::new(AtomicUsize::new(0));

        let first = verifications
            .run_exclusive("v3:0xabc", || {
                let runs = Arc::clone(&runs);
                async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    OK
                }
            })
            .await;
        assert_eq!(first, Ok(()));
        assert!(verifications.is_verified("v3:0xabc"));
        assert_eq!(verifications.verified_pool_count(), 1);

        let second = verifications
            .run_exclusive("v3:0xabc", || {
                let runs = Arc::clone(&runs);
                async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    OK
                }
            })
            .await;
        assert_eq!(second, Ok(()), "a verified pool returns the settled answer");
        assert_eq!(runs.load(Ordering::SeqCst), 1, "no re-run after success");
        assert_eq!(verifications.live_claim_count(), 0);
    }

    /// A failed lifecycle records NO fact and stays retriable; the retry that
    /// succeeds then becomes durable.
    #[tokio::test]
    async fn pool_verifications_failure_is_retriable_and_records_no_fact() {
        let verifications = PoolVerifications::<FakeVerifyError>::new();

        let first = verifications
            .run_exclusive("v3:0xdef", || async {
                Err(FakeVerifyError { kind: "rpc" })
            })
            .await;
        assert_eq!(first, Err(FakeVerifyError { kind: "rpc" }));
        assert!(!verifications.is_verified("v3:0xdef"));
        assert_eq!(verifications.verified_pool_count(), 0);

        let second = verifications
            .run_exclusive("v3:0xdef", || async { OK })
            .await;
        assert_eq!(second, Ok(()), "a failed claim must stay retriable");
        assert!(verifications.is_verified("v3:0xdef"));
    }

    /// Concurrent peers still enter ONE live window: the durable fact does not
    /// weaken the at-most-once-per-window policy.
    #[tokio::test]
    async fn pool_verifications_concurrent_peers_share_one_run() {
        let verifications = Arc::new(PoolVerifications::<FakeVerifyError>::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(8));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let verifications = Arc::clone(&verifications);
            let runs = Arc::clone(&runs);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                verifications
                    .run_exclusive("v4:0xM:0x1", || {
                        let runs = Arc::clone(&runs);
                        async move {
                            runs.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            OK
                        }
                    })
                    .await
            }));
        }
        for handle in handles {
            assert_eq!(handle.await.unwrap(), Ok(()));
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "one run per live window");
        assert_eq!(verifications.verified_pool_count(), 1);
        assert_eq!(verifications.live_claim_count(), 0);
    }

    /// The fact is KEY-scoped: two families at one address and two V4 pools
    /// under one `PoolManager` are independent — a success on one never
    /// verifies the other.
    #[tokio::test]
    async fn pool_verifications_facts_are_key_scoped_not_address_scoped() {
        let verifications = PoolVerifications::<FakeVerifyError>::new();

        verifications
            .run_exclusive("v3:0xabc", || async { OK })
            .await
            .unwrap();
        assert!(verifications.is_verified("v3:0xabc"));
        assert!(
            !verifications.is_verified("v2:0xabc"),
            "the family is part of the identity at one address"
        );
        assert!(
            !verifications.is_verified("v4:0xabc:0x1"),
            "a V4 pair is a different identity"
        );

        verifications
            .run_exclusive("v4:0xM:0x1", || async { OK })
            .await
            .unwrap();
        assert!(verifications.is_verified("v4:0xM:0x1"));
        assert!(
            !verifications.is_verified("v4:0xM:0x2"),
            "one PoolManager hosts many independently-verified pools"
        );
    }
}
