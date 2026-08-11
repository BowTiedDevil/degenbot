//! Integration test for the per-pool DB liquidity clock (`fetch_liquidity_update_block`
//! / `fetch_liquidity_update_block_v4`, task 4TWM7C).
//!
//! The Rust `PoolBuilder` stamps a DB-seeded (`Tracked`) pool's `tick_data_block`
//! (liquidity clock) at `liquidity_update_block` rather than the live head, so the
//! registration seed/post-drain verify anchors at the block the DB liquidity map is
//! exact at — not a head block that moved underneath it.

#![expect(clippy::unwrap_used)]

use alloy::primitives::{Address, B256};
use degenbot_db::connection::DegenbotDb;

fn addr_word(address: Address) -> B256 {
    B256::from(address.into_word())
}

/// Seed a minimal `pools` row + a `liquidity_update_block` in `table` for
/// `pool_id`, with `foreign_keys=OFF` (fixture needs no parent rows).
fn seed_v3_pool(
    db: &DegenbotDb,
    pool_id: i64,
    address: Address,
    kind: &str,
    table: &str,
    block: i64,
) {
    let conn = db.lock();
    conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    conn.execute(
        "INSERT INTO pools (id, address, chain, kind, token0_id, token1_id, exchange_id) \
         VALUES (?1, ?2, 1, ?3, 1, 2, 1)",
        rusqlite::params![pool_id, address.to_checksum(None), kind],
    )
    .unwrap();
    let sql = format!(
        "INSERT INTO {table} \
            (pool_id, tick_spacing, liquidity_update_block, liquidity_update_log_index, \
             fee_token0, fee_token1, fee_denominator) \
         VALUES (?1, 60, ?2, 0, 3000, 3000, 1000000)"
    );
    conn.execute(&sql, rusqlite::params![pool_id, block])
        .unwrap();
}

#[test]
fn fetch_liquidity_update_block_reads_v3_pool_row() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    seed_v3_pool(
        &db,
        1,
        Address::from([0x11u8; 20]),
        "uniswap_v3",
        "uniswap_v3_pools",
        12345,
    );
    assert_eq!(
        db.fetch_liquidity_update_block(Address::from([0x11u8; 20]))
            .unwrap(),
        Some(12345),
        "a Uniswap V3 row's liquidity_update_block is the authoritative liquidity clock"
    );
}

#[test]
fn fetch_liquidity_update_block_reads_nondex_v3_kinds() {
    // Task 4TWM7C follow-up: a pancake/sushi/aerodrome V3 pool stores its
    // liquidity clock in its OWN per-dex table — a uniswap-only lookup returned
    // None and the seed verify fell back to head (pool 0x1ac1... pancake crash).
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    seed_v3_pool(
        &db,
        100,
        Address::from([0x31u8; 20]),
        "pancakeswap_v3",
        "pancakeswap_v3_pools",
        25_725_845,
    );
    seed_v3_pool(
        &db,
        101,
        Address::from([0x32u8; 20]),
        "sushiswap_v3",
        "sushiswap_v3_pools",
        11_111,
    );
    seed_v3_pool(
        &db,
        102,
        Address::from([0x33u8; 20]),
        "aerodrome_v3",
        "aerodrome_v3_pools",
        22_222,
    );
    assert_eq!(
        db.fetch_liquidity_update_block(Address::from([0x31u8; 20]))
            .unwrap(),
        Some(25_725_845)
    );
    assert_eq!(
        db.fetch_liquidity_update_block(Address::from([0x32u8; 20]))
            .unwrap(),
        Some(11_111)
    );
    assert_eq!(
        db.fetch_liquidity_update_block(Address::from([0x33u8; 20]))
            .unwrap(),
        Some(22_222)
    );
}

#[test]
fn fetch_liquidity_update_block_none_for_non_v3_kind() {
    // A V2-kind pool (or unknown) yields no V3 liquidity clock — miss (None).
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    {
        let conn = db.lock();
        conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        conn.execute(
            "INSERT INTO pools (id, address, chain, kind, token0_id, token1_id, exchange_id) \
             VALUES (1, ?1, 1, 'uniswap_v2', 1, 2, 1)",
            rusqlite::params![Address::from([0x24u8; 20]).to_checksum(None)],
        )
        .unwrap();
    }
    assert_eq!(
        db.fetch_liquidity_update_block(Address::from([0x24u8; 20]))
            .unwrap(),
        None
    );
    // Unknown address → miss.
    assert_eq!(
        db.fetch_liquidity_update_block(Address::from([0x22u8; 20]))
            .unwrap(),
        None
    );
}

#[test]
fn fetch_liquidity_update_block_none_for_unknown_pool() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    // No pools rows inserted → miss (None), matching fetch_liquidity_map's miss.
    assert_eq!(
        db.fetch_liquidity_update_block(Address::from([0x22u8; 20]))
            .unwrap(),
        None
    );
}

#[test]
fn fetch_liquidity_update_block_v4_reads_v4_pool_row() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let manager: Address = Address::from([0x33u8; 20]);
    let pool_hash: B256 = addr_word(Address::from([0x44u8; 20]));
    let hash_hex = format!("{pool_hash}");
    {
        let conn = db.lock();
        conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        conn.execute(
            "INSERT INTO pool_managers (id, chain, address, kind, state_view, exchange_id) \
             VALUES (1, 1, ?1, 'uniswap_v4', ?2, 1)",
            rusqlite::params![manager.to_checksum(None), Address::ZERO.to_checksum(None)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO managed_pools (id, kind, manager_id) VALUES (1, 'uniswap_v4', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO uniswap_v4_pools \
                (managed_pool_id, pool_hash, hooks, currency0_id, currency1_id, \
                 fee_currency0, fee_currency1, fee_denominator, tick_spacing, \
                 liquidity_update_block) \
             VALUES (1, ?1, ?2, 1, 2, 500, 500, 1000000, 10, 999)",
            rusqlite::params![hash_hex, Address::ZERO.to_checksum(None)],
        )
        .unwrap();
    }
    // Only the manager + pool-hash resolving to the V4 row yields the clock; a
    // different manager (miss) must return None.
    assert_eq!(
        db.fetch_liquidity_update_block_v4(manager, pool_hash)
            .unwrap(),
        Some(999)
    );
    assert_eq!(
        db.fetch_liquidity_update_block_v4(Address::from([0x55u8; 20]), pool_hash)
            .unwrap(),
        None
    );
}
