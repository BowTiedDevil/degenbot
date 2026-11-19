//! The settlement arm's nonce lane is filed under its host scope in the
//! process's lane registry, so a peer host's filing is never a fallback, an
//! explicitly shared lane stays one authority, the resolved lane serializes the
//! operator account's nonces, and teardown removes only the registration its
//! own stake filed.

#![expect(clippy::expect_used, reason = "test assertions fail loudly")]

use std::sync::Arc;

use alloy::primitives::{Address, B256};

use degenbot_submission::{SettlementLanes, SubmissionLedger};
use degenbot_substrate::driver_spawn::LaneNamespace;
use degenbot_substrate::lane_authority::{LaneRefusal, LaneScope};
use degenbot_substrate::nonce::{HoldWindow, LanePolicy, NonceAuthority, NonceLane, StrategyId};

fn account(byte: u8) -> Address {
    Address::repeat_byte(byte)
}

fn scope(root: &str, strategy: &str) -> LaneScope {
    LaneScope::new(
        Some(LaneNamespace::under(root, &StrategyId::new(strategy))),
        strategy,
    )
}

fn lane_for(
    authority: &Arc<NonceAuthority>,
    ledger: &Arc<SubmissionLedger>,
    strategy: &str,
) -> Arc<NonceLane<SubmissionLedger>> {
    Arc::new(authority.mint_lane(
        strategy,
        Arc::clone(ledger),
        LanePolicy::new(HoldWindow::DEFAULT, "settlement"),
    ))
}

/// The resolved settlement lane submits the account's nonces in order: each
/// signed-then-broadcast submission takes the next slot, and a peer strategy on
/// the same authority continues the same prefix rather than colliding with it.
#[test]
fn the_resolved_lane_serializes_the_accounts_nonces() {
    let lanes = Arc::new(SettlementLanes::new());
    let authority = Arc::new(NonceAuthority::new(5));
    let ledger = Arc::new(SubmissionLedger::new());
    let filed = lane_for(&authority, &ledger, "settlement");
    let stake = lanes.install(scope("/state", "settlement"), Arc::clone(&filed));

    let resolved = lanes
        .resolve(&stake, account(0x01))
        .expect("the host's own filing resolves");
    assert!(
        Arc::ptr_eq(&resolved, &filed),
        "the boot's lane is the lane the seam stamps through"
    );

    let mut nonces = Vec::new();
    for head in 0..3 {
        let lease = resolved.stamp().expect("the lane stamps");
        nonces.push(lease.nonce());
        resolved
            .record_signed(&lease, B256::ZERO, B256::ZERO, head, None)
            .expect("the ledger records the signed submission");
        resolved
            .authority()
            .record_broadcast(&lease)
            .expect("the authority promotes the lease to a broadcast");
        resolved
            .record_broadcast(&lease)
            .expect("the ledger follows the authority's promotion");
    }
    assert_eq!(
        nonces,
        vec![5, 6, 7],
        "submissions serialize in nonce order"
    );

    // A peer strategy signs through its own lane over the one authority: it
    // continues the prefix instead of reissuing a slot settlement holds.
    let peer = lane_for(&authority, &ledger, "txpool_backrun");
    assert_eq!(peer.stamp().expect("the peer lane stamps").nonce(), 8);
}

/// Two hosts that each minted their own lane over one registry — the production
/// shape, one registry per process — sign distinct accounts independently, and
/// a second lane cannot claim an account another lane already stamps.
#[test]
fn two_hosts_on_one_registry_cannot_both_stamp_one_account() {
    let lanes = Arc::new(SettlementLanes::new());
    let first_authority = Arc::new(NonceAuthority::new(0));
    let second_authority = Arc::new(NonceAuthority::new(0));
    let ledger = Arc::new(SubmissionLedger::new());
    let first = lanes.install(
        scope("/state", "settlement"),
        lane_for(&first_authority, &ledger, "settlement"),
    );
    let second = lanes.install(
        scope("/state", "settlement"),
        lane_for(&second_authority, &ledger, "settlement"),
    );

    let third = lanes.install(
        scope("/state", "settlement"),
        lane_for(&Arc::new(NonceAuthority::new(0)), &ledger, "settlement"),
    );

    lanes
        .resolve(&first, account(0x0a))
        .expect("the first host binds its own account");
    lanes
        .resolve(&second, account(0x0b))
        .expect("the second host's distinct account is untouched");
    assert_eq!(
        lanes.resolve(&third, account(0x0a)).err(),
        Some(LaneRefusal::AccountOwned {
            account: account(0x0a)
        }),
        "one account has one authority, so no other lane may claim it"
    );
}

/// Two scopes that filed the one lane are the declared sharing of one
/// authority: both resolve to it, their submissions interleave in nonce order,
/// and the shared lane still signs for the one account it bound.
#[test]
fn an_explicitly_shared_lane_is_one_authority() {
    let lanes = Arc::new(SettlementLanes::new());
    let authority = Arc::new(NonceAuthority::new(0));
    let ledger = Arc::new(SubmissionLedger::new());
    let shared = lane_for(&authority, &ledger, "settlement");
    let a = lanes.install(scope("/host-a", "settlement"), Arc::clone(&shared));
    let b = lanes.install(scope("/host-b", "settlement"), Arc::clone(&shared));

    let lane_a = lanes.resolve(&a, account(0x01)).expect("A resolves");
    let lane_b = lanes.resolve(&b, account(0x01)).expect("B resolves");
    assert!(
        Arc::ptr_eq(&lane_a, &lane_b),
        "the declared sharing is one lane"
    );
    assert_eq!(
        lanes.resolve(&b, account(0x02)).err(),
        Some(LaneRefusal::AccountBound {
            bound: account(0x01)
        }),
        "the shared lane signs for the account it bound, not one per host"
    );

    let first = lane_a.stamp().expect("A stamps");
    lane_a
        .record_signed(&first, B256::ZERO, B256::ZERO, 0, None)
        .expect("signed");
    lane_a
        .authority()
        .record_broadcast(&first)
        .expect("the shared authority promotes the lease");
    lane_a.record_broadcast(&first).expect("the ledger follows");
    assert_eq!(lane_b.stamp().expect("B stamps").nonce(), 1);
}

/// Teardown removes the registration its own stake filed; a peer host's filing
/// over the same registry survives it.
#[test]
fn teardown_removes_only_the_owned_registration() {
    let lanes = Arc::new(SettlementLanes::new());
    let authority = Arc::new(NonceAuthority::new(0));
    let ledger = Arc::new(SubmissionLedger::new());
    let mine = lanes.install(
        scope("/host-a", "settlement"),
        lane_for(&authority, &ledger, "settlement"),
    );
    let peer = lanes.install(
        scope("/host-b", "settlement"),
        lane_for(&authority, &ledger, "settlement"),
    );
    lanes
        .resolve(&mine, account(0x01))
        .expect("A binds its account");
    lanes
        .resolve(&peer, account(0x02))
        .expect("B binds its account");
    assert_eq!(
        lanes.scopes(),
        vec![
            scope("/host-a", "settlement"),
            scope("/host-b", "settlement")
        ]
    );

    drop(mine);

    assert_eq!(
        lanes.scopes(),
        vec![scope("/host-b", "settlement")],
        "the teardown removed only the filing its stake owned"
    );
    lanes
        .resolve(&peer, account(0x02))
        .expect("the peer host's filing survives");
}
