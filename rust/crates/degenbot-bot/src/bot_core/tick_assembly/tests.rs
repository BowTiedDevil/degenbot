//! Tests for `assemble_v3_tick_map` / `assemble_v4_tick_map` (Candidate 1 / UHPXSD).
//!
//! Six branches per family, per the ME7I5P acceptance criteria:
//!  1. Store hit  → `Ok(Some((ticks, Tracked)))`, store entry consumed.
//!  2. Store miss + Db hit (non-empty)  → `Ok(Some((ticks, Tracked)))`.
//!  3. Store miss + Db hit (empty map) → `Ok(None)`.
//!  4. Store miss + Db miss (`Ok(None)`) → `Ok(None)`.
//!  5. Store miss + Db error (`Err`) → `Err(DbError)` (propagated — Decision 8 (A)).
//!  6. `db = None` (cold-start) → Store arm only; miss if store is empty.

use std::collections::HashMap;

use alloy::primitives::{aliases::U128, Address, I256, U256};
use degenbot_db::connection::DegenbotDb;
use degenbot_db::discovery::{V3PoolRowInput, V4PoolRowInput};
use degenbot_db::{ApplyBitmapAtWord, ApplyLiquidityAtTick};

use super::{assemble_v3_tick_map, assemble_v4_tick_map};
use crate::bot_core::snapshot_verify::SnapshotStore;
use crate::bot_core::{PoolTickCoverage, TickInfo};

// ── fixtures ──────────────────────────────────────────────────────────────

const CHAIN: i64 = 1;

fn make_pool_addr() -> Address {
    Address::from([0xaa; 20])
}
fn make_token0() -> Address {
    Address::from([0x11; 20])
}
fn make_token1() -> Address {
    Address::from([0x22; 20])
}
fn make_factory() -> Address {
    Address::from([0xdd; 20])
}
fn make_manager() -> Address {
    Address::from([0xbb; 20])
}

/// A tick that's easy to assert against in the hit cases.
fn sample_tick_info(tick: i32) -> TickInfo {
    TickInfo {
        liquidity_gross: U128::from(1_000_000u64 * u64::from(tick.unsigned_abs()) + 1),
        liquidity_net: I256::try_from(i64::from(tick) * 1_000).unwrap(),
        block: 0,
    }
}

/// Seed a V3 pool in the DB; return the DB handle + internal `pool_id` so the
/// test can also seed ticks (init maps + liq positions) and the helper's
/// `fetch_liquidity_map` will find them.
fn v3_db_with_pool() -> (DegenbotDb, i64) {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let factory = make_factory();
    db.upsert_exchange(CHAIN, "uniswap_v3", factory, None)
        .unwrap();
    let pool_address = make_pool_addr();
    db.upsert_v3_pools(
        CHAIN,
        "uniswap_v3",
        1, // the first-inserted exchange's id; mirrors liquidity_updater tests.
        1_000_000,
        &[V3PoolRowInput {
            address: pool_address,
            token0_address: make_token0(),
            token1_address: make_token1(),
            fee: 0,
            tick_spacing: 10,
        }],
    )
    .unwrap();
    let addr_s = pool_address.to_checksum(None);
    let pool_id: i64 = {
        let conn = db.lock();
        conn.query_row(
            "SELECT id FROM pools WHERE address = ?1 LIMIT 1",
            [&addr_s],
            |r| r.get(0),
        )
        .unwrap()
    };
    (db, pool_id)
}

/// Seed a V4 manager + pool in the DB; return the DB handle + `managed_pool_id` + `pool_id_hash`.
fn v4_db_with_pool() -> (DegenbotDb, i64, [u8; 32]) {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let manager = make_manager();
    db.upsert_exchange(CHAIN, "uniswap_v4", manager, None)
        .unwrap();
    db.upsert_pool_manager(manager, CHAIN, "uniswap_v4", None, 1)
        .unwrap();
    let pool_id: [u8; 32] = [0xcc; 32];
    let mut pool_hash = String::from("0x");
    for b in pool_id {
        use std::fmt::Write as _;
        let _ = write!(pool_hash, "{b:02x}");
    }
    db.upsert_v4_pools(
        CHAIN,
        &manager.to_checksum(None),
        1_000_000,
        &[V4PoolRowInput {
            pool_hash: pool_hash.clone(),
            hooks: Address::ZERO,
            currency0_address: make_token0(),
            currency1_address: make_token1(),
            fee: 0,
            tick_spacing: 10,
        }],
    )
    .unwrap();
    let managed_pool_id: i64 = {
        let conn = db.lock();
        conn.query_row(
            "SELECT managed_pool_id FROM uniswap_v4_pools WHERE pool_hash = ?1",
            [&pool_hash],
            |r| r.get(0),
        )
        .unwrap()
    };
    (db, managed_pool_id, pool_id)
}

/// Seed V3 ticks (both the init map + the liquidity position) — the 1:1 shape
/// a healthy DB carries.
fn seed_v3_ticks(db: &DegenbotDb, pool_id: i64, ticks: &[i32]) {
    let mut tick_bitmap: HashMap<i32, ApplyBitmapAtWord> = HashMap::new();
    let mut tick_data: HashMap<i32, ApplyLiquidityAtTick> = HashMap::new();
    for &tick in ticks {
        let info = sample_tick_info(tick);
        tick_data.insert(
            tick,
            ApplyLiquidityAtTick {
                liquidity_gross: info.liquidity_gross,
                liquidity_net: info.liquidity_net,
                block: 0,
            },
        );
        tick_bitmap.entry(0).or_insert(ApplyBitmapAtWord {
            bitmap: U256::from(1u64),
            block: 0,
        });
    }
    db.upsert_v3_liquidity_positions(pool_id, &tick_data)
        .unwrap();
    db.upsert_v3_initialization_maps(pool_id, &tick_bitmap)
        .unwrap();
}

/// Seed V4 ticks (`managed_pool_id` version of `seed_v3_ticks`).
fn seed_v4_ticks(db: &DegenbotDb, managed_pool_id: i64, ticks: &[i32]) {
    let mut tick_bitmap: HashMap<i32, ApplyBitmapAtWord> = HashMap::new();
    let mut tick_data: HashMap<i32, ApplyLiquidityAtTick> = HashMap::new();
    for &tick in ticks {
        let info = sample_tick_info(tick);
        tick_data.insert(
            tick,
            ApplyLiquidityAtTick {
                liquidity_gross: info.liquidity_gross,
                liquidity_net: info.liquidity_net,
                block: 0,
            },
        );
        tick_bitmap.entry(0).or_insert(ApplyBitmapAtWord {
            bitmap: U256::from(1u64),
            block: 0,
        });
    }
    db.upsert_v4_liquidity_positions(managed_pool_id, &tick_data)
        .unwrap();
    db.upsert_v4_initialization_maps(managed_pool_id, &tick_bitmap)
        .unwrap();
}

/// A no-op store probe that reports a miss (Sparse, empty ticks).
fn store_miss() -> (HashMap<i32, TickInfo>, PoolTickCoverage) {
    (HashMap::new(), PoolTickCoverage::Sparse)
}

// ── V3 tests ──────────────────────────────────────────────────────────────

#[test]
fn v3_store_hit_returns_tracked_ticks_and_consumes_entry() {
    let (db, _pool_id) = v3_db_with_pool();
    let store: SnapshotStore<Address> = SnapshotStore::new();
    let addr = make_pool_addr();
    let seed: HashMap<i32, TickInfo> = [(10, sample_tick_info(10))].into_iter().collect();
    store.load({
        let mut m = HashMap::new();
        m.insert(addr, seed.clone());
        m
    });

    let result = assemble_v3_tick_map(|| store.take(&addr), Some(&db), addr).unwrap();
    let Some((ticks, coverage)) = result else {
        panic!("Store hit should return Some");
    };
    assert_eq!(coverage, PoolTickCoverage::Tracked);
    assert_eq!(ticks, seed);

    // Store entry was consumed by `take` — a second probe returns Sparse.
    let (again, cov) = store.take(&addr);
    assert_eq!(cov, PoolTickCoverage::Sparse);
    assert!(again.is_empty());
}

#[test]
fn v3_store_miss_db_hit_non_empty_returns_tracked_ticks() {
    let (db, pool_id) = v3_db_with_pool();
    seed_v3_ticks(&db, pool_id, &[10, 20]);
    let addr = make_pool_addr();

    let result = assemble_v3_tick_map(store_miss, Some(&db), addr).unwrap();
    let Some((ticks, coverage)) = result else {
        panic!("Db hit should return Some");
    };
    assert_eq!(coverage, PoolTickCoverage::Tracked);
    assert_eq!(ticks.len(), 2);
    assert_eq!(ticks.get(&10), Some(&sample_tick_info(10)));
    assert_eq!(ticks.get(&20), Some(&sample_tick_info(20)));
}

#[test]
fn v3_store_miss_db_hit_empty_map_returns_miss() {
    // Pool exists in the DB but has no init/liq rows — both tick_bitmap and
    // tick_data are empty, so the helper returns Ok(None) (Python's
    // `if not init_maps or not liq_positions: return ..., False` heuristic).
    let (db, _pool_id) = v3_db_with_pool();

    let result = assemble_v3_tick_map(store_miss, Some(&db), make_pool_addr()).unwrap();
    assert!(result.is_none(), "empty LiquidityMap → miss");
}

#[test]
fn v3_store_miss_db_miss_pool_not_found_returns_miss() {
    // A fresh DB with no pools at all — `fetch_liquidity_map` returns Ok(None).
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();

    let result = assemble_v3_tick_map(store_miss, Some(&db), make_pool_addr()).unwrap();
    assert!(result.is_none(), "pool not in DB → miss");
}

#[test]
fn v3_store_miss_db_error_is_propagated_not_swallowed() {
    // Corrupt the `liquidity_gross` column with an undecodable string. The Db
    // query returns `Err(DbError::Decode)`; the helper MUST propagate it
    // (Decision 8 (A) — behavior change from Python's contextlib.suppress).
    let (db, pool_id) = v3_db_with_pool();
    seed_v3_ticks(&db, pool_id, &[10]);
    {
        let conn = db.lock();
        conn.execute(
            "UPDATE liquidity_positions SET liquidity_gross = 'not-a-number' \
             WHERE pool_id = ?1",
            [pool_id],
        )
        .unwrap();
    }

    let result = assemble_v3_tick_map(store_miss, Some(&db), make_pool_addr());
    assert!(
        result.is_err(),
        "Db error must propagate (Decision 8 (A)) — helper must NOT swallow"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, degenbot_db::DbError::Decode(_)),
        "expected DbError::Decode from the bad VARCHAR, got {err:?}"
    );
}

#[test]
fn v3_db_none_cold_start_returns_store_result_only() {
    // Cold-start path: no Db handle. A store hit still returns Some.
    let store: SnapshotStore<Address> = SnapshotStore::new();
    let addr = make_pool_addr();
    let seed: HashMap<i32, TickInfo> = [(10, sample_tick_info(10))].into_iter().collect();
    store.load({
        let mut m = HashMap::new();
        m.insert(addr, seed.clone());
        m
    });

    let result = assemble_v3_tick_map(|| store.take(&addr), None, addr).unwrap();
    let Some((ticks, coverage)) = result else {
        panic!("Store hit with db=None should still return Some");
    };
    assert_eq!(coverage, PoolTickCoverage::Tracked);
    assert_eq!(ticks, seed);
}

#[test]
fn v3_db_none_cold_start_store_empty_returns_miss() {
    let result = assemble_v3_tick_map(store_miss, None, make_pool_addr()).unwrap();
    assert!(result.is_none(), "cold-start + empty store → miss");
}

// ── V4 tests ──────────────────────────────────────────────────────────────

#[test]
fn v4_store_hit_returns_tracked_ticks_and_consumes_entry() {
    let (db, _managed_id, pool_id) = v4_db_with_pool();
    let store: SnapshotStore<(Address, [u8; 32])> = SnapshotStore::new();
    let mgr = make_manager();
    let key = (mgr, pool_id);
    let seed: HashMap<i32, TickInfo> = [(10, sample_tick_info(10))].into_iter().collect();
    store.load({
        let mut m = HashMap::new();
        m.insert(key, seed.clone());
        m
    });

    let result = assemble_v4_tick_map(|| store.take(&key), Some(&db), mgr, pool_id).unwrap();
    let Some((ticks, coverage)) = result else {
        panic!("Store hit should return Some");
    };
    assert_eq!(coverage, PoolTickCoverage::Tracked);
    assert_eq!(ticks, seed);

    let (again, cov) = store.take(&key);
    assert_eq!(cov, PoolTickCoverage::Sparse);
    assert!(again.is_empty());
}

#[test]
fn v4_store_miss_db_hit_non_empty_returns_tracked_ticks() {
    let (db, managed_id, pool_id) = v4_db_with_pool();
    seed_v4_ticks(&db, managed_id, &[-10, 10]);
    let mgr = make_manager();

    let result = assemble_v4_tick_map(store_miss, Some(&db), mgr, pool_id).unwrap();
    let Some((ticks, coverage)) = result else {
        panic!("Db hit should return Some");
    };
    assert_eq!(coverage, PoolTickCoverage::Tracked);
    assert_eq!(ticks.len(), 2);
    assert_eq!(ticks.get(&-10), Some(&sample_tick_info(-10)));
    assert_eq!(ticks.get(&10), Some(&sample_tick_info(10)));
}

#[test]
fn v4_store_miss_db_hit_empty_map_returns_miss() {
    let (db, _managed_id, pool_id) = v4_db_with_pool();
    let result = assemble_v4_tick_map(store_miss, Some(&db), make_manager(), pool_id).unwrap();
    assert!(result.is_none(), "empty LiquidityMap → miss");
}

#[test]
fn v4_store_miss_db_miss_pool_not_found_returns_miss() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let pool_id = [0xee; 32];
    let result = assemble_v4_tick_map(store_miss, Some(&db), make_manager(), pool_id).unwrap();
    assert!(result.is_none(), "pool not in DB → miss");
}

#[test]
fn v4_store_miss_db_error_is_propagated_not_swallowed() {
    let (db, managed_id, pool_id) = v4_db_with_pool();
    seed_v4_ticks(&db, managed_id, &[10]);
    {
        let conn = db.lock();
        conn.execute(
            "UPDATE managed_pool_liquidity_positions SET liquidity_gross = 'bad' \
             WHERE managed_pool_id = ?1",
            [managed_id],
        )
        .unwrap();
    }

    let result = assemble_v4_tick_map(store_miss, Some(&db), make_manager(), pool_id);
    assert!(result.is_err(), "Db error must propagate (Decision 8 (A))");
    let err = result.unwrap_err();
    assert!(
        matches!(err, degenbot_db::DbError::Decode(_)),
        "expected DbError::Decode, got {err:?}"
    );
}

#[test]
fn v4_db_none_cold_start_store_empty_returns_miss() {
    let pool_id = [0xee; 32];
    let result = assemble_v4_tick_map(store_miss, None, make_manager(), pool_id).unwrap();
    assert!(result.is_none(), "cold-start + empty store → miss");
}
