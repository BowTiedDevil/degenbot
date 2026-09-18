//! Journal round-trip + frame-liveness convergence: parks and their tentative
//! classifications survive a reload, a restart mid-finality-window still sees
//! the frame as tentative, finality writes a tombstone exactly once, and no
//! frame is ever dropped for age or pool absence.

#![expect(
    clippy::expect_used,
    reason = "integration fixtures and assertions fail loudly"
)]

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, Bytes, B256, U256};
use degenbot_rpc::backrun_feed::BackrunFeedEvent;
use degenbot_submission::gap_quarantine::{
    FrameState, NonceConsumed, Quarantine, QuarantineDecision,
};
use degenbot_submission::gap_quarantine_journal::{
    compact, corrupt_sidecar_path, read_pending, ParkRecord, QuarantineJournal, RecordFidelity,
    Resolution, TentativeRecord, JOURNAL_FILE_NAME,
};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "degenbot-quarantine-roundtrip-{}-{tag}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
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

/// Byte-subslice containment used by the sidecar byte-exactness pins.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
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

/// A hand-written pre-versioning park line missing fields the current schema
/// requires.
fn legacy_park_line(hash_byte: u8, claimed_nonce: u64, expected_nonce: u64) -> String {
    let h = hash(hash_byte);
    format!(
        "{{\"kind\":\"park\",\"frame\":{{\"hash\":\"{h}\",\"from\":\"{SENDER}\",\"nonce\":{claimed_nonce},\"value\":\"0x7b\"}},\"sender\":\"{SENDER}\",\"claimed_nonce\":{claimed_nonce},\"expected_nonce\":{expected_nonce}}}"
    )
}

/// A legacy park line carrying no `v`, missing fields the current schema
/// requires, still folds as a tracked alive frame.
#[test]
fn legacy_park_without_version_folds_as_tracked_alive() {
    let dir = scratch("legacy-park");
    let path = dir.join(JOURNAL_FILE_NAME);
    std::fs::write(&path, format!("{}\n", legacy_park_line(1, 12, 10))).expect("write");

    let read = read_pending(&path).expect("read");
    assert_eq!(read.migrated, 1);
    assert_eq!(read.degraded, 1);
    assert_eq!(read.skipped, 0);
    assert_eq!(read.pending.len(), 1);
    let record = &read.pending[0];
    assert_eq!(record.frame.hash, hash(1).to_string());
    assert_eq!(record.claimed_nonce, 12);
    assert_eq!(record.expected_nonce, 10);
    assert_eq!(record.fidelity, RecordFidelity::Degraded);
    let frame = record.to_parked_frame().expect("frame");
    assert_eq!(frame.claimed_nonce, 12);
    assert!(frame.is_degraded());

    let _ = std::fs::remove_dir_all(&dir);
}

/// A garbage line is excluded from pending, preserved verbatim in the
/// `.corrupt` sidecar, and survives a read+compact cycle.
#[test]
fn corrupt_line_is_preserved_in_sidecar_and_survives_compaction() {
    let dir = scratch("corrupt-sidecar");
    let path = dir.join(JOURNAL_FILE_NAME);
    {
        let mut journal = QuarantineJournal::open(&path).expect("open");
        journal
            .record_park(&ParkRecord::new(&event(1, 12, 1_000), 10, now_ms()))
            .expect("park");
    }
    let junk = "}}} not json {{{";
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("reopen")
        .write_all(format!("{junk}\n").as_bytes())
        .expect("append junk");

    let read = read_pending(&path).expect("read");
    assert_eq!(read.skipped, 1);
    assert_eq!(read.pending.len(), 1);

    let sidecar = corrupt_sidecar_path(&path);
    let preserved = std::fs::read_to_string(&sidecar).expect("sidecar");
    assert!(
        preserved.lines().any(|line| line == junk),
        "the rejected line is preserved verbatim"
    );

    compact(&path, &read.pending).expect("compact");
    let after = std::fs::read_to_string(&sidecar).expect("sidecar after compact");
    assert!(
        after.lines().any(|line| line == junk),
        "compaction does not destroy the sidecar evidence"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Every line this binary writes carries the explicit schema version.
#[test]
fn current_records_serialize_with_explicit_version_two() {
    let dir = scratch("version-marker");
    let path = dir.join(JOURNAL_FILE_NAME);
    {
        let mut journal = QuarantineJournal::open(&path).expect("open");
        journal
            .record_park(&ParkRecord::new(&event(1, 12, 1_000), 10, now_ms()))
            .expect("park");
    }
    let text = std::fs::read_to_string(&path).expect("read file");
    let value: serde_json::Value =
        serde_json::from_str(text.lines().next().expect("line")).expect("json");
    assert_eq!(
        value.get("v").and_then(serde_json::Value::as_u64),
        Some(2),
        "the version marker is explicit"
    );
    assert_eq!(
        value.get("kind").and_then(serde_json::Value::as_str),
        Some("park")
    );

    let read = read_pending(&path).expect("read");
    assert_eq!(read.pending.len(), 1);
    assert_eq!(read.skipped, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Resolve removes only its own hash, a legacy park survives tracked, junk
/// lands in the sidecar, and compaction rewrites to current-version parks.
#[test]
fn mixed_journal_folds_and_normalizes_to_current_version() {
    let dir = scratch("mixed");
    let path = dir.join(JOURNAL_FILE_NAME);
    {
        let mut journal = QuarantineJournal::open(&path).expect("open");
        journal
            .record_park(&ParkRecord::new(&event(1, 12, 1_000), 10, now_ms()))
            .expect("park");
        journal
            .record_resolve(hash(1), Resolution::MinedFinalized, now_ms())
            .expect("resolve");
    }
    let legacy = legacy_park_line(2, 22, 20);
    let junk = "{\"kind\":\"park\", broken";
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("reopen")
        .write_all(format!("{legacy}\n{junk}\n").as_bytes())
        .expect("append");

    let read = read_pending(&path).expect("read");
    assert_eq!(read.skipped, 1);
    assert_eq!(read.migrated, 1);
    assert_eq!(read.pending.len(), 1);
    assert_eq!(read.pending[0].frame.hash, hash(2).to_string());

    compact(&path, &read.pending).expect("compact");
    let text = std::fs::read_to_string(&path).expect("journal");
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "compaction keeps exactly the surviving parks"
    );
    let value: serde_json::Value = serde_json::from_str(lines[0]).expect("json");
    assert_eq!(
        value.get("v").and_then(serde_json::Value::as_u64),
        Some(2),
        "compaction normalizes to the current version"
    );
    assert_eq!(
        value
            .get("frame")
            .and_then(|frame| frame.get("hash"))
            .and_then(serde_json::Value::as_str),
        Some(hash(2).to_string().as_str())
    );

    let sidecar = std::fs::read_to_string(corrupt_sidecar_path(&path)).expect("sidecar");
    assert!(sidecar.contains(junk), "junk is preserved in the sidecar");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A park from a future schema version is migrated, never destroyed.
#[test]
fn future_version_park_is_migrated_not_destroyed() {
    let dir = scratch("future");
    let path = dir.join(JOURNAL_FILE_NAME);
    let line = format!(
        "{{\"v\":7,\"kind\":\"park\",\"frame\":{{\"hash\":\"{}\",\"from\":\"{SENDER}\",\"nonce\":33}},\"claimed_nonce\":33,\"expected_nonce\":30}}",
        hash(3)
    );
    std::fs::write(&path, format!("{line}\n")).expect("write");

    let read = read_pending(&path).expect("read");
    assert_eq!(read.skipped, 0);
    assert_eq!(read.migrated, 1);
    assert_eq!(read.pending.len(), 1);
    assert_eq!(read.pending[0].frame.hash, hash(3).to_string());

    let _ = std::fs::remove_dir_all(&dir);
}

/// A full-fidelity legacy park rebuilds the wire fields it carried.
#[test]
fn full_fidelity_legacy_park_rebuilds_the_present_wire_fields() {
    let dir = scratch("legacy-fidelity");
    let path = dir.join(JOURNAL_FILE_NAME);
    let h = hash(4);
    let line = format!(
        "{{\"kind\":\"park\",\"frame\":{{\"hash\":\"{h}\",\"from\":\"{SENDER}\",\"to\":\"{}\",\"nonce\":44,\"chain_id\":1,\"value\":\"0x7b\",\"data\":\"0xabcd\",\"gas\":300000,\"max_fee_per_gas\":\"514684409\",\"max_priority_fee_per_gas\":\"1000000000\",\"tx_type\":2,\"received_unix_ms\":1700000000000,\"access_list\":[]}},\"sender\":\"{SENDER}\",\"claimed_nonce\":44,\"expected_nonce\":40}}",
        Address::with_last_byte(9)
    );
    std::fs::write(&path, format!("{line}\n")).expect("write");

    let read = read_pending(&path).expect("read");
    assert_eq!(read.pending.len(), 1);
    let frame = read.pending[0].to_parked_frame().expect("frame");
    let event_out = frame.to_event();
    assert_eq!(event_out.hash, h);
    assert_eq!(event_out.from, SENDER);
    assert_eq!(event_out.nonce, 44);
    assert_eq!(event_out.to, Some(Address::with_last_byte(9)));
    assert_eq!(event_out.gas, 300_000);
    assert_eq!(event_out.data, Bytes::from(vec![0xab, 0xcd]));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A park-shaped line whose typed fields fail to parse (any version) is never
/// junk: it migrates as a degraded park and survives compaction.
#[test]
fn current_version_park_shaped_parse_failure_migrates_not_sidecar() {
    let dir = scratch("v2-park-parse-failure");
    let path = dir.join(JOURNAL_FILE_NAME);
    let line = format!(
        "{{\"v\":2,\"kind\":\"park\",\"frame\":{{\"hash\":\"{}\",\"from\":\"{SENDER}\",\"nonce\":12,\"value\":\"0x7b\"}},\"sender\":\"{SENDER}\",\"claimed_nonce\":12,\"expected_nonce\":10}}",
        hash(8)
    );
    std::fs::write(&path, format!("{line}\n")).expect("write");

    let read = read_pending(&path).expect("read");
    assert_eq!(read.skipped, 0, "a park-shaped line is never junk");
    assert_eq!(read.pending.len(), 1);
    assert_eq!(read.pending[0].fidelity, RecordFidelity::Degraded);
    assert!(
        !corrupt_sidecar_path(&path).exists(),
        "a park-shaped line is never preserved only as sidecar bytes"
    );

    compact(&path, &read.pending).expect("compact");
    let reread = read_pending(&path).expect("reread");
    assert_eq!(reread.pending.len(), 1);
    assert_eq!(reread.pending[0].fidelity, RecordFidelity::Degraded);
    assert_eq!(reread.pending[0].claimed_nonce, 12);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A degraded migrated park boot-folds as tracked, never rescues at the
/// frontier, and dies only when the chain proves its nonce consumed.
#[test]
fn degraded_migrated_park_boot_folds_tracked_and_dies_by_chain_proof() {
    let dir = scratch("degraded-boot-fold");
    let path = dir.join(JOURNAL_FILE_NAME);
    std::fs::write(&path, format!("{}\n", legacy_park_line(1, 12, 10))).expect("write");

    let read = read_pending(&path).expect("read");
    assert_eq!(read.pending.len(), 1);
    let frame = read.pending[0].to_parked_frame().expect("frame");
    assert!(frame.is_degraded());
    let frame_hash = frame.hash;
    let mut q = Quarantine::new();
    q.push(frame);
    assert_eq!(q.state(frame_hash), Some(FrameState::Tracked));

    // Frontier (count == claim): a degraded frame has no wire to replay, so
    // it must never rescue into the funnel.
    let frontier = q.poll(SENDER, 12, &[10, 11]);
    assert!(
        frontier
            .iter()
            .all(|(_, decision)| !matches!(decision, QuarantineDecision::Rescue { .. })),
        "no rescue for a degraded frame"
    );
    assert_eq!(q.state(frame_hash), Some(FrameState::Tracked));

    // Head advances past the claim: the consumption classification begins.
    let consumed = q.poll(SENDER, 13, &[]);
    assert!(
        consumed.iter().any(|(f, decision)| f.hash == frame_hash
            && matches!(decision, QuarantineDecision::NonceConsumed)),
        "head past the claim classifies consumption"
    );
    assert!(q.enter_tentative(
        frame_hash,
        NonceConsumed::MinedAt {
            block: 100,
            block_hash: hash(7)
        }
    ));
    let dead = q.check_finalized(100);
    assert_eq!(dead.len(), 1);
    assert!(dead[0].1.mined(), "the chain proved the consumption");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A park whose wire fields are all defaults is DEGRADED, never Full: a
/// defaulted wire cannot smuggle a fabricated feed event into the funnel.
#[test]
fn degenerate_wire_is_degraded_never_full() {
    let dir = scratch("degenerate-wire");
    let path = dir.join(JOURNAL_FILE_NAME);
    // Missing-wire migrate path: only identity survives.
    let missing = format!(
        "{{\"kind\":\"park\",\"frame\":{{\"hash\":\"{}\",\"nonce\":45}},\"claimed_nonce\":45}}",
        hash(6)
    );
    // Current-version but all-zero wire path: typed parse succeeds.
    let zeroed = format!(
        "{{\"v\":2,\"kind\":\"park\",\"frame\":{{\"hash\":\"{}\",\"chain_id\":0,\"from\":\"0x0000000000000000000000000000000000000000\",\"to\":null,\"value\":\"0x0\",\"data\":\"0x\",\"gas\":0,\"max_fee_per_gas\":\"0\",\"max_priority_fee_per_gas\":\"0\",\"nonce\":46,\"tx_type\":0,\"received_unix_ms\":0,\"access_list\":[]}},\"sender\":\"0x0000000000000000000000000000000000000000\",\"claimed_nonce\":46,\"expected_nonce\":45,\"parked_at_unix_ms\":0}}",
        hash(7)
    );
    std::fs::write(&path, format!("{missing}\n{zeroed}\n")).expect("write");

    let read = read_pending(&path).expect("read");
    assert_eq!(read.skipped, 0);
    assert_eq!(read.pending.len(), 2);
    for record in &read.pending {
        assert_eq!(
            record.fidelity,
            RecordFidelity::Degraded,
            "a defaulted wire is degraded, never Full"
        );
        assert!(
            record.to_parked_frame().expect("frame").is_degraded(),
            "to_parked_frame agrees with the fidelity label"
        );
    }
    assert_eq!(read.degraded, 1, "the migrated line counts as degraded");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Sidecar preservation is byte-exact: leading/trailing whitespace and a BOM
/// prefix survive the round trip, and a second read+compact cycle keeps them.
#[test]
fn sidecar_preserves_raw_whitespace_and_bom_bytes() {
    let dir = scratch("sidecar-verbatim");
    let path = dir.join(JOURNAL_FILE_NAME);
    {
        let mut journal = QuarantineJournal::open(&path).expect("open");
        journal
            .record_park(&ParkRecord::new(&event(1, 12, 1_000), 10, now_ms()))
            .expect("park");
    }
    let spaced = "  }}} leading and trailing  ";
    let bom = "\u{feff}}} bom junk";
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("reopen")
        .write_all(format!("{spaced}\n{bom}\n").as_bytes())
        .expect("append");

    let read = read_pending(&path).expect("read");
    assert_eq!(read.skipped, 2);

    let sidecar = corrupt_sidecar_path(&path);
    let bytes = std::fs::read(&sidecar).expect("sidecar bytes");
    assert!(
        contains(&bytes, spaced.as_bytes()),
        "leading/trailing whitespace is preserved byte-exact"
    );
    assert!(
        contains(&bytes, bom.as_bytes()),
        "the BOM prefix is preserved byte-exact"
    );

    compact(&path, &read.pending).expect("compact");
    let reread = read_pending(&path).expect("reread");
    assert_eq!(reread.pending.len(), 1);
    let after = std::fs::read(&sidecar).expect("sidecar after compact");
    assert!(contains(&after, spaced.as_bytes()));
    assert!(contains(&after, bom.as_bytes()));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A leading BOM is stripped before parsing, so the first line is not junk.
#[test]
fn leading_bom_is_stripped_before_parsing() {
    let dir = scratch("bom-parse");
    let path = dir.join(JOURNAL_FILE_NAME);
    let line = format!("\u{feff}{}", legacy_park_line(5, 52, 50));
    std::fs::write(&path, format!("{line}\n")).expect("write");

    let read = read_pending(&path).expect("read");
    assert_eq!(read.skipped, 0, "a BOM must not junk the first line");
    assert_eq!(read.pending.len(), 1);
    assert_eq!(read.pending[0].claimed_nonce, 52);
    assert!(!corrupt_sidecar_path(&path).exists());

    let _ = std::fs::remove_dir_all(&dir);
}
