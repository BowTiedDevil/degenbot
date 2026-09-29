//! `Bot::load_snapshot_from_db` seed-block tests. Relocated to the host
//! crate: they drive `Bot` (the orchestrator facade, which stays here) over
//! the substrate `BotState` (ADR-067). Fixture-skip + killswitch pin unchanged.

use crate::bot_core::Bot;
use degenbot_substrate::state_lock::LockSite;

/// `Bot::load_snapshot_from_db` against the parity
/// fixture DB (`crates/foundation/degenbot-db/tests/fixtures/parity.db`) — opens a
/// `SnapshotDb` (held read tx) + records `S = min(newest_update_block(V3),
/// V4)` read INSIDE the held tx. The `SnapshotStore` is NOT populated
/// (the Store is retired; the held tx replaces it).
#[test]
fn load_snapshot_from_db_populates_store_and_seed_block() {
    use degenbot_db::snapshot::TickMapDb;
    use std::path::PathBuf;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../foundation/degenbot-db/tests/fixtures/parity.db");
    if !fixture.exists() {
        // The fixture lives in the sibling crate; skip if absent.
        eprintln!("skipping: parity fixture not at {}", fixture.display());
        return;
    }
    // Pin the ADR-052 D1 heal-at-open killswitch so this read never rewrites
    // the committed fixture.
    std::env::set_var(degenbot_db::AUTO_HEAL_ENV, "0");
    let (snap, _state) = degenbot_db::snapshot_db::SnapshotDb::open(&fixture).unwrap();
    let bot = Bot::new(8453);
    bot.load_snapshot_from_db(&snap, 8453).unwrap();

    let state = bot.state_arc();
    let core = state.read_at(LockSite::Core);
    // S = min(newest V3, newest V4). The parity fixture records both; the
    // exact S is whatever the fixture DB carries (we assert it's Some and
    // matches the per-family min computed independently inside the SAME
    // held tx).
    let v3 = snap
        .fetch_newest_update_block(8453, degenbot_db::read::ExchangeFamily::V3)
        .unwrap();
    let v4 = snap
        .fetch_newest_update_block(8453, degenbot_db::read::ExchangeFamily::V4)
        .unwrap();
    let expected_s = match (v3, v4) {
        (Some(a), Some(b)) => Some(u64::try_from(a.min(b)).expect("block number non-negative")),
        (Some(a), None) => Some(u64::try_from(a).expect("block number non-negative")),
        (None, Some(b)) => Some(u64::try_from(b).expect("block number non-negative")),
        (None, None) => None,
    };
    assert_eq!(
        core.snapshot_seed_block(),
        expected_s,
        "snapshot_seed_block must be min(newest_update_block(V3), V4)"
    );
}

/// `load_snapshot_from_db` on an empty chain → no snapshot loaded,
/// S = None (cold-start path: the pump will anchor on `first_observed_block`).
#[test]
fn load_snapshot_from_db_empty_chain_is_cold_start() {
    let (snap, _state) = degenbot_db::snapshot_db::SnapshotDb::open_in_memory().unwrap();
    let bot = Bot::new(1);
    bot.load_snapshot_from_db(&snap, 1).unwrap();
    let state = bot.state_arc();
    let core = state.read_at(LockSite::Core);
    // No pools → no seed block (cold-start path: the pump anchors on
    // `first_observed_block`).
    assert_eq!(
        core.snapshot_seed_block(),
        None,
        "empty chain → no seed block (cold start)"
    );
}
