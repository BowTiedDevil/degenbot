//! The per-strategy submission ledger: the record of every transaction a
//! strategy has signed, from sign time to its on-chain fate.
//!
//! A record is created when a strategy signs against an authority-granted
//! nonce ([`SubmissionState::Signed`]) and promoted to
//! [`SubmissionState::Broadcast`] once the bytes are on the wire. Chain
//! reconciliation then closes the record out: a nonce the chain has passed
//! landed, a nonce that left the authority's outstanding set without landing
//! went stale, and a record whose immediate predecessor was vacated is
//! orphaned — the gap below it is the strategy's natural fill.
//!
//! # Why the ledger keeps its own state
//!
//! The nonce authority owns *wire truth*: which nonces are still reserved. The
//! ledger owns *submission truth*: what was built, and what became of it. They
//! are reconciled per head, never merged — a record can be terminal in the
//! ledger while its nonce is still outstanding at the authority (a re-stamp
//! leaves the old broadcast live), which is exactly the divergence the
//! notifications surface to the owning strategy.
//!
//! # Addressing
//!
//! A reconciliation notification names the owning strategy and the account
//! nonce whose state changed, so a driver receives only the outcomes for
//! submissions it built. This module never fans a notification out to every
//! strategy.
//!
//! The ledger lock is non-poisoning (`parking_lot`), so one strategy's failure
//! cannot wedge another's submission path.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use alloy::primitives::B256;
use degenbot_bot::nonce_authority::{DeclineKind, NonceAuthority, NonceLease, StrategyId};
use degenbot_bot::strategy_host::{HeadNotice, HeadReconciler, StrategyNotice};
use parking_lot::Mutex;

/// The identity of the transaction a submitted record was built to follow.
///
/// For a bundle submission this is the pending target's transaction hash (the
/// auction's `txs[0]`); a public submission has no distinct target and uses the
/// submitted transaction's own hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TargetId(B256);

impl TargetId {
    /// Name a target.
    #[must_use]
    pub const fn new(hash: B256) -> Self {
        Self(hash)
    }

    /// The target's hash.
    #[must_use]
    pub const fn hash(&self) -> B256 {
        self.0
    }
}

impl fmt::Display for TargetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", alloy::hex::encode(self.0))
    }
}

impl From<B256> for TargetId {
    fn from(hash: B256) -> Self {
        Self::new(hash)
    }
}

/// Where a submitted record sits between sign time and its on-chain fate.
///
/// `Landed`, `Stale`, and `Orphaned` are terminal: a record is a record of what
/// happened, never a candidate for a second outcome. The transitions are a
/// total, closed table — every event from every state answers with a next state
/// or a typed decline, never a panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionState {
    /// Signed against an authority nonce; not yet on the wire.
    Signed,
    /// Broadcast; the transaction hash is on the wire.
    Broadcast,
    /// The chain's next nonce passed this record's nonce: the nonce slot was
    /// consumed on-chain. This is a nonce-level claim — a record that was
    /// signed but never broadcast can reach `Landed` because the account's
    /// nonce advanced through that slot by other means.
    Landed,
    /// The nonce left the authority's outstanding set without landing: the
    /// reservation was released (or consumed by a replacement) before the
    /// chain confirmed it.
    Stale,
    /// A gap opened immediately below this record: its predecessor nonce is
    /// neither confirmed nor outstanding. The strategy may re-stamp at the
    /// vacated predecessor.
    Orphaned,
}

impl SubmissionState {
    /// Whether no further transition is possible.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Landed | Self::Stale | Self::Orphaned)
    }

    /// The broadcast move: [`SubmissionState::Signed`] -> [`SubmissionState::Broadcast`].
    ///
    /// # Errors
    ///
    /// [`LedgerDecline::BroadcastRequiresSigned`] from any other state.
    pub fn on_broadcast(self) -> Result<Self, LedgerDecline> {
        if self == Self::Signed {
            Ok(Self::Broadcast)
        } else {
            Err(LedgerDecline::BroadcastRequiresSigned)
        }
    }

    /// The chain-confirmation move: an outstanding record -> [`SubmissionState::Landed`].
    ///
    /// # Errors
    ///
    /// [`LedgerDecline::LandedRequiresOutstanding`] from a terminal state.
    pub fn on_landed(self) -> Result<Self, LedgerDecline> {
        if self.is_terminal() {
            Err(LedgerDecline::LandedRequiresOutstanding)
        } else {
            Ok(Self::Landed)
        }
    }

    /// The vacated-nonce move: an outstanding record -> [`SubmissionState::Stale`].
    ///
    /// # Errors
    ///
    /// [`LedgerDecline::StaleRequiresOutstanding`] from a terminal state.
    pub fn on_stale(self) -> Result<Self, LedgerDecline> {
        if self.is_terminal() {
            Err(LedgerDecline::StaleRequiresOutstanding)
        } else {
            Ok(Self::Stale)
        }
    }

    /// The vacated-predecessor move: an outstanding record -> [`SubmissionState::Orphaned`].
    ///
    /// # Errors
    ///
    /// [`LedgerDecline::OrphanedRequiresOutstanding`] from a terminal state.
    pub fn on_orphaned(self) -> Result<Self, LedgerDecline> {
        if self.is_terminal() {
            Err(LedgerDecline::OrphanedRequiresOutstanding)
        } else {
            Ok(Self::Orphaned)
        }
    }
}

/// Why a ledger verb was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LedgerDecline {
    /// An *outstanding* record for this strategy and nonce is already tracked.
    /// A record whose nonce was reused after reaching a terminal state is
    /// superseded, not declined.
    #[error("an outstanding record for this strategy and nonce is already tracked")]
    DuplicateRecord,
    /// No record is tracked for this strategy and nonce.
    #[error("no record is tracked for this strategy and nonce")]
    UnknownRecord,
    /// `broadcast` is only legal from [`SubmissionState::Signed`].
    #[error("broadcast requires a Signed record")]
    BroadcastRequiresSigned,
    /// `landed` refuses a terminal record.
    #[error("landed requires an outstanding (Signed or Broadcast) record")]
    LandedRequiresOutstanding,
    /// `stale` refuses a terminal record.
    #[error("stale requires an outstanding (Signed or Broadcast) record")]
    StaleRequiresOutstanding,
    /// `orphaned` refuses a terminal record.
    #[error("orphaned requires an outstanding (Signed or Broadcast) record")]
    OrphanedRequiresOutstanding,
}

/// One strategy's record of a submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionRecord {
    strategy: StrategyId,
    nonce: u64,
    target: TargetId,
    bundle_hash: B256,
    built_at_head: u64,
    state: SubmissionState,
}

impl SubmissionRecord {
    /// The owning strategy.
    #[must_use]
    pub fn strategy(&self) -> &StrategyId {
        &self.strategy
    }

    /// The account nonce the record was signed against.
    #[must_use]
    pub const fn nonce(&self) -> u64 {
        self.nonce
    }

    /// The transaction the record was built to follow.
    #[must_use]
    pub const fn target(&self) -> TargetId {
        self.target
    }

    /// The wire hash of the signed submission.
    #[must_use]
    pub const fn bundle_hash(&self) -> B256 {
        self.bundle_hash
    }

    /// The head the record was built at.
    #[must_use]
    pub const fn built_at_head(&self) -> u64 {
        self.built_at_head
    }

    /// The record's current state.
    #[must_use]
    pub const fn state(&self) -> SubmissionState {
        self.state
    }
}

/// The typed outcome reconciliation reports for one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    /// The record's nonce was confirmed on-chain.
    Landed,
    /// The record's nonce left the outstanding set without landing.
    Stale,
    /// The record's predecessor was vacated while the record stayed
    /// outstanding. The payload is the vacated predecessor nonce the strategy
    /// may re-stamp at — the default v1 policy's [`RepackageRequest`] advisory.
    Orphaned {
        /// The vacated predecessor nonce.
        fillable_nonce: u64,
    },
}

/// A reconciliation notification, addressed to the strategy that owns the
/// record and keyed by the account nonce whose state changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    strategy: StrategyId,
    nonce: u64,
    kind: NotificationKind,
}

impl Notification {
    /// The strategy that owns the affected record.
    #[must_use]
    pub fn strategy(&self) -> &StrategyId {
        &self.strategy
    }

    /// The affected account nonce.
    #[must_use]
    pub const fn nonce(&self) -> u64 {
        self.nonce
    }

    /// What happened to the record.
    #[must_use]
    pub const fn kind(&self) -> NotificationKind {
        self.kind
    }

    /// The default-policy advisory this notification carries, if it is
    /// [`NotificationKind::Orphaned`]: the strategy may re-stamp its intent at
    /// the vacated predecessor. The re-bid itself — the economics re-check and
    /// the fresh sign — is the strategy's job, out of this module's scope.
    ///
    /// The named nonce is fillable relative to the `confirmed`/`outstanding`
    /// snapshot this reconciliation observed. The re-stamp must still go
    /// through the nonce authority, which arbitrates and may hand back a
    /// different lowest-free nonce.
    #[must_use]
    pub fn repackage_request(&self) -> Option<RepackageRequest> {
        match self.kind {
            NotificationKind::Orphaned { fillable_nonce } => Some(RepackageRequest {
                strategy: self.strategy.clone(),
                orphaned_nonce: self.nonce,
                fillable_nonce,
            }),
            NotificationKind::Landed | NotificationKind::Stale => None,
        }
    }
}

/// The v1 advisory an orphaned record raises: re-stamp at the vacated
/// predecessor. Routing it to the strategy is the caller's job; acting on it is
/// the strategy's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepackageRequest {
    strategy: StrategyId,
    orphaned_nonce: u64,
    fillable_nonce: u64,
}

impl RepackageRequest {
    /// The strategy that owns the orphaned record.
    #[must_use]
    pub fn strategy(&self) -> &StrategyId {
        &self.strategy
    }

    /// The nonce of the orphaned record.
    #[must_use]
    pub const fn orphaned_nonce(&self) -> u64 {
        self.orphaned_nonce
    }

    /// The vacated predecessor nonce to re-stamp at.
    #[must_use]
    pub const fn fillable_nonce(&self) -> u64 {
        self.fillable_nonce
    }
}

/// The v1 wire-level shadow posture.
///
/// Reconciliation is nonce-level: a strategy can be told its nonce landed
/// even when its exact bytes did not (another account transaction consumed the
/// slot, or the authority handed the nonce to a successor). The ledger records
/// that divergence in the record's own fate; it never acts on the shadow. The
/// accept-the-shadow choice is deliberately deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowPosture {
    /// The shadow is observable in the record's nonce-level outcome but the
    /// submission path makes no decision from it.
    ObservedNotActedOn,
}

/// The host-minted sign-time nonce seam for one strategy.
///
/// Both submission paths obtain their nonce here at TX-build/sign time: the
/// lane's [`stamp`](Self::stamp) is the only issuance entry, and every signed
/// submission is recorded in the same ledger the host reconciles per head. A
/// lane therefore keeps no private nonce reservation table — the shared
/// authority arbitrates the operator account's nonces across strategies.
#[derive(Debug)]
pub struct NonceLane {
    authority: Arc<NonceAuthority>,
    ledger: Arc<SubmissionLedger>,
    strategy: StrategyId,
}

impl NonceLane {
    /// Bind a strategy to the shared authority and ledger.
    #[must_use]
    pub fn new(
        authority: Arc<NonceAuthority>,
        ledger: Arc<SubmissionLedger>,
        strategy: impl Into<StrategyId>,
    ) -> Self {
        Self {
            authority,
            ledger,
            strategy: strategy.into(),
        }
    }

    /// The lane's strategy identity.
    #[must_use]
    pub fn strategy(&self) -> &StrategyId {
        &self.strategy
    }

    /// The shared nonce authority.
    #[must_use]
    pub fn authority(&self) -> &Arc<NonceAuthority> {
        &self.authority
    }

    /// The shared per-strategy ledger.
    #[must_use]
    pub fn ledger(&self) -> &Arc<SubmissionLedger> {
        &self.ledger
    }

    /// Sign-time stamp: obtain the lowest-free nonce from the authority.
    ///
    /// The default v1 repackage policy runs here: when the authority declines
    /// because the lane's own stale reservation is in the way
    /// ([`DeclineKind::StrategyLeaseOutstanding`] or
    /// [`DeclineKind::BelowChainNonce`]), the lane releases that lease and
    /// re-stamps once at the next available nonce. A non-stale decline
    /// (exhausted space, no lease to relinquish) propagates unchanged.
    ///
    /// # Errors
    ///
    /// The authority's [`DeclineKind`] once no stale lease can be relinquished.
    pub fn stamp(&self) -> Result<NonceLease, DeclineKind> {
        let mut relinquished = false;
        loop {
            match self.authority.lease(&self.strategy) {
                Ok(lease) => return Ok(lease),
                Err(decline) => {
                    if relinquished {
                        return Err(decline);
                    }
                    let Some(existing) = self.authority.lease_of(&self.strategy) else {
                        return Err(decline);
                    };
                    self.authority.release_lease(&existing)?;
                    relinquished = true;
                }
            }
        }
    }

    /// Record a freshly signed submission against the stamped lease's nonce.
    ///
    /// # Errors
    ///
    /// [`LedgerDecline`] when the nonce already carries an outstanding record.
    pub fn record_signed(
        &self,
        lease: &NonceLease,
        target: TargetId,
        bundle_hash: B256,
        built_at_head: u64,
    ) -> Result<(), LedgerDecline> {
        self.ledger.record_signed(
            &self.strategy,
            lease.nonce(),
            target,
            bundle_hash,
            built_at_head,
        )
    }

    /// Release a lease whose transaction never reached the wire, freeing the
    /// nonce without clearing the lane's outstanding broadcasts.
    ///
    /// # Errors
    ///
    /// [`DeclineKind::UnknownLease`] when the lease is no longer outstanding.
    pub fn release(&self, lease: &NonceLease) -> Result<u64, DeclineKind> {
        self.authority.release_lease(lease)
    }

    /// Release a broadcast the lane's strategy owns after a monitor declared
    /// the transaction expired (it will never land). Frees the authority slot
    /// so the account's nonce prefix cannot wedge on a dropped transaction.
    ///
    /// # Errors
    ///
    /// [`DeclineKind::UnknownBroadcast`] when the nonce is not a broadcast
    /// owned by this lane's strategy.
    pub fn release_broadcast(&self, nonce: u64) -> Result<u64, DeclineKind> {
        self.authority.release_broadcast(&self.strategy, nonce)
    }

    /// Seed or advance the authority's confirmed chain nonce from a head read,
    /// never rewinding it.
    ///
    /// A submission path that already fetched the account's next nonce seeds
    /// the authority before its first stamp so the lane cannot issue a nonce
    /// the chain has consumed. A later head read that reports a lower value (an
    /// RPC race) must not revoke a live lease, so only a forward move is
    /// applied; the reorg-safe rewind stays the head feed's call.
    pub fn observe_chain_nonce(&self, confirmed: u64) {
        if confirmed > self.authority.confirmed() {
            let _ = self.authority.set_confirmed_reorg(confirmed);
        }
    }
}

/// What the default lane policy did with a typed head notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAction {
    /// Re-stamped at the authority's lowest-free nonce: the vacated
    /// predecessor for an orphan fill, the recovered head for a revoked
    /// lease.
    Restamped {
        /// The nonce the fresh stamp obtained.
        nonce: u64,
    },
    /// The lane's decide stage re-evaluates the frame fresh and re-bids or
    /// drops.
    Reevaluate,
    /// The submission landed; nothing to do.
    Retired,
}

/// The default v1 driver policy over typed head notices.
///
/// A driver feeds its own notices here: an orphan or a revoked lease re-stamps
/// through the authority (the re-bid itself — the economics re-check and the
/// fresh sign — stays with the caller); a stale outcome defers to the lane's
/// decide stage; a landed outcome retires.
#[derive(Debug)]
pub struct HeadPolicy {
    lane: Arc<NonceLane>,
}

impl HeadPolicy {
    /// Bind the policy to a lane.
    #[must_use]
    pub fn new(lane: Arc<NonceLane>) -> Self {
        Self { lane }
    }

    /// The lane the policy stamps through.
    #[must_use]
    pub fn lane(&self) -> &Arc<NonceLane> {
        &self.lane
    }

    /// Apply the default policy to one typed head notice.
    ///
    /// # Errors
    ///
    /// The authority's [`DeclineKind`] when a re-stamp cannot obtain a nonce.
    pub fn on_notice(&self, notice: &HeadNotice) -> Result<PolicyAction, DeclineKind> {
        match notice.notice() {
            StrategyNotice::Orphaned { .. } | StrategyNotice::LeaseRevoked { .. } => {
                let lease = self.lane.stamp()?;
                Ok(PolicyAction::Restamped {
                    nonce: lease.nonce(),
                })
            }
            StrategyNotice::Stale => Ok(PolicyAction::Reevaluate),
            StrategyNotice::Landed => Ok(PolicyAction::Retired),
        }
    }
}

impl HeadReconciler for SubmissionLedger {
    fn reconcile_head(&self, confirmed: u64, outstanding: &[u64]) -> Vec<HeadNotice> {
        self.reconcile(confirmed, outstanding)
            .into_iter()
            .map(|notification| {
                let notice = match notification.kind() {
                    NotificationKind::Landed => StrategyNotice::Landed,
                    NotificationKind::Stale => StrategyNotice::Stale,
                    NotificationKind::Orphaned { fillable_nonce } => {
                        StrategyNotice::Orphaned { fillable_nonce }
                    }
                };
                HeadNotice::new(
                    notification.strategy().clone(),
                    notification.nonce(),
                    notice,
                )
            })
            .collect()
    }

    fn has_outstanding(&self) -> bool {
        SubmissionLedger::has_outstanding(self)
    }
}

#[derive(Debug, Default)]
struct LedgerState {
    /// Records grouped per strategy, each keyed by account nonce ascending.
    by_strategy: BTreeMap<StrategyId, BTreeMap<u64, SubmissionRecord>>,
}

/// The per-strategy ledger of submitted records.
#[derive(Debug, Default)]
pub struct SubmissionLedger {
    state: Mutex<LedgerState>,
}

impl SubmissionLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The v1 wire-level shadow posture: observed, not acted on.
    pub const SHADOW_POSTURE: ShadowPosture = ShadowPosture::ObservedNotActedOn;

    /// Record a freshly signed submission.
    ///
    /// Re-signing at a nonce whose record is already terminal supersedes the
    /// old record: the authority hands a released nonce back out, and the
    /// orphan-fill policy re-stamps at a vacated predecessor.
    ///
    /// # Errors
    ///
    /// [`LedgerDecline::DuplicateRecord`] if the strategy already tracks an
    /// *outstanding* record at this nonce. Nonces are unique per operator
    /// account while outstanding, so a live repeat is a caller bug, not a
    /// routine decline.
    pub fn record_signed(
        &self,
        strategy: &StrategyId,
        nonce: u64,
        target: TargetId,
        bundle_hash: B256,
        built_at_head: u64,
    ) -> Result<(), LedgerDecline> {
        let mut state = self.state.lock();
        let records = state.by_strategy.entry(strategy.clone()).or_default();
        if records
            .get(&nonce)
            .is_some_and(|existing| !existing.state.is_terminal())
        {
            return Err(LedgerDecline::DuplicateRecord);
        }
        records.insert(
            nonce,
            SubmissionRecord {
                strategy: strategy.clone(),
                nonce,
                target,
                bundle_hash,
                built_at_head,
                state: SubmissionState::Signed,
            },
        );
        Ok(())
    }

    /// Promote a signed record to broadcast.
    ///
    /// # Errors
    ///
    /// [`LedgerDecline::UnknownRecord`] if the strategy tracks no record at this
    /// nonce; [`LedgerDecline::BroadcastRequiresSigned`] if the record is not
    /// signed.
    pub fn record_broadcast(
        &self,
        strategy: &StrategyId,
        nonce: u64,
    ) -> Result<SubmissionState, LedgerDecline> {
        let mut state = self.state.lock();
        let record = state
            .by_strategy
            .get_mut(strategy)
            .and_then(|records| records.get_mut(&nonce))
            .ok_or(LedgerDecline::UnknownRecord)?;
        record.state = record.state.on_broadcast()?;
        Ok(record.state)
    }

    /// Reconcile every outstanding record against the chain's confirmed nonce
    /// and the authority's outstanding set, returning one notification per
    /// state change.
    ///
    /// A record below `confirmed` landed; a record at or above `confirmed` that
    /// is absent from `outstanding` went stale; a record still outstanding
    /// whose immediate predecessor is neither confirmed nor outstanding was
    /// orphaned. Terminal records are left untouched, so a second call with the
    /// same inputs reports nothing.
    ///
    /// Notifications are emitted deterministically: strategies in name order,
    /// then ascending nonce. `outstanding` is treated as a set, so duplicate
    /// entries are harmless. Every classification reads this call's immutable
    /// snapshot, so records never interfere with one another within a pass.
    ///
    /// Outcomes are nonce-level: the notification certifies what happened to
    /// the *nonce*, and a nonce can be reused by a different strategy after a
    /// release. Reconciling a stalled record before a successor signs the same
    /// nonce keeps the two apart.
    #[must_use]
    pub fn reconcile(&self, confirmed: u64, outstanding: &[u64]) -> Vec<Notification> {
        let outstanding: BTreeSet<u64> = outstanding.iter().copied().collect();
        let mut state = self.state.lock();
        let mut notifications = Vec::new();
        for (strategy, records) in &mut state.by_strategy {
            for (nonce, record) in records.iter_mut() {
                if record.state.is_terminal() {
                    continue;
                }
                let event = if *nonce < confirmed {
                    Some(NotificationKind::Landed)
                } else if !outstanding.contains(nonce) {
                    Some(NotificationKind::Stale)
                } else {
                    match nonce.checked_sub(1) {
                        Some(prev) if prev >= confirmed && !outstanding.contains(&prev) => {
                            Some(NotificationKind::Orphaned {
                                fillable_nonce: prev,
                            })
                        }
                        _ => None,
                    }
                };
                let Some(kind) = event else {
                    continue;
                };
                let next = match kind {
                    NotificationKind::Landed => record.state.on_landed(),
                    NotificationKind::Stale => record.state.on_stale(),
                    NotificationKind::Orphaned { .. } => record.state.on_orphaned(),
                };
                let Ok(next) = next else {
                    // A non-terminal record admits every reconcile event; a
                    // decline here would mean the state table and this branch
                    // disagree, so leave the record untouched rather than
                    // mislabel it.
                    continue;
                };
                record.state = next;
                notifications.push(Notification {
                    strategy: strategy.clone(),
                    nonce: *nonce,
                    kind,
                });
            }
        }
        notifications
    }

    /// The state of one strategy's record, or `None` if it is untracked.
    #[must_use]
    pub fn state_of(&self, strategy: &StrategyId, nonce: u64) -> Option<SubmissionState> {
        self.state
            .lock()
            .by_strategy
            .get(strategy)
            .and_then(|records| records.get(&nonce))
            .map(SubmissionRecord::state)
    }

    /// One strategy's record, or `None` if it is untracked.
    #[must_use]
    pub fn record_of(&self, strategy: &StrategyId, nonce: u64) -> Option<SubmissionRecord> {
        self.state
            .lock()
            .by_strategy
            .get(strategy)
            .and_then(|records| records.get(&nonce))
            .cloned()
    }

    /// One strategy's records, in nonce order.
    #[must_use]
    pub fn records_for(&self, strategy: &StrategyId) -> Vec<SubmissionRecord> {
        self.state
            .lock()
            .by_strategy
            .get(strategy)
            .map(|records| records.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Every record, grouped by strategy in name order and by nonce within each.
    #[must_use]
    pub fn records(&self) -> Vec<SubmissionRecord> {
        self.state
            .lock()
            .by_strategy
            .values()
            .flat_map(|records| records.values().cloned())
            .collect()
    }

    /// The number of tracked records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .by_strategy
            .values()
            .map(BTreeMap::len)
            .sum()
    }

    /// Whether no records are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether any record is still non-terminal.
    ///
    /// The head feed's reconcile guard reads this together with the authority's
    /// outstanding set: a ledger with only terminal records has nothing left to
    /// reconcile.
    #[must_use]
    pub fn has_outstanding(&self) -> bool {
        self.state
            .lock()
            .by_strategy
            .values()
            .any(|records| records.values().any(|record| !record.state.is_terminal()))
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "unit tests: a malformed fixture must fail the test loudly"
)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sid(name: &str) -> StrategyId {
        StrategyId::new(name)
    }

    /// A deterministic byte from a nonce without a lossy cast.
    fn byte_of(nonce: u64) -> u8 {
        nonce.to_le_bytes()[0]
    }

    fn target(nonce: u64) -> TargetId {
        TargetId::new(B256::repeat_byte(byte_of(nonce)))
    }

    fn hash(nonce: u64) -> B256 {
        B256::repeat_byte(byte_of(nonce))
    }

    fn sign(ledger: &SubmissionLedger, strategy: &str, nonce: u64, head: u64) {
        ledger
            .record_signed(&sid(strategy), nonce, target(nonce), hash(nonce), head)
            .expect("record_signed");
    }

    #[test]
    fn the_state_transition_table_is_total_and_closed() {
        let states = [
            SubmissionState::Signed,
            SubmissionState::Broadcast,
            SubmissionState::Landed,
            SubmissionState::Stale,
            SubmissionState::Orphaned,
        ];
        for state in states {
            let outstanding = !state.is_terminal();

            if state == SubmissionState::Signed {
                assert_eq!(state.on_broadcast(), Ok(SubmissionState::Broadcast));
            } else {
                assert_eq!(
                    state.on_broadcast(),
                    Err(LedgerDecline::BroadcastRequiresSigned)
                );
            }

            if outstanding {
                assert_eq!(state.on_landed(), Ok(SubmissionState::Landed));
                assert_eq!(state.on_stale(), Ok(SubmissionState::Stale));
                assert_eq!(state.on_orphaned(), Ok(SubmissionState::Orphaned));
            } else {
                assert_eq!(
                    state.on_landed(),
                    Err(LedgerDecline::LandedRequiresOutstanding)
                );
                assert_eq!(
                    state.on_stale(),
                    Err(LedgerDecline::StaleRequiresOutstanding)
                );
                assert_eq!(
                    state.on_orphaned(),
                    Err(LedgerDecline::OrphanedRequiresOutstanding)
                );
            }
        }
    }

    #[test]
    fn a_record_walks_signed_then_broadcast() {
        let ledger = SubmissionLedger::new();
        sign(&ledger, "settlement", 10, 100);
        assert_eq!(ledger.len(), 1);
        assert!(!ledger.is_empty());
        assert_eq!(
            ledger.state_of(&sid("settlement"), 10),
            Some(SubmissionState::Signed)
        );
        let record = ledger.record_of(&sid("settlement"), 10).expect("record");
        assert_eq!(record.strategy(), &sid("settlement"));
        assert_eq!(record.nonce(), 10);
        assert_eq!(record.target(), target(10));
        assert_eq!(record.bundle_hash(), hash(10));
        assert_eq!(record.built_at_head(), 100);

        assert_eq!(
            ledger.record_broadcast(&sid("settlement"), 10),
            Ok(SubmissionState::Broadcast)
        );
        assert_eq!(
            ledger.state_of(&sid("settlement"), 10),
            Some(SubmissionState::Broadcast)
        );
    }

    #[test]
    fn recording_the_same_nonce_twice_is_declined() {
        let ledger = SubmissionLedger::new();
        sign(&ledger, "settlement", 10, 100);
        assert_eq!(
            ledger.record_signed(&sid("settlement"), 10, target(10), hash(10), 101),
            Err(LedgerDecline::DuplicateRecord)
        );
    }

    #[test]
    fn broadcasting_an_unknown_record_is_declined() {
        let ledger = SubmissionLedger::new();
        assert_eq!(
            ledger.record_broadcast(&sid("settlement"), 10),
            Err(LedgerDecline::UnknownRecord)
        );
    }

    #[test]
    fn broadcasting_twice_is_declined() {
        let ledger = SubmissionLedger::new();
        sign(&ledger, "settlement", 10, 100);
        ledger
            .record_broadcast(&sid("settlement"), 10)
            .expect("broadcast");
        assert_eq!(
            ledger.record_broadcast(&sid("settlement"), 10),
            Err(LedgerDecline::BroadcastRequiresSigned)
        );
    }

    #[test]
    fn reconcile_lands_every_record_below_the_confirmed_nonce() {
        let ledger = SubmissionLedger::new();
        sign(&ledger, "settlement", 10, 100);
        ledger
            .record_broadcast(&sid("settlement"), 10)
            .expect("broadcast");
        // The chain has passed nonce 10; the authority no longer tracks it.
        let notifications = ledger.reconcile(11, &[]);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].strategy(), &sid("settlement"));
        assert_eq!(notifications[0].nonce(), 10);
        assert_eq!(notifications[0].kind(), NotificationKind::Landed);
        assert_eq!(
            ledger.state_of(&sid("settlement"), 10),
            Some(SubmissionState::Landed)
        );
    }

    #[test]
    fn reconcile_stales_a_record_whose_nonce_left_the_outstanding_set() {
        let ledger = SubmissionLedger::new();
        sign(&ledger, "settlement", 10, 100);
        // The reservation was released while the chain still sits at 10.
        let notifications = ledger.reconcile(10, &[]);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].kind(), NotificationKind::Stale);
        assert_eq!(
            ledger.state_of(&sid("settlement"), 10),
            Some(SubmissionState::Stale)
        );
    }

    #[test]
    fn reconcile_leaves_a_contiguous_outstanding_prefix_untouched() {
        let ledger = SubmissionLedger::new();
        for nonce in 10..=12 {
            sign(&ledger, "settlement", nonce, 100);
            ledger
                .record_broadcast(&sid("settlement"), nonce)
                .expect("broadcast");
        }
        let notifications = ledger.reconcile(10, &[10, 11, 12]);
        assert!(notifications.is_empty());
        for nonce in 10..=12 {
            assert_eq!(
                ledger.state_of(&sid("settlement"), nonce),
                Some(SubmissionState::Broadcast)
            );
        }
    }

    #[test]
    fn reconcile_orphans_a_record_above_a_vacated_predecessor() {
        let ledger = SubmissionLedger::new();
        sign(&ledger, "settlement", 10, 100);
        sign(&ledger, "settlement", 11, 100);
        // Nonce 10 vacated, nonce 11 still outstanding: 11's predecessor is a gap.
        let notifications = ledger.reconcile(10, &[11]);
        // 10 is absent from the outstanding set, so it is stale; 11 is the orphan.
        assert_eq!(notifications.len(), 2);
        assert_eq!(notifications[0].nonce(), 10);
        assert_eq!(notifications[0].kind(), NotificationKind::Stale);
        assert_eq!(notifications[1].nonce(), 11);
        assert_eq!(
            notifications[1].kind(),
            NotificationKind::Orphaned { fillable_nonce: 10 }
        );
        assert_eq!(
            ledger.state_of(&sid("settlement"), 11),
            Some(SubmissionState::Orphaned)
        );
    }

    #[test]
    fn orphan_notification_yields_a_repackage_request_for_the_fillable_predecessor() {
        let ledger = SubmissionLedger::new();
        sign(&ledger, "backrun", 20, 100);
        sign(&ledger, "backrun", 21, 100);
        let notifications = ledger.reconcile(20, &[21]);
        let orphan = notifications
            .iter()
            .find(|n| n.nonce() == 21)
            .expect("orphan notification");
        let request = orphan.repackage_request().expect("repackage request");
        assert_eq!(request.strategy(), &sid("backrun"));
        assert_eq!(request.orphaned_nonce(), 21);
        assert_eq!(request.fillable_nonce(), 20);
        // The landed/stale outcomes carry no advisory.
        let stale = notifications
            .iter()
            .find(|n| n.nonce() == 20)
            .expect("stale notification");
        assert_eq!(stale.repackage_request(), None);
    }

    #[test]
    fn reconcile_is_idempotent_on_terminal_records() {
        let ledger = SubmissionLedger::new();
        sign(&ledger, "settlement", 10, 100);
        let first = ledger.reconcile(11, &[]);
        assert_eq!(first.len(), 1);
        let second = ledger.reconcile(11, &[]);
        assert!(second.is_empty());
        assert_eq!(
            ledger.state_of(&sid("settlement"), 10),
            Some(SubmissionState::Landed)
        );
    }

    #[test]
    fn notifications_name_the_owning_strategy() {
        let ledger = SubmissionLedger::new();
        sign(&ledger, "settlement", 10, 100);
        sign(&ledger, "backrun", 11, 100);
        // Chain passed 10; 11's reservation was released.
        let notifications = ledger.reconcile(11, &[]);
        assert_eq!(notifications.len(), 2);
        assert_eq!(notifications[0].strategy(), &sid("backrun"));
        assert_eq!(notifications[0].nonce(), 11);
        assert_eq!(notifications[0].kind(), NotificationKind::Stale);
        assert_eq!(notifications[1].strategy(), &sid("settlement"));
        assert_eq!(notifications[1].nonce(), 10);
        assert_eq!(notifications[1].kind(), NotificationKind::Landed);
    }

    #[test]
    fn records_for_returns_one_strategys_records_in_nonce_order() {
        let ledger = SubmissionLedger::new();
        sign(&ledger, "backrun", 11, 100);
        sign(&ledger, "backrun", 10, 100);
        sign(&ledger, "settlement", 12, 100);
        let backrun = ledger.records_for(&sid("backrun"));
        let nonces: Vec<u64> = backrun.iter().map(SubmissionRecord::nonce).collect();
        assert_eq!(nonces, vec![10, 11]);
        assert_eq!(ledger.records().len(), 3);
        assert!(ledger.records_for(&sid("ghost")).is_empty());
    }

    #[test]
    fn resigning_at_a_terminal_nonce_supersedes_the_old_record() {
        let ledger = SubmissionLedger::new();
        sign(&ledger, "settlement", 10, 100);
        // The reservation is released before the chain confirms it.
        let reconciled = ledger.reconcile(10, &[]);
        assert_eq!(reconciled.len(), 1);
        assert_eq!(
            ledger.state_of(&sid("settlement"), 10),
            Some(SubmissionState::Stale)
        );
        // The authority may hand the same nonce back out; the strategy re-signs.
        ledger
            .record_signed(&sid("settlement"), 10, target(99), hash(99), 101)
            .expect("re-sign at a reused nonce");
        let record = ledger.record_of(&sid("settlement"), 10).expect("record");
        assert_eq!(record.state(), SubmissionState::Signed);
        assert_eq!(record.bundle_hash(), hash(99));
        assert_eq!(record.built_at_head(), 101);
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn orphan_fill_can_resign_at_the_vacated_predecessor() {
        let ledger = SubmissionLedger::new();
        sign(&ledger, "settlement", 10, 100);
        sign(&ledger, "settlement", 11, 100);
        let notifications = ledger.reconcile(10, &[11]);
        let request = notifications
            .iter()
            .find_map(Notification::repackage_request)
            .expect("repackage request");
        assert_eq!(request.fillable_nonce(), 10);
        // The strategy re-stamps at the vacated predecessor: the stale record
        // at 10 is superseded rather than refused.
        sign(&ledger, "settlement", request.fillable_nonce(), 101);
        assert_eq!(
            ledger.state_of(&sid("settlement"), 10),
            Some(SubmissionState::Signed)
        );
        assert_eq!(ledger.len(), 2);
    }

    /// The independent oracle: what each record's outcome must be, derived
    /// directly from the reconciliation definition — no ledger code.
    #[test]
    fn the_ledger_maps_reconcile_outcomes_to_host_notices() {
        let ledger = SubmissionLedger::new();
        sign(&ledger, "settlement", 10, 100);
        sign(&ledger, "settlement", 11, 100);
        let notices = ledger.reconcile_head(10, &[11]);
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0].strategy(), &sid("settlement"));
        assert_eq!(notices[0].notice(), &StrategyNotice::Stale);
        assert_eq!(notices[1].nonce(), 11);
        assert_eq!(
            notices[1].notice(),
            &StrategyNotice::Orphaned { fillable_nonce: 10 }
        );
    }

    #[test]
    fn has_outstanding_tracks_non_terminal_records() {
        let ledger = SubmissionLedger::new();
        assert!(!ledger.has_outstanding());
        sign(&ledger, "settlement", 10, 100);
        assert!(ledger.has_outstanding());
        ledger
            .record_broadcast(&sid("settlement"), 10)
            .expect("broadcast");
        assert!(ledger.has_outstanding());
        let _ = ledger.reconcile(11, &[]);
        assert!(!ledger.has_outstanding(), "a landed record is terminal");
    }

    #[test]
    fn observe_chain_nonce_advances_without_rewinding() {
        let authority = Arc::new(NonceAuthority::new(10));
        let ledger = Arc::new(SubmissionLedger::new());
        let lane = NonceLane::new(Arc::clone(&authority), ledger, sid("a"));
        lane.observe_chain_nonce(12);
        assert_eq!(authority.confirmed(), 12);
        lane.observe_chain_nonce(11);
        assert_eq!(
            authority.confirmed(),
            12,
            "a stale head read never rewinds the confirmed nonce"
        );
    }

    #[test]
    fn the_wire_level_shadow_posture_is_observed_only() {
        assert_eq!(
            SubmissionLedger::SHADOW_POSTURE,
            ShadowPosture::ObservedNotActedOn
        );
    }

    #[test]
    fn a_lane_stamp_repackages_past_a_stale_lease() {
        let authority = Arc::new(NonceAuthority::new(10));
        let ledger = Arc::new(SubmissionLedger::new());
        let lane = NonceLane::new(Arc::clone(&authority), Arc::clone(&ledger), sid("a"));
        let lease = lane.stamp().expect("first stamp");
        assert_eq!(lease.nonce(), 10);
        // The chain passes the lease; the next sign repackages at 11.
        authority.set_confirmed(11);
        let restamped = lane.stamp().expect("repackage");
        assert_eq!(restamped.nonce(), 11);
        assert_eq!(authority.lease_of(&sid("a")).expect("lease").nonce(), 11);
    }

    /// Cross-driver probe: two lanes over one shared authority, each holding
    /// one outstanding lease, never share a nonce. The second lane's stamp
    /// must skip the first lane's held value rather than issue it again.
    #[test]
    fn two_lanes_one_outstanding_each_never_share_a_nonce() {
        let authority = Arc::new(NonceAuthority::new(50));
        let ledger = Arc::new(SubmissionLedger::new());
        let a = NonceLane::new(Arc::clone(&authority), Arc::clone(&ledger), sid("a"));
        let b = NonceLane::new(Arc::clone(&authority), Arc::clone(&ledger), sid("b"));

        let lease_a = a.stamp().expect("a stamp");
        let lease_b = b.stamp().expect("b stamp");
        assert_eq!((lease_a.nonce(), lease_b.nonce()), (50, 51));
        assert_ne!(
            lease_a.nonce(),
            lease_b.nonce(),
            "the second lane never re-issues the first lane's held value"
        );
        // Both reservations are live and distinct in the authority.
        assert_eq!(authority.outstanding_nonces(), vec![50, 51]);
    }

    /// The expiry escape hatch at the lane level: releasing a broadcast frees
    /// exactly that nonce and the next stamp refills it, keeping the prefix
    /// contiguous.
    #[test]
    fn a_lane_releases_an_expired_broadcast_and_refills_the_slot() {
        let authority = Arc::new(NonceAuthority::new(50));
        let ledger = Arc::new(SubmissionLedger::new());
        let lane = NonceLane::new(Arc::clone(&authority), Arc::clone(&ledger), sid("a"));
        let lease = lane.stamp().expect("stamp");
        authority.record_broadcast(&lease).expect("broadcast");
        assert_eq!(authority.outstanding_nonces(), vec![50]);

        assert_eq!(lane.release_broadcast(50), Ok(50));
        assert_eq!(lane.stamp().expect("refill").nonce(), 50);
        assert_eq!(
            lane.release_broadcast(50),
            Err(DeclineKind::UnknownBroadcast),
            "the lease is not a broadcast"
        );
    }

    #[test]
    fn the_default_policy_restamps_an_orphan_and_defers_stale() {
        let authority = Arc::new(NonceAuthority::new(5));
        let ledger = Arc::new(SubmissionLedger::new());
        let lane = Arc::new(NonceLane::new(
            Arc::clone(&authority),
            Arc::clone(&ledger),
            sid("a"),
        ));
        let policy = HeadPolicy::new(Arc::clone(&lane));
        let orphan = HeadNotice::new(sid("a"), 6, StrategyNotice::Orphaned { fillable_nonce: 5 });
        assert_eq!(
            policy.on_notice(&orphan).expect("restamp"),
            PolicyAction::Restamped { nonce: 5 }
        );
        assert_eq!(
            policy
                .on_notice(&HeadNotice::new(sid("a"), 5, StrategyNotice::Stale))
                .expect("stale"),
            PolicyAction::Reevaluate
        );
    }

    /// The in-process two-strategy integration: the real authority, ledger, and
    /// `StrategyHost` head feed drive the lowest-free issuance policy, a
    /// repackage-on-decline round, and an orphan fill through the notification
    /// policy.
    #[test]
    fn two_strategies_drive_lowest_free_repackage_and_orphan_fill() {
        use degenbot_bot::bot_core::route_registry::RouteRegistry;
        use degenbot_bot::connector_index::V2ConnectorIndex;
        use degenbot_bot::strategy_host::{FacetStatus, StrategyHost};
        use degenbot_eventhub::Hub;
        use tokio::sync::mpsc::unbounded_channel;

        let authority = Arc::new(NonceAuthority::new(0));
        let ledger = Arc::new(SubmissionLedger::new());
        let mut host = StrategyHost::new(
            Arc::new(Hub::new()),
            Arc::new(RouteRegistry::new(V2ConnectorIndex::default())),
            Arc::clone(&authority),
        );
        let settlement = sid("settlement");
        let backrun = sid("backrun");
        for id in [&settlement, &backrun] {
            host.register(id.clone(), FacetStatus::Configured)
                .expect("register");
            host.enable(id).expect("enable");
        }
        let reconciler: Arc<dyn HeadReconciler> = ledger.clone();
        host.attach_reconciler(reconciler);
        let (settlement_tx, mut settlement_rx) = unbounded_channel();
        host.subscribe_head(&settlement, settlement_tx)
            .expect("subscribe");

        let settlement_lane = Arc::new(NonceLane::new(
            Arc::clone(&authority),
            Arc::clone(&ledger),
            settlement.clone(),
        ));
        let backrun_lane = Arc::new(NonceLane::new(
            Arc::clone(&authority),
            Arc::clone(&ledger),
            backrun.clone(),
        ));
        let settlement_policy = HeadPolicy::new(Arc::clone(&settlement_lane));

        // (i) Lowest-free issuance under a pending prior nonce: settlement's
        // broadcast holds 0, so backrun stamps 1.
        let settlement0 = settlement_lane.stamp().expect("settlement stamp");
        assert_eq!(settlement0.nonce(), 0);
        settlement_lane
            .record_signed(&settlement0, target(0), hash(0), 0)
            .expect("record signed");
        authority.record_broadcast(&settlement0).expect("broadcast");
        ledger
            .record_broadcast(&settlement, 0)
            .expect("ledger broadcast");
        let backrun1 = backrun_lane.stamp().expect("backrun stamp");
        assert_eq!(backrun1.nonce(), 1, "the pending 0 is skipped");
        backrun_lane
            .record_signed(&backrun1, target(1), hash(1), 0)
            .expect("record signed");

        // (ii) Repackage-on-decline round: the chain advances past backrun's
        // lease, so the default stamp policy releases it and re-stamps.
        let landed = host.on_head(1);
        assert_eq!(landed.len(), 1);
        assert_eq!(landed[0].strategy(), &settlement);
        assert_eq!(landed[0].notice(), &StrategyNotice::Landed);
        assert_eq!(
            settlement_rx.try_recv().expect("landed delivered").notice(),
            &StrategyNotice::Landed
        );
        let advanced = host.on_head(2);
        assert!(advanced
            .iter()
            .any(|notice| notice.strategy() == &backrun
                && notice.notice() == &StrategyNotice::Landed));
        let backrun2 = backrun_lane.stamp().expect("repackage");
        assert_eq!(backrun2.nonce(), 2);

        // (iii) Orphan fill via the notification policy: backrun's broadcast
        // at 2 is vacated, leaving settlement's outstanding 3 above a gap.
        backrun_lane
            .record_signed(&backrun2, target(2), hash(2), 2)
            .expect("record signed");
        authority.record_broadcast(&backrun2).expect("broadcast");
        ledger
            .record_broadcast(&backrun, 2)
            .expect("ledger broadcast");
        let settlement3 = settlement_lane.stamp().expect("settlement stamp");
        assert_eq!(settlement3.nonce(), 3);
        settlement_lane
            .record_signed(&settlement3, target(3), hash(3), 2)
            .expect("record signed");
        authority.release_strategy(&backrun);

        let final_notices = host.on_head(2);
        let orphan = final_notices
            .iter()
            .find(|notice| notice.strategy() == &settlement)
            .expect("orphan notice");
        assert_eq!(
            orphan.notice(),
            &StrategyNotice::Orphaned { fillable_nonce: 2 }
        );
        let delivered = settlement_rx.try_recv().expect("orphan delivered");
        assert_eq!(delivered.notice(), orphan.notice());
        assert_eq!(
            settlement_policy
                .on_notice(&delivered)
                .expect("orphan fill"),
            PolicyAction::Restamped { nonce: 2 },
            "the default policy re-stamps at the vacated predecessor"
        );
    }

    fn oracle(
        before: SubmissionState,
        nonce: u64,
        confirmed: u64,
        outstanding: &BTreeSet<u64>,
    ) -> (SubmissionState, Option<NotificationKind>) {
        if before.is_terminal() {
            return (before, None);
        }
        if nonce < confirmed {
            return (SubmissionState::Landed, Some(NotificationKind::Landed));
        }
        if !outstanding.contains(&nonce) {
            return (SubmissionState::Stale, Some(NotificationKind::Stale));
        }
        if let Some(prev) = nonce.checked_sub(1) {
            if prev >= confirmed && !outstanding.contains(&prev) {
                return (
                    SubmissionState::Orphaned,
                    Some(NotificationKind::Orphaned {
                        fillable_nonce: prev,
                    }),
                );
            }
        }
        (before, None)
    }

    fn fixture() -> impl Strategy<Value = (u64, Vec<(usize, u64, bool)>, Vec<bool>)> {
        (
            0u64..6,
            proptest::collection::vec((0usize..2, 0u64..8, any::<bool>()), 0..8),
            proptest::collection::vec(any::<bool>(), 0..8),
        )
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(256))]

        #[test]
        fn reconciliation_is_total_and_closes_every_record(
            (confirmed, seeds, outstanding_flags) in fixture(),
        ) {
            let ledger = SubmissionLedger::new();
            // Nonces are unique per account; drop duplicate seeds.
            let mut deduped: BTreeMap<(usize, u64), bool> = BTreeMap::new();
            for (index, nonce, broadcast) in seeds {
                deduped.insert((index, nonce), broadcast);
            }
            for (&(index, nonce), &broadcast) in &deduped {
                let strategy = sid(&format!("s{index}"));
                ledger
                    .record_signed(&strategy, nonce, target(nonce), hash(nonce), 100)
                    .expect("record_signed");
                if broadcast {
                    ledger.record_broadcast(&strategy, nonce).expect("broadcast");
                }
            }
            let outstanding: Vec<u64> = outstanding_flags
                .iter()
                .enumerate()
                .filter(|&(_, live)| *live)
                .map(|(nonce, _)| u64::try_from(nonce).expect("index fits u64"))
                .collect();
            let outstanding_set: BTreeSet<u64> = outstanding.iter().copied().collect();

            let notifications = ledger.reconcile(confirmed, &outstanding);
            let reported: BTreeMap<(usize, u64), NotificationKind> = notifications
                .iter()
                .map(|n| {
                    let index = n
                        .strategy()
                        .as_str()
                        .trim_start_matches('s')
                        .parse::<usize>()
                        .expect("strategy index");
                    ((index, n.nonce()), n.kind())
                })
                .collect();

            let mut expected_reported: BTreeMap<(usize, u64), NotificationKind> = BTreeMap::new();
            for (&(index, nonce), &broadcast) in &deduped {
                let strategy = sid(&format!("s{index}"));
                let before = if broadcast {
                    SubmissionState::Broadcast
                } else {
                    SubmissionState::Signed
                };
                let (after, kind) = oracle(before, nonce, confirmed, &outstanding_set);
                prop_assert_eq!(
                    ledger.state_of(&strategy, nonce),
                    Some(after),
                    "state for s{} at nonce {}",
                    index,
                    nonce
                );
                if let Some(kind) = kind {
                    expected_reported.insert((index, nonce), kind);
                }
            }
            prop_assert_eq!(reported, expected_reported);
        }
    }
}
