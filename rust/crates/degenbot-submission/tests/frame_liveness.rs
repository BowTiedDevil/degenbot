//! Frame-liveness FSM acceptance: finality-only death, reorg revival,
//! no pool-absence eviction, and the nonce-consumed classification handoff.

use alloy::primitives::{Address, Bytes, B256, U256};
use degenbot_submission::gap_quarantine::{
    FrameState, NonceConsumed, ParkedFrame, Quarantine, QuarantineDecision,
};

const SENDER: Address = Address::ZERO;

fn hash(byte: u8) -> B256 {
    let mut raw = [0u8; 32];
    raw[0] = byte;
    B256::from(raw)
}

fn frame(nonce: u64, expected: u64) -> ParkedFrame {
    ParkedFrame {
        hash: hash(1),
        from: SENDER,
        to: None,
        value: U256::ZERO,
        data: Bytes::new(),
        gas: 300_000,
        max_fee_per_gas: 514_684_409,
        max_priority_fee_per_gas: 1_000_000_000,
        claimed_nonce: nonce,
        expected_at_capture: expected,
    }
}

#[test]
fn rescue_lists_every_predecessor_in_order() {
    let mut q = Quarantine::new();
    q.push(frame(12, 10));
    let out = q.poll(SENDER, 10, &[10, 11]);
    assert_eq!(
        out[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![10, 11]
        }
    );
    assert!(q.is_empty(), "a rescued frame leaves the FSM");
}

#[test]
fn gap_range_is_the_explicit_predecessor_order() {
    assert_eq!(frame(5, 5).gap_range(), Vec::<u64>::new());
    assert_eq!(frame(8, 5).gap_range(), vec![5, 6, 7]);
}

#[test]
fn frontier_nonce_is_a_quiet_hold_not_consumption() {
    let mut q = Quarantine::new();
    let f = frame(12, 10);
    let h = f.hash;
    q.push(f);
    // head == claim: the frame's slot is the OPEN frontier -- pending,
    // not consumed. No decision, no classification probes, quiet hold.
    let out = q.poll(SENDER, 12, &[]);
    assert!(
        out.is_empty(),
        "frontier pending emits no decision: {out:?}"
    );
    assert_eq!(q.state(h), Some(FrameState::Tracked));
    // head passes the claim: consumption, classification begins.
    let out = q.poll(SENDER, 13, &[]);
    assert_eq!(out[0].1, QuarantineDecision::NonceConsumed);
    assert_eq!(q.state(h), Some(FrameState::Tracked));
    assert!(q.enter_tentative(
        h,
        NonceConsumed::MinedAt {
            block: 100,
            block_hash: hash(7)
        }
    ));
    assert_eq!(
        q.state(h),
        Some(FrameState::Tentative(NonceConsumed::MinedAt {
            block: 100,
            block_hash: hash(7)
        }))
    );
    assert_eq!(q.len(), 1, "the frame survives into the tentative set");
}

#[test]
fn finality_tombstones_only_at_or_below_the_tag() {
    let mut q = Quarantine::new();
    let f = frame(12, 10);
    let h = f.hash;
    q.push(f);
    let _ = q.poll(SENDER, 12, &[]);
    q.enter_tentative(
        h,
        NonceConsumed::MinedAt {
            block: 100,
            block_hash: hash(7),
        },
    );
    assert!(q.check_finalized(99).is_empty());
    assert_eq!(q.tentative_count(), 1);
    let dead = q.check_finalized(100);
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].1.block(), 100);
    assert!(dead[0].1.mined());
    assert!(q.is_empty(), "a finalized frame leaves the FSM");
    assert!(q.check_finalized(200).is_empty(), "never tombstoned twice");
}

#[test]
fn reorg_revives_on_hash_mismatch_but_not_on_match() {
    let mut q = Quarantine::new();
    let f = frame(12, 10);
    let h = f.hash;
    q.push(f);
    let _ = q.poll(SENDER, 12, &[]);
    q.enter_tentative(
        h,
        NonceConsumed::SlotTakenAt {
            block: 100,
            block_hash: hash(7),
            by: Some(hash(9)),
        },
    );
    assert!(q.check_reorg(100, Some(hash(7))).is_empty());
    assert_eq!(q.tentative_count(), 1);
    let revived = q.check_reorg(100, Some(hash(8)));
    assert_eq!(revived.len(), 1);
    assert_eq!(q.state(h), Some(FrameState::Tracked));
    q.enter_tentative(
        h,
        NonceConsumed::MinedAt {
            block: 101,
            block_hash: hash(3),
        },
    );
    assert_eq!(q.check_reorg(101, None).len(), 1);
    assert_eq!(q.state(h), Some(FrameState::Tracked));
}

#[test]
fn tentative_blocks_are_deduped() {
    let mut q = Quarantine::new();
    let mut a = frame(12, 10);
    a.hash = hash(1);
    let mut b = frame(13, 11);
    b.hash = hash(2);
    q.push(a);
    q.push(b);
    for f in q.poll(SENDER, 20, &[]) {
        q.enter_tentative(
            f.0.hash,
            NonceConsumed::MinedAt {
                block: 100,
                block_hash: hash(7),
            },
        );
    }
    assert_eq!(q.tentative_blocks(), vec![(100, hash(7))]);
}

#[test]
fn no_pool_absence_ever_evicts() {
    let mut q = Quarantine::new();
    let f = frame(12, 10);
    let h = f.hash;
    q.push(f);
    for _ in 0..100 {
        let out = q.poll(SENDER, 10, &[]);
        assert!(matches!(out[0].1, QuarantineDecision::StillWaiting { .. }));
    }
    assert_eq!(q.state(h), Some(FrameState::Tracked));
}
