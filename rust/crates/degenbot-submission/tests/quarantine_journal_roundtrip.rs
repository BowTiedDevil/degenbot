//! Journal round-trip + frame-liveness convergence: parks and their tentative
//! classifications survive a reload, a restart mid-finality-window still sees
//! the frame as tentative, finality writes a tombstone exactly once, and no
//! frame is ever dropped for age or pool absence.

#![expect(
    clippy::expect_used,
    reason = "integration fixtures and assertions fail loudly"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, Bytes, B256, U256};
use degenbot_rpc::backrun_feed::BackrunFeedEvent;
use degenbot_submission::gap_quarantine::{
    FrameState, NonceConsumed, Quarantine, QuarantineDecision,
};
use degenbot_submission::gap_quarantine_journal::{
    compact, read_pending, ParkRecord, QuarantineJournal, Resolution, TentativeRecord,
    JOURNAL_FILE_NAME,
};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "degenbot-quarantine-roundtrip-{}-{tag}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

const SENDER: Address = Address::new([0x7; 20]);

fn event(hash_byte: u8, nonce: u64, received_unix_ms: u64) -> BackrunFeedEvent {
    let mut raw = [0u8; 32];
    raw[0] = hash_byte;
    BackrunFeedEvent {
        chain_id: 1,
        from: SENDER,
        to: Some(Address::with_last_byte(9)),
        value: U256::from(123u64),
        data: Bytes::from(vec![0xab, 0xcd]),
        gas: 300_000,
        max_fee_per_gas: 514_684_409,
        max_priority_fee_per_gas: 1_000_000_000,
        nonce,
        hash: B256::from(raw),
        access_list: serde_json::json!([]),
        tx_type: 2,
        received_unix_ms,
    }
}

fn hash(byte: u8) -> B256 {
    let mut raw = [0u8; 32];
    raw[0] = byte;
    B256::from(raw)
}

/// Park -> tentative -> restart mid-window -> still tentative -> finalized ->
/// tombstone written exactly once.
#[test]
fn journal_round_trips_tentative_state_across_a_restart() {
    let dir = scratch("tentative-roundtrip");
    let path = dir.join(JOURNAL_FILE_NAME);
    let consumed = NonceConsumed::MinedAt {
        block: 100,
        block_hash: hash(7),
    };

    // Session 1: park + classify, and a second park whose tombstone lands in
    // the same window.
    {
        let mut journal = QuarantineJournal::open(&path).expect("open");
        let frame = ParkRecord::new(&event(1, 12, 1_000), 10, now_ms());
        journal.record_park(&frame).expect("park");
        journal
            .record_tentative(frame.frame.hash.parse().expect("hash"), consumed, now_ms())
            .expect("tentative");

        let dead = ParkRecord::new(&event(2, 20, 2_000), 18, now_ms());
        journal.record_park(&dead).expect("park dead");
        journal
            .record_resolve(
                dead.frame.hash.parse().expect("hash"),
                Resolution::SlotTakenFinalized,
                now_ms(),
            )
            .expect("resolve");
    }

    // Boot mid-finality-window: the fold carries the tentative classification,
    // and the resolved park is gone.
    let read = read_pending(&path).expect("read");
    assert_eq!(read.skipped, 0);
    assert_eq!(read.pending.len(), 1, "only the tentative park remains");
    let record = &read.pending[0];
    assert_eq!(record.claimed_nonce, 12);
    assert_eq!(
        record.tentative.map(TentativeRecord::to_consumed),
        Some(consumed)
    );
    compact(&path, &read.pending).expect("compact");

    // Reload reconstructs the tentative set exactly -- not reclassified, not
    // dropped.
    let mut q = Quarantine::new();
    let rebuilt = record.to_parked_frame().expect("frame");
    let frame_hash = rebuilt.hash;
    q.push(rebuilt);
    assert!(
        q.enter_tentative(
            frame_hash,
            record.tentative.expect("tentative").to_consumed()
        ),
        "reload reclassifies the tentative frame"
    );
    assert!(matches!(
        q.state(frame_hash),
        Some(FrameState::Tentative(_))
    ));

    // Finality at the block ends it exactly once.
    let dead = q.check_finalized(100);
    assert_eq!(dead.len(), 1);
    assert!(dead[0].1.mined());
    assert!(q.check_finalized(200).is_empty(), "never tombstoned twice");
    assert!(q.is_empty());

    {
        let mut journal = QuarantineJournal::open(&path).expect("reopen");
        let resolution = if dead[0].1.mined() {
            Resolution::MinedFinalized
        } else {
            Resolution::SlotTakenFinalized
        };
        journal
            .record_resolve(dead[0].0.hash, resolution, now_ms())
            .expect("resolve");
    }
    assert!(read_pending(&path).expect("final fold").pending.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// A frame received an arbitrarily long time ago (six hours, or six years) is
/// never dropped for age: the journal keeps it and the FSM keeps it tracked.
#[test]
fn parked_frame_to_event_round_trips_the_full_wire() {
    let original = event(9, 42, 1_700_000_000_123);
    let record = ParkRecord::new(&original, 40, now_ms());
    let frame = record.to_parked_frame().expect("frame");
    assert_eq!(frame.chain_id, original.chain_id);
    assert_eq!(frame.tx_type, original.tx_type);
    assert_eq!(frame.access_list, original.access_list);
    assert_eq!(frame.received_unix_ms, original.received_unix_ms);
    assert_eq!(frame.claimed_nonce, original.nonce);
    assert_eq!(frame.expected_at_capture, 40);
    assert_eq!(
        frame.to_event(),
        original,
        "to_event is lossless against the source feed event"
    );
}

#[test]
fn a_very_late_frame_is_never_dropped_for_age_or_pool_absence() {
    let dir = scratch("late-frame");
    let path = dir.join(JOURNAL_FILE_NAME);
    // received_unix_ms = 1: an arbitrarily ancient receipt.
    let ancient = ParkRecord::new(&event(3, 30, 1), 28, now_ms());
    {
        let mut journal = QuarantineJournal::open(&path).expect("open");
        journal.record_park(&ancient).expect("park");
    }
    let read = read_pending(&path).expect("read");
    assert_eq!(read.pending[0].original_received_unix_ms(), 1);

    let mut q = Quarantine::new();
    let frame = read.pending[0].to_parked_frame().expect("frame");
    let frame_hash = frame.hash;
    q.push(frame);
    for _ in 0..10 {
        let out = q.poll(SENDER, 28, &[]);
        assert!(matches!(out[0].1, QuarantineDecision::StillWaiting { .. }));
    }
    assert_eq!(q.state(frame_hash), Some(FrameState::Tracked));

    let _ = std::fs::remove_dir_all(&dir);
}
