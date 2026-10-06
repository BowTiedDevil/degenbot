//! Offline end-to-end replay of the pool-updater chunk loop over a committed
//! cassette (ADR-068).
//!
//! This is the replay half of the golden-capture harness: the committed
//! cassette (the repo-root corpus home's
//! `tests/fixtures/cassettes/pool_update_chunk_26102622-26102626.json` —
//! machine-emitted by the `record_updater_cassette` example against the live
//! node, drift-gated) serves `run_pool_update`'s full fetch surface for the
//! pinned span through the real [`AlloyProvider`] seam (D5 injection — a
//! [`CassetteReplayTransport`] wrapped as a provider, NOT a mock server and
//! NOT a socket). The chunk loop runs its real fetch → decode → apply → stamp
//! cycle with zero network.
//!
//! The seeded temp DB carries one ACTIVE `uniswap_v3` exchange whose
//! `last_update_block` pins the run's span to the cassette's recorded window;
//! the run must commit the span's pools, advance the stamp to the span end,
//! and a restarted run must be a no-op (the restart invariant over the
//! replayed surface).

#![expect(clippy::unwrap_used)]

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use alloy::primitives::Address;
use degenbot_db::DegenbotDb;
use degenbot_pool_updater::{run_pool_update, NoProgress};
use degenbot_rpc::cassette::Cassette;
use degenbot_rpc::cassette_replay::CassetteReplayTransport;
use tempfile::TempDir;

/// The committed golden capture: the Uniswap V3 factory's `PoolCreated` logs
/// plus the whole-chain V3 Mint/Burn scan over blocks 26102622..=26102626,
/// recorded against the live node by `record_updater_cassette`.
const CASSETTE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/cassettes/pool_update_chunk_26102622-26102626.json"
);

/// The Uniswap V3 factory the capture was recorded against (the recorder
/// example's documented `--factory` for this corpus file).
const V3_FACTORY: &str = "0x1F98431c8aD98523631AE4a59f267346ea31F984";

/// A temp DB with one ACTIVE `uniswap_v3` exchange stamped at `from - 1`, so
/// the run's fetch window is exactly the cassette's recorded span.
fn seeded_db(dir: &Path, chain_id: i64, from: u64, file_name: &str) -> std::path::PathBuf {
    let path = dir.join(file_name);
    let (db, _state) = DegenbotDb::open_for_writes(&path).unwrap();
    let factory: Address = V3_FACTORY.parse().unwrap();
    let exchange = db
        .upsert_exchange(chain_id, "uniswap_v3", factory, None)
        .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE exchanges SET active = 1, last_update_block = ?1 WHERE id = ?2",
        rusqlite::params![i64::try_from(from - 1).unwrap(), exchange.id,],
    )
    .unwrap();
    path
}

fn committed_last_update_block(path: &Path, chain_id: i64) -> i64 {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.query_row(
        "SELECT MAX(last_update_block) FROM exchanges WHERE chain_id = ?1 AND active = 1",
        [chain_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn committed_pool_count(path: &Path, chain_id: i64) -> i64 {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM pools WHERE chain = ?1",
        [chain_id],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn run_pool_update_commits_the_recorded_span_offline_and_restarts_clean() {
    let bytes = std::fs::read(CASSETTE_PATH).expect("the committed cassette must exist");
    let cassette = Cassette::from_json_bytes(&bytes).expect("a valid v1 cassette");
    let chain_id = i64::try_from(cassette.chain_id).unwrap();
    let span = cassette.provenance.span;
    assert_eq!(span.from_block, 2_610_2622);
    assert_eq!(span.to_block, 2_610_2626);

    // D5 injection: the replay transport presents as a live AlloyProvider —
    // the chunk loop runs unchanged, and its answers come only from the
    // ledger (no socket exists to dial).
    let provider = CassetteReplayTransport::new(cassette).as_alloy_provider();

    let dir = TempDir::new().unwrap();
    let path = seeded_db(dir.path(), chain_id, span.from_block, "replay.db");

    // One chunk covering the whole recorded span (chunk_size > span width).
    let report = run_pool_update(
        &path,
        chain_id,
        Some(span.to_block),
        span.to_block - span.from_block + 1,
        provider.clone(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        // The verification gate stays OFF: the corpus records the fetch
        // surface (getLogs + block tags), not the gate's per-pool tick reads.
        false,
        None,
        false,
    )
    .expect("the replayed run must commit cleanly");

    assert_eq!(report.chain_id, chain_id);
    assert_eq!(
        report.from_block, span.from_block,
        "the run starts at the pin"
    );
    assert_eq!(
        report.to_block, span.to_block,
        "the run advances to the pin"
    );
    assert_eq!(
        report.chunks_committed, 1,
        "the whole recorded span is one chunk"
    );
    assert_eq!(
        report.total_pools_written, 1,
        "the committed corpus carries exactly one PoolCreated event in the span — a 0 here would mean decode/apply never ran"
    );
    assert_eq!(
        i64::try_from(report.total_pools_written).unwrap(),
        committed_pool_count(&path, chain_id),
        "the report's pool count is exactly what the apply committed"
    );
    assert_eq!(
        committed_last_update_block(&path, chain_id),
        i64::try_from(span.to_block).unwrap(),
        "the stamp advanced to the span end (the restart cursor)"
    );

    // Restart no-op: the committed stamp roots the second run's cursor past
    // the pin, so it commits nothing and advances nothing — the restart
    // invariant over the replayed surface.
    let restart = run_pool_update(
        &path,
        chain_id,
        Some(span.to_block),
        span.to_block - span.from_block + 1,
        provider,
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        false,
        None,
        false,
    )
    .expect("the restarted run must be a clean no-op");
    assert_eq!(restart.chunks_committed, 0, "nothing left to commit");
    assert_eq!(restart.total_pools_written, 0);
    assert_eq!(
        committed_last_update_block(&path, chain_id),
        i64::try_from(span.to_block).unwrap(),
        "the stamp did not move past the pin"
    );
}
