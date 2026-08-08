//! Tests for `assemble_v3_tick_map` / `assemble_v4_tick_map` (Candidate 1 / UHPXSD).
//!
//! Six branches per family, per the ME7I5P acceptance criteria:
//!  1. Store hit  → `Ok(Some((ticks, Tracked)))`, store entry consumed.
//!  2. Store miss + Db hit (non-empty)  → `Ok(Some((ticks, Tracked)))`.
//!  3. Store miss + Db hit (empty map) → `Ok(None)`.
//!  4. Store miss + Db miss (`Ok(None)`) → `Ok(None)`.
//!  5. Store miss + Db error (`Err`) → `Err(DbError)` (propagated — Decision 8 (A)).
//!  6. `db = None` (cold-start) → Store arm only; miss if store is empty.

#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::collections::HashMap;

use alloy::primitives::{aliases::U128, Address, I256, U256};
use degenbot_db::connection::DegenbotDb;
use degenbot_db::discovery::{V3PoolRowInput, V4PoolRowInput};
use degenbot_db::{ApplyBitmapAtWord, ApplyLiquidityAtTick};

use super::{assemble_v3_tick_map, assemble_v4_tick_map, TickMapAssemblyError};
use crate::bot_core::{PoolTickCoverage, TickInfo};
use degenbot_pools::tick_fetch::{BootstrapTickError, BootstrapTickWord, TickBootstrapRpc};

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

/// A distinct V4 `StateView` contract address for tests (dummy — the tests use
/// `chain=None` or a `FakeChainRpc` that ignores the address).
fn make_state_view() -> Address {
    Address::from([0xee; 20])
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
// ── V3 tests ──────────────────────────────────────────────────────────────

#[test]
fn v3_store_miss_db_hit_non_empty_returns_tracked_ticks() {
    let (db, pool_id) = v3_db_with_pool();
    seed_v3_ticks(&db, pool_id, &[10, 20]);
    let addr = make_pool_addr();

    let result = assemble_v3_tick_map(Some(&db), addr, 0, 10, 0, None).unwrap();
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

    let result = assemble_v3_tick_map(Some(&db), make_pool_addr(), 0, 10, 0, None).unwrap();
    assert!(result.is_none(), "empty LiquidityMap → miss");
}

#[test]
fn v3_store_miss_db_miss_pool_not_found_returns_miss() {
    // A fresh DB with no pools at all — `fetch_liquidity_map` returns Ok(None).
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();

    let result = assemble_v3_tick_map(Some(&db), make_pool_addr(), 0, 10, 0, None).unwrap();
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

    let result = assemble_v3_tick_map(Some(&db), make_pool_addr(), 0, 10, 0, None);
    assert!(
        result.is_err(),
        "Db error must propagate (Decision 8 (A)) — helper must NOT swallow"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(
            err,
            TickMapAssemblyError::Db(degenbot_db::DbError::Decode(_))
        ),
        "expected DbError::Decode from the bad VARCHAR, got {err:?}"
    );
}

#[test]
fn v3_db_none_cold_start_returns_miss() {
    let result = assemble_v3_tick_map(None, make_pool_addr(), 0, 10, 0, None).unwrap();
    assert!(result.is_none(), "cold-start (db=None, chain=None) → miss");
}

// ── V4 tests ──────────────────────────────────────────────────────────────

#[test]
fn v4_store_miss_db_hit_non_empty_returns_tracked_ticks() {
    let (db, managed_id, pool_id) = v4_db_with_pool();
    seed_v4_ticks(&db, managed_id, &[-10, 10]);
    let mgr = make_manager();

    let result =
        assemble_v4_tick_map(Some(&db), mgr, make_state_view(), pool_id, 0, 10, 0, None).unwrap();
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
    let result = assemble_v4_tick_map(
        Some(&db),
        make_manager(),
        make_state_view(),
        pool_id,
        0,
        10,
        0,
        None,
    )
    .unwrap();
    assert!(result.is_none(), "empty LiquidityMap → miss");
}

#[test]
fn v4_store_miss_db_miss_pool_not_found_returns_miss() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let pool_id = [0xee; 32];
    let result = assemble_v4_tick_map(
        Some(&db),
        make_manager(),
        make_state_view(),
        pool_id,
        0,
        10,
        0,
        None,
    )
    .unwrap();
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

    let result = assemble_v4_tick_map(
        Some(&db),
        make_manager(),
        make_state_view(),
        pool_id,
        0,
        10,
        0,
        None,
    );
    assert!(result.is_err(), "Db error must propagate (Decision 8 (A))");
    let err = result.unwrap_err();
    assert!(
        matches!(
            err,
            TickMapAssemblyError::Db(degenbot_db::DbError::Decode(_))
        ),
        "expected DbError::Decode, got {err:?}"
    );
}

#[test]
fn v4_db_none_cold_start_returns_miss() {
    let pool_id = [0xee; 32];
    let result = assemble_v4_tick_map(
        None,
        make_manager(),
        make_state_view(),
        pool_id,
        0,
        10,
        0,
        None,
    )
    .unwrap();
    assert!(result.is_none(), "cold-start (db=None, chain=None) → miss");
}

// ── Chain arm tests (epic 5NT2OC / task U4KLPV) ────────────────────────────
//
//  (a) Chain hit after Store+Db miss → Some((ticks, Sparse)).
//  (b) Chain returns zero-bitmap → None.
//  (c) Chain error propagation (BootstrapTickError::Rpc surfaces as
//      TickMapAssemblyError::Chain, NOT swallowed — Decision 8 (A)).
//  (d) chain=None falls through to current behavior (Ok(None)).
//
//  V3 + V4 variants both gain the Chain arm (AC: "V3 + V4 variants both
//  gain the Chain arm").

/// A `TickBootstrapRpc` fake for the Chain-arm tests. Mirrors
/// `degenbot_pools::tick_fetch::bootstrap::tests::FakeBootstrapRpc` but lives
/// here so the assemble-helper tests stay self-contained.
#[derive(Debug)]
struct FakeChainRpc {
    /// The `BootstrapTickWord` to return on a hit. `None` → the trait method
    /// returns `Ok(None)` (all-zero bitmap).
    hit: Option<BootstrapTickWord>,
    /// If `true`, the trait method returns `Err(BootstrapTickError::Rpc)`.
    fail_rpc: bool,
}

impl TickBootstrapRpc for FakeChainRpc {
    fn bootstrap_v3_tick_word(
        &self,
        _pool_address: &str,
        _tick: i32,
        _tick_spacing: i32,
        _block: u64,
    ) -> Result<Option<BootstrapTickWord>, BootstrapTickError> {
        if self.fail_rpc {
            return Err(BootstrapTickError::Rpc);
        }
        Ok(self.hit.clone())
    }

    fn bootstrap_v4_tick_word(
        &self,
        _state_view: &str,
        _pool_id: &[u8; 32],
        _tick: i32,
        _tick_spacing: i32,
        _block: u64,
    ) -> Result<Option<BootstrapTickWord>, BootstrapTickError> {
        if self.fail_rpc {
            return Err(BootstrapTickError::Rpc);
        }
        Ok(self.hit.clone())
    }
}

fn chain_tick_info(tick: i32) -> TickInfo {
    TickInfo {
        liquidity_gross: U128::from(7_000u64 * u64::from(tick.unsigned_abs()) + 1),
        liquidity_net: I256::try_from(i64::from(tick) * 700).unwrap(),
        block: 0,
    }
}

#[test]
fn v3_chain_hit_after_store_db_miss_returns_sparse_ticks() {
    // (a) Store miss + Db miss + Chain hit → Some((ticks, Sparse)).
    // Sparse (NOT Tracked): only one word's ticks seeded.
    let (db, _pool_id) = v3_db_with_pool(); // pool exists but no ticks seeded → Db miss
    let addr = make_pool_addr();
    let chain_hit = BootstrapTickWord {
        word: 0,
        ticks: [(10, chain_tick_info(10)), (20, chain_tick_info(20))]
            .into_iter()
            .collect(),
    };
    let rpc = FakeChainRpc {
        hit: Some(chain_hit),
        fail_rpc: false,
    };

    let result = assemble_v3_tick_map(Some(&db), addr, 10, 1, 100, Some(&rpc)).unwrap();
    let Some((ticks, coverage)) = result else {
        panic!("Chain hit should return Some");
    };
    assert_eq!(coverage, PoolTickCoverage::Sparse, "Chain hit → Sparse");
    assert_eq!(ticks.len(), 2, "both seeded ticks present");
    assert_eq!(ticks.get(&10), Some(&chain_tick_info(10)));
    assert_eq!(ticks.get(&20), Some(&chain_tick_info(20)));
}

#[test]
fn v3_chain_zero_bitmap_returns_none() {
    // (b) Chain returns None (all-zero bitmap) → Ok(None). The pool genuinely
    // has no initialized ticks in this word.
    let (db, _pool_id) = v3_db_with_pool();
    let addr = make_pool_addr();
    let rpc = FakeChainRpc {
        hit: None,
        fail_rpc: false,
    };

    let result = assemble_v3_tick_map(Some(&db), addr, 10, 1, 100, Some(&rpc)).unwrap();
    assert!(result.is_none(), "all-zero bitmap → Ok(None)");
}

#[test]
fn v3_chain_rpc_error_is_propagated_not_swallowed() {
    // (c) BootstrapTickError::Rpc surfaces as TickMapAssemblyError::Chain —
    // NOT swallowed (Decision 8 (A) extended to the Chain arm).
    let (db, _pool_id) = v3_db_with_pool();
    let addr = make_pool_addr();
    let rpc = FakeChainRpc {
        hit: None,
        fail_rpc: true,
    };

    let result = assemble_v3_tick_map(Some(&db), addr, 10, 1, 100, Some(&rpc));
    let err = result.expect_err("Chain Rpc error must propagate (Decision 8 (A))");
    assert!(
        matches!(err, TickMapAssemblyError::Chain(BootstrapTickError::Rpc)),
        "expected Chain(BootstrapTickError::Rpc), got {err:?}"
    );
}

#[test]
fn v3_chain_none_falls_through_returns_none() {
    // (d) chain=None → Chain arm doesn't fire; Db miss → Ok(None)
    // (current behavior preserved).
    let (db, _pool_id) = v3_db_with_pool();
    let addr = make_pool_addr();

    let result = assemble_v3_tick_map(Some(&db), addr, 10, 1, 100, None).unwrap();
    assert!(result.is_none(), "chain=None + Db miss → Ok(None)");
}

#[test]
fn v4_chain_hit_after_store_db_miss_returns_sparse_ticks() {
    // V4 Chain hit — twin of v3_chain_hit_after_store_db_miss_returns_sparse_ticks.
    let (db, _managed_id, pool_id) = v4_db_with_pool();
    let mgr = make_manager();
    let chain_hit = BootstrapTickWord {
        word: 0,
        ticks: [(-10, chain_tick_info(-10)), (10, chain_tick_info(10))]
            .into_iter()
            .collect(),
    };
    let rpc = FakeChainRpc {
        hit: Some(chain_hit),
        fail_rpc: false,
    };

    let result = assemble_v4_tick_map(
        Some(&db),
        mgr,
        make_state_view(),
        pool_id,
        10,
        1,
        100,
        Some(&rpc),
    )
    .unwrap();
    let Some((ticks, coverage)) = result else {
        panic!("Chain hit should return Some");
    };
    assert_eq!(coverage, PoolTickCoverage::Sparse);
    assert_eq!(ticks.len(), 2);
    assert_eq!(ticks.get(&-10), Some(&chain_tick_info(-10)));
    assert_eq!(ticks.get(&10), Some(&chain_tick_info(10)));
}

#[test]
fn v4_chain_rpc_error_is_propagated_not_swallowed() {
    // V4 Chain-error propagation — twin of the V3 test (c).
    let (db, _managed_id, pool_id) = v4_db_with_pool();
    let mgr = make_manager();
    let rpc = FakeChainRpc {
        hit: None,
        fail_rpc: true,
    };

    let result = assemble_v4_tick_map(
        Some(&db),
        mgr,
        make_state_view(),
        pool_id,
        10,
        1,
        100,
        Some(&rpc),
    );
    let err = result.expect_err("Chain Rpc error must propagate");
    assert!(
        matches!(err, TickMapAssemblyError::Chain(BootstrapTickError::Rpc)),
        "expected Chain(BootstrapTickError::Rpc), got {err:?}"
    );
}
