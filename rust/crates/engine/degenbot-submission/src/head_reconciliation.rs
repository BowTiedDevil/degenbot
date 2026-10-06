//! Head reconciliation: the one per-head owner of the operator account's
//! nonce-lifespan facts and their fold into each strategy's default policy.
//!
//! Before this module the per-head choreography existed in as many homes as
//! there were clocks: every driver loop refreshed the shared [`NonceAuthority`]
//! and reconciled the shared submission ledger on its own head tick, and the
//! hosted head clock did it again — so N active strategies re-ran one
//! operation over shared state per head while two of the three passes
//! discarded the notices they produced. Here the choreography is one
//! interface: [`HeadReconciliation::reconcile_head`] claims a head at most
//! once, gates on hosted activity, reads the chain's confirmed nonce, drives
//! the host's reorg-safe refresh + reconcile, and folds every typed notice
//! through the owning lane's [`HeadPolicy`]. Extra triggers are free — a head
//! already reconciled (or consumed by a failed read, which defers to the next
//! head exactly as the hosted clock always did) is deduped — so callers
//! coordinate nothing among themselves.
//!
//! The fold carries each [`PolicyAction`] outcome rather than acting on it:
//! the reaction to an orphan fill or a stale outcome stays with the strategy's
//! decide stage, not here.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use alloy::primitives::Address;
use degenbot_bot::strategy_host::StrategyHost;
use degenbot_substrate::nonce::StrategyId;
use parking_lot::Mutex;

use crate::submission_ledger::{HeadPolicy, NonceLane};

/// The per-strategy sign-time lanes the head feed folds notices through,
/// keyed by the owning strategy.
pub type HeadLanes = HashMap<StrategyId, Arc<NonceLane>>;

/// The chain's confirmed-nonce read the reconciliation drives once per head.
///
/// Production dials the operator's chain node; tests script the answer. The
/// method boxes its future so the read stays a shared `Arc<dyn …>` seam
/// without an async-trait dependency.
pub trait ChainNonceRead: Send + Sync {
    /// The operator account's next unused nonce at the chain's confirmed head.
    ///
    /// # Errors
    ///
    /// The transport's own refusal; a failure defers reconciliation to the
    /// next head and is never fatal.
    fn next_nonce<'a>(
        &'a self,
        operator: Address,
    ) -> Pin<Box<dyn Future<Output = Result<u64, String>> + Send + 'a>>;
}

/// The production read over an `alloy` provider.
pub struct AlloyChainNonceRead(pub Arc<degenbot_rpc::provider::AlloyProvider>);

impl ChainNonceRead for AlloyChainNonceRead {
    fn next_nonce<'a>(
        &'a self,
        operator: Address,
    ) -> Pin<Box<dyn Future<Output = Result<u64, String>> + Send + 'a>> {
        Box::pin(async move {
            self.0
                .get_transaction_count(&operator, None)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

/// What one [`HeadReconciliation::reconcile_head`] call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// No lease and no non-terminal record: nothing to reconcile, no chain
    /// read paid.
    Idle,
    /// This head (or a newer one) already ran; the trigger was redundant.
    Deduped,
    /// The chain read failed; the next head retries.
    ReadFailed,
    /// The head reconciled; the fold counted its policy outcomes.
    Reconciled {
        /// How many typed notices folded into a lane policy.
        folded: u64,
    },
}

/// The once-per-head reconciliation over the host's shared nonce facts.
pub struct HeadReconciliation {
    host: Arc<Mutex<StrategyHost>>,
    lanes: HeadLanes,
    read: Arc<dyn ChainNonceRead>,
    operator: Address,
    last: AtomicU64,
}

impl HeadReconciliation {
    /// Build the reconciliation over the host, the per-strategy lanes, and the
    /// chain read it drives.
    #[must_use]
    pub fn new(
        host: Arc<Mutex<StrategyHost>>,
        lanes: HeadLanes,
        read: Arc<dyn ChainNonceRead>,
        operator: Address,
    ) -> Self {
        Self {
            host,
            lanes,
            read,
            operator,
            last: AtomicU64::new(0),
        }
    }

    /// Reconcile one head: gate → claim → read → refresh/reconcile → fold.
    ///
    /// At most one call per head reaches the chain read, however many clocks
    /// (driver loops, the hosted head clock) trigger it; an idle host pays
    /// nothing at all.
    pub async fn reconcile_head(&self, head: u64) -> ReconcileOutcome {
        if !self.host.lock().has_hosted_activity() {
            return ReconcileOutcome::Idle;
        }
        if !self.claim(head) {
            return ReconcileOutcome::Deduped;
        }
        let confirmed = match self.read.next_nonce(self.operator).await {
            Ok(confirmed) => confirmed,
            Err(error) => {
                tracing::warn!(
                    target: "degenbot.submission.head",
                    head,
                    operator = %self.operator,
                    %error,
                    "per-head chain nonce read failed; reconciliation deferred to the next head"
                );
                return ReconcileOutcome::ReadFailed;
            }
        };
        let notices = self.host.lock().on_head(confirmed);
        let mut folded = 0u64;
        for notice in &notices {
            let Some(lane) = self.lanes.get(notice.strategy()) else {
                continue;
            };
            let policy = HeadPolicy::new(Arc::clone(lane));
            match policy.on_notice(notice) {
                Ok(action) => {
                    folded += 1;
                    tracing::info!(
                        target: "degenbot.submission.head",
                        head,
                        strategy = %notice.strategy(),
                        nonce = notice.nonce(),
                        action = ?action,
                        "head notice folded into the owning lane policy"
                    );
                }
                Err(decline) => {
                    tracing::warn!(
                        target: "degenbot.submission.head",
                        head,
                        strategy = %notice.strategy(),
                        nonce = notice.nonce(),
                        %decline,
                        "head policy could not re-stamp"
                    );
                }
            }
        }
        ReconcileOutcome::Reconciled { folded }
    }

    /// Claim `head` for reconciliation exactly once.
    fn claim(&self, head: u64) -> bool {
        let mut last = self.last.load(Ordering::Acquire);
        loop {
            if head <= last {
                return false;
            }
            match self
                .last
                .compare_exchange(last, head, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(actual) => last = actual,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::expect_used,
        reason = "rig + assert helpers use expect for setup failures"
    )]
    use std::sync::atomic::AtomicBool;

    use degenbot_bot::strategy_host::{FacetStatus, StrategyHost};
    use degenbot_eventhub::Hub;
    use degenbot_substrate::nonce::{NonceAuthority, NonceLease};
    use degenbot_substrate::route_registry::RouteRegistry;

    use super::*;
    use crate::submission_ledger::{SubmissionLedger, TargetId};

    struct FakeRead {
        calls: AtomicU64,
        fail: AtomicBool,
        nonce: AtomicU64,
    }

    impl FakeRead {
        fn new() -> Self {
            Self {
                calls: AtomicU64::new(0),
                fail: AtomicBool::new(false),
                nonce: AtomicU64::new(0),
            }
        }

        fn calls(&self) -> u64 {
            self.calls.load(Ordering::SeqCst)
        }

        /// Script the account's next unused nonce the next read reports.
        fn reports(&self, nonce: u64) {
            self.nonce.store(nonce, Ordering::SeqCst);
        }
    }

    impl ChainNonceRead for FakeRead {
        fn next_nonce<'a>(
            &'a self,
            _operator: Address,
        ) -> Pin<Box<dyn Future<Output = Result<u64, String>> + Send + 'a>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if self.fail.load(Ordering::SeqCst) {
                    Err(String::from("scripted transport failure"))
                } else {
                    Ok(self.nonce.load(Ordering::SeqCst))
                }
            })
        }
    }

    fn sid(name: &str) -> StrategyId {
        StrategyId::new(name)
    }

    fn target(byte: u8) -> TargetId {
        TargetId::new(alloy::primitives::B256::repeat_byte(byte))
    }

    fn operator() -> Address {
        Address::repeat_byte(0x5c)
    }

    /// The real host + authority + ledger + one strategy lane, the production
    /// choreography's full shareable state.
    struct Rig {
        host: Arc<Mutex<StrategyHost>>,
        authority: Arc<NonceAuthority>,
        ledger: Arc<SubmissionLedger>,
        lane: Arc<NonceLane>,
        read: Arc<FakeRead>,
        strategy: StrategyId,
    }

    fn rig() -> Rig {
        let authority = Arc::new(NonceAuthority::new(0));
        let ledger = Arc::new(SubmissionLedger::new());
        let mut host = StrategyHost::new(
            Arc::new(Hub::new()),
            Arc::new(RouteRegistry::new(
                degenbot_bot::connector_index::V2ConnectorIndex::default(),
            )),
            Arc::clone(&authority),
        );
        let strategy = sid("settlement");
        host.register(strategy.clone(), FacetStatus::Configured)
            .expect("register");
        host.attach_reconciler(
            Arc::clone(&ledger) as Arc<dyn degenbot_bot::strategy_host::HeadReconciler>
        );
        let lane = Arc::new(NonceLane::new(
            Arc::clone(&authority),
            Arc::clone(&ledger),
            strategy.clone(),
        ));
        Rig {
            host: Arc::new(Mutex::new(host)),
            authority,
            ledger,
            lane,
            read: Arc::new(FakeRead::new()),
            strategy,
        }
    }

    fn reconciliation(rig: &Rig) -> HeadReconciliation {
        let mut lanes = HeadLanes::new();
        lanes.insert(rig.strategy.clone(), Arc::clone(&rig.lane));
        HeadReconciliation::new(
            Arc::clone(&rig.host),
            lanes,
            Arc::clone(&rig.read) as Arc<dyn ChainNonceRead>,
            operator(),
        )
    }

    fn signed_broadcast(rig: &Rig, lease: &NonceLease) {
        rig.lane
            .record_signed(lease, target(0), alloy::primitives::B256::repeat_byte(7), 0)
            .expect("record signed");
        rig.authority.record_broadcast(lease).expect("broadcast");
        rig.ledger
            .record_broadcast(&rig.strategy, lease.nonce())
            .expect("ledger broadcast");
    }

    #[tokio::test]
    async fn an_open_guard_reconciles_each_head_exactly_once() {
        let rig = rig();
        rig.lane.stamp().expect("stamp opens hosted activity");
        let rec = reconciliation(&rig);

        assert_eq!(
            rec.reconcile_head(100).await,
            ReconcileOutcome::Reconciled { folded: 0 }
        );
        assert_eq!(rig.read.calls(), 1, "the head pays one chain read");
        assert_eq!(rec.reconcile_head(100).await, ReconcileOutcome::Deduped);
        assert_eq!(rec.reconcile_head(99).await, ReconcileOutcome::Deduped);
        assert_eq!(rig.read.calls(), 1, "redundant triggers are free");
        assert_eq!(
            rec.reconcile_head(101).await,
            ReconcileOutcome::Reconciled { folded: 0 }
        );
        assert_eq!(rig.read.calls(), 2, "the next head reconciles again");
    }

    #[tokio::test]
    async fn an_idle_host_pays_no_chain_read() {
        let rig = rig();
        let rec = reconciliation(&rig);
        assert_eq!(rec.reconcile_head(100).await, ReconcileOutcome::Idle);
        assert_eq!(rig.read.calls(), 0, "the closed guard pays no chain read");
    }

    #[tokio::test]
    async fn a_failed_chain_read_defers_to_the_next_head() {
        let rig = rig();
        rig.lane.stamp().expect("stamp");
        rig.read.fail.store(true, Ordering::SeqCst);
        let rec = reconciliation(&rig);

        assert_eq!(rec.reconcile_head(100).await, ReconcileOutcome::ReadFailed);
        assert_eq!(rig.read.calls(), 1);
        assert_eq!(rec.reconcile_head(100).await, ReconcileOutcome::Deduped);
        assert_eq!(rec.reconcile_head(101).await, ReconcileOutcome::ReadFailed);
        assert_eq!(rig.read.calls(), 2, "the next head retries the read");
    }

    #[tokio::test]
    async fn a_landed_notice_folds_through_the_owning_lane() {
        let rig = rig();
        let lease = rig.lane.stamp().expect("stamp");
        signed_broadcast(&rig, &lease);
        // The broadcast at nonce 0 is mined: the account's next unused nonce
        // moves to 1, which is what lands the record.
        rig.read.reports(1);
        let rec = reconciliation(&rig);

        assert_eq!(
            rec.reconcile_head(200).await,
            ReconcileOutcome::Reconciled { folded: 1 },
            "the chain nonce landing the broadcast folds one Landed notice"
        );
    }

    #[tokio::test]
    async fn an_orphaned_notice_folds_as_a_re_stamp() {
        let rig = rig();
        let lease = rig.lane.stamp().expect("stamp");
        signed_broadcast(&rig, &lease);
        let gap = rig.lane.stamp().expect("the slot above the broadcast");
        assert_eq!(gap.nonce(), 1);
        // A rewind to the broadcast's own nonce revokes the lease at 1, and
        // the fold's re-stamp claims the vacated predecessor. The rewind is
        // only reachable from a clock that had advanced past the broadcast, so
        // an earlier head tick first lands the record and lifts the confirmed
        // clock to 2.
        rig.authority
            .release_broadcast(&rig.strategy, 0)
            .expect("vacate");
        let _ = rig.host.lock().on_head(2);
        let rec = reconciliation(&rig);

        assert_eq!(
            rec.reconcile_head(300).await,
            ReconcileOutcome::Reconciled { folded: 1 },
            "the rewind folds one revocation notice"
        );
        assert_eq!(
            rig.authority
                .lease_of(&rig.strategy)
                .as_ref()
                .map(NonceLease::nonce),
            Some(0),
            "the fold's re-stamp claims the vacated predecessor"
        );
    }
}
