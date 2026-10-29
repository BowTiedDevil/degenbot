//! The sign-time nonce authority: the process-wide owner of the operator
//! account's next nonce.
//!
//! Every strategy that signs with the shared operator EOA asks this authority
//! for a nonce at the last moment (sign time), so two strategies can never
//! stamp the same nonce. The authority always hands back the **lowest** nonce
//! at or above the confirmed chain nonce that no strategy currently holds,
//! which keeps the tracked set a filling prefix above the chain: a gap left by
//! a released reservation is refilled by the next signer rather than filled
//! with an artificial self-send.
//!
//! Three disjoint sets make up the outstanding ledger: at most one **lease**
//! per strategy (the reservation a strategy signs against), the **broadcasts** a
//! lease becomes once its transaction is on the wire, and the
//! **delivery-unknown holds** a lease becomes when its delivery attempt left the
//! transaction's fate open. A hold occupies its nonce exactly as a broadcast
//! does — the bytes may be live, so nothing is issued there for free — and it
//! carries the fee caps a replacement must dominate so an intentionally repriced
//! lane replaces the held transaction instead of shadowing it. Advancing the
//! confirmed chain nonce reconciles landed broadcasts and lapsed holds out of the
//! tracked set; releasing a strategy (a skip or a tombstone) removes its lease,
//! every broadcast it still owns, and its hold, so a successor's stamp self-heals
//! the chain.
//!
//! Strategies reach the authority through the seam it mints for them: a
//! [`NonceLane`] binds one strategy's name to the shared authority and to that
//! strategy's own ledger, so signing is lane-shaped while issuance stays
//! process-wide.
//!
//! The authority never panics on caller input: every state contradiction is a
//! typed [`DeclineKind`], and the shared ledger lock is non-poisoning
//! (`parking_lot`) so one strategy's failure cannot wedge another's sign path.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use alloy::primitives::B256;
use parking_lot::Mutex;

/// The name a strategy is registered under at the host, used as the ledger key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StrategyId(String);

impl StrategyId {
    /// Name a strategy.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The strategy's registered name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StrategyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for StrategyId {
    fn from(name: &str) -> Self {
        Self::new(name)
    }
}

impl From<String> for StrategyId {
    fn from(name: String) -> Self {
        Self::new(name)
    }
}

/// Why the authority refused a call. Declines are routine, never fatal: the
/// strategy repackages against the next available nonce or releases its stale
/// reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclineKind {
    /// The strategy already holds the one outstanding lease v1 allows.
    StrategyLeaseOutstanding,
    /// A tracked reservation sits below the confirmed chain nonce, so the
    /// ledger contradicts the chain. Fail closed until the stale reservation
    /// is released rather than sign against a nonce the chain has passed.
    BelowChainNonce,
    /// The named lease is not the strategy's current outstanding reservation.
    UnknownLease,
    /// The named broadcast is not outstanding for the strategy.
    UnknownBroadcast,
    /// The strategy's own delivery-unknown hold occupies a nonce: the bytes it
    /// covers may be live, so a fresh stamp would sign a second transaction
    /// over a possibly-live one. Reprice the held transaction through the
    /// explicit replace, or wait for chain evidence.
    DeliveryUnknownHold,
    /// The strategy holds no delivery-unknown hold (at the named nonce).
    UnknownHold,
    /// A replacement's fee caps do not dominate the held transaction's under
    /// the standard replacement bump, so no node would admit it.
    FeesDoNotDominate,
    /// No nonce remains at or above the confirmed chain nonce.
    Exhausted,
}

impl fmt::Display for DeclineKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::StrategyLeaseOutstanding => "strategy already holds an outstanding lease",
            Self::BelowChainNonce => "a tracked reservation is below the confirmed chain nonce",
            Self::UnknownLease => "lease is not outstanding for the named strategy",
            Self::UnknownBroadcast => "broadcast is not outstanding for the named strategy",
            Self::DeliveryUnknownHold => "the strategy holds a delivery-unknown nonce",
            Self::UnknownHold => "no delivery-unknown hold is outstanding for the strategy",
            Self::FeesDoNotDominate => "replacement fees do not dominate the held transaction",
            Self::Exhausted => "no nonce remains at or above the confirmed chain nonce",
        };
        f.write_str(text)
    }
}

/// The per-lane policy a mint fixes: how long an unresolved delivery may freeze
/// its nonce, and the census label the lane reports under.
///
/// Both are per-strategy. Hold windows differ because a settlement arm and a
/// speculative pending-tx backrun put different prices on a slot-minute, and the
/// label is a compile-time constant because a metric label must be: the mint is
/// the one place a lane's identity becomes reportable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanePolicy {
    hold_window: HoldWindow,
    label: &'static str,
}

impl LanePolicy {
    /// The policy for a lane with this hold window, reporting under `label`.
    #[must_use]
    pub const fn new(hold_window: HoldWindow, label: &'static str) -> Self {
        Self { hold_window, label }
    }

    /// The lane's hold window.
    #[must_use]
    pub const fn hold_window(&self) -> HoldWindow {
        self.hold_window
    }

    /// The lane's census label.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        self.label
    }
}

impl Default for LanePolicy {
    /// The default window under the `unlabelled` census label: what a lane with
    /// no registered strategy behind it (a fixture, a one-off console arm)
    /// reports as.
    fn default() -> Self {
        Self {
            hold_window: HoldWindow::DEFAULT,
            label: "unlabelled",
        }
    }
}

/// How long a delivery-unknown hold keeps its nonce frozen, in blocks.
///
/// The window is the bet that an unresolved delivery is still live: the bytes
/// may sit in a channel, so the nonce must not be re-issued until chain
/// evidence or the window's end settles it. Five blocks is the window the
/// receipt monitor already gives a pending transaction before declaring it
/// expired — a hold is that same bet without a hash to poll, so it inherits the
/// same chain-time budget. A lane whose economics price time differently (a
/// settled-block settlement arm and a speculative pending-tx backrun do not
/// agree on the value of a slot-minute) mints with its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HoldWindow {
    blocks: u64,
}

impl HoldWindow {
    /// The window a lane is minted with unless its owner says otherwise.
    pub const DEFAULT: Self = Self { blocks: 5 };

    /// A window of `blocks` blocks.
    #[must_use]
    pub const fn new(blocks: u64) -> Self {
        Self { blocks }
    }

    /// The window's length in blocks.
    #[must_use]
    pub const fn blocks(self) -> u64 {
        self.blocks
    }
}

/// The fee caps a delivery-unknown hold recorded when it was entered.
///
/// The held transaction may be live, so the slot it occupies is not free: a
/// replacement may reuse the nonce only when it dominates these caps. They are
/// recorded at hold time because the failed delivery is the only place the
/// signed transaction's own fees were known — the bytes are gone by then.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeldFees {
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
}

impl HeldFees {
    /// The fee caps of one signed transaction.
    #[must_use]
    pub const fn new(max_fee_per_gas: u128, max_priority_fee_per_gas: u128) -> Self {
        Self {
            max_fee_per_gas,
            max_priority_fee_per_gas,
        }
    }

    /// The gas-price cap (`maxFeePerGas`).
    #[must_use]
    pub const fn max_fee_per_gas(&self) -> u128 {
        self.max_fee_per_gas
    }

    /// The priority-fee cap (`maxPriorityFeePerGas`).
    #[must_use]
    pub const fn max_priority_fee_per_gas(&self) -> u128 {
        self.max_priority_fee_per_gas
    }

    /// Whether these caps dominate `held` under the replacement bump: both caps
    /// must clear the held transaction's, or a node refuses the replacement as
    /// underpriced and it can never be included.
    #[must_use]
    pub fn dominates(&self, held: Self) -> bool {
        self.max_fee_per_gas >= replacement_floor(held.max_fee_per_gas)
            && self.max_priority_fee_per_gas >= replacement_floor(held.max_priority_fee_per_gas)
    }
}

/// The bump a replacement must carry over the caps it replaces, in percent.
///
/// Mainstream clients admit a second transaction at an occupied nonce only when
/// both its fee caps exceed the pending transaction's by ten percent; under that
/// they answer "replacement transaction underpriced" and the bytes can never be
/// included. The bump is the clients' threshold rather than this process's
/// choice, so a dominating candidate is admitted everywhere instead of by one
/// node.
const REPLACEMENT_FEE_BUMP_PERCENT: u128 = 110;

/// The floor one replacement cap must clear: `REPLACEMENT_FEE_BUMP_PERCENT` of
/// the held cap, rounded down as the clients do.
///
/// Saturating because `held * 110` comfortably fits `u128` for any fee the
/// network can produce (the overflow point is ~3e18 ether of gas price); the
/// saturation is a guard, never a policy.
fn replacement_floor(held: u128) -> u128 {
    held.saturating_mul(REPLACEMENT_FEE_BUMP_PERCENT) / 100
}

/// Where a delivery-unknown hold sits in its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldPhase {
    /// The window is open. The slot is frozen: nothing is issued at it.
    Frozen,
    /// The head sweep released the hold because its window closed. The slot is
    /// not free — it is issuable only to a replacement whose fees dominate the
    /// held transaction's, so the possibly-live transaction is replaced rather
    /// than shadowed by a second one at the same nonce.
    ReplaceOrDominate,
}

/// One lane's delivery-unknown hold: the nonce its signed bytes occupy, the fee
/// caps a replacement must dominate, and where it sits in its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryHold {
    strategy: StrategyId,
    nonce: u64,
    fees: HeldFees,
    entered_at_head: u64,
    window: HoldWindow,
    phase: HoldPhase,
}

impl DeliveryHold {
    /// The lane that signed the held transaction.
    #[must_use]
    pub fn strategy(&self) -> &StrategyId {
        &self.strategy
    }

    /// The nonce the held transaction was signed against.
    #[must_use]
    pub const fn nonce(&self) -> u64 {
        self.nonce
    }

    /// The held transaction's fee caps.
    #[must_use]
    pub const fn fees(&self) -> HeldFees {
        self.fees
    }

    /// The chain head at which the hold was entered.
    #[must_use]
    pub const fn entered_at_head(&self) -> u64 {
        self.entered_at_head
    }

    /// The lane's window.
    #[must_use]
    pub const fn window(&self) -> HoldWindow {
        self.window
    }

    /// Where the hold sits in its window.
    #[must_use]
    pub const fn phase(&self) -> HoldPhase {
        self.phase
    }

    /// The head at or past which the window is closed.
    #[must_use]
    pub const fn expires_at_head(&self) -> u64 {
        self.entered_at_head.saturating_add(self.window.blocks())
    }

    fn with_phase(mut self, phase: HoldPhase) -> Self {
        self.phase = phase;
        self
    }
}

/// One strategy's sign-time reservation: the nonce it may sign against until
/// the transaction is recorded as broadcast or the strategy is released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonceLease {
    strategy: StrategyId,
    nonce: u64,
}

impl NonceLease {
    /// The strategy the lease belongs to.
    #[must_use]
    pub fn strategy(&self) -> &StrategyId {
        &self.strategy
    }

    /// The reserved nonce.
    #[must_use]
    pub fn nonce(&self) -> u64 {
        self.nonce
    }
}

/// One live lease a rewind revoked. The named strategy should re-lease at the
/// recovered head: the lease handle it held no longer names an outstanding
/// reservation, so re-stamping it declines `UnknownLease`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReorgAdvisory {
    strategy: StrategyId,
    nonce: u64,
}

impl ReorgAdvisory {
    /// The strategy whose lease was revoked.
    #[must_use]
    pub fn strategy(&self) -> &StrategyId {
        &self.strategy
    }

    /// The nonce the revoked lease had reserved.
    #[must_use]
    pub fn nonce(&self) -> u64 {
        self.nonce
    }
}

#[derive(Debug, Default)]
struct NonceState {
    confirmed: u64,
    leases: BTreeMap<StrategyId, u64>,
    broadcasts: BTreeMap<u64, StrategyId>,
    /// Broadcasts the chain had confirmed below the tracked nonce, retained so
    /// a rewind can restore them: a reorged transaction returns to the wire
    /// and its nonce must not be re-issued out of the landed range.
    landed: BTreeMap<u64, StrategyId>,
    /// Delivery-unknown holds, keyed by the nonce their bytes were signed
    /// against. A hold occupies its nonce exactly as a broadcast does — in
    /// either phase — so nothing is issued at the slot for free.
    holds: BTreeMap<u64, DeliveryHold>,
    /// Holds the chain had confirmed below the tracked nonce, retained so a
    /// rewind can restore them: a reorged hold returns to the wire and its
    /// nonce must not be issued freely out of the rewound range. The `landed`
    /// index's counterpart for holds.
    lapsed_holds: BTreeMap<u64, DeliveryHold>,
}

impl NonceState {
    /// Move every broadcast and hold strictly below `confirmed` out of the
    /// outstanding set and into its retention index. Shared by the monotonic
    /// and reorg head paths so they cannot drift.
    ///
    /// A hold the chain has passed is over: whichever transaction consumed the
    /// slot, the held bytes can no longer be included at it. The slot stops
    /// occupying the nonce, and the hold is retained (not discarded) so a
    /// rewind can restore it into replace-or-dominate.
    fn land_below(&mut self, confirmed: u64) {
        let landed: Vec<u64> = self
            .broadcasts
            .keys()
            .copied()
            .filter(|nonce| *nonce < confirmed)
            .collect();
        for nonce in landed {
            if let Some(owner) = self.broadcasts.remove(&nonce) {
                self.landed.insert(nonce, owner);
            }
        }
        let lapsed: Vec<u64> = self
            .holds
            .keys()
            .copied()
            .filter(|nonce| *nonce < confirmed)
            .collect();
        for nonce in lapsed {
            if let Some(hold) = self.holds.remove(&nonce) {
                self.lapsed_holds
                    .insert(nonce, hold.with_phase(HoldPhase::ReplaceOrDominate));
            }
        }
    }

    /// The lane's outstanding delivery-unknown hold, if it has one.
    fn hold_of(&self, strategy: &StrategyId) -> Option<&DeliveryHold> {
        self.holds.values().find(|hold| hold.strategy == *strategy)
    }

    fn is_outstanding(&self, nonce: u64) -> bool {
        self.broadcasts.contains_key(&nonce)
            || self.holds.contains_key(&nonce)
            || self.leases.values().any(|held| *held == nonce)
    }

    fn lowest_free(&self, confirmed: u64) -> Option<u64> {
        let mut nonce = confirmed;
        loop {
            if !self.is_outstanding(nonce) {
                return Some(nonce);
            }
            nonce = nonce.checked_add(1)?;
        }
    }
}

/// The process-wide nonce ledger shared by every signed strategy.
#[derive(Debug, Default)]
pub struct NonceAuthority {
    state: Mutex<NonceState>,
}

impl NonceAuthority {
    /// An authority whose confirmed chain nonce is `confirmed`.
    #[must_use]
    pub fn new(confirmed: u64) -> Self {
        Self {
            state: Mutex::new(NonceState {
                confirmed,
                ..NonceState::default()
            }),
        }
    }

    /// The tracked confirmed chain nonce (the account's next nonce).
    #[must_use]
    pub fn confirmed(&self) -> u64 {
        self.state.lock().confirmed
    }

    /// Advance the confirmed chain nonce to the chain's authoritative value.
    ///
    /// Broadcasts strictly below the new value are confirmed on-chain and leave
    /// the outstanding set; they are retained in the landed index (not
    /// discarded) so [`set_confirmed_reorg`](Self::set_confirmed_reorg) can
    /// restore them if a reorg un-confirms that range. A strategy lease is
    /// deliberately not pruned here, so a reservation the chain has passed
    /// still declines at the next sign instead of silently becoming reusable.
    ///
    /// This is the monotonic path: it defines only the advancing case. A head
    /// update that may move backward must go through
    /// [`set_confirmed_reorg`](Self::set_confirmed_reorg).
    pub fn set_confirmed(&self, confirmed: u64) {
        let mut state = self.state.lock();
        state.confirmed = confirmed;
        state.land_below(confirmed);
    }

    /// Advance the confirmed chain nonce from an observed chain read, never
    /// rewinding it.
    ///
    /// A sign path that fetched the account's next nonce seeds the clock before
    /// its first stamp, so no lane can issue a nonce the chain has already
    /// consumed. The per-head refresh is gated on outstanding work, so an idle
    /// process has no head read to lean on: this is the entry point that makes
    /// its first sign safe. A read that reports a lower value (an RPC race)
    /// must not revoke a live lease, so only a forward move is applied; the
    /// reorg-safe rewind stays the head feed's call
    /// ([`set_confirmed_reorg`](Self::set_confirmed_reorg)).
    pub fn observe_confirmed(&self, confirmed: u64) {
        if confirmed > self.confirmed() {
            let _ = self.set_confirmed_reorg(confirmed);
        }
    }

    /// Reconcile the confirmed chain nonce after a head update that may rewind,
    /// returning one [`ReorgAdvisory`] per lease the rewind voids.
    ///
    /// A forward move is the ordinary confirmation path and behaves exactly
    /// like [`set_confirmed`](Self::set_confirmed). A backward move is a reorg
    /// rewind: every broadcast the old head had confirmed inside the rewound
    /// window is restored to the outstanding set (its transaction may be back
    /// on the wire, so its nonce must not be re-issued), and every live lease
    /// reserved inside that window is released with an advisory so the owning
    /// strategy re-leases at the recovered head. Leases at or above the old
    /// nonce are untouched.
    ///
    /// Advisories are returned in strategy-name order, then ascending nonce.
    /// The call is idempotent for a repeated same-value rewind. Revived
    /// broadcasts are not returned: a caller that needs them diffs
    /// [`outstanding_nonces`](Self::outstanding_nonces) across the call, and
    /// the per-head policy owns deciding whether a revived broadcast is still
    /// alive or must be released.
    #[must_use]
    pub fn set_confirmed_reorg(&self, confirmed: u64) -> Vec<ReorgAdvisory> {
        let mut state = self.state.lock();
        let old = state.confirmed;
        if confirmed >= old {
            state.confirmed = confirmed;
            state.land_below(confirmed);
            return Vec::new();
        }
        state.confirmed = confirmed;

        // Restore the broadcasts the old head confirmed inside the rewound
        // window. This is the re-issue fix: without it the forward prune would
        // leave an in-flight nonce looking free.
        let revived: Vec<u64> = state
            .landed
            .range(confirmed..old)
            .map(|(nonce, _)| *nonce)
            .collect();
        for nonce in revived {
            if let Some(owner) = state.landed.remove(&nonce) {
                state.broadcasts.insert(nonce, owner);
            }
        }

        // Restore the holds the old head had lapsed inside the rewound
        // window: the held bytes may be back on the wire, so the slot returns
        // to replace-or-dominate instead of looking free. The landed-index
        // rule, one book over.
        let restored: Vec<u64> = state
            .lapsed_holds
            .range(confirmed..old)
            .map(|(nonce, _)| *nonce)
            .collect();
        for nonce in restored {
            if let Some(hold) = state.lapsed_holds.remove(&nonce) {
                state.holds.insert(nonce, hold);
            }
        }

        // A lease reserved in the rewound window was stamped against a head
        // that is no longer canonical: release it and advise its owner.
        let revoked: Vec<(StrategyId, u64)> = state
            .leases
            .iter()
            .filter(|(_, nonce)| **nonce >= confirmed && **nonce < old)
            .map(|(strategy, nonce)| (strategy.clone(), *nonce))
            .collect();
        let mut advisories = Vec::with_capacity(revoked.len());
        for (strategy, nonce) in revoked {
            state.leases.remove(&strategy);
            advisories.push(ReorgAdvisory { strategy, nonce });
        }
        advisories
    }

    /// Reserve the lowest nonce at or above the confirmed chain nonce that no
    /// lease, broadcast, or hold occupies.
    ///
    /// A delivery-unknown hold occupies its nonce in either phase: a swept
    /// release is not a free nonce, so ordinary issuance parks the slot and
    /// only the owning lane's explicit [`lease_replace`](Self::lease_replace)
    /// can reuse it.
    ///
    /// # Errors
    ///
    /// [`DeclineKind::StrategyLeaseOutstanding`] if `strategy` already holds a
    /// lease; [`DeclineKind::DeliveryUnknownHold`] if it holds a
    /// delivery-unknown nonce (the bytes there may be live);
    /// [`DeclineKind::BelowChainNonce`] if a tracked reservation sits below the
    /// confirmed chain nonce (reconcile before signing).
    pub fn lease(&self, strategy: &StrategyId) -> Result<NonceLease, DeclineKind> {
        let mut state = self.state.lock();
        if state.leases.contains_key(strategy) {
            return Err(DeclineKind::StrategyLeaseOutstanding);
        }
        if state.hold_of(strategy).is_some() {
            return Err(DeclineKind::DeliveryUnknownHold);
        }
        if state.leases.values().any(|held| *held < state.confirmed)
            || state.broadcasts.keys().any(|held| *held < state.confirmed)
        {
            return Err(DeclineKind::BelowChainNonce);
        }
        let confirmed = state.confirmed;
        let nonce = state.lowest_free(confirmed).ok_or(DeclineKind::Exhausted)?;
        state.leases.insert(strategy.clone(), nonce);
        Ok(NonceLease {
            strategy: strategy.clone(),
            nonce,
        })
    }

    /// Park `lease` as a delivery-unknown hold: the bytes may be live at the
    /// lease's nonce, so the slot stays occupied and a replacement must dominate
    /// the fee caps recorded here.
    ///
    /// The hold replaces the lease rather than joining it — one slot, one
    /// occupant — and it always starts [`HoldPhase::Frozen`]: the window has to
    /// run before a sweep may release the slot.
    ///
    /// # Errors
    ///
    /// [`DeclineKind::UnknownLease`] if `lease` is not the strategy's current
    /// reservation; [`DeclineKind::BelowChainNonce`] if the chain has already
    /// passed the reserved nonce (the slot is consumed, so there is nothing
    /// left to hold).
    pub fn record_hold(
        &self,
        lease: &NonceLease,
        fees: HeldFees,
        window: HoldWindow,
        at_head: u64,
    ) -> Result<DeliveryHold, DeclineKind> {
        let mut state = self.state.lock();
        match state.leases.get(lease.strategy()) {
            Some(nonce) if *nonce == lease.nonce() => {}
            _ => return Err(DeclineKind::UnknownLease),
        }
        if lease.nonce() < state.confirmed {
            return Err(DeclineKind::BelowChainNonce);
        }
        state.leases.remove(lease.strategy());
        let hold = DeliveryHold {
            strategy: lease.strategy().clone(),
            nonce: lease.nonce(),
            fees,
            entered_at_head: at_head,
            window,
            phase: HoldPhase::Frozen,
        };
        state.holds.insert(hold.nonce(), hold.clone());
        Ok(hold)
    }

    /// Release a delivery-unknown hold whose window closed: the slot moves to
    /// [`HoldPhase::ReplaceOrDominate`], so a replacement may reuse the nonce
    /// only against dominating fees.
    ///
    /// Idempotent: a hold already released answers with its current state, so a
    /// second sweep cannot re-release (and re-report) the same slot.
    ///
    /// # Errors
    ///
    /// [`DeclineKind::UnknownHold`] when `strategy` holds no delivery-unknown
    /// hold at `nonce`.
    pub fn release_hold(
        &self,
        strategy: &StrategyId,
        nonce: u64,
    ) -> Result<DeliveryHold, DeclineKind> {
        let mut state = self.state.lock();
        let Some(hold) = state.holds.get_mut(&nonce) else {
            return Err(DeclineKind::UnknownHold);
        };
        if hold.strategy != *strategy {
            return Err(DeclineKind::UnknownHold);
        }
        hold.phase = HoldPhase::ReplaceOrDominate;
        Ok(hold.clone())
    }

    /// Grant the lane that holds a delivery-unknown hold a fresh lease at the
    /// very nonce it occupies, in exchange for fees that dominate the held
    /// transaction's.
    ///
    /// This is the only way a held slot is reused: the nonce is not handed to
    /// the next signer (that would shadow a possibly-live transaction), it is
    /// re-signed by the lane that created the hazard and only with bytes that
    /// win the replacement race.
    ///
    /// # Errors
    ///
    /// [`DeclineKind::UnknownHold`] when the strategy holds no delivery-unknown
    /// nonce; [`DeclineKind::BelowChainNonce`] when the chain has already passed
    /// the held nonce; [`DeclineKind::FeesDoNotDominate`] when `fees` miss the
    /// replacement bump.
    pub fn lease_replace(
        &self,
        strategy: &StrategyId,
        fees: HeldFees,
    ) -> Result<NonceLease, DeclineKind> {
        let mut state = self.state.lock();
        let Some(hold) = state.hold_of(strategy).cloned() else {
            return Err(DeclineKind::UnknownHold);
        };
        if hold.nonce() < state.confirmed {
            return Err(DeclineKind::BelowChainNonce);
        }
        if !fees.dominates(hold.fees()) {
            return Err(DeclineKind::FeesDoNotDominate);
        }
        state.holds.remove(&hold.nonce());
        state.leases.insert(strategy.clone(), hold.nonce());
        Ok(NonceLease {
            strategy: strategy.clone(),
            nonce: hold.nonce(),
        })
    }

    /// Every delivery-unknown hold, ascending by nonce.
    ///
    /// The per-head sweep reads this: it is the only way a released slot is
    /// ever freed, so the pass that owns the chain read owns the list.
    #[must_use]
    pub fn holds(&self) -> Vec<DeliveryHold> {
        self.state.lock().holds.values().cloned().collect()
    }

    /// The lane's outstanding delivery-unknown hold, if it has one.
    #[must_use]
    pub fn hold_of(&self, strategy: &StrategyId) -> Option<DeliveryHold> {
        self.state.lock().hold_of(strategy).cloned()
    }

    /// Every outstanding lease, ascending by nonce.
    ///
    /// The head sweep reads this to release reservations the chain has passed:
    /// one stale lease otherwise declines every lane's stamp account-wide.
    #[must_use]
    pub fn outstanding_leases(&self) -> Vec<NonceLease> {
        let state = self.state.lock();
        let mut leases: Vec<NonceLease> = state
            .leases
            .iter()
            .map(|(strategy, nonce)| NonceLease {
                strategy: strategy.clone(),
                nonce: *nonce,
            })
            .collect();
        leases.sort_by_key(NonceLease::nonce);
        leases
    }

    /// Promote `lease` to a broadcast: the transaction is on the wire and its
    /// nonce stays outstanding until the chain confirms it.
    ///
    /// # Errors
    ///
    /// [`DeclineKind::UnknownLease`] if `lease` is not the strategy's current
    /// reservation; [`DeclineKind::BelowChainNonce`] if the chain has already
    /// passed the reserved nonce.
    pub fn record_broadcast(&self, lease: &NonceLease) -> Result<(), DeclineKind> {
        let mut state = self.state.lock();
        match state.leases.get(lease.strategy()) {
            Some(nonce) if *nonce == lease.nonce() => {}
            _ => return Err(DeclineKind::UnknownLease),
        }
        if lease.nonce() < state.confirmed {
            return Err(DeclineKind::BelowChainNonce);
        }
        state.leases.remove(lease.strategy());
        state
            .broadcasts
            .insert(lease.nonce(), lease.strategy().clone());
        Ok(())
    }

    /// Release one strategy's outstanding lease without touching the
    /// broadcasts it still owns, returning the freed nonce.
    ///
    /// A sign path that built against a lease but never got the transaction
    /// onto the wire frees only that reservation: the nonce is reusable, while
    /// an in-flight broadcast at an earlier nonce stays outstanding and is not
    /// re-issued. [`release_strategy`](Self::release_strategy) remains the
    /// tombstone path that clears both.
    ///
    /// # Errors
    ///
    /// [`DeclineKind::UnknownLease`] if `lease` is not the strategy's current
    /// outstanding reservation.
    pub fn release_lease(&self, lease: &NonceLease) -> Result<u64, DeclineKind> {
        let mut state = self.state.lock();
        match state.leases.get(lease.strategy()) {
            Some(nonce) if *nonce == lease.nonce() => {
                state.leases.remove(lease.strategy());
                Ok(lease.nonce())
            }
            _ => Err(DeclineKind::UnknownLease),
        }
    }

    /// Release every reservation a strategy owns — its lease and its
    /// broadcasts — and return the freed nonces, ascending. Idempotent: a
    /// strategy with nothing outstanding frees nothing.
    pub fn release_strategy(&self, strategy: &StrategyId) -> Vec<u64> {
        let mut state = self.state.lock();
        let mut freed = Vec::new();
        if let Some(nonce) = state.leases.remove(strategy) {
            freed.push(nonce);
        }
        state.broadcasts.retain(|nonce, owner| {
            if owner == strategy {
                freed.push(*nonce);
                false
            } else {
                true
            }
        });
        // A tombstone clears holds too: a hold occupies its nonce exactly as a
        // broadcast does, so leaving one behind would wedge the account's nonce
        // prefix on a lane that no longer exists.
        state.holds.retain(|nonce, hold| {
            if hold.strategy == *strategy {
                freed.push(*nonce);
                false
            } else {
                true
            }
        });
        freed.sort_unstable();
        freed
    }

    /// Release one broadcast the strategy owns, returning the freed nonce.
    ///
    /// This is the expiry escape hatch: a broadcast that never lands would
    /// otherwise stay outstanding forever, wedging the account's nonce prefix
    /// (no successor may consume an outstanding slot, and an account that
    /// never advances has no head event to reconcile it away). The caller
    /// asserts the transaction will never land; releasing a broadcast that is
    /// still on the wire is the double-issuance hazard the authority otherwise
    /// refuses, so this entry point exists only for a monitor's expiry verdict.
    ///
    /// The landed index is untouched: a nonce that already confirmed is not a
    /// broadcast and declines here.
    ///
    /// # Errors
    ///
    /// [`DeclineKind::UnknownBroadcast`] if `nonce` is not a broadcast owned
    /// by `strategy`.
    pub fn release_broadcast(&self, strategy: &StrategyId, nonce: u64) -> Result<u64, DeclineKind> {
        let mut state = self.state.lock();
        match state.broadcasts.get(&nonce) {
            Some(owner) if owner == strategy => {
                state.broadcasts.remove(&nonce);
                Ok(nonce)
            }
            _ => Err(DeclineKind::UnknownBroadcast),
        }
    }

    /// The strategy's outstanding lease, if it holds one.
    #[must_use]
    pub fn lease_of(&self, strategy: &StrategyId) -> Option<NonceLease> {
        let state = self.state.lock();
        state.leases.get(strategy).map(|nonce| NonceLease {
            strategy: strategy.clone(),
            nonce: *nonce,
        })
    }

    /// Whether any lease or broadcast is currently outstanding.
    ///
    /// The per-head reconcile guard reads this to skip the chain-nonce refresh
    /// when no strategy has signed anything since boot.
    #[must_use]
    pub fn has_outstanding(&self) -> bool {
        let state = self.state.lock();
        !state.leases.is_empty() || !state.broadcasts.is_empty() || !state.holds.is_empty()
    }

    /// Every outstanding nonce — leases, broadcasts, and holds — ascending, with
    /// no duplicates.
    ///
    /// A hold belongs here in either phase: the ledger's reconcile reads this
    /// set to decide a record went stale, and a held or released-but-unreplaced
    /// slot is still the lane's.
    #[must_use]
    pub fn outstanding_nonces(&self) -> Vec<u64> {
        let state = self.state.lock();
        let mut nonces: Vec<u64> = state.broadcasts.keys().copied().collect();
        nonces.extend(state.holds.keys().copied());
        nonces.extend(state.leases.values().copied());
        nonces.sort_unstable();
        nonces
    }
}

/// What a lane's per-strategy ledger must provide: the two writes a sign path
/// makes into submission truth.
///
/// The lane is the authority's seam, so it names no submission type: a target
/// crosses as the raw hash it is built to follow, and the decline stays the
/// ledger's own (the lane forwards it unchanged). The reconcile half of that
/// truth is not a lane read — the landed/stale/orphaned facts travel the host's
/// notice seam to the strategy that owns them.
pub trait NonceLedger: fmt::Debug + Send + Sync {
    /// Why the ledger refused a write.
    type Decline: fmt::Display;

    /// Record a freshly signed submission against its stamped nonce.
    ///
    /// `replaces` is the wire hash of the submission these bytes supersede at
    /// the same nonce — the replacement chain's one link — or `None` for
    /// ordinary issuance.
    ///
    /// # Errors
    ///
    /// The ledger's own decline — an outstanding record is already tracked at
    /// this strategy and nonce.
    fn record_signed(
        &self,
        strategy: &StrategyId,
        nonce: u64,
        target: B256,
        bundle_hash: B256,
        built_at_head: u64,
        replaces: Option<B256>,
    ) -> Result<(), Self::Decline>;

    /// Promote the strategy's signed record at `nonce` to broadcast: the bytes
    /// are on the wire.
    ///
    /// # Errors
    ///
    /// The ledger's own decline — no record is tracked, or it is not signed.
    fn record_broadcast(&self, strategy: &StrategyId, nonce: u64) -> Result<(), Self::Decline>;
}

impl NonceAuthority {
    /// Mint one strategy's lane: the only way a lane comes into existence.
    ///
    /// The authority is the issuance owner, so the binding to itself is minted
    /// here; the caller supplies the strategy's ledger, whose concrete type is
    /// the composition root's to choose, and the lane's own policy — its hold
    /// window and the census label it reports under.
    #[must_use]
    pub fn mint_lane<L: NonceLedger>(
        self: &Arc<Self>,
        strategy: impl Into<StrategyId>,
        ledger: Arc<L>,
        policy: LanePolicy,
    ) -> NonceLane<L> {
        NonceLane {
            authority: Arc::clone(self),
            ledger,
            strategy: strategy.into(),
            policy,
        }
    }
}

/// The sign-time nonce seam for one strategy.
///
/// A submission path obtains its nonce here at TX-build/sign time: the lane's
/// [`stamp`](Self::stamp) is the issuance entry, and every signed submission is
/// recorded in the same ledger the host reconciles per head. A lane therefore
/// keeps no private nonce reservation table — the shared authority arbitrates
/// the operator account's nonces across strategies.
#[derive(Debug)]
pub struct NonceLane<L: NonceLedger> {
    authority: Arc<NonceAuthority>,
    ledger: Arc<L>,
    strategy: StrategyId,
    policy: LanePolicy,
}

impl<L: NonceLedger> NonceLane<L> {
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

    /// The strategy's submission ledger.
    #[must_use]
    pub fn ledger(&self) -> &Arc<L> {
        &self.ledger
    }

    /// The lane's hold window: how long an unresolved delivery freezes its
    /// nonce before the head sweep may release the slot.
    #[must_use]
    pub const fn hold_window(&self) -> HoldWindow {
        self.policy.hold_window()
    }

    /// The census label the lane reports under.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        self.policy.label()
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
    /// A delivery-unknown hold is not a stale reservation: the bytes at that
    /// nonce may be live, so relinquishing it would sign a second transaction
    /// over a possibly-live one. It declines
    /// ([`DeclineKind::DeliveryUnknownHold`]) and the lane reprices through
    /// [`replace`](Self::replace) or waits for chain evidence.
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
                    if relinquished || decline == DeclineKind::DeliveryUnknownHold {
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
    /// The ledger's [`Decline`](NonceLedger::Decline) when it refuses the stamp
    /// record.
    pub fn record_signed(
        &self,
        lease: &NonceLease,
        target: B256,
        bundle_hash: B256,
        built_at_head: u64,
        replaces: Option<B256>,
    ) -> Result<(), L::Decline> {
        self.ledger.record_signed(
            &self.strategy,
            lease.nonce(),
            target,
            bundle_hash,
            built_at_head,
            replaces,
        )
    }

    /// Promote the stamp record to broadcast once the signed bytes are on the
    /// wire.
    ///
    /// # Errors
    ///
    /// The ledger's [`Decline`](NonceLedger::Decline) when it refuses the
    /// promotion.
    pub fn record_broadcast(&self, lease: &NonceLease) -> Result<(), L::Decline> {
        self.ledger.record_broadcast(&self.strategy, lease.nonce())
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

    /// Park the lease as a delivery-unknown hold under this lane's window.
    ///
    /// The held transaction's fee caps ride along: a replacement has to
    /// dominate them, and the signed bytes are the only place those caps were
    /// known.
    ///
    /// # Errors
    ///
    /// [`DeclineKind::UnknownLease`] when the lease is no longer outstanding;
    /// [`DeclineKind::BelowChainNonce`] when the chain has already passed the
    /// reserved nonce.
    pub fn record_hold(
        &self,
        lease: &NonceLease,
        fees: HeldFees,
        at_head: u64,
    ) -> Result<DeliveryHold, DeclineKind> {
        self.authority
            .record_hold(lease, fees, self.hold_window(), at_head)
    }

    /// Release the lane's delivery-unknown hold at `nonce` once its window
    /// closed: the slot moves into replace-or-dominate.
    ///
    /// # Errors
    ///
    /// [`DeclineKind::UnknownHold`] when the lane holds no delivery-unknown
    /// hold at `nonce`.
    pub fn release_hold(&self, nonce: u64) -> Result<DeliveryHold, DeclineKind> {
        self.authority.release_hold(&self.strategy, nonce)
    }

    /// The lane's outstanding delivery-unknown hold, if it has one.
    #[must_use]
    pub fn hold(&self) -> Option<DeliveryHold> {
        self.authority.hold_of(&self.strategy)
    }

    /// Re-sign at the nonce this lane's delivery-unknown hold occupies,
    /// replacing the held transaction rather than shadowing it.
    ///
    /// The only way the held slot is reused: the fees must dominate the held
    /// transaction's caps under the standard replacement bump, so the fresh
    /// bytes win the race against the possibly-live ones.
    ///
    /// # Errors
    ///
    /// [`DeclineKind::UnknownHold`] when the lane holds no delivery-unknown
    /// nonce; [`DeclineKind::BelowChainNonce`] when the chain has already passed
    /// it; [`DeclineKind::FeesDoNotDominate`] when the fees miss the bump.
    pub fn replace(&self, fees: HeldFees) -> Result<NonceLease, DeclineKind> {
        self.authority.lease_replace(&self.strategy, fees)
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "unit tests: a malformed fixture must fail the test loudly"
)]
mod tests {
    use super::*;

    fn sid(name: &str) -> StrategyId {
        StrategyId::new(name)
    }

    #[test]
    fn lease_returns_the_lowest_free_nonce_at_or_above_confirmed() {
        let authority = NonceAuthority::new(10);
        let lease = authority.lease(&sid("a")).expect("lease");
        assert_eq!(lease.nonce(), 10);
        assert_eq!(authority.lease_of(&sid("a")).unwrap().nonce(), 10);
        assert_eq!(authority.outstanding_nonces(), vec![10]);
    }

    #[test]
    fn second_lease_for_a_strategy_is_declined_as_outstanding() {
        let authority = NonceAuthority::new(10);
        authority.lease(&sid("a")).expect("first lease");
        assert_eq!(
            authority.lease(&sid("a")),
            Err(DeclineKind::StrategyLeaseOutstanding)
        );
    }

    #[test]
    fn a_different_strategy_leases_the_next_nonce() {
        let authority = NonceAuthority::new(10);
        let a = authority.lease(&sid("a")).expect("a");
        let b = authority.lease(&sid("b")).expect("b");
        assert_eq!((a.nonce(), b.nonce()), (10, 11));
    }

    #[test]
    fn recording_a_broadcast_moves_the_nonce_from_lease_to_broadcast() {
        let authority = NonceAuthority::new(10);
        let lease = authority.lease(&sid("a")).expect("lease");
        authority.record_broadcast(&lease).expect("broadcast");
        assert!(authority.lease_of(&sid("a")).is_none());
        assert_eq!(authority.outstanding_nonces(), vec![10]);
        // The strategy may now lease the next nonce.
        assert_eq!(authority.lease(&sid("a")).unwrap().nonce(), 11);
    }

    #[test]
    fn recording_a_consumed_lease_is_declined_as_unknown() {
        let authority = NonceAuthority::new(10);
        let lease = authority.lease(&sid("a")).expect("lease");
        authority.record_broadcast(&lease).expect("broadcast");
        // The lease was consumed into a broadcast; the same handle no longer
        // names the strategy's outstanding reservation.
        assert_eq!(
            authority.record_broadcast(&lease),
            Err(DeclineKind::UnknownLease)
        );
    }

    #[test]
    fn recording_a_lease_under_a_different_strategy_is_declined_as_unknown() {
        let authority = NonceAuthority::new(10);
        let lease = authority.lease(&sid("a")).expect("lease");
        // The nonce is the same, but it is not outstanding for `b`.
        let forged = NonceLease {
            strategy: sid("b"),
            nonce: lease.nonce(),
        };
        assert_eq!(
            authority.record_broadcast(&forged),
            Err(DeclineKind::UnknownLease)
        );
        authority
            .record_broadcast(&lease)
            .expect("genuine broadcast");
    }

    #[test]
    fn advancing_confirmed_reconciles_landed_broadcasts() {
        let authority = NonceAuthority::new(10);
        let lease = authority.lease(&sid("a")).expect("lease");
        authority.record_broadcast(&lease).expect("broadcast");
        assert_eq!(authority.outstanding_nonces(), vec![10]);
        authority.set_confirmed(11);
        assert_eq!(authority.confirmed(), 11);
        assert!(authority.outstanding_nonces().is_empty());
    }

    #[test]
    fn a_lease_below_the_confirmed_nonce_declines() {
        let authority = NonceAuthority::new(10);
        authority.lease(&sid("a")).expect("lease");
        authority.set_confirmed(11);
        assert_eq!(
            authority.lease(&sid("b")),
            Err(DeclineKind::BelowChainNonce)
        );
        // Releasing the stale reservation restores issuance.
        authority.release_strategy(&sid("a"));
        assert_eq!(authority.lease(&sid("b")).unwrap().nonce(), 11);
    }

    #[test]
    fn recording_a_stale_broadcast_declines() {
        let authority = NonceAuthority::new(10);
        let lease = authority.lease(&sid("a")).expect("lease");
        authority.set_confirmed(11);
        assert_eq!(
            authority.record_broadcast(&lease),
            Err(DeclineKind::BelowChainNonce)
        );
    }

    #[test]
    fn release_clears_a_strategy_lease_and_broadcasts() {
        let authority = NonceAuthority::new(10);
        let first = authority.lease(&sid("a")).expect("a");
        authority.record_broadcast(&first).expect("broadcast");
        let second = authority.lease(&sid("a")).expect("a2");
        assert_eq!(authority.outstanding_nonces(), vec![10, 11]);
        let freed = authority.release_strategy(&sid("a"));
        assert_eq!(freed, vec![10, 11]);
        assert!(authority.lease_of(&sid("a")).is_none());
        assert!(authority.outstanding_nonces().is_empty());
        assert_eq!(second.nonce(), 11);
    }

    #[test]
    fn releasing_a_lease_keeps_the_strategys_broadcasts_outstanding() {
        let authority = NonceAuthority::new(10);
        let broadcast = authority.lease(&sid("a")).expect("broadcast");
        authority.record_broadcast(&broadcast).expect("broadcast");
        let stale_lease = authority.lease(&sid("a")).expect("lease");
        assert_eq!(authority.outstanding_nonces(), vec![10, 11]);
        assert_eq!(
            authority
                .release_lease(&stale_lease)
                .expect("release lease"),
            11
        );
        assert_eq!(
            authority.outstanding_nonces(),
            vec![10],
            "the in-flight broadcast at 10 is not freed with the lease"
        );
        assert!(authority.lease_of(&sid("a")).is_none());
    }

    #[test]
    fn releasing_a_lease_that_is_not_outstanding_declines() {
        let authority = NonceAuthority::new(10);
        let lease = authority.lease(&sid("a")).expect("lease");
        authority.record_broadcast(&lease).expect("broadcast");
        assert_eq!(
            authority.release_lease(&lease),
            Err(DeclineKind::UnknownLease),
            "the lease was consumed into a broadcast"
        );
        assert_eq!(authority.outstanding_nonces(), vec![10]);
    }

    #[test]
    fn a_released_gap_is_refilled_by_the_next_lease() {
        let authority = NonceAuthority::new(10);
        let a0 = authority.lease(&sid("a")).expect("a");
        authority.record_broadcast(&a0).expect("broadcast a0");
        let b1 = authority.lease(&sid("b")).expect("b");
        authority.record_broadcast(&b1).expect("broadcast b1");
        // `a` skips: its broadcast 10 is freed, leaving a gap below b's 11.
        authority.release_strategy(&sid("a"));
        assert_eq!(authority.outstanding_nonces(), vec![11]);
        // The next signer refills the gap rather than leaping past it.
        assert_eq!(authority.lease(&sid("c")).unwrap().nonce(), 10);
    }

    #[test]
    fn exhausting_the_nonce_space_declines() {
        let authority = NonceAuthority::new(u64::MAX);
        let lease = authority.lease(&sid("a")).expect("highest nonce");
        assert_eq!(lease.nonce(), u64::MAX);
        assert_eq!(authority.lease(&sid("b")), Err(DeclineKind::Exhausted));
    }

    #[test]
    fn release_of_an_unknown_strategy_is_idempotent() {
        let authority = NonceAuthority::new(10);
        assert!(authority.release_strategy(&sid("ghost")).is_empty());
        assert!(authority.outstanding_nonces().is_empty());
    }

    #[test]
    fn releasing_a_broadcast_frees_only_that_nonce() {
        let authority = NonceAuthority::new(10);
        let a = authority.lease(&sid("a")).expect("a");
        authority.record_broadcast(&a).expect("broadcast a");
        let b = authority.lease(&sid("b")).expect("b");
        authority.record_broadcast(&b).expect("broadcast b");
        assert_eq!(authority.outstanding_nonces(), vec![10, 11]);

        assert_eq!(authority.release_broadcast(&sid("a"), 10), Ok(10));
        assert_eq!(
            authority.outstanding_nonces(),
            vec![11],
            "the sibling broadcast is untouched"
        );
    }

    #[test]
    fn releasing_a_broadcast_owned_by_another_strategy_declines() {
        let authority = NonceAuthority::new(10);
        let a = authority.lease(&sid("a")).expect("a");
        authority.record_broadcast(&a).expect("broadcast a");
        assert_eq!(
            authority.release_broadcast(&sid("b"), a.nonce()),
            Err(DeclineKind::UnknownBroadcast)
        );
        assert_eq!(authority.outstanding_nonces(), vec![10]);
    }

    /// The expiry escape hatch: a broadcast the caller declares dead is freed,
    /// and the next lease refills the slot rather than leaping over a phantom
    /// reservation. Releasing a lease instead declines: expiry acts on a
    /// broadcast.
    #[test]
    fn an_expired_broadcast_reopens_its_slot_for_the_next_lease() {
        let authority = NonceAuthority::new(10);
        let lease = authority.lease(&sid("a")).expect("lease");
        authority.record_broadcast(&lease).expect("broadcast");
        assert_eq!(
            authority.release_lease(&lease),
            Err(DeclineKind::UnknownLease)
        );

        assert_eq!(authority.release_broadcast(&sid("a"), 10), Ok(10));
        assert!(!authority.has_outstanding());
        assert_eq!(
            authority.lease(&sid("a")).expect("refill").nonce(),
            10,
            "the vacated slot is refilled, not skipped"
        );
    }

    #[test]
    fn has_outstanding_tracks_leases_and_broadcasts() {
        let authority = NonceAuthority::new(10);
        assert!(!authority.has_outstanding());
        let lease = authority.lease(&sid("a")).expect("lease");
        assert!(authority.has_outstanding());
        authority.record_broadcast(&lease).expect("broadcast");
        assert!(authority.has_outstanding());
        authority.set_confirmed(11);
        assert!(
            !authority.has_outstanding(),
            "a confirmed broadcast leaves the outstanding set"
        );
    }

    /// The re-issue hazard the forward prune created: a broadcast confirmed by
    /// an advance, then un-confirmed by a rewind, must be revived so its nonce
    /// is not handed back out while the transaction may be back on the wire.
    #[test]
    fn a_rewind_revives_landed_broadcasts_and_blocks_reissue() {
        let authority = NonceAuthority::new(10);
        let lease = authority.lease(&sid("a")).expect("lease");
        authority.record_broadcast(&lease).expect("broadcast");
        authority.set_confirmed(11);
        assert!(authority.outstanding_nonces().is_empty());

        let advisories = authority.set_confirmed_reorg(10);
        assert!(advisories.is_empty(), "no lease was inside the window");
        assert_eq!(
            authority.outstanding_nonces(),
            vec![10],
            "the rewind restores the broadcast as outstanding"
        );
        assert_eq!(
            authority.lease(&sid("b")).expect("b").nonce(),
            11,
            "the in-flight nonce is not re-issued"
        );
    }

    /// A lease reserved inside the rewound window was stamped against a head
    /// that is no longer canonical: it is released with an advisory.
    #[test]
    fn a_rewind_revokes_leases_in_the_rewound_window_with_advisories() {
        let authority = NonceAuthority::new(10);
        let lease = authority.lease(&sid("a")).expect("a");
        assert_eq!(lease.nonce(), 10);
        // The advance leaves the lease in place (below the confirmed nonce);
        // the rewind then finds it inside the rewound window [10, 11).
        authority.set_confirmed(11);

        let advisories = authority.set_confirmed_reorg(10);
        assert_eq!(advisories.len(), 1);
        assert_eq!(advisories[0].strategy(), &sid("a"));
        assert_eq!(advisories[0].nonce(), 10);
        assert!(authority.lease_of(&sid("a")).is_none());
        // Issuance recovers at the rewound head; the released lease no longer
        // blocks the sign path with BelowChainNonce.
        assert_eq!(authority.lease(&sid("b")).expect("b").nonce(), 10);
    }

    /// A lease above the rewound window was not stamped against the rewound
    /// head: it survives the rewind untouched.
    #[test]
    fn a_rewind_keeps_leases_above_the_rewound_window() {
        let authority = NonceAuthority::new(10);
        let a0 = authority.lease(&sid("a")).expect("a0");
        authority.record_broadcast(&a0).expect("broadcast a0");
        let b1 = authority.lease(&sid("b")).expect("b1");
        assert_eq!(b1.nonce(), 11);
        authority.set_confirmed(11);

        let advisories = authority.set_confirmed_reorg(10);
        assert!(
            advisories.is_empty(),
            "b's lease 11 is at the old confirmed nonce, above the window [10, 11)"
        );
        assert_eq!(authority.lease_of(&sid("b")).expect("b").nonce(), 11);
        assert_eq!(authority.outstanding_nonces(), vec![10, 11]);
    }

    /// A forward `set_confirmed_reorg` behaves exactly like the monotonic
    /// path: it reconciles landed broadcasts and returns no advisories.
    #[test]
    fn a_forward_reorg_move_reconciles_like_set_confirmed() {
        let authority = NonceAuthority::new(10);
        let lease = authority.lease(&sid("a")).expect("lease");
        authority.record_broadcast(&lease).expect("broadcast");

        let advisories = authority.set_confirmed_reorg(11);
        assert!(advisories.is_empty());
        assert_eq!(authority.confirmed(), 11);
        assert!(authority.outstanding_nonces().is_empty());
    }

    /// A same-value rewind is a no-op: no advisory, no reservation change.
    #[test]
    fn a_same_value_reorg_move_is_a_noop() {
        let authority = NonceAuthority::new(10);
        let lease = authority.lease(&sid("a")).expect("lease");
        assert_eq!(lease.nonce(), 10);

        let advisories = authority.set_confirmed_reorg(10);
        assert!(advisories.is_empty());
        assert_eq!(
            authority.lease_of(&sid("a")).expect("live lease").nonce(),
            10
        );
    }

    /// Re-confirming a revived broadcast lands it again, and a second rewind
    /// restores it again: the landed index is the durable evidence.
    #[test]
    fn a_revived_broadcast_relands_and_rewinds_again() {
        let authority = NonceAuthority::new(10);
        let lease = authority.lease(&sid("a")).expect("lease");
        authority.record_broadcast(&lease).expect("broadcast");
        authority.set_confirmed(11);
        let _ = authority.set_confirmed_reorg(10);
        assert_eq!(authority.outstanding_nonces(), vec![10]);

        authority.set_confirmed(11);
        assert!(
            authority.outstanding_nonces().is_empty(),
            "re-confirmation lands the revived broadcast again"
        );
        let _ = authority.set_confirmed_reorg(10);
        assert_eq!(authority.outstanding_nonces(), vec![10]);
    }

    /// One stamp record: the owning strategy, the nonce, the target and wire
    /// hashes, the head the stamp was built at, and the hash the record
    /// supersedes.
    type StampRecord = (StrategyId, u64, B256, B256, u64, Option<B256>);

    /// A lane's ledger fixture: records what the lane wrote, in order.
    #[derive(Debug, Default)]
    struct RecordingLedger {
        signed: Mutex<Vec<StampRecord>>,
        broadcasts: Mutex<Vec<(StrategyId, u64)>>,
    }

    impl NonceLedger for RecordingLedger {
        type Decline = LedgerRefusal;

        fn record_signed(
            &self,
            strategy: &StrategyId,
            nonce: u64,
            target: B256,
            bundle_hash: B256,
            built_at_head: u64,
            replaces: Option<B256>,
        ) -> Result<(), LedgerRefusal> {
            self.signed.lock().push((
                strategy.clone(),
                nonce,
                target,
                bundle_hash,
                built_at_head,
                replaces,
            ));
            Ok(())
        }

        fn record_broadcast(&self, strategy: &StrategyId, nonce: u64) -> Result<(), LedgerRefusal> {
            self.broadcasts.lock().push((strategy.clone(), nonce));
            Ok(())
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct LedgerRefusal;

    impl fmt::Display for LedgerRefusal {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("the fixture ledger refused the write")
        }
    }

    /// One authority, one ledger, two minted lanes: issuance is process-wide,
    /// so the two strategies stamp one contiguous prefix and each stamp record
    /// lands in the lane's own ledger row.
    #[test]
    fn two_strategies_minted_from_one_authority_stamp_a_contiguous_prefix() {
        let authority = Arc::new(NonceAuthority::new(41));
        let ledger = Arc::new(RecordingLedger::default());
        let settlement =
            authority.mint_lane("settlement", Arc::clone(&ledger), LanePolicy::default());
        let backrun = authority.mint_lane(
            "mevblocker_backrun",
            Arc::clone(&ledger),
            LanePolicy::default(),
        );

        assert_eq!(settlement.strategy().as_str(), "settlement");
        assert!(
            Arc::ptr_eq(settlement.authority(), &authority),
            "both lanes bind the one authority they were minted from"
        );
        assert!(
            Arc::ptr_eq(settlement.ledger(), &ledger),
            "both lanes bind the one ledger the composition root supplied"
        );

        let first = settlement.stamp().expect("settlement stamp");
        let second = backrun.stamp().expect("backrun stamp");
        assert_eq!(
            (first.nonce(), second.nonce()),
            (41, 42),
            "one authority hands the two lanes one contiguous prefix"
        );
        assert_eq!(authority.outstanding_nonces(), vec![41, 42]);

        // The stamp record is the lane's: it names its own strategy and nonce.
        settlement
            .record_signed(&first, B256::repeat_byte(1), B256::repeat_byte(2), 7, None)
            .expect("record the settlement stamp");
        assert_eq!(
            *ledger.signed.lock(),
            vec![(
                sid("settlement"),
                41,
                B256::repeat_byte(1),
                B256::repeat_byte(2),
                7,
                None
            )]
        );

        settlement
            .record_broadcast(&first)
            .expect("promote the settlement stamp");
        assert_eq!(*ledger.broadcasts.lock(), vec![(sid("settlement"), 41)]);
    }

    /// A rewind inside a minted lane's window revokes that lane's lease and
    /// advises its owner; the sibling lane's reservations are untouched.
    #[test]
    fn a_rewind_advises_the_minted_lane_whose_lease_it_revoked() {
        let authority = Arc::new(NonceAuthority::new(10));
        let ledger = Arc::new(RecordingLedger::default());
        let settlement =
            authority.mint_lane("settlement", Arc::clone(&ledger), LanePolicy::default());
        let backrun =
            authority.mint_lane("txpool_backrun", Arc::clone(&ledger), LanePolicy::default());

        assert_eq!(backrun.stamp().expect("backrun stamp").nonce(), 10);
        authority.set_confirmed(11);

        let advisories = authority.set_confirmed_reorg(10);
        assert_eq!(advisories.len(), 1);
        assert_eq!(advisories[0].strategy(), backrun.strategy());
        assert_eq!(advisories[0].nonce(), 10);
        assert!(authority.lease_of(backrun.strategy()).is_none());

        // Issuance recovers at the rewound head: the revoked lane re-stamps
        // above the sibling's fresh lease rather than behind it.
        assert_eq!(
            settlement.stamp().expect("settlement stamp").nonce(),
            10,
            "the recovered head is fillable by the lane the rewind never named"
        );
        assert_eq!(backrun.stamp().expect("backrun re-stamp").nonce(), 11);
    }

    #[test]
    fn observe_confirmed_advances_without_rewinding() {
        let authority = NonceAuthority::new(10);
        authority.observe_confirmed(12);
        assert_eq!(authority.confirmed(), 12);
        authority.observe_confirmed(11);
        assert_eq!(
            authority.confirmed(),
            12,
            "a stale chain read never rewinds the confirmed nonce"
        );
    }

    #[test]
    fn a_lane_stamp_repackages_past_a_stale_lease() {
        let authority = Arc::new(NonceAuthority::new(10));
        let lane = authority.mint_lane(
            "a",
            Arc::new(RecordingLedger::default()),
            LanePolicy::default(),
        );
        let lease = lane.stamp().expect("first stamp");
        assert_eq!(lease.nonce(), 10);
        // The chain passes the lease; the next sign repackages at 11.
        authority.set_confirmed(11);
        let restamped = lane.stamp().expect("repackage");
        assert_eq!(restamped.nonce(), 11);
        assert_eq!(
            authority.lease_of(lane.strategy()).expect("lease").nonce(),
            11
        );
    }

    /// The expiry escape hatch at the lane level: releasing a broadcast frees
    /// exactly that nonce and the next stamp refills it, keeping the prefix
    /// contiguous.
    #[test]
    fn a_lane_releases_an_expired_broadcast_and_refills_the_slot() {
        let authority = Arc::new(NonceAuthority::new(50));
        let lane = authority.mint_lane(
            "a",
            Arc::new(RecordingLedger::default()),
            LanePolicy::default(),
        );
        let lease = lane.stamp().expect("stamp");
        authority.record_broadcast(&lease).expect("broadcast");
        assert_eq!(authority.outstanding_nonces(), vec![50]);

        assert_eq!(lane.release_broadcast(50), Ok(50));
        assert_eq!(lane.stamp().expect("refill").nonce(), 50);
    }

    /// The fee caps a hold records: one gwei cap, two gwei tip — small enough
    /// that the 110% replacement floor is easy to clear or miss by hand.
    fn held_fees() -> HeldFees {
        HeldFees::new(30_000_000_000, 2_000_000_000)
    }

    fn holding_lane(authority: &Arc<NonceAuthority>) -> NonceLane<RecordingLedger> {
        authority.mint_lane(
            "a",
            Arc::new(RecordingLedger::default()),
            LanePolicy::default(),
        )
    }

    /// Park `lane`'s lease as a delivery-unknown hold at head 100.
    fn hold_lane(lane: &NonceLane<RecordingLedger>, lease: &NonceLease) {
        lane.record_hold(lease, held_fees(), 100)
            .expect("park the delivery-unknown hold");
    }

    #[test]
    fn a_hold_occupies_its_nonce_in_both_phases() {
        let authority = Arc::new(NonceAuthority::new(10));
        let lane = holding_lane(&authority);
        let lease = lane.stamp().expect("stamp");
        assert_eq!(lease.nonce(), 10);
        hold_lane(&lane, &lease);

        let hold = authority.hold_of(lane.strategy()).expect("hold");
        assert_eq!(hold.nonce(), 10);
        assert_eq!(hold.entered_at_head(), 100);
        assert_eq!(
            hold.expires_at_head(),
            105,
            "the default window is 5 blocks"
        );
        assert_eq!(hold.phase(), HoldPhase::Frozen);
        assert_eq!(
            authority.outstanding_nonces(),
            vec![10],
            "a hold occupies its nonce exactly as a broadcast does"
        );
        assert!(
            authority.lease_of(lane.strategy()).is_none(),
            "the lease became the hold"
        );

        // A peer lane parks the held slot rather than shadowing it.
        assert_eq!(
            authority
                .lease(&sid("b"))
                .expect("the peer stamps above the hold")
                .nonce(),
            11
        );

        // The release does not free the nonce either: it is replaceable, not
        // reusable.
        let released = authority
            .release_hold(lane.strategy(), 10)
            .expect("release the hold");
        assert_eq!(released.phase(), HoldPhase::ReplaceOrDominate);
        assert_eq!(authority.outstanding_nonces(), vec![10, 11]);
    }

    #[test]
    fn a_lane_stamp_declines_on_its_own_delivery_unknown_hold() {
        let authority = Arc::new(NonceAuthority::new(10));
        let lane = holding_lane(&authority);
        let lease = lane.stamp().expect("stamp");
        hold_lane(&lane, &lease);

        assert_eq!(lane.stamp(), Err(DeclineKind::DeliveryUnknownHold));
        assert_eq!(
            authority.outstanding_nonces(),
            vec![10],
            "the decline is not a silent relinquish: the hold still owns the slot"
        );
        assert_eq!(
            authority.hold_of(lane.strategy()).map(|hold| hold.nonce()),
            Some(10)
        );
    }

    #[test]
    fn a_replacement_requires_dominating_fees() {
        let authority = Arc::new(NonceAuthority::new(10));
        let lane = holding_lane(&authority);
        let lease = lane.stamp().expect("stamp");
        hold_lane(&lane, &lease);

        // One wei under the 10% bump on the fee cap: refused.
        let underpriced = HeldFees::new(32_999_999_999, 2_200_000_000);
        assert_eq!(
            lane.replace(underpriced),
            Err(DeclineKind::FeesDoNotDominate)
        );
        // A tip-only bump misses the fee-cap floor: both caps must dominate.
        assert_eq!(
            lane.replace(HeldFees::new(30_000_000_000, 9_000_000_000)),
            Err(DeclineKind::FeesDoNotDominate)
        );
        assert_eq!(
            authority.hold_of(lane.strategy()).map(|hold| hold.nonce()),
            Some(10),
            "a refused replacement leaves the hold in place"
        );

        let dominating = HeldFees::new(33_000_000_000, 2_200_000_000);
        let replacement = lane.replace(dominating).expect("dominating replacement");
        assert_eq!(
            replacement.nonce(),
            10,
            "the replacement re-signs at the held nonce, not above it"
        );
        assert!(authority.hold_of(lane.strategy()).is_none());
        assert_eq!(
            authority.lease_of(lane.strategy()).map(|held| held.nonce()),
            Some(10)
        );
        assert_eq!(authority.outstanding_nonces(), vec![10]);
    }

    #[test]
    fn a_swept_release_hands_the_slot_only_to_a_dominating_replacement() {
        let authority = Arc::new(NonceAuthority::new(10));
        let lane = holding_lane(&authority);
        let lease = lane.stamp().expect("stamp");
        hold_lane(&lane, &lease);
        let _ = authority
            .release_hold(lane.strategy(), 10)
            .expect("release");

        // Ordinary issuance parks the released slot for everyone, the owner
        // included: a sweep release is not a free nonce.
        let peer = authority.lease(&sid("b")).expect("peer stamp");
        assert_eq!(peer.nonce(), 11, "the released slot is not issued for free");
        assert_eq!(lane.stamp(), Err(DeclineKind::DeliveryUnknownHold));

        let re_leased = lane
            .replace(HeldFees::new(33_000_000_000, 2_200_000_000))
            .expect("dominating replacement");
        assert_eq!(re_leased.nonce(), 10);
    }

    #[test]
    fn recording_a_hold_below_the_confirmed_nonce_declines() {
        let authority = Arc::new(NonceAuthority::new(10));
        let lease = authority.lease(&sid("a")).expect("lease");
        authority.set_confirmed(11);
        assert_eq!(
            authority.record_hold(&lease, held_fees(), HoldWindow::DEFAULT, 100),
            Err(DeclineKind::BelowChainNonce),
            "a consumed slot has nothing left to hold"
        );
    }

    #[test]
    fn a_release_of_an_unknown_hold_declines_and_a_repeat_release_is_idempotent() {
        let authority = Arc::new(NonceAuthority::new(10));
        let lane = holding_lane(&authority);
        assert_eq!(
            authority.release_hold(lane.strategy(), 10),
            Err(DeclineKind::UnknownHold)
        );
        let lease = lane.stamp().expect("stamp");
        hold_lane(&lane, &lease);
        assert_eq!(
            authority.release_hold(&sid("b"), 10),
            Err(DeclineKind::UnknownHold),
            "a peer cannot release another lane's hold"
        );
        let first = authority
            .release_hold(lane.strategy(), 10)
            .expect("release");
        let second = authority
            .release_hold(lane.strategy(), 10)
            .expect("a repeated release answers with the current state");
        assert_eq!(first, second);
        assert_eq!(second.phase(), HoldPhase::ReplaceOrDominate);
    }

    #[test]
    fn a_dominating_replacement_needs_a_hold() {
        let authority = Arc::new(NonceAuthority::new(10));
        let lane = holding_lane(&authority);
        assert_eq!(
            lane.replace(HeldFees::new(u128::MAX, u128::MAX)),
            Err(DeclineKind::UnknownHold)
        );
    }

    #[test]
    fn a_tombstone_releases_a_hold() {
        let authority = Arc::new(NonceAuthority::new(10));
        let lane = holding_lane(&authority);
        let lease = lane.stamp().expect("stamp");
        hold_lane(&lane, &lease);
        assert_eq!(authority.release_strategy(lane.strategy()), vec![10]);
        assert!(authority.hold_of(lane.strategy()).is_none());
        assert!(authority.outstanding_nonces().is_empty());
    }

    #[test]
    fn a_rewind_restores_a_swept_hold_into_replace_or_dominate() {
        let authority = Arc::new(NonceAuthority::new(10));
        let lane = holding_lane(&authority);
        let lease = lane.stamp().expect("stamp");
        hold_lane(&lane, &lease);
        let _ = authority
            .release_hold(lane.strategy(), 10)
            .expect("release");
        // The chain consumes the slot: the hold lapses out of the outstanding
        // set, retained so a rewind can restore it.
        authority.set_confirmed(11);
        assert!(authority.outstanding_nonces().is_empty());
        assert!(authority.hold_of(lane.strategy()).is_none());

        let advisories = authority.set_confirmed_reorg(10);
        assert!(advisories.is_empty(), "no lease was inside the window");
        assert_eq!(
            authority.outstanding_nonces(),
            vec![10],
            "the rewind restores the held slot"
        );
        let restored = authority.hold_of(lane.strategy()).expect("restored hold");
        assert_eq!(restored.nonce(), 10);
        assert_eq!(
            restored.phase(),
            HoldPhase::ReplaceOrDominate,
            "a restored hold is replaceable, never freely issuable"
        );
        assert_eq!(
            authority.lease(&sid("b")),
            Ok(NonceLease {
                strategy: sid("b"),
                nonce: 11,
            }),
            "the restored slot is not handed out for free"
        );
    }

    #[test]
    fn a_lane_window_overrides_the_default_and_sets_the_expiry() {
        let authority = Arc::new(NonceAuthority::new(10));
        let patient = authority.mint_lane(
            "patient",
            Arc::new(RecordingLedger::default()),
            LanePolicy::new(HoldWindow::new(1), "patient"),
        );
        let cautious = authority.mint_lane(
            "cautious",
            Arc::new(RecordingLedger::default()),
            LanePolicy::new(HoldWindow::new(9), "cautious"),
        );
        assert_eq!(patient.hold_window().blocks(), 1);
        assert_eq!(cautious.hold_window().blocks(), 9);
        assert_eq!(patient.label(), "patient");

        let lease = patient.stamp().expect("stamp");
        patient
            .record_hold(&lease, held_fees(), 100)
            .expect("hold under the short window");
        let hold = authority.hold_of(patient.strategy()).expect("hold");
        assert_eq!(
            hold.expires_at_head(),
            101,
            "the mint's window, not the default, sets the expiry"
        );
    }

    #[test]
    fn the_replacement_floor_is_the_standard_ten_percent_bump() {
        // 110% of the held cap, rounded down as the clients do.
        assert!(HeldFees::new(1_100, 1_100).dominates(HeldFees::new(1_000, 1_000)));
        assert!(!HeldFees::new(1_099, 1_100).dominates(HeldFees::new(1_000, 1_000)));
        assert!(!HeldFees::new(1_100, 1_099).dominates(HeldFees::new(1_000, 1_000)));
        // A zero cap has no floor to clear, so any tip dominates it.
        assert!(HeldFees::new(0, 0).dominates(HeldFees::default()));
    }

    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    enum Op {
        Lease(usize),
        Broadcast(usize),
        Release(usize),
        SetConfirmed(u64),
    }

    fn strategy(index: usize) -> StrategyId {
        StrategyId::new(format!("s{index}"))
    }

    /// The lowest nonce at or above `confirmed` absent from the public
    /// `outstanding` snapshot, by a plain scan. This is the independent
    /// oracle: it shares no code with the authority's private ledger scan.
    fn brute_lowest_free(confirmed: u64, outstanding: &[u64]) -> Option<u64> {
        let mut nonce = confirmed;
        loop {
            if !outstanding.contains(&nonce) {
                return Some(nonce);
            }
            nonce = nonce.checked_add(1)?;
        }
    }

    fn op_sequence() -> impl Strategy<Value = Vec<Op>> {
        let op = prop_oneof![
            (0usize..4).prop_map(Op::Lease),
            (0usize..4).prop_map(Op::Broadcast),
            (0usize..4).prop_map(Op::Release),
            (0u64..24).prop_map(Op::SetConfirmed),
        ];
        proptest::collection::vec(op, 0..64)
    }

    /// The ledger mirror the property checks against: same sets, maintained by
    /// the test rather than the authority.
    #[derive(Default)]
    struct Mirror {
        confirmed: u64,
        leases: BTreeMap<usize, u64>,
        broadcasts: BTreeMap<u64, usize>,
    }

    impl Mirror {
        fn stale_reservation(&self) -> bool {
            self.leases.values().any(|held| *held < self.confirmed)
                || self.broadcasts.keys().any(|held| *held < self.confirmed)
        }

        fn outstanding(&self) -> Vec<u64> {
            let mut nonces: Vec<u64> = self.broadcasts.keys().copied().collect();
            nonces.extend(self.leases.values().copied());
            nonces.sort_unstable();
            nonces
        }
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(256))]

        #[test]
        fn lease_is_always_the_lowest_non_outstanding_nonce_above_confirmed(
            ops in op_sequence(),
        ) {
            let authority = NonceAuthority::new(0);
            let mut mirror = Mirror::default();
            // Lease handles still available to broadcast, keyed by strategy.
            let mut handles: BTreeMap<usize, NonceLease> = BTreeMap::new();

            for op in ops {
                match op {
                    Op::Lease(index) => {
                        let before_confirmed = authority.confirmed();
                        let before_outstanding = authority.outstanding_nonces();
                        let result = authority.lease(&strategy(index));
                        if mirror.leases.contains_key(&index) {
                            prop_assert_eq!(result, Err(DeclineKind::StrategyLeaseOutstanding));
                        } else if mirror.stale_reservation() {
                            prop_assert_eq!(result, Err(DeclineKind::BelowChainNonce));
                        } else {
                            let lease = result.expect("free nonce must be issued");
                            prop_assert_eq!(
                                lease.nonce(),
                                brute_lowest_free(before_confirmed, &before_outstanding).unwrap()
                            );
                            prop_assert_eq!(lease.strategy(), &strategy(index));
                            mirror.leases.insert(index, lease.nonce());
                            handles.insert(index, lease);
                        }
                    }
                    Op::Broadcast(index) => {
                        let Some(lease) = handles.remove(&index) else {
                            continue;
                        };
                        prop_assert!(mirror.leases.contains_key(&index));
                        let result = authority.record_broadcast(&lease);
                        if lease.nonce() < mirror.confirmed {
                            prop_assert_eq!(result, Err(DeclineKind::BelowChainNonce));
                            handles.insert(index, lease);
                        } else {
                            result.expect("current lease must broadcast");
                            mirror.leases.remove(&index);
                            mirror.broadcasts.insert(lease.nonce(), index);
                        }
                    }
                    Op::Release(index) => {
                        handles.remove(&index);
                        let freed = authority.release_strategy(&strategy(index));
                        let mut expected: Vec<u64> = Vec::new();
                        if let Some(nonce) = mirror.leases.remove(&index) {
                            expected.push(nonce);
                        }
                        mirror.broadcasts.retain(|nonce, owner| {
                            if *owner == index {
                                expected.push(*nonce);
                                false
                            } else {
                                true
                            }
                        });
                        expected.sort_unstable();
                        prop_assert_eq!(freed, expected);
                    }
                    Op::SetConfirmed(value) => {
                        authority.set_confirmed(value);
                        mirror.confirmed = value;
                        mirror.broadcasts.retain(|nonce, _| *nonce >= value);
                    }
                }

                prop_assert_eq!(authority.confirmed(), mirror.confirmed);
                prop_assert_eq!(authority.outstanding_nonces(), mirror.outstanding());
                for index in 0..4 {
                    let live = authority.lease_of(&strategy(index)).map(|lease| lease.nonce());
                    prop_assert_eq!(live, mirror.leases.get(&index).copied());
                    // A live lease names its own strategy.
                    if let Some(lease) = authority.lease_of(&strategy(index)) {
                        prop_assert_eq!(lease.strategy(), &strategy(index));
                    }
                }
            }
        }
    }
}
