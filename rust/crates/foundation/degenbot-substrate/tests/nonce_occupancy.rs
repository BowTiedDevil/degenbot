//! Occupancy scenarios at the sign-time nonce authority's public seam: one
//! scenario at a time through `NonceAuthority` / `NonceLane` observations —
//! exclusivity, the typed declines, the replacement gate, and the retained
//! confirmation forms. The total state/event matrix lives beside this file.

#![expect(
    clippy::expect_used,
    reason = "fixtures and scenario expectations fail loudly on a malformed setup"
)]

use std::fmt;
use std::sync::Arc;

use alloy::primitives::B256;
use degenbot_substrate::nonce::{
    DeclineKind, HeldFees, HoldPhase, HoldWindow, LanePolicy, NonceAuthority, NonceLane,
    NonceLedger, NonceOccupant, StrategyId,
};

fn sid(name: &str) -> StrategyId {
    StrategyId::new(name)
}

fn held_fees() -> HeldFees {
    HeldFees::new(30_000_000_000, 2_000_000_000)
}

fn dominating_fees() -> HeldFees {
    HeldFees::new(33_000_000_000, 2_200_000_000)
}

fn underpriced_fees() -> HeldFees {
    HeldFees::new(32_999_999_999, 2_200_000_000)
}

/// A lane's ledger fixture: accepts every write.
#[derive(Debug)]
struct NullLedger;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LedgerRefusal;

impl fmt::Display for LedgerRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the fixture ledger refused the write")
    }
}

impl NonceLedger for NullLedger {
    type Decline = LedgerRefusal;

    fn record_signed(
        &self,
        _strategy: &StrategyId,
        _nonce: u64,
        _target: B256,
        _bundle_hash: B256,
        _built_at_head: u64,
        _replaces: Option<B256>,
    ) -> Result<(), Self::Decline> {
        Ok(())
    }

    fn record_broadcast(&self, _strategy: &StrategyId, _nonce: u64) -> Result<(), Self::Decline> {
        Ok(())
    }
}

/// The retained lapsed-hold payload of an occupant, when that is its form.
fn lapsed_hold(occupant: Option<NonceOccupant>) -> Option<degenbot_substrate::nonce::DeliveryHold> {
    match occupant {
        Some(NonceOccupant::LapsedHold(hold)) => Some(hold),
        _ => None,
    }
}

fn lane(authority: &Arc<NonceAuthority>, name: &'static str) -> NonceLane<NullLedger> {
    authority.mint_lane(
        name,
        Arc::new(NullLedger),
        LanePolicy::new(HoldWindow::DEFAULT, name),
    )
}

/// A nonce carries at most one occupant: the lease a strategy stamps against
/// becomes the broadcast or hold its bytes occupy, never a second booking of
/// the same slot.
#[test]
fn a_slot_carries_exactly_one_occupant_across_lease_broadcast_and_hold() {
    let authority = NonceAuthority::new(0);
    let lease = authority.lease(&sid("a")).expect("lease");
    assert_eq!(
        authority.occupant(0),
        Some(NonceOccupant::Lease { strategy: sid("a") }),
        "a lease is the slot's one occupant"
    );

    authority.record_broadcast(&lease).expect("broadcast");
    assert_eq!(
        authority.occupant(0),
        Some(NonceOccupant::Broadcast { strategy: sid("a") }),
        "the broadcast occupant takes the slot the lease occupant left"
    );
    assert_eq!(
        authority.lease_of(&sid("a")),
        None,
        "the lease occupant is gone: one slot, one occupant"
    );

    let b_lease = authority.lease(&sid("b")).expect("b's lease");
    let hold = authority
        .record_hold(&b_lease, held_fees(), HoldWindow::DEFAULT, 100)
        .expect("hold");
    assert_eq!(
        authority.occupant(1),
        Some(NonceOccupant::DeliveryHold(hold.clone())),
        "the hold occupant takes the slot the lease occupant left"
    );
    assert_eq!(
        authority.occupancy(),
        vec![
            (0, NonceOccupant::Broadcast { strategy: sid("a") }),
            (1, NonceOccupant::DeliveryHold(hold)),
        ]
    );
    assert_eq!(authority.occupant(2), None, "absence means free");
}

/// The typed declines that keep dual occupancy unconstructible: a second
/// lease, and holds or broadcasts claimed over an occupant that is not the
/// caller's live lease.
#[test]
fn a_second_lease_and_a_hold_over_outstanding_occupancy_decline() {
    let authority = NonceAuthority::new(0);
    let live = authority.lease(&sid("c")).expect("live lease");
    assert_eq!(
        authority.lease(&sid("c")),
        Err(DeclineKind::StrategyLeaseOutstanding),
        "at most one outstanding lease per strategy"
    );

    let held = authority.lease(&sid("b")).expect("b's lease");
    authority
        .record_hold(&held, held_fees(), HoldWindow::DEFAULT, 100)
        .expect("hold");
    assert_eq!(
        authority.lease(&sid("b")),
        Err(DeclineKind::DeliveryUnknownHold),
        "a hold's owner cannot stamp over its own possibly-live bytes"
    );
    assert_eq!(
        authority.record_hold(&held, held_fees(), HoldWindow::DEFAULT, 100),
        Err(DeclineKind::UnknownLease),
        "the consumed handle names no live lease to hold"
    );
    assert_eq!(
        authority.record_broadcast(&held),
        Err(DeclineKind::UnknownLease),
        "the consumed handle names no live lease to broadcast"
    );

    assert_eq!(
        authority.occupancy(),
        vec![
            (0, NonceOccupant::Lease { strategy: sid("c") }),
            (
                1,
                NonceOccupant::DeliveryHold(authority.hold_of(&sid("b")).expect("hold"))
            ),
        ],
        "every refused call left the occupancy untouched"
    );
    assert_eq!(authority.lease_of(&sid("c")), Some(live));
}

/// The broadcast and hold entries consume their lease occupant in the same
/// transition that installs the new one, and the strategy may stamp again
/// above it.
#[test]
fn broadcast_and_hold_entry_consume_the_lease_occupant() {
    let authority = NonceAuthority::new(0);
    let a = authority.lease(&sid("a")).expect("a");
    authority.record_broadcast(&a).expect("broadcast");
    assert_eq!(
        authority.lease_of(&sid("a")),
        None,
        "the broadcast entry consumed the lease occupant"
    );
    assert_eq!(authority.outstanding_nonces(), vec![0]);

    let b = authority.lease(&sid("b")).expect("b");
    let hold = authority
        .record_hold(&b, held_fees(), HoldWindow::DEFAULT, 100)
        .expect("hold");
    assert_eq!(
        authority.lease_of(&sid("b")),
        None,
        "the hold entry consumed the lease occupant"
    );
    assert!(
        authority.outstanding_leases().is_empty(),
        "both lease occupants are gone"
    );
    assert_eq!(authority.outstanding_nonces(), vec![0, 1]);
    assert_eq!(
        authority.occupant(1),
        Some(NonceOccupant::DeliveryHold(hold))
    );
}

/// Ordinary issuance parks a held slot in both phases: the bytes at a frozen
/// or replace-or-dominate slot may be live, so only the owning lane's explicit
/// replacement may reuse it.
#[test]
fn ordinary_issuance_never_reuses_a_frozen_or_replace_or_dominate_slot() {
    let authority = Arc::new(NonceAuthority::new(0));
    let owner = lane(&authority, "a");
    let lease = owner.stamp().expect("stamp");
    let hold = owner.record_hold(&lease, held_fees(), 100).expect("hold");
    assert_eq!(hold.phase(), HoldPhase::Frozen);

    assert_eq!(
        authority.lease(&sid("b")).expect("peer").nonce(),
        1,
        "the frozen slot is parked"
    );
    assert_eq!(
        owner.stamp(),
        Err(DeclineKind::DeliveryUnknownHold),
        "the owner cannot stamp over its own possibly-live bytes"
    );

    let released = owner.release_hold(0).expect("sweep release");
    assert_eq!(released.phase(), HoldPhase::ReplaceOrDominate);
    assert_eq!(
        authority.lease(&sid("c")).expect("peer").nonce(),
        2,
        "the replace-or-dominate slot is still parked"
    );
    assert_eq!(owner.stamp(), Err(DeclineKind::DeliveryUnknownHold));
    assert_eq!(
        authority.occupant(0),
        Some(NonceOccupant::DeliveryHold(released)),
        "both phases keep the occupant"
    );
}

/// Only a dominating replacement may reuse the replace-or-dominate slot: it
/// re-signs at the held nonce, never above it.
#[test]
fn only_a_dominating_replacement_reuses_the_replace_or_dominate_slot() {
    let authority = Arc::new(NonceAuthority::new(0));
    let owner = lane(&authority, "a");
    let lease = owner.stamp().expect("stamp");
    owner.record_hold(&lease, held_fees(), 100).expect("hold");
    owner.release_hold(0).expect("sweep release");

    let replacement = owner
        .replace(dominating_fees())
        .expect("dominating replacement");
    assert_eq!(
        replacement.nonce(),
        0,
        "the replacement re-signs at the held nonce"
    );
    assert_eq!(
        authority.occupant(0),
        Some(NonceOccupant::Lease { strategy: sid("a") })
    );
}

/// An underpriced replacement is refused with the typed decline and changes
/// nothing: the held occupant stays exactly where it was.
#[test]
fn an_underpriced_replacement_is_refused_without_changing_state() {
    let authority = Arc::new(NonceAuthority::new(0));
    let owner = lane(&authority, "a");
    let lease = owner.stamp().expect("stamp");
    owner.record_hold(&lease, held_fees(), 100).expect("hold");
    owner.release_hold(0).expect("sweep release");

    let before = authority.occupancy();
    assert_eq!(
        owner.replace(underpriced_fees()),
        Err(DeclineKind::FeesDoNotDominate)
    );
    assert_eq!(
        authority.occupancy(),
        before,
        "a refused replacement leaves the occupant untouched"
    );
}

/// Confirmation moves broadcast and hold occupants into their retained forms:
/// history leaves the outstanding set but keeps its slot, so a rewind has
/// something to restore.
#[test]
fn confirmation_moves_occupants_into_their_retained_forms() {
    let authority = NonceAuthority::new(0);
    let a = authority.lease(&sid("a")).expect("a");
    authority.record_broadcast(&a).expect("broadcast");
    let b = authority.lease(&sid("b")).expect("b");
    authority
        .record_hold(&b, held_fees(), HoldWindow::DEFAULT, 100)
        .expect("hold");

    authority.set_confirmed(2);
    let lapsed = lapsed_hold(authority.occupant(1)).expect("the lapsed hold is retained");
    assert_eq!(
        lapsed.nonce(),
        1,
        "the lapsed hold keeps the slot its bytes were signed against"
    );
    assert_eq!(lapsed.strategy(), &sid("b"));
    assert_eq!(lapsed.fees(), held_fees());
    assert_eq!(
        lapsed.phase(),
        HoldPhase::ReplaceOrDominate,
        "a hold the chain passed is over: it can only ever be replaced"
    );
    assert_eq!(
        authority.occupancy(),
        vec![
            (0, NonceOccupant::Landed { strategy: sid("a") }),
            (1, NonceOccupant::LapsedHold(lapsed)),
        ],
        "the retained forms keep their slots"
    );
    assert_eq!(
        authority.hold_of(&sid("b")),
        None,
        "a lapsed hold is history, not an open hold"
    );
    assert!(
        !authority.has_outstanding(),
        "retained forms leave the outstanding set"
    );
}

/// The retained land forms occupy their nonce: a landed broadcast and a lapsed
/// hold are retained history, not free slots, so issuance never hands them out
/// again — not even when the confirmed clock is fed a stale value and moves
/// backward over them. Only reorg revival may return a retained slot to the
/// wire.
#[test]
fn a_retained_land_form_never_yields_its_slot_to_issuance() {
    let authority = NonceAuthority::new(0);
    let lease = authority.lease(&sid("a")).expect("lease");
    authority.record_broadcast(&lease).expect("broadcast");
    authority.set_confirmed(1);
    assert!(authority.outstanding_nonces().is_empty());

    // A stale chain read feeds the monotonic path a lower value; the
    // authority must keep the landed occupant rather than hand its slot out.
    authority.set_confirmed(0);
    let reissued = authority.lease(&sid("b")).expect("issuance resumes");
    assert_eq!(
        reissued.nonce(),
        1,
        "the landed broadcast is retained occupancy, never re-issued"
    );

    let authority = NonceAuthority::new(0);
    let lease = authority.lease(&sid("a")).expect("lease");
    authority
        .record_hold(&lease, HeldFees::new(1, 1), HoldWindow::DEFAULT, 100)
        .expect("hold");
    authority.set_confirmed(1);
    authority.set_confirmed(0);
    let reissued = authority.lease(&sid("b")).expect("issuance resumes");
    assert_eq!(
        reissued.nonce(),
        1,
        "the lapsed hold is retained occupancy, never re-issued"
    );
}

/// A rewind restores exactly the rewound range — the landed forms inside
/// [recovered, old head) return to the wire, everything below the recovered
/// head stays retained — and a repeated same-head reconciliation changes
/// nothing.
#[test]
fn a_reorg_restores_exactly_the_rewound_range_and_repeats_idempotently() {
    let authority = NonceAuthority::new(0);
    for owner in ["a", "b"] {
        let lease = authority.lease(&sid(owner)).expect("lease");
        authority.record_broadcast(&lease).expect("broadcast");
    }
    authority.set_confirmed(2);
    assert_eq!(
        authority.occupancy(),
        vec![
            (0, NonceOccupant::Landed { strategy: sid("a") }),
            (1, NonceOccupant::Landed { strategy: sid("b") }),
        ]
    );

    let advisories = authority.set_confirmed_reorg(1);
    assert!(advisories.is_empty(), "a revival is no advisory");
    assert_eq!(
        authority.occupancy(),
        vec![
            (0, NonceOccupant::Landed { strategy: sid("a") }),
            (1, NonceOccupant::Broadcast { strategy: sid("b") }),
        ],
        "the rewind restores exactly the rewound range [1, 2)"
    );
    assert_eq!(authority.outstanding_nonces(), vec![1]);

    let again = authority.set_confirmed_reorg(1);
    assert!(again.is_empty());
    assert_eq!(
        authority.occupancy(),
        vec![
            (0, NonceOccupant::Landed { strategy: sid("a") }),
            (1, NonceOccupant::Broadcast { strategy: sid("b") }),
        ],
        "a repeated same-head reconciliation is idempotent"
    );
}

/// A lease inside the rewound range is freed with exactly one advisory, while
/// a stale reservation below the confirmed nonce keeps declining issuance
/// rather than being silently reused.
#[test]
fn a_rewound_lease_is_freed_with_one_advisory_and_stale_reservations_still_decline() {
    let authority = NonceAuthority::new(0);
    let lease = authority.lease(&sid("a")).expect("lease");
    authority.set_confirmed(1);

    assert_eq!(
        authority.occupant(0),
        Some(NonceOccupant::Lease { strategy: sid("a") }),
        "a stale reservation still occupies its slot"
    );
    assert_eq!(
        authority.lease(&sid("b")),
        Err(DeclineKind::BelowChainNonce),
        "a stale reservation declines issuance rather than being reused"
    );

    let advisories = authority.set_confirmed_reorg(0);
    assert_eq!(advisories.len(), 1);
    assert_eq!(advisories[0].strategy(), &sid("a"));
    assert_eq!(advisories[0].nonce(), 0);
    assert_eq!(
        authority.occupancy(),
        Vec::<(u64, NonceOccupant)>::new(),
        "the rewound lease is free again"
    );
    assert_eq!(
        authority.record_broadcast(&lease),
        Err(DeclineKind::UnknownLease),
        "the revoked handle names no outstanding reservation"
    );
    assert_eq!(authority.lease(&sid("b")).expect("re-issue").nonce(), 0);
}

/// The lane seam walks the same occupancy: a stamp takes the slot, the hold
/// entry takes it over, and the lane's replacement re-signs at it.
#[test]
fn a_lane_walks_stamp_hold_and_replace_through_the_occupancy_map() {
    let authority = Arc::new(NonceAuthority::new(0));
    let owner = lane(&authority, "a");
    let lease = owner.stamp().expect("stamp");
    assert_eq!(
        authority.occupant(0),
        Some(NonceOccupant::Lease { strategy: sid("a") })
    );

    let hold = owner.record_hold(&lease, held_fees(), 100).expect("hold");
    assert_eq!(owner.hold().map(|h| h.nonce()), Some(0));
    assert_eq!(
        authority.occupant(0),
        Some(NonceOccupant::DeliveryHold(hold))
    );

    let replacement = owner
        .replace(dominating_fees())
        .expect("dominating replacement");
    assert_eq!(replacement.nonce(), 0);
    assert_eq!(
        owner.authority().occupant(0),
        Some(NonceOccupant::Lease { strategy: sid("a") }),
        "the replacement lane holds the slot again"
    );
}
