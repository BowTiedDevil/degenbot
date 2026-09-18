//! Frame-liveness FSM acceptance: finality-only death, reorg revival,
//! no pool-absence eviction, and the nonce-consumed classification handoff.

#![expect(
    clippy::expect_used,
    reason = "integration fixtures and assertions fail loudly"
)]

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
        chain_id: 1,
        from: SENDER,
        to: None,
        value: U256::ZERO,
        data: Bytes::new(),
        gas: 300_000,
        max_fee_per_gas: 514_684_409,
        max_priority_fee_per_gas: 1_000_000_000,
        claimed_nonce: nonce,
        expected_at_capture: expected,
        tx_type: 2,
        access_list: serde_json::json!([]),
        received_unix_ms: 1_700_000_000_000,
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
fn frontier_rescues_with_an_empty_prefix_and_leaves_the_fsm() {
    let mut q = Quarantine::new();
    let f = frame(12, 10);
    q.push(f.clone());
    // head == claim: every predecessor is already mined, so the whole gap is
    // replayed against the current head with no prefix. The quiet hold from
    // 94425a315 is superseded: the frame now LEAVES the FSM into the funnel.
    let out = q.poll(SENDER, 12, &[]);
    assert_eq!(out.len(), 1, "the frontier emits exactly one decision");
    assert_eq!(
        out[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![]
        },
        "the frontier is a rescue with an empty prefix, not consumption"
    );
    assert!(
        !matches!(out[0].1, QuarantineDecision::NonceConsumed),
        "no NonceConsumed AT the claim"
    );
    assert!(q.is_empty(), "a frontier rescue removes the frame");
    assert_eq!(q.state(f.hash), None);

    // A frame whose head LATER passes the claim still classifies as consumed.
    let mut q = Quarantine::new();
    let g = frame(12, 10);
    let h = g.hash;
    q.push(g);
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
fn re_park_after_rescue_requires_a_new_gap() {
    let mut q = Quarantine::new();
    q.push(frame(12, 10));
    let out = q.poll(SENDER, 12, &[]);
    assert_eq!(
        out[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![]
        }
    );
    assert!(q.is_empty(), "the rescued frame is gone");
    // Same head afterwards: nothing left to re-park, so no ping-pong.
    assert!(q.poll(SENDER, 12, &[]).is_empty());

    // Only a genuinely NEW captured boundary re-parks (a later gap).
    let mut fresh = frame(15, 12);
    fresh.hash = hash(2);
    q.push(fresh);
    assert_eq!(q.len(), 1);
    let out = q.poll(SENDER, 12, &[]);
    assert_eq!(
        out[0].1,
        QuarantineDecision::StillWaiting {
            unknown: vec![12, 13, 14]
        }
    );
    assert_eq!(q.state(hash(2)), Some(FrameState::Tracked));
}

#[test]
fn finality_tombstones_only_at_or_below_the_tag() {
    let mut q = Quarantine::new();
    let f = frame(12, 10);
    let h = f.hash;
    q.push(f);
    let _ = q.poll(SENDER, 13, &[]);
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
    let _ = q.poll(SENDER, 13, &[]);
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

/// A legacy park migrated into a tracked frame outlives pool absence: the
/// journal fold keeps it alive and only chain proof ends it.
#[test]
fn legacy_migrated_park_reenters_the_fsm_as_tracked() {
    use degenbot_submission::gap_quarantine_journal::{read_pending, JOURNAL_FILE_NAME};

    let dir = std::env::temp_dir().join(format!(
        "degenbot-frame-liveness-legacy-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(JOURNAL_FILE_NAME);
    let line = format!(
        "{{\"kind\":\"park\",\"frame\":{{\"hash\":\"{}\",\"from\":\"{SENDER}\",\"nonce\":45}},\"sender\":\"{SENDER}\",\"claimed_nonce\":45,\"expected_nonce\":44}}",
        hash(5)
    );
    std::fs::write(&path, format!("{line}\n")).expect("write");

    let read = read_pending(&path).expect("read");
    assert_eq!(read.pending.len(), 1);
    let mut q = Quarantine::new();
    let frame = read.pending[0].to_parked_frame().expect("frame");
    assert!(frame.is_degraded());
    let frame_hash = frame.hash;
    q.push(frame);
    for _ in 0..10 {
        let out = q.poll(SENDER, 44, &[]);
        assert!(
            out.iter()
                .all(|(_, decision)| !matches!(decision, QuarantineDecision::Rescue { .. })),
            "a degraded frame never rescues without a wire"
        );
    }
    assert_eq!(
        q.state(frame_hash),
        Some(FrameState::Tracked),
        "a migrated legacy frame never dies without chain proof"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
