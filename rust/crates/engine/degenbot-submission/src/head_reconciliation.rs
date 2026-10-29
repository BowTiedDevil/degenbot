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
use degenbot_bot::strategy_host::{HeadNotice, StrategyHost, StrategyNotice};
use degenbot_substrate::nonce::{DeliveryHold, HoldPhase, NonceLane, StrategyId};
use parking_lot::Mutex;

use crate::submission_ledger::{HeadPolicy, SubmissionLedger};

/// How many blocks deep a hold must be before the sweep may release it,
/// whatever its own window says.
///
/// A hold younger than the reorg window sits inside the range a rewind can
/// rewrite, so releasing it would rest the release on a head that may still
/// move: the reorg-lagged rule is what keeps the sweep out of its own recovery
/// path. Eight blocks is the depth the pool state's reorg journals retain (a
/// deeper rewind is refused as too deep), so a hold that old is beyond the
/// depth this process can rewind to.
const REORG_SAFETY_DEPTH_BLOCKS: u64 = 8;

/// The per-strategy sign-time lanes the head feed folds notices through,
/// keyed by the owning strategy.
pub type HeadLanes = HashMap<StrategyId, Arc<NonceLane<SubmissionLedger>>>;

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
        /// What the sweep did with the slots the chain had finished with.
        swept: SweepOutcome,
    },
}

/// What one sweep pass did to the account's slot ledger.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepOutcome {
    /// Delivery-unknown holds released because their window closed: the
    /// authority slot moved into replace-or-dominate and the ledger record
    /// closed stale.
    pub holds_released: u64,
    /// Leases released because the chain had already passed their nonce. One
    /// stale reservation otherwise declines every lane's stamp account-wide
    /// until its owner happens to re-stamp.
    pub leases_released: u64,
    /// Releases one of the two books refused. A disagreement, logged; a hold
    /// left frozen here simply waits for the next head or chain evidence.
    pub failed: u64,
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
        // The sweep rides the same fresh read: a head whose read failed never
        // reaches here, so no hold is ever released against stale data — the
        // failed head extends every hold to the next one.
        let (swept, sweep_notices) = self.sweep(head, confirmed);
        let notices = self.host.lock().on_head(confirmed);
        let mut folded = 0u64;
        for notice in sweep_notices.iter().chain(notices.iter()) {
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
        ReconcileOutcome::Reconciled { folded, swept }
    }

    /// Release the slots the chain has finished with, on both books.
    ///
    /// Two arms, one evidence base — the fresh chain nonce read this head
    /// already paid for:
    ///
    /// - a delivery-unknown hold whose window closed, whose nonce the chain has
    ///   not consumed, and which is deeper than the reorg window: the slot moves
    ///   into replace-or-dominate, so a replacement may reuse the nonce but only
    ///   against dominating fees, and the ledger record closes stale;
    /// - a lease below the confirmed nonce: the chain consumed that slot, so the
    ///   reservation can never be honoured and its owner is told to re-obtain.
    ///
    /// A head that moved backward is a reorg: the authority's own restore path
    /// owns it, and nothing is released on evidence that is about to be
    /// rewritten. Released slots are retained by the authority, so the same
    /// rewind restores a swept hold instead of leaving its nonce looking free.
    fn sweep(&self, head: u64, confirmed: u64) -> (SweepOutcome, Vec<HeadNotice>) {
        let mut outcome = SweepOutcome::default();
        let mut notices = Vec::new();
        let authority = Arc::clone(self.host.lock().nonce());
        if confirmed < authority.confirmed() {
            return (outcome, notices);
        }

        for hold in authority.holds() {
            if !Self::hold_is_sweepable(&hold, head, confirmed) {
                continue;
            }
            let Some(lane) = self.lanes.get(hold.strategy()) else {
                // Without its lane the hold has no ledger half to pair with, so
                // it stays frozen rather than half-released.
                tracing::warn!(
                    target: "degenbot.submission.head",
                    head,
                    strategy = %hold.strategy(),
                    nonce = hold.nonce(),
                    "expired hold has no lane in the fold map; left frozen"
                );
                continue;
            };
            match crate::submission_ledger::release_hold(lane, hold.nonce()) {
                Ok(_hold) => {
                    outcome.holds_released += 1;
                    degenbot_bot::instruments::record(
                        degenbot_bot::instruments::CensusEvent::DeliveryHoldSwept {
                            strategy: lane.label(),
                        },
                    );
                    notices.push(HeadNotice::new(
                        hold.strategy().clone(),
                        hold.nonce(),
                        StrategyNotice::Stale,
                    ));
                }
                Err(error) => {
                    outcome.failed += 1;
                    tracing::warn!(
                        target: "degenbot.submission.head",
                        head,
                        strategy = %hold.strategy(),
                        nonce = hold.nonce(),
                        %error,
                        "expired hold could not be released on both books"
                    );
                }
            }
        }

        for lease in authority.outstanding_leases() {
            if lease.nonce() >= confirmed {
                continue;
            }
            let Some(lane) = self.lanes.get(lease.strategy()) else {
                continue;
            };
            match authority.release_lease(&lease) {
                Ok(nonce) => {
                    outcome.leases_released += 1;
                    degenbot_bot::instruments::record(
                        degenbot_bot::instruments::CensusEvent::NonceLeaseSwept {
                            strategy: lane.label(),
                        },
                    );
                    notices.push(HeadNotice::new(
                        lease.strategy().clone(),
                        nonce,
                        StrategyNotice::LeaseRevoked { nonce },
                    ));
                }
                Err(error) => {
                    outcome.failed += 1;
                    tracing::warn!(
                        target: "degenbot.submission.head",
                        head,
                        strategy = %lease.strategy(),
                        nonce = lease.nonce(),
                        %error,
                        "stale lease could not be released"
                    );
                }
            }
        }

        (outcome, notices)
    }

    /// Whether the sweep may release `hold` at this head: the window has
    /// closed, the chain has not consumed the slot, and the hold is deeper than
    /// the reorg window.
    fn hold_is_sweepable(hold: &DeliveryHold, head: u64, confirmed: u64) -> bool {
        hold.phase() == HoldPhase::Frozen
            && hold.nonce() >= confirmed
            && head >= hold.expires_at_head()
            && head.saturating_sub(hold.entered_at_head()) >= REORG_SAFETY_DEPTH_BLOCKS
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
    use degenbot_substrate::nonce::{
        DeclineKind, HeldFees, HoldWindow, LanePolicy, NonceAuthority, NonceLease,
    };
    use degenbot_substrate::route_registry::RouteRegistry;

    use super::*;
    use crate::submission_ledger::{SubmissionLedger, SubmissionState, TargetId};

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

    /// The real host + authority + ledger + the two strategy lanes the
    /// multi-strategy scenarios need, the production choreography's full
    /// shareable state.
    struct Rig {
        host: Arc<Mutex<StrategyHost>>,
        authority: Arc<NonceAuthority>,
        ledger: Arc<SubmissionLedger>,
        lane: Arc<NonceLane<SubmissionLedger>>,
        peer_lane: Arc<NonceLane<SubmissionLedger>>,
        read: Arc<FakeRead>,
        strategy: StrategyId,
        peer: StrategyId,
    }

    /// The fee caps a held submission records: one gwei cap, two gwei tip.
    fn held_fees() -> HeldFees {
        HeldFees::new(30_000_000_000, 2_000_000_000)
    }

    /// The caps that dominate [`held_fees`] under the replacement bump.
    fn dominating_fees() -> HeldFees {
        HeldFees::new(33_000_000_000, 2_200_000_000)
    }

    fn mint(
        authority: &Arc<NonceAuthority>,
        ledger: &Arc<SubmissionLedger>,
        name: &'static str,
        window: HoldWindow,
    ) -> Arc<NonceLane<SubmissionLedger>> {
        Arc::new(authority.mint_lane(sid(name), Arc::clone(ledger), LanePolicy::new(window, name)))
    }

    fn rig_with_windows(window: HoldWindow, peer_window: HoldWindow) -> Rig {
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
        let peer = sid("backrun");
        for id in [&strategy, &peer] {
            host.register(id.clone(), FacetStatus::Configured)
                .expect("register");
        }
        host.attach_reconciler(
            Arc::clone(&ledger) as Arc<dyn degenbot_bot::strategy_host::HeadReconciler>
        );
        let lane = mint(&authority, &ledger, "settlement", window);
        let peer_lane = mint(&authority, &ledger, "backrun", peer_window);
        Rig {
            host: Arc::new(Mutex::new(host)),
            authority,
            ledger,
            lane,
            peer_lane,
            read: Arc::new(FakeRead::new()),
            strategy,
            peer,
        }
    }

    fn rig() -> Rig {
        rig_with_windows(HoldWindow::DEFAULT, HoldWindow::DEFAULT)
    }

    fn reconciliation(rig: &Rig) -> HeadReconciliation {
        let mut lanes = HeadLanes::new();
        lanes.insert(rig.strategy.clone(), Arc::clone(&rig.lane));
        lanes.insert(rig.peer.clone(), Arc::clone(&rig.peer_lane));
        HeadReconciliation::new(
            Arc::clone(&rig.host),
            lanes,
            Arc::clone(&rig.read) as Arc<dyn ChainNonceRead>,
            operator(),
        )
    }

    /// The wire hash a held submission's record carries at hold time: the hash
    /// a replacement's record must name as the one it superseded.
    fn held_hash() -> alloy::primitives::B256 {
        alloy::primitives::B256::repeat_byte(9)
    }

    /// Park `lane`'s freshly stamped lease as a delivery-unknown hold at
    /// `at_head`, on both books.
    fn hold_at(lane: &Arc<NonceLane<SubmissionLedger>>, at_head: u64) -> NonceLease {
        let lease = lane.stamp().expect("stamp");
        lane.record_signed(&lease, target(0).hash(), held_hash(), at_head, None)
            .expect("record signed");
        lane.record_hold(&lease, held_fees(), at_head)
            .expect("park the delivery-unknown hold");
        lane.ledger()
            .record_delivery_unknown(lane.strategy(), lease.nonce())
            .expect("ledger hold");
        lease
    }

    fn signed_broadcast(rig: &Rig, lease: &NonceLease) {
        rig.lane
            .record_signed(
                lease,
                target(0).hash(),
                alloy::primitives::B256::repeat_byte(7),
                0,
                None,
            )
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
            ReconcileOutcome::Reconciled {
                folded: 0,
                swept: SweepOutcome::default(),
            }
        );
        assert_eq!(rig.read.calls(), 1, "the head pays one chain read");
        assert_eq!(rec.reconcile_head(100).await, ReconcileOutcome::Deduped);
        assert_eq!(rec.reconcile_head(99).await, ReconcileOutcome::Deduped);
        assert_eq!(rig.read.calls(), 1, "redundant triggers are free");
        assert_eq!(
            rec.reconcile_head(101).await,
            ReconcileOutcome::Reconciled {
                folded: 0,
                swept: SweepOutcome::default(),
            }
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
            ReconcileOutcome::Reconciled {
                folded: 1,
                swept: SweepOutcome::default(),
            },
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
            ReconcileOutcome::Reconciled {
                folded: 1,
                swept: SweepOutcome::default(),
            },
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
    /// The bounded hold's whole point: an expired window lets the sweep release
    /// the slot, and the release lands on both books.
    #[tokio::test]
    async fn the_sweep_releases_an_expired_hold_and_the_ledger_closes_stale() {
        let rig = rig_with_windows(HoldWindow::new(1), HoldWindow::DEFAULT);
        let lease = hold_at(&rig.lane, 100);
        assert_eq!(lease.nonce(), 0);
        let rec = reconciliation(&rig);
        rig.read.reports(0);

        // The window closed at head 101, but the hold is not yet deeper than
        // the reorg window: a shallower release would rest on a head a rewind
        // can still rewrite.
        assert_eq!(
            rec.reconcile_head(107).await,
            ReconcileOutcome::Reconciled {
                folded: 0,
                swept: SweepOutcome::default(),
            }
        );
        assert_eq!(
            rig.authority.hold_of(&rig.strategy).expect("hold").phase(),
            HoldPhase::Frozen
        );
        assert_eq!(
            rig.ledger.state_of(&rig.strategy, 0),
            Some(SubmissionState::DeliveryUnknown)
        );

        assert_eq!(
            rec.reconcile_head(108).await,
            ReconcileOutcome::Reconciled {
                folded: 1,
                swept: SweepOutcome {
                    holds_released: 1,
                    leases_released: 0,
                    failed: 0,
                },
            },
            "the release folds one Stale notice through the owning lane"
        );
        assert_eq!(
            rig.authority.hold_of(&rig.strategy).expect("hold").phase(),
            HoldPhase::ReplaceOrDominate,
            "the released slot is replaceable, not free"
        );
        assert_eq!(
            rig.ledger.state_of(&rig.strategy, 0),
            Some(SubmissionState::Stale),
            "the ledger record closes with the authority release"
        );
        assert_eq!(
            rig.authority.outstanding_nonces(),
            vec![0],
            "the released slot is still occupied: a sweep release is not a free nonce"
        );
    }

    /// Fail closed: a head whose chain read failed never reaches the sweep, so
    /// an expired hold is extended to the next head rather than released
    /// against stale data.
    #[tokio::test]
    async fn a_failed_chain_read_extends_the_hold_to_the_next_head() {
        let rig = rig_with_windows(HoldWindow::new(1), HoldWindow::DEFAULT);
        let _lease = hold_at(&rig.lane, 100);
        rig.read.reports(0);
        let rec = reconciliation(&rig);

        rig.read.fail.store(true, Ordering::SeqCst);
        assert_eq!(rec.reconcile_head(110).await, ReconcileOutcome::ReadFailed);
        assert_eq!(
            rig.authority.hold_of(&rig.strategy).expect("hold").phase(),
            HoldPhase::Frozen,
            "a failed read never releases a hold"
        );
        assert_eq!(
            rig.ledger.state_of(&rig.strategy, 0),
            Some(SubmissionState::DeliveryUnknown)
        );

        rig.read.fail.store(false, Ordering::SeqCst);
        let outcome = rec.reconcile_head(111).await;
        assert!(
            matches!(
                outcome,
                ReconcileOutcome::Reconciled {
                    swept: SweepOutcome {
                        holds_released: 1,
                        ..
                    },
                    ..
                }
            ),
            "the next fresh read releases it: {outcome:?}"
        );
        assert_eq!(
            rig.ledger.state_of(&rig.strategy, 0),
            Some(SubmissionState::Stale)
        );
    }

    /// One stale reservation used to decline every other lane's stamp
    /// account-wide until its owner happened to re-stamp. The sweep releases it
    /// at the next head, so nothing aborts a batch on it.
    #[tokio::test]
    async fn the_sweep_clears_the_account_wide_below_chain_freeze() {
        let rig = rig();
        let lease = rig.lane.stamp().expect("settlement stamps a lease");
        assert_eq!(lease.nonce(), 0);
        // The chain consumed a slot the settlement lane never broadcast.
        rig.authority.set_confirmed(1);
        assert_eq!(
            rig.peer_lane.stamp(),
            Err(DeclineKind::BelowChainNonce),
            "one below-confirmed reservation freezes every lane"
        );

        rig.read.reports(1);
        let rec = reconciliation(&rig);
        assert_eq!(
            rec.reconcile_head(108).await,
            ReconcileOutcome::Reconciled {
                folded: 1,
                swept: SweepOutcome {
                    holds_released: 0,
                    leases_released: 1,
                    failed: 0,
                },
            }
        );
        assert_eq!(
            rig.peer_lane.stamp().expect("the freeze is gone").nonce(),
            2,
            "the owner lane's revocation re-stamp claimed the recovered head"
        );
        assert_eq!(
            rig.authority
                .lease_of(&rig.strategy)
                .map(|held| held.nonce()),
            Some(1),
            "the released lane re-obtained a lease at the recovered head"
        );
    }

    /// The window is per lane: a lane that prices a held slot-minute cheaply is
    /// swept while its patient peer keeps holding.
    #[tokio::test]
    async fn a_per_lane_window_sweeps_only_the_lane_whose_window_closed() {
        let rig = rig_with_windows(HoldWindow::new(1), HoldWindow::new(20));
        let _impatient = hold_at(&rig.lane, 100);
        let _patient = hold_at(&rig.peer_lane, 100);
        rig.read.reports(0);
        let rec = reconciliation(&rig);

        assert_eq!(
            rec.reconcile_head(108).await,
            ReconcileOutcome::Reconciled {
                folded: 1,
                swept: SweepOutcome {
                    holds_released: 1,
                    leases_released: 0,
                    failed: 0,
                },
            }
        );
        assert_eq!(
            rig.authority.hold_of(&rig.strategy).expect("hold").phase(),
            HoldPhase::ReplaceOrDominate
        );
        assert_eq!(
            rig.authority.hold_of(&rig.peer).expect("hold").phase(),
            HoldPhase::Frozen,
            "the patient lane's window is still open at head 108"
        );
    }

    /// The multi-strategy scenario: lane A holds a delivery-unknown nonce, lane
    /// B stamps and broadcasts the next one, the sweep frees A's slot — and the
    /// freed slot is re-issued only to a dominating replacement of A's own
    /// transaction, recorded with the hash linkage.
    #[tokio::test]
    async fn a_swept_slot_is_reissued_only_to_a_dominating_replacement() {
        let rig = rig_with_windows(HoldWindow::new(1), HoldWindow::DEFAULT);
        let _held = hold_at(&rig.lane, 100);

        // Lane B is unaffected by A's hold: it stamps and broadcasts above it.
        let peer = rig.peer_lane.stamp().expect("peer stamp");
        assert_eq!(peer.nonce(), 1);
        rig.peer_lane
            .record_signed(&peer, target(1).hash(), held_hash(), 100, None)
            .expect("peer record signed");
        rig.authority
            .record_broadcast(&peer)
            .expect("peer broadcast");
        rig.ledger
            .record_broadcast(&rig.peer, peer.nonce())
            .expect("peer ledger broadcast");

        rig.read.reports(0);
        let rec = reconciliation(&rig);
        let outcome = rec.reconcile_head(108).await;
        assert!(
            matches!(
                outcome,
                ReconcileOutcome::Reconciled {
                    swept: SweepOutcome {
                        holds_released: 1,
                        ..
                    },
                    ..
                }
            ),
            "the sweep frees lane A's slot: {outcome:?}"
        );

        // Ordinary issuance still parks the freed slot — for both lanes.
        assert_eq!(rig.lane.stamp(), Err(DeclineKind::DeliveryUnknownHold));
        assert_eq!(
            rig.peer_lane
                .stamp()
                .expect("peer stamp above the slot")
                .nonce(),
            2,
            "the freed slot is not handed to the next signer"
        );

        // An under-priced re-issue is refused on both books: nothing moves.
        assert_eq!(
            crate::submission_ledger::replace_hold(&rig.lane, held_fees()),
            Err(crate::submission_ledger::HoldDecline::Authority(
                DeclineKind::FeesDoNotDominate
            ))
        );
        assert_eq!(
            rig.ledger.state_of(&rig.strategy, 0),
            Some(SubmissionState::Stale)
        );

        // A dominating re-issue is licensed and records the chain's one link.
        let replacement = crate::submission_ledger::replace_hold(&rig.lane, dominating_fees())
            .expect("dominating replacement");
        assert_eq!(
            replacement.lease().nonce(),
            0,
            "the replacement re-signs the held nonce, not a fresh one"
        );
        assert_eq!(
            replacement.replaces(),
            held_hash(),
            "the replacement names the transaction it supersedes"
        );
        rig.lane
            .record_signed(
                replacement.lease(),
                target(2).hash(),
                alloy::primitives::B256::repeat_byte(0xaa),
                108,
                Some(replacement.replaces()),
            )
            .expect("re-sign at the held nonce");
        let record = rig.ledger.record_of(&rig.strategy, 0).expect("record");
        assert_eq!(record.state(), SubmissionState::Signed);
        assert_eq!(record.replaces(), Some(held_hash()));
        assert_eq!(
            record.bundle_hash(),
            alloy::primitives::B256::repeat_byte(0xaa),
            "the monitor tracks the replacement's bytes"
        );
        assert!(rig.authority.hold_of(&rig.strategy).is_none());
    }
}
