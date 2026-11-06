//! The total state/event matrix for the nonce authority's occupancy model:
//! every occupancy variant against every authority event, asserting the exact
//! next public observation or the typed decline.
//!
//! The expectations below are scenario data written out per pair — an
//! independent account of what each situation must do, not a mirror of the
//! production transition code. The runner only executes the event and
//! compares the observation.

#![expect(
    clippy::expect_used,
    reason = "fixtures and scenario expectations fail loudly on a malformed setup"
)]

use degenbot_substrate::nonce::{
    DeclineKind, HeldFees, HoldPhase, HoldWindow, NonceAuthority, NonceLease, NonceOccupant,
    StrategyId,
};

/// The strategy every matrix row names as the slot's owner.
const OWNER: &str = "a";

/// The slot every matrix row opens; every event names it.
const SLOT: u64 = 0;

fn sid(name: &str) -> StrategyId {
    StrategyId::new(name)
}

fn held_fees() -> HeldFees {
    HeldFees::new(30_000_000_000, 2_000_000_000)
}

fn dominating_fees() -> HeldFees {
    HeldFees::new(33_000_000_000, 2_200_000_000)
}

/// The situation every row opens from: the slot free, or one occupant at
/// [`SLOT`] under the head context that form implies. A retained form sits
/// below the confirmed nonce; so does a stale lease, the one live reservation
/// confirmation deliberately leaves behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    Free,
    Lease,
    StaleLease,
    Broadcast,
    HoldFrozen,
    HoldRoD,
    Landed,
    LapsedHold,
}

const OPENS: [Open; 8] = [
    Open::Free,
    Open::Lease,
    Open::StaleLease,
    Open::Broadcast,
    Open::HoldFrozen,
    Open::HoldRoD,
    Open::Landed,
    Open::LapsedHold,
];

/// One event on the authority's closed transition surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Lease,
    Broadcast,
    Hold,
    ExpireHold,
    Replace,
    ReleaseLease,
    ReleaseBroadcast,
    ReleaseStrategy,
    AdvanceHead,
    ReorgHead,
}

const EVENTS: [Event; 10] = [
    Event::Lease,
    Event::Broadcast,
    Event::Hold,
    Event::ExpireHold,
    Event::Replace,
    Event::ReleaseLease,
    Event::ReleaseBroadcast,
    Event::ReleaseStrategy,
    Event::AdvanceHead,
    Event::ReorgHead,
];

/// The exact answer an event gives: its typed result or its typed decline.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    /// `Ok(NonceLease)` at this nonce.
    Issued(u64),
    /// `Ok(DeliveryHold)` at this nonce in this window phase.
    Held(u64, HoldPhase),
    /// `Ok(u64)`: the freed nonce.
    Freed(u64),
    /// `Vec<u64>`: the freed nonces, ascending.
    FreedAll(&'static [u64]),
    /// `Ok(())` or the unit-returning head advance.
    Done,
    /// The typed decline.
    Declined(DeclineKind),
    /// The reorg advisories, in strategy-name order.
    Advised(&'static [(&'static str, u64)]),
}

impl Verdict {
    /// The observed-side shape this expectation asserts against.
    fn observe(&self) -> Observed {
        match self {
            Self::Issued(nonce) => Observed::Issued(*nonce),
            Self::Held(nonce, phase) => Observed::Held(*nonce, *phase),
            Self::Freed(nonce) => Observed::Freed(*nonce),
            Self::FreedAll(nonces) => Observed::FreedAll(nonces.to_vec()),
            Self::Done => Observed::Done,
            Self::Declined(decline) => Observed::Declined(*decline),
            Self::Advised(advisories) => Observed::Advised(
                advisories
                    .iter()
                    .map(|(strategy, nonce)| ((*strategy).to_string(), *nonce))
                    .collect(),
            ),
        }
    }
}

/// What a run produced, in the comparable shape of [`Verdict`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum Observed {
    Issued(u64),
    Held(u64, HoldPhase),
    Freed(u64),
    FreedAll(Vec<u64>),
    Done,
    Declined(DeclineKind),
    Advised(Vec<(String, u64)>),
}

/// The occupancy vocabulary, compressed to one tag per form for whole-slot
/// snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Lease,
    Broadcast,
    HoldFrozen,
    HoldRoD,
    Landed,
    LapsedHold,
}

impl Tag {
    /// Whether the tag is an open reservation rather than retained history.
    const fn is_live(self) -> bool {
        matches!(
            self,
            Self::Lease | Self::Broadcast | Self::HoldFrozen | Self::HoldRoD
        )
    }
}

fn tag(occupant: &NonceOccupant) -> Tag {
    match occupant {
        NonceOccupant::Lease { .. } => Tag::Lease,
        NonceOccupant::Broadcast { .. } => Tag::Broadcast,
        NonceOccupant::DeliveryHold(hold) if hold.phase() == HoldPhase::Frozen => Tag::HoldFrozen,
        NonceOccupant::DeliveryHold(_) => Tag::HoldRoD,
        NonceOccupant::Landed { .. } => Tag::Landed,
        NonceOccupant::LapsedHold(_) => Tag::LapsedHold,
    }
}

/// One scenario expectation: the opening, the event, and the exact answer —
/// the event's verdict, the confirmed nonce after, and the occupancy after.
struct Cell {
    open: Open,
    event: Event,
    verdict: Verdict,
    confirmed: u64,
    then: &'static [(u64, Tag)],
}

const CELLS: &[Cell] = &[
    // Free: the fixture leased and released its slot, so the owner still
    // holds the stale handle the handle-shaped events replay.
    Cell {
        open: Open::Free,
        event: Event::Lease,
        verdict: Verdict::Issued(0),
        confirmed: 0,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::Free,
        event: Event::Broadcast,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 0,
        then: &[],
    },
    Cell {
        open: Open::Free,
        event: Event::Hold,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 0,
        then: &[],
    },
    Cell {
        open: Open::Free,
        event: Event::ExpireHold,
        verdict: Verdict::Declined(DeclineKind::UnknownHold),
        confirmed: 0,
        then: &[],
    },
    Cell {
        open: Open::Free,
        event: Event::Replace,
        verdict: Verdict::Declined(DeclineKind::UnknownHold),
        confirmed: 0,
        then: &[],
    },
    Cell {
        open: Open::Free,
        event: Event::ReleaseLease,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 0,
        then: &[],
    },
    Cell {
        open: Open::Free,
        event: Event::ReleaseBroadcast,
        verdict: Verdict::Declined(DeclineKind::UnknownBroadcast),
        confirmed: 0,
        then: &[],
    },
    Cell {
        open: Open::Free,
        event: Event::ReleaseStrategy,
        verdict: Verdict::FreedAll(&[]),
        confirmed: 0,
        then: &[],
    },
    Cell {
        open: Open::Free,
        event: Event::AdvanceHead,
        verdict: Verdict::Done,
        confirmed: 1,
        then: &[],
    },
    Cell {
        open: Open::Free,
        event: Event::ReorgHead,
        verdict: Verdict::Advised(&[]),
        confirmed: 0,
        then: &[],
    },
    // Lease: the owner's live reservation at the slot.
    Cell {
        open: Open::Lease,
        event: Event::Lease,
        verdict: Verdict::Declined(DeclineKind::StrategyLeaseOutstanding),
        confirmed: 0,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::Lease,
        event: Event::Broadcast,
        verdict: Verdict::Done,
        confirmed: 0,
        then: &[(0, Tag::Broadcast)],
    },
    Cell {
        open: Open::Lease,
        event: Event::Hold,
        verdict: Verdict::Held(0, HoldPhase::Frozen),
        confirmed: 0,
        then: &[(0, Tag::HoldFrozen)],
    },
    Cell {
        open: Open::Lease,
        event: Event::ExpireHold,
        verdict: Verdict::Declined(DeclineKind::UnknownHold),
        confirmed: 0,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::Lease,
        event: Event::Replace,
        verdict: Verdict::Declined(DeclineKind::UnknownHold),
        confirmed: 0,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::Lease,
        event: Event::ReleaseLease,
        verdict: Verdict::Freed(0),
        confirmed: 0,
        then: &[],
    },
    Cell {
        open: Open::Lease,
        event: Event::ReleaseBroadcast,
        verdict: Verdict::Declined(DeclineKind::UnknownBroadcast),
        confirmed: 0,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::Lease,
        event: Event::ReleaseStrategy,
        verdict: Verdict::FreedAll(&[0]),
        confirmed: 0,
        then: &[],
    },
    Cell {
        open: Open::Lease,
        event: Event::AdvanceHead,
        verdict: Verdict::Done,
        confirmed: 1,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::Lease,
        event: Event::ReorgHead,
        verdict: Verdict::Advised(&[]),
        confirmed: 0,
        then: &[(0, Tag::Lease)],
    },
    // StaleLease: the reservation confirmation deliberately left behind,
    // below the confirmed nonce.
    Cell {
        open: Open::StaleLease,
        event: Event::Lease,
        verdict: Verdict::Declined(DeclineKind::StrategyLeaseOutstanding),
        confirmed: 1,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::StaleLease,
        event: Event::Broadcast,
        verdict: Verdict::Declined(DeclineKind::BelowChainNonce),
        confirmed: 1,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::StaleLease,
        event: Event::Hold,
        verdict: Verdict::Declined(DeclineKind::BelowChainNonce),
        confirmed: 1,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::StaleLease,
        event: Event::ExpireHold,
        verdict: Verdict::Declined(DeclineKind::UnknownHold),
        confirmed: 1,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::StaleLease,
        event: Event::Replace,
        verdict: Verdict::Declined(DeclineKind::UnknownHold),
        confirmed: 1,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::StaleLease,
        event: Event::ReleaseLease,
        verdict: Verdict::Freed(0),
        confirmed: 1,
        then: &[],
    },
    Cell {
        open: Open::StaleLease,
        event: Event::ReleaseBroadcast,
        verdict: Verdict::Declined(DeclineKind::UnknownBroadcast),
        confirmed: 1,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::StaleLease,
        event: Event::ReleaseStrategy,
        verdict: Verdict::FreedAll(&[0]),
        confirmed: 1,
        then: &[],
    },
    Cell {
        open: Open::StaleLease,
        event: Event::AdvanceHead,
        verdict: Verdict::Done,
        confirmed: 2,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::StaleLease,
        event: Event::ReorgHead,
        verdict: Verdict::Advised(&[("a", 0)]),
        confirmed: 0,
        then: &[],
    },
    // Broadcast: the bytes are on the wire at the slot.
    Cell {
        open: Open::Broadcast,
        event: Event::Lease,
        verdict: Verdict::Issued(1),
        confirmed: 0,
        then: &[(0, Tag::Broadcast), (1, Tag::Lease)],
    },
    Cell {
        open: Open::Broadcast,
        event: Event::Broadcast,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 0,
        then: &[(0, Tag::Broadcast)],
    },
    Cell {
        open: Open::Broadcast,
        event: Event::Hold,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 0,
        then: &[(0, Tag::Broadcast)],
    },
    Cell {
        open: Open::Broadcast,
        event: Event::ExpireHold,
        verdict: Verdict::Declined(DeclineKind::UnknownHold),
        confirmed: 0,
        then: &[(0, Tag::Broadcast)],
    },
    Cell {
        open: Open::Broadcast,
        event: Event::Replace,
        verdict: Verdict::Declined(DeclineKind::UnknownHold),
        confirmed: 0,
        then: &[(0, Tag::Broadcast)],
    },
    Cell {
        open: Open::Broadcast,
        event: Event::ReleaseLease,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 0,
        then: &[(0, Tag::Broadcast)],
    },
    Cell {
        open: Open::Broadcast,
        event: Event::ReleaseBroadcast,
        verdict: Verdict::Freed(0),
        confirmed: 0,
        then: &[],
    },
    Cell {
        open: Open::Broadcast,
        event: Event::ReleaseStrategy,
        verdict: Verdict::FreedAll(&[0]),
        confirmed: 0,
        then: &[],
    },
    Cell {
        open: Open::Broadcast,
        event: Event::AdvanceHead,
        verdict: Verdict::Done,
        confirmed: 1,
        then: &[(0, Tag::Landed)],
    },
    Cell {
        open: Open::Broadcast,
        event: Event::ReorgHead,
        verdict: Verdict::Advised(&[]),
        confirmed: 0,
        then: &[(0, Tag::Broadcast)],
    },
    // HoldFrozen: the delivery-unknown hold with its window open.
    Cell {
        open: Open::HoldFrozen,
        event: Event::Lease,
        verdict: Verdict::Declined(DeclineKind::DeliveryUnknownHold),
        confirmed: 0,
        then: &[(0, Tag::HoldFrozen)],
    },
    Cell {
        open: Open::HoldFrozen,
        event: Event::Broadcast,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 0,
        then: &[(0, Tag::HoldFrozen)],
    },
    Cell {
        open: Open::HoldFrozen,
        event: Event::Hold,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 0,
        then: &[(0, Tag::HoldFrozen)],
    },
    Cell {
        open: Open::HoldFrozen,
        event: Event::ExpireHold,
        verdict: Verdict::Held(0, HoldPhase::ReplaceOrDominate),
        confirmed: 0,
        then: &[(0, Tag::HoldRoD)],
    },
    Cell {
        open: Open::HoldFrozen,
        event: Event::Replace,
        verdict: Verdict::Issued(0),
        confirmed: 0,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::HoldFrozen,
        event: Event::ReleaseLease,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 0,
        then: &[(0, Tag::HoldFrozen)],
    },
    Cell {
        open: Open::HoldFrozen,
        event: Event::ReleaseBroadcast,
        verdict: Verdict::Declined(DeclineKind::UnknownBroadcast),
        confirmed: 0,
        then: &[(0, Tag::HoldFrozen)],
    },
    Cell {
        open: Open::HoldFrozen,
        event: Event::ReleaseStrategy,
        verdict: Verdict::FreedAll(&[0]),
        confirmed: 0,
        then: &[],
    },
    Cell {
        open: Open::HoldFrozen,
        event: Event::AdvanceHead,
        verdict: Verdict::Done,
        confirmed: 1,
        then: &[(0, Tag::LapsedHold)],
    },
    Cell {
        open: Open::HoldFrozen,
        event: Event::ReorgHead,
        verdict: Verdict::Advised(&[]),
        confirmed: 0,
        then: &[(0, Tag::HoldFrozen)],
    },
    // HoldRoD: the hold whose window closed; replaceable, never freely
    // issuable.
    Cell {
        open: Open::HoldRoD,
        event: Event::Lease,
        verdict: Verdict::Declined(DeclineKind::DeliveryUnknownHold),
        confirmed: 0,
        then: &[(0, Tag::HoldRoD)],
    },
    Cell {
        open: Open::HoldRoD,
        event: Event::Broadcast,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 0,
        then: &[(0, Tag::HoldRoD)],
    },
    Cell {
        open: Open::HoldRoD,
        event: Event::Hold,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 0,
        then: &[(0, Tag::HoldRoD)],
    },
    Cell {
        open: Open::HoldRoD,
        event: Event::ExpireHold,
        verdict: Verdict::Held(0, HoldPhase::ReplaceOrDominate),
        confirmed: 0,
        then: &[(0, Tag::HoldRoD)],
    },
    Cell {
        open: Open::HoldRoD,
        event: Event::Replace,
        verdict: Verdict::Issued(0),
        confirmed: 0,
        then: &[(0, Tag::Lease)],
    },
    Cell {
        open: Open::HoldRoD,
        event: Event::ReleaseLease,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 0,
        then: &[(0, Tag::HoldRoD)],
    },
    Cell {
        open: Open::HoldRoD,
        event: Event::ReleaseBroadcast,
        verdict: Verdict::Declined(DeclineKind::UnknownBroadcast),
        confirmed: 0,
        then: &[(0, Tag::HoldRoD)],
    },
    Cell {
        open: Open::HoldRoD,
        event: Event::ReleaseStrategy,
        verdict: Verdict::FreedAll(&[0]),
        confirmed: 0,
        then: &[],
    },
    Cell {
        open: Open::HoldRoD,
        event: Event::AdvanceHead,
        verdict: Verdict::Done,
        confirmed: 1,
        then: &[(0, Tag::LapsedHold)],
    },
    Cell {
        open: Open::HoldRoD,
        event: Event::ReorgHead,
        verdict: Verdict::Advised(&[]),
        confirmed: 0,
        then: &[(0, Tag::HoldRoD)],
    },
    // Landed: a broadcast the chain confirmed, retained for a rewind.
    Cell {
        open: Open::Landed,
        event: Event::Lease,
        verdict: Verdict::Issued(1),
        confirmed: 1,
        then: &[(0, Tag::Landed), (1, Tag::Lease)],
    },
    Cell {
        open: Open::Landed,
        event: Event::Broadcast,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 1,
        then: &[(0, Tag::Landed)],
    },
    Cell {
        open: Open::Landed,
        event: Event::Hold,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 1,
        then: &[(0, Tag::Landed)],
    },
    Cell {
        open: Open::Landed,
        event: Event::ExpireHold,
        verdict: Verdict::Declined(DeclineKind::UnknownHold),
        confirmed: 1,
        then: &[(0, Tag::Landed)],
    },
    Cell {
        open: Open::Landed,
        event: Event::Replace,
        verdict: Verdict::Declined(DeclineKind::UnknownHold),
        confirmed: 1,
        then: &[(0, Tag::Landed)],
    },
    Cell {
        open: Open::Landed,
        event: Event::ReleaseLease,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 1,
        then: &[(0, Tag::Landed)],
    },
    Cell {
        open: Open::Landed,
        event: Event::ReleaseBroadcast,
        verdict: Verdict::Declined(DeclineKind::UnknownBroadcast),
        confirmed: 1,
        then: &[(0, Tag::Landed)],
    },
    Cell {
        open: Open::Landed,
        event: Event::ReleaseStrategy,
        verdict: Verdict::FreedAll(&[]),
        confirmed: 1,
        then: &[(0, Tag::Landed)],
    },
    Cell {
        open: Open::Landed,
        event: Event::AdvanceHead,
        verdict: Verdict::Done,
        confirmed: 2,
        then: &[(0, Tag::Landed)],
    },
    Cell {
        open: Open::Landed,
        event: Event::ReorgHead,
        verdict: Verdict::Advised(&[]),
        confirmed: 0,
        then: &[(0, Tag::Broadcast)],
    },
    // LapsedHold: a hold the chain passed, retained for a rewind.
    Cell {
        open: Open::LapsedHold,
        event: Event::Lease,
        verdict: Verdict::Issued(1),
        confirmed: 1,
        then: &[(0, Tag::LapsedHold), (1, Tag::Lease)],
    },
    Cell {
        open: Open::LapsedHold,
        event: Event::Broadcast,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 1,
        then: &[(0, Tag::LapsedHold)],
    },
    Cell {
        open: Open::LapsedHold,
        event: Event::Hold,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 1,
        then: &[(0, Tag::LapsedHold)],
    },
    Cell {
        open: Open::LapsedHold,
        event: Event::ExpireHold,
        verdict: Verdict::Declined(DeclineKind::UnknownHold),
        confirmed: 1,
        then: &[(0, Tag::LapsedHold)],
    },
    Cell {
        open: Open::LapsedHold,
        event: Event::Replace,
        verdict: Verdict::Declined(DeclineKind::UnknownHold),
        confirmed: 1,
        then: &[(0, Tag::LapsedHold)],
    },
    Cell {
        open: Open::LapsedHold,
        event: Event::ReleaseLease,
        verdict: Verdict::Declined(DeclineKind::UnknownLease),
        confirmed: 1,
        then: &[(0, Tag::LapsedHold)],
    },
    Cell {
        open: Open::LapsedHold,
        event: Event::ReleaseBroadcast,
        verdict: Verdict::Declined(DeclineKind::UnknownBroadcast),
        confirmed: 1,
        then: &[(0, Tag::LapsedHold)],
    },
    Cell {
        open: Open::LapsedHold,
        event: Event::ReleaseStrategy,
        verdict: Verdict::FreedAll(&[]),
        confirmed: 1,
        then: &[(0, Tag::LapsedHold)],
    },
    Cell {
        open: Open::LapsedHold,
        event: Event::AdvanceHead,
        verdict: Verdict::Done,
        confirmed: 2,
        then: &[(0, Tag::LapsedHold)],
    },
    Cell {
        open: Open::LapsedHold,
        event: Event::ReorgHead,
        verdict: Verdict::Advised(&[]),
        confirmed: 0,
        then: &[(0, Tag::HoldRoD)],
    },
];

/// Open the matrix's fixture: the owner leases the slot and the fixture
/// decides what becomes of it, keeping the lease handle every handle-shaped
/// event replays (stale once the fixture consumed it).
fn fixture(open: Open) -> (NonceAuthority, NonceLease) {
    let authority = NonceAuthority::new(0);
    let handle = authority.lease(&sid(OWNER)).expect("fixture lease");
    match open {
        Open::Free => {
            authority.release_lease(&handle).expect("fixture release");
        }
        Open::Lease => {}
        Open::StaleLease => {
            authority.set_confirmed(1);
        }
        Open::Broadcast => {
            authority
                .record_broadcast(&handle)
                .expect("fixture broadcast");
        }
        Open::HoldFrozen => {
            authority
                .record_hold(&handle, held_fees(), HoldWindow::DEFAULT, 100)
                .expect("fixture hold");
        }
        Open::HoldRoD => {
            authority
                .record_hold(&handle, held_fees(), HoldWindow::DEFAULT, 100)
                .expect("fixture hold");
            authority
                .release_hold(&sid(OWNER), SLOT)
                .expect("fixture sweep");
        }
        Open::Landed => {
            authority
                .record_broadcast(&handle)
                .expect("fixture broadcast");
            authority.set_confirmed(1);
        }
        Open::LapsedHold => {
            authority
                .record_hold(&handle, held_fees(), HoldWindow::DEFAULT, 100)
                .expect("fixture hold");
            authority.set_confirmed(1);
        }
    }
    (authority, handle)
}

/// Run one authority event against the fixture's opening.
fn run(authority: &NonceAuthority, handle: &NonceLease, event: Event) -> Observed {
    match event {
        Event::Lease => authority
            .lease(&sid(OWNER))
            .map_or_else(Observed::Declined, |lease| Observed::Issued(lease.nonce())),
        Event::Broadcast => authority
            .record_broadcast(handle)
            .map_or_else(Observed::Declined, |()| Observed::Done),
        Event::Hold => authority
            .record_hold(handle, held_fees(), HoldWindow::DEFAULT, 100)
            .map_or_else(Observed::Declined, |hold| {
                Observed::Held(hold.nonce(), hold.phase())
            }),
        Event::ExpireHold => authority
            .release_hold(&sid(OWNER), SLOT)
            .map_or_else(Observed::Declined, |hold| {
                Observed::Held(hold.nonce(), hold.phase())
            }),
        Event::Replace => authority
            .lease_replace(&sid(OWNER), dominating_fees())
            .map_or_else(Observed::Declined, |lease| Observed::Issued(lease.nonce())),
        Event::ReleaseLease => authority
            .release_lease(handle)
            .map_or_else(Observed::Declined, Observed::Freed),
        Event::ReleaseBroadcast => authority
            .release_broadcast(&sid(OWNER), SLOT)
            .map_or_else(Observed::Declined, Observed::Freed),
        Event::ReleaseStrategy => Observed::FreedAll(authority.release_strategy(&sid(OWNER))),
        Event::AdvanceHead => {
            authority.set_confirmed(authority.confirmed() + 1);
            Observed::Done
        }
        Event::ReorgHead => Observed::Advised(
            authority
                .set_confirmed_reorg(SLOT)
                .into_iter()
                .map(|advisory| (advisory.strategy().to_string(), advisory.nonce()))
                .collect(),
        ),
    }
}

/// The public occupancy snapshot as tag pairs, ascending by nonce.
fn slots(authority: &NonceAuthority) -> Vec<(u64, Tag)> {
    authority
        .occupancy()
        .into_iter()
        .map(|(nonce, occupant)| (nonce, tag(&occupant)))
        .collect()
}

/// Every occupancy variant against every authority event answers with one
/// total outcome: the exact verdict, the exact confirmed nonce after, and the
/// exact occupancy after — with no dual occupancy ever constructible and the
/// live projection agreeing with the occupancy map.
#[test]
fn every_occupancy_variant_and_authority_event_has_a_total_outcome() {
    let mut covered = [[false; 10]; 8];
    for cell in CELLS {
        let (authority, handle) = fixture(cell.open);
        let observed = run(&authority, &handle, cell.event);

        assert_eq!(
            observed,
            cell.verdict.observe(),
            "{:?} + {:?}: the event's exact verdict",
            cell.open,
            cell.event
        );
        assert_eq!(
            authority.confirmed(),
            cell.confirmed,
            "{:?} + {:?}: the confirmed nonce after",
            cell.open,
            cell.event
        );

        let after = slots(&authority);
        assert_eq!(
            after.as_slice(),
            cell.then,
            "{:?} + {:?}: the occupancy after",
            cell.open,
            cell.event
        );
        assert!(
            after.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "{:?} + {:?}: one occupant per nonce, ascending",
            cell.open,
            cell.event
        );

        let live: Vec<u64> = after
            .iter()
            .filter(|(_, slot)| slot.is_live())
            .map(|(nonce, _)| *nonce)
            .collect();
        assert_eq!(
            authority.outstanding_nonces(),
            live,
            "{:?} + {:?}: the live projection agrees with the occupancy map",
            cell.open,
            cell.event
        );

        covered[OPENS
            .iter()
            .position(|open| *open == cell.open)
            .expect("opening enumerated")][EVENTS
            .iter()
            .position(|event| *event == cell.event)
            .expect("event enumerated")] = true;
    }

    for (row, open) in OPENS.iter().enumerate() {
        for (column, event) in EVENTS.iter().enumerate() {
            assert!(
                covered[row][column],
                "no matrix cell for {open:?} + {event:?}"
            );
        }
    }
}
