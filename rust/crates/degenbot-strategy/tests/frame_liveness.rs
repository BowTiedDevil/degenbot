//! Frame-liveness FSM acceptance: finality-only death, reorg revival,
//! no pool-absence eviction, and the nonce-consumed classification handoff.

#![expect(
    clippy::expect_used,
    reason = "integration fixtures and assertions fail loudly"
)]

use alloy::primitives::{address, Address, Bytes, B256, U256};
use degenbot_rpc::backrun_feed::BackrunFeedEvent;
use degenbot_simulation::sim::evm::frame_replay::{
    PredStatus, ReplayStatus, ReplayableTx, ScratchBlock, ScratchEvm,
};
use degenbot_strategy::gap_quarantine::{
    FrameState, NonceConsumed, ParkedFrame, Quarantine, QuarantineDecision,
};
use degenbot_strategy::gap_quarantine_journal::{
    read_pending, ParkRecord, QuarantineJournal, Resolution, JOURNAL_FILE_NAME,
};
use revm::bytecode::Bytecode;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::state::AccountInfo;

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
        raw_signed_tx: None,
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

/// A gap missing even one predecessor stays parked and NAMES the missing
/// nonce: the pool view only rescues a COMPLETE prefix.
#[test]
fn partial_pool_view_still_waits_and_names_the_unknown_tail() {
    let mut q = Quarantine::new();
    q.push(frame(12, 10));
    let out = q.poll(SENDER, 10, &[11]);
    assert_eq!(
        out[0].1,
        QuarantineDecision::StillWaiting { unknown: vec![10] }
    );
    assert_eq!(q.len(), 1, "the incomplete gap keeps the frame parked");
}

/// An unrelated pool transaction that only moves the count lane is not
/// evidence: the guard keys on the predecessor list and the frame's captured
/// boundary, so the identical re-park must not re-fire the rescue.
#[test]
fn unrelated_pool_tx_does_not_re_arm_a_futile_rescue() {
    let mut q = Quarantine::new();
    q.push(frame(12, 10));
    let first = q.poll(SENDER, 10, &[10, 11]);
    assert_eq!(
        first[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![10, 11]
        }
    );
    assert!(q.is_empty(), "the rescued frame leaves the FSM");

    // The rescue re-parked on the same boundary; the caller records the guard
    // with the predecessors' content identity.
    let content = [(10, hash(0xa1)), (11, hash(0xa2))];
    q.record_repark_guard_with_content(
        frame(12, 10).hash,
        &[10, 11],
        &[content[0].1, content[1].1],
        10,
    );
    q.push(frame(12, 10));
    assert_eq!(
        q.poll_with_content(SENDER, 10, &[10, 11], &content)[0].1,
        QuarantineDecision::StillWaiting {
            unknown: vec![10, 11]
        },
        "the count lane moved but the frame's predecessor list did not"
    );
    assert_eq!(q.len(), 1);
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
fn same_head_rescue_is_not_refired_but_a_later_boundary_waits() {
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

    // A fresh captured boundary (a later gap) is its own frame.
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

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "degenbot-frame-liveness-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn event(hash_byte: u8, nonce: u64) -> BackrunFeedEvent {
    BackrunFeedEvent {
        chain_id: 1,
        from: Address::with_last_byte(7),
        to: Some(Address::with_last_byte(9)),
        value: U256::from(123u64),
        data: Bytes::from(vec![0xab, 0xcd]),
        gas: 300_000,
        max_fee_per_gas: 514_684_409,
        max_priority_fee_per_gas: 1_000_000_000,
        nonce,
        hash: hash(hash_byte),
        access_list: serde_json::json!([]),
        tx_type: 2,
        received_unix_ms: 1_700_000_000_000,
        raw_signed_tx: None,
    }
}

/// P1: a transient re-entry failure (`replay_unavailable`/`replay_failed`)
/// must not resolve the frame. The original park record stands and the frame is
/// back in the FSM, so a boot fold reloads it alive with its ORIGINAL boundary.
/// The bin unit tests pin the routing that produces this state.
#[test]
fn transient_reentry_failure_keeps_the_frame_tracked_and_unresolved() {
    let dir = scratch("transient");
    let path = dir.join(JOURNAL_FILE_NAME);
    let mut journal = QuarantineJournal::open(&path).expect("open");
    let original = ParkRecord::new(&event(21, 12), 10, 1_700_000_000_000);
    journal.record_park(&original).expect("park");

    let mut q = Quarantine::new();
    let mut f = frame(12, 10);
    f.hash = hash(21);
    q.push(f.clone());

    assert_eq!(q.state(f.hash), Some(FrameState::Tracked));
    let read = read_pending(&path).expect("read");
    assert_eq!(read.pending.len(), 1, "no resolve tombstone was written");
    assert_eq!(read.pending[0].expected_nonce, 10, "original boundary");

    let _ = std::fs::remove_dir_all(&dir);
}

/// P5: a rescue of a non-empty predecessor prefix followed by a transient
/// hydration failure leaves the frame tracked and un-resolved in the journal
/// fold -- the pool-pred retry path is a re-park, not a death.
#[test]
fn rescue_with_predecessors_then_transient_failure_keeps_the_frame_tracked() {
    let dir = scratch("rescue-transient");
    let path = dir.join(JOURNAL_FILE_NAME);
    let mut journal = QuarantineJournal::open(&path).expect("open");
    let original = ParkRecord::new(&event(61, 12), 10, 1_700_000_000_000);
    journal.record_park(&original).expect("park");

    let mut q = Quarantine::new();
    let mut f = frame(12, 10);
    f.hash = hash(61);
    q.push(f.clone());

    let rescued = q.poll(f.from, 10, &[10, 11]);
    assert_eq!(
        rescued[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![10, 11]
        },
        "every predecessor is pool-known, so the frame enters the prefix case"
    );
    assert!(q.is_empty(), "a rescue removes the frame pending hydration");

    // The hydration failed transiently: the caller re-parks the ORIGINAL frame
    // and writes no resolve (the journal router's Transient arm).
    q.push(f.clone());
    assert_eq!(q.state(f.hash), Some(FrameState::Tracked));

    let read = read_pending(&path).expect("read");
    assert_eq!(read.pending.len(), 1, "no resolve tombstone was written");
    assert_eq!(read.pending[0].expected_nonce, 10, "original boundary");
    let _ = std::fs::remove_dir_all(&dir);
}

/// P2: a gap-pending re-entry already re-parked the frame and journaled the
/// fresh park in the same pass; the router adds no resolve and no duplicate.
/// The bin unit tests pin the routing that produces this state.
#[test]
fn gap_pending_reentry_leaves_only_the_fresh_park() {
    let dir = scratch("gap-pending");
    let path = dir.join(JOURNAL_FILE_NAME);
    let mut journal = QuarantineJournal::open(&path).expect("open");
    let fresh = ParkRecord::new(&event(31, 15), 12, 1_700_000_000_000);
    journal.record_park(&fresh).expect("fresh park");

    let mut q = Quarantine::new();
    let mut re_parked = frame(15, 12);
    re_parked.hash = hash(31);
    q.push(re_parked);

    let read = read_pending(&path).expect("read");
    assert_eq!(read.pending.len(), 1, "the fresh park is the only record");
    assert_eq!(read.pending[0].expected_nonce, 12, "the fresh boundary");
    assert_eq!(read.pending[0].claimed_nonce, 15);
    assert_eq!(q.len(), 1, "no duplicate re-park");

    let _ = std::fs::remove_dir_all(&dir);
}

/// P3: a terminal re-entry writes exactly one resolve, after any funnel
/// records the same pass produced; the boot fold drops the hash. The bin unit
/// tests pin the routing that produces this state.
#[test]
fn terminal_reentry_writes_one_resolve_and_the_hash_folds_dead() {
    let dir = scratch("terminal");
    let path = dir.join(JOURNAL_FILE_NAME);
    let mut journal = QuarantineJournal::open(&path).expect("open");
    let original = ParkRecord::new(&event(41, 12), 10, 1_700_000_000_000);
    journal.record_park(&original).expect("park");
    journal
        .record_tentative(
            hash(41),
            NonceConsumed::SlotTakenAt {
                block: 21_000_000,
                block_hash: hash(7),
                by: None,
            },
            1_700_000_000_001,
        )
        .expect("tentative");
    journal
        .record_resolve(hash(41), Resolution::RescueConsumed, 1_700_000_000_002)
        .expect("resolve");

    let raw = std::fs::read_to_string(&path).expect("raw");
    assert_eq!(
        raw.lines()
            .filter(|line| line.contains("\"kind\":\"resolve\""))
            .count(),
        1,
        "exactly one resolve"
    );
    assert!(
        read_pending(&path).expect("read").pending.is_empty(),
        "the hash folds dead"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// P4: the fold is line-ordered. A park, its resolve, then a fresh park for the
/// same hash reloads ALIVE with the NEW boundary.
#[test]
fn park_resolve_park_folds_alive_with_the_new_boundary() {
    let dir = scratch("fold-order");
    let path = dir.join(JOURNAL_FILE_NAME);
    let mut journal = QuarantineJournal::open(&path).expect("open");
    let old = ParkRecord::new(&event(51, 12), 10, 1_700_000_000_000);
    journal.record_park(&old).expect("old park");
    journal
        .record_resolve(hash(51), Resolution::RescueConsumed, 1_700_000_000_001)
        .expect("resolve");
    let new = ParkRecord::new(&event(51, 15), 12, 1_700_000_000_002);
    journal.record_park(&new).expect("new park");

    let read = read_pending(&path).expect("read");
    assert_eq!(read.pending.len(), 1);
    assert_eq!(read.pending[0].expected_nonce, 12, "new boundary");
    assert_eq!(read.pending[0].claimed_nonce, 15);

    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────── the sequence seam over a fabricated queue ───────────────

const SEQ_ADDR: Address = address!("0x6666666666666666666666666666666666666666");

/// Dual-mode sequence contract: empty calldata stores 0x2A at its own slot 0;
/// non-empty calldata mirrors slot 0 into slot 1.
const SEQ_CODE: &[u8] = &[
    0x36, 0x60, 0x0A, 0x57, // CALLDATASIZE; PUSH1 0x0A; JUMPI (non-empty -> mirror)
    0x60, 0x2A, 0x60, 0x00, 0x55, 0x00, // write slot 0 = 0x2A; STOP
    0x5B, // 0x0A JUMPDEST
    0x60, 0x00, 0x54, 0x60, 0x01, 0x55, 0x00, // slot 1 = slot 0; STOP
];

fn sequence_scratch() -> ScratchEvm<CacheDB<EmptyDB>> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        SENDER,
        AccountInfo {
            balance: U256::from(1_000_000_000_000_000_000u64),
            nonce: 10,
            ..Default::default()
        },
    );
    db.insert_account_info(
        SEQ_ADDR,
        AccountInfo {
            code: Some(Bytecode::new_raw(Bytes::copy_from_slice(SEQ_CODE))),
            ..Default::default()
        },
    );
    ScratchEvm::new(
        db,
        ScratchBlock {
            number: 2050,
            timestamp: 1_780_000_000,
            base_fee_next: 1_000_000_000,
        },
    )
}

fn replay_tx(to: Address, data: &[u8], nonce: u64) -> ReplayableTx {
    ReplayableTx {
        from: SENDER,
        to: Some(to),
        value: U256::ZERO,
        data: Bytes::copy_from_slice(data),
        gas_limit: 100_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        nonce,
    }
}

/// A two-predecessor pool-known queue rescues through the FSM, the fabricated
/// queue executes ascending over the sequence overlay, and the frame's settled
/// state reflects the composed view (the first predecessor's slot-0 write is
/// mirrored by the frame).
#[test]
fn two_predecessor_rescue_executes_ascending_before_the_frame() {
    let mut q = Quarantine::new();
    q.push(frame(12, 10));
    let rescued = q.poll(SENDER, 10, &[10, 11]);
    assert_eq!(
        rescued[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![10, 11]
        }
    );
    assert!(q.is_empty(), "the rescue leaves the FSM");

    let mut scratch = sequence_scratch();
    let prefix = [replay_tx(SEQ_ADDR, &[], 10), replay_tx(SENDER, &[], 11)];
    let frame_tx = replay_tx(SEQ_ADDR, &[0x02], 12);
    let sequence = scratch
        .replay_sequence(&prefix, &frame_tx)
        .expect("sequence executes");
    assert_eq!(
        sequence.predecessors,
        vec![PredStatus::Success, PredStatus::Success],
        "both predecessors settle, ascending"
    );
    assert!(matches!(sequence.frame.status, ReplayStatus::Success));
    assert_eq!(
        sequence
            .frame
            .state
            .get(&SEQ_ADDR)
            .and_then(|account| account.storage.get(&U256::from(1u64)))
            .map(|slot| slot.present_value),
        Some(U256::from(0x2A)),
        "the frame mirrors the accumulated slot-0 write"
    );
}

/// Same predecessor nonces, a replaced predecessor hash: the content identity
/// in the guard key re-arms the pool-pred rescue after a futile pass, while
/// identical content keeps it suppressed.
#[test]
fn content_replaced_predecessor_re_arms_a_guarded_rescue() {
    let mut q = Quarantine::new();
    q.record_repark_guard_with_content(hash(1), &[10, 11], &[hash(0xa1), hash(0xa2)], 10);
    q.push(frame(12, 10));

    let same = q.poll_with_content(SENDER, 10, &[10, 11], &[(10, hash(0xa1)), (11, hash(0xa2))]);
    assert_eq!(
        same[0].1,
        QuarantineDecision::StillWaiting {
            unknown: vec![10, 11]
        },
        "identical nonces + hashes + boundary stay suppressed"
    );
    assert_eq!(q.len(), 1);

    let replaced =
        q.poll_with_content(SENDER, 10, &[10, 11], &[(10, hash(0xb1)), (11, hash(0xa2))]);
    assert_eq!(
        replaced[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![10, 11]
        },
        "a replaced predecessor hash re-arms the rescue"
    );
    assert!(q.is_empty(), "the re-armed rescue leaves the FSM");
}
