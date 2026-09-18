//! Journal round-trip: parks and tombstones survive a reload, pending frames
//! re-enter the live quarantine FSM, and frames whose gap closed while the
//! process was down are dropped at boot.

#![expect(
    clippy::expect_used,
    reason = "integration fixtures and assertions fail loudly"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, Bytes, B256, U256};
use degenbot_rpc::backrun_feed::BackrunFeedEvent;
use degenbot_submission::gap_quarantine::{Quarantine, QuarantineDecision};
use degenbot_submission::gap_quarantine_journal::{
    compact, read_pending, ParkRecord, QuarantineJournal, Resolution, JOURNAL_FILE_NAME,
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

#[test]
fn journal_round_trips_pending_parks_into_the_live_fsm() {
    let dir = scratch("roundtrip");
    let path = dir.join(JOURNAL_FILE_NAME);

    // Session 1: two parks, one already resolved by head closure.
    {
        let mut journal = QuarantineJournal::open(&path).expect("open");
        let closed = ParkRecord::new(&event(1, 12, 1_000), 10, now_ms());
        let pending = ParkRecord::new(&event(2, 20, 2_000), 18, now_ms());
        journal.record_park(&closed).expect("park closed");
        journal.record_park(&pending).expect("park pending");
        journal
            .record_resolve(
                closed.frame.hash.parse().expect("hash"),
                Resolution::ClosedByHead,
                now_ms(),
            )
            .expect("resolve");
    }

    // Boot: the tombstone removed the first park; the second is still pending.
    let read = read_pending(&path).expect("read");
    assert_eq!(read.skipped, 0);
    assert_eq!(read.pending.len(), 1, "only the unresolved park remains");
    let record = &read.pending[0];
    assert_eq!(record.claimed_nonce, 20);
    assert_eq!(record.original_received_unix_ms(), 2_000);

    // Compaction on boot keeps exactly the re-parked set.
    compact(&path, &read.pending).expect("compact");
    assert_eq!(read_pending(&path).expect("reread").pending.len(), 1);

    // Re-park into the live FSM: head is at 18, so the gap [18, 20) is not yet
    // closed and the pool view knows both predecessors -> Rescue.
    let reload = Instant::now();
    let mut quarantine = Quarantine::new();
    quarantine.push(record.to_parked_frame(reload).expect("parked frame"));
    let out = quarantine.poll(SENDER, 18, &[18, 19], Instant::now());
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].1,
        QuarantineDecision::Rescue {
            predecessors: vec![18, 19]
        }
    );
    assert!(quarantine.is_empty(), "a rescued frame leaves the FSM");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reload_drops_frames_whose_gap_closed_while_down() {
    let dir = scratch("drop");
    let path = dir.join(JOURNAL_FILE_NAME);

    {
        let mut journal = QuarantineJournal::open(&path).expect("open");
        let settled = ParkRecord::new(&event(3, 30, 3_000), 28, now_ms());
        journal.record_park(&settled).expect("park");
    }

    let read = read_pending(&path).expect("read");
    assert_eq!(read.pending.len(), 1);

    // Boot probe: the account nonce already reached the claimed nonce.
    let head_nonce = 30u64;
    let kept: Vec<ParkRecord> = read
        .pending
        .iter()
        .filter(|record| !record.gap_closed_while_down(head_nonce))
        .cloned()
        .collect();
    assert!(kept.is_empty(), "a settled gap never re-enters the FSM");

    // Compaction records the drop: the journal is empty after boot.
    compact(&path, &kept).expect("compact");
    assert!(read_pending(&path).expect("reread").pending.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}
