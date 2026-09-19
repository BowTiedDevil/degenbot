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
//! Two disjoint sets make up the outstanding ledger: at most one **lease**
//! per strategy (the reservation a strategy signs against), and the
//! **broadcasts** a lease becomes once its transaction is on the wire.
//! Advancing the confirmed chain nonce reconciles landed broadcasts out of the
//! tracked set; releasing a strategy (a skip or a tombstone) removes both its
//! lease and every broadcast it still owns, so a successor's stamp self-heals
//! the chain.
//!
//! The authority never panics on caller input: every state contradiction is a
//! typed [`DeclineKind`], and the shared ledger lock is non-poisoning
//! (`parking_lot`) so one strategy's failure cannot wedge another's sign path.

use std::collections::BTreeMap;
use std::fmt;

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
    /// No nonce remains at or above the confirmed chain nonce.
    Exhausted,
}

impl fmt::Display for DeclineKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::StrategyLeaseOutstanding => "strategy already holds an outstanding lease",
            Self::BelowChainNonce => "a tracked reservation is below the confirmed chain nonce",
            Self::UnknownLease => "lease is not outstanding for the named strategy",
            Self::Exhausted => "no nonce remains at or above the confirmed chain nonce",
        };
        f.write_str(text)
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
}

impl NonceState {
    /// Move every broadcast strictly below `confirmed` out of the outstanding
    /// set and into the landed index. Shared by the monotonic and reorg head
    /// paths so they cannot drift.
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
    }

    fn is_outstanding(&self, nonce: u64) -> bool {
        self.broadcasts.contains_key(&nonce) || self.leases.values().any(|held| *held == nonce)
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
    /// lease or broadcast holds.
    ///
    /// # Errors
    ///
    /// [`DeclineKind::StrategyLeaseOutstanding`] if `strategy` already holds a
    /// lease; [`DeclineKind::BelowChainNonce`] if a tracked reservation sits
    /// below the confirmed chain nonce (reconcile before signing).
    pub fn lease(&self, strategy: &StrategyId) -> Result<NonceLease, DeclineKind> {
        let mut state = self.state.lock();
        if state.leases.contains_key(strategy) {
            return Err(DeclineKind::StrategyLeaseOutstanding);
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
        freed.sort_unstable();
        freed
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
        !state.leases.is_empty() || !state.broadcasts.is_empty()
    }

    /// Every outstanding nonce — leases and broadcasts — ascending, with no
    /// duplicates.
    #[must_use]
    pub fn outstanding_nonces(&self) -> Vec<u64> {
        let state = self.state.lock();
        let mut nonces: Vec<u64> = state.broadcasts.keys().copied().collect();
        nonces.extend(state.leases.values().copied());
        nonces.sort_unstable();
        nonces
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
