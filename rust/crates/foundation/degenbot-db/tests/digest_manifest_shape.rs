//! Shape conformance for the committed completed-market record
//! (`tests/fixtures/sql_goldens/mainnet_completed_market.manifest.json`).
//! A regeneration that changes the record's SHAPE fails here instead of
//! drifting silently; the digest bytes themselves are the drive's outcome,
//! re-pinned per run.

#![expect(clippy::expect_used, clippy::panic)]

use serde_json::Value;

use degenbot_db::digest::{MARKET_DIGEST_SERIALIZATION, MARKET_DIGEST_TABLES};

/// The committed record, at the repo root (four levels above this crate).
const MANIFEST_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/sql_goldens/mainnet_completed_market.manifest.json"
);

/// The committed record's parsed bytes.
fn load_committed_record() -> Value {
    let raw = std::fs::read_to_string(MANIFEST_PATH).unwrap_or_else(|error| {
        panic!("the committed completed-market manifest must be readable: {error}")
    });
    serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("the committed manifest must parse as JSON: {error}"))
}

#[test]
fn the_committed_record_carries_the_market_and_drive_facts() {
    let record = load_committed_record();

    // Exactly three top-level keys, sorted (the writer's canonical layout).
    let top = record.as_object().expect("the manifest is a JSON object");
    let top_keys: Vec<&str> = top.keys().map(String::as_str).collect();
    assert_eq!(top_keys, ["market", "serialization", "tables"]);

    // The writer's serialization stamp names the pipeline that produced the
    // digests.
    assert_eq!(
        record.get("serialization").and_then(Value::as_str),
        Some(MARKET_DIGEST_SERIALIZATION),
        "the manifest must carry the digest writer's serialization version"
    );

    // The market block: the completed drive's facts.
    let market = record
        .get("market")
        .and_then(Value::as_object)
        .expect("the manifest carries a market block");
    let market_keys: Vec<&str> = market.keys().map(String::as_str).collect();
    assert_eq!(
        market_keys,
        [
            "active",
            "chain_id",
            "db_bytes",
            "drive",
            "last_update_block",
            "name"
        ]
    );
    assert_eq!(
        market.get("name").and_then(Value::as_str),
        Some("Aave Ethereum Market")
    );
    assert_eq!(market.get("chain_id").and_then(Value::as_i64), Some(1));
    assert_eq!(market.get("active").and_then(Value::as_bool), Some(true));
    let last_update_block = market
        .get("last_update_block")
        .and_then(Value::as_i64)
        .expect("last_update_block is an integer");
    assert!(
        market
            .get("db_bytes")
            .and_then(Value::as_u64)
            .is_some_and(|bytes| bytes > 0),
        "db_bytes is a positive integer"
    );

    // The drive block: the drive's own bookkeeping — four non-negative
    // integers + the verification policy.
    let drive = market
        .get("drive")
        .and_then(Value::as_object)
        .expect("the completed record carries a drive block");
    let drive_keys: Vec<&str> = drive.keys().map(String::as_str).collect();
    assert_eq!(
        drive_keys,
        [
            "chunks",
            "events_applied",
            "from_block",
            "to_block",
            "verify"
        ]
    );
    for key in ["chunks", "events_applied", "from_block", "to_block"] {
        assert!(
            drive.get(key).and_then(Value::as_u64).is_some(),
            "drive.{key} is a non-negative integer"
        );
    }
    assert!(
        drive
            .get("verify")
            .and_then(Value::as_str)
            .is_some_and(|policy| !policy.is_empty()),
        "drive.verify is a non-empty string"
    );

    // The drive's to_block is the cursor the market block carries.
    assert_eq!(
        drive.get("to_block").and_then(Value::as_i64),
        Some(last_update_block),
        "the drive's to_block is the market's committed cursor"
    );
}

#[test]
fn the_committed_record_digests_exactly_the_ten_market_tables() {
    let record = load_committed_record();
    let tables = record
        .get("tables")
        .and_then(Value::as_object)
        .expect("the manifest carries a tables block");
    let mut table_names: Vec<&str> = tables.keys().map(String::as_str).collect();
    table_names.sort_unstable();
    let mut expected: Vec<&str> = MARKET_DIGEST_TABLES.to_vec();
    expected.sort_unstable();
    assert_eq!(
        table_names, expected,
        "the record digests exactly the ten market tables"
    );
    for (name, entry) in tables {
        let digest = entry
            .get("digest")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("table {name} carries a digest string"));
        assert_eq!(digest.len(), 64, "table {name} digest is 64 characters");
        assert!(
            digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "table {name} digest is lowercase hex"
        );
        assert!(
            entry.get("rows").and_then(Value::as_u64).is_some(),
            "table {name} carries a non-negative row count"
        );
    }
}
