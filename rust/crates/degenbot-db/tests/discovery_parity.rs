//! §4.2 parity for the pool discovery writers (WR7EA6). Seed an in-memory
//! write-capable DB with an exchange + a `PoolManager` + the V2/V3/V4
//! `PoolCreated`-event row fixtures, run each `upsert_v*_pools` batch + the
//! `set_exchange_last_update_block` stamp, + assert the resulting
//! multi-table state matches the Python ORM trajectory field-for-field.
//!
//! The cross-DB byte-comparison vs a Python-written fixture lives in
//! `tests/rust/test_discovery_seam.py`; this Rust test pins the Rust
//! trajectory so that the Python oracle gate has a stable reference.

#![expect(clippy::unwrap_used)]
use alloy::primitives::Address;
use degenbot_db::{DegenbotDb, V2PoolRowInput, V3PoolRowInput, V4PoolRowInput};

/// Parse a 0x-prefixed hex address into an alloy [`Address`].
fn addr(hex: &str) -> Address {
    hex.parse::<Address>().unwrap()
}

/// Seed a fresh in-memory write-capable DB with:
/// - one exchange (id 1, chain 1, factory `0xf…f`) — the `FK` parent for V2/V3
///   pool rows + the `PoolManager`'s exchange.
/// - one `PoolManager` (id 1, chain 1, address `0xb…b`, `exchange_id` 1) — the V4
///   `FK` parent.
fn fresh_seeded_db() -> DegenbotDb {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO exchanges (id, chain_id, name, active, last_update_block, factory, \
             deployer) VALUES (1, 1, 'uniswap_v2', 1, NULL, '0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF', NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pool_managers (id, address, chain, kind, state_view, exchange_id) \
             VALUES (1, '0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 1, 'uniswap_v4', NULL, 1)",
            [],
        )
        .unwrap();
    }
    db
}

#[test]
fn upsert_v2_pool_inserts_base_and_subclass_row() {
    let db = fresh_seeded_db();
    let rows = vec![V2PoolRowInput {
        address: addr("0x1111111111111111111111111111111111111111"),
        token0_address: addr("0x000000000000000000000000000000000000000a"),
        token1_address: addr("0x000000000000000000000000000000000000000b"),
        fee_token0: 3,
        fee_token1: 3,
        stable: None,
    }];
    db.upsert_v2_pools(1, "uniswap_v2", 1, 1_000, &rows)
        .unwrap();

    let conn = db.lock();
    // base pools row
    let (kind, token0_id, token1_id, exchange_id): (String, i64, i64, i64) = conn
        .query_row(
            "SELECT kind, token0_id, token1_id, exchange_id FROM pools WHERE chain = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(kind, "uniswap_v2");
    assert_eq!(exchange_id, 1);
    // tokens were get-or-create'd (ids 1 + 2)
    assert_eq!(token0_id, 1);
    assert_eq!(token1_id, 2);

    // subclass detail row
    let (pool_id, fee_token0, fee_token1, fee_denominator): (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT pool_id, fee_token0, fee_token1, fee_denominator FROM uniswap_v2_pools",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    let pools_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM pools", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pool_id, pools_count); // subclass PK == base pools.id
    assert_eq!((fee_token0, fee_token1, fee_denominator), (3, 3, 1_000));
}

#[test]
fn upsert_v2_aerodrome_includes_stable_column() {
    let db = fresh_seeded_db();
    let rows = vec![V2PoolRowInput {
        address: addr("0x2222222222222222222222222222222222222222"),
        token0_address: addr("0x000000000000000000000000000000000000000a"),
        token1_address: addr("0x000000000000000000000000000000000000000b"),
        fee_token0: 0,
        fee_token1: 0,
        stable: Some(true),
    }];
    db.upsert_v2_pools(1, "aerodrome_v2", 1, 10_000, &rows)
        .unwrap();

    let conn = db.lock();
    let (kind,): (String,) = conn
        .query_row("SELECT kind FROM pools", [], |r| Ok((r.get(0)?,)))
        .unwrap();
    assert_eq!(kind, "aerodrome_v2");
    let (stable,): (bool,) = conn
        .query_row("SELECT stable FROM aerodrome_v2_pools", [], |r| {
            Ok((r.get(0)?,))
        })
        .unwrap();
    assert!(stable);
}

#[test]
fn upsert_v3_pool_inserts_base_and_subclass_row() {
    let db = fresh_seeded_db();
    let rows = vec![V3PoolRowInput {
        address: addr("0x3333333333333333333333333333333333333333"),
        token0_address: addr("0x000000000000000000000000000000000000000a"),
        token1_address: addr("0x000000000000000000000000000000000000000b"),
        fee: 500,
        tick_spacing: 10,
    }];
    db.upsert_v3_pools(1, "uniswap_v3", 1, 1_000_000, &rows)
        .unwrap();

    let conn = db.lock();
    let (kind,): (String,) = conn
        .query_row("SELECT kind FROM pools", [], |r| Ok((r.get(0)?,)))
        .unwrap();
    assert_eq!(kind, "uniswap_v3");
    let (tick_spacing, fee_token0, fee_token1, fee_denominator, lub, lui): (
        i64,
        i64,
        i64,
        i64,
        Option<i64>,
        Option<i64>,
    ) = conn
        .query_row(
            "SELECT tick_spacing, fee_token0, fee_token1, fee_denominator, \
             liquidity_update_block, liquidity_update_log_index FROM uniswap_v3_pools",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        (tick_spacing, fee_token0, fee_token1, fee_denominator),
        (10, 500, 500, 1_000_000)
    );
    // new pool → no liquidity-update stamps yet
    assert_eq!(lub, None);
    assert_eq!(lui, None);
}

#[test]
fn upsert_v4_pool_uses_managed_pools_base() {
    let db = fresh_seeded_db();
    let pool_hash = "0x".to_string() + &"cc".repeat(32);
    let rows = vec![V4PoolRowInput {
        pool_hash: pool_hash.clone(),
        hooks: addr("0x0000000000000000000000000000000000000000"),
        currency0_address: addr("0x000000000000000000000000000000000000000a"),
        currency1_address: addr("0x000000000000000000000000000000000000000b"),
        fee: 3000,
        tick_spacing: 60,
    }];
    db.upsert_v4_pools(
        1,
        "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        1_000_000,
        &rows,
    )
    .unwrap();

    let conn = db.lock();
    // managed_pools base row (separate from `pools`)
    let (kind, manager_id): (String, i64) = conn
        .query_row("SELECT kind, manager_id FROM managed_pools", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(kind, "uniswap_v4");
    assert_eq!(manager_id, 1);
    // uniswap_v4_pools detail row
    let (mpid, ph, hooks, c0, c1, fc0, fc1, fden, ts): (
        i64,
        String,
        String,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = conn
        .query_row(
            "SELECT managed_pool_id, pool_hash, hooks, currency0_id, currency1_id, \
             fee_currency0, fee_currency1, fee_denominator, tick_spacing \
             FROM uniswap_v4_pools",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .unwrap();
    let managed_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM managed_pools", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mpid, managed_count); // detail PK == managed_pools.id
    assert_eq!(ph, pool_hash);
    assert_eq!(hooks, "0x0000000000000000000000000000000000000000");
    assert_eq!((c0, c1), (1, 2));
    assert_eq!((fc0, fc1, fden, ts), (3000, 3000, 1_000_000, 60));
    // V4 does NOT write to the V2/V3 `pools` table
    let pools_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM pools", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pools_count, 0);
}

#[test]
fn upsert_v4_pools_resolves_manager_by_address() {
    let db = fresh_seeded_db();
    let rows = vec![V4PoolRowInput {
        pool_hash: "0x".to_string() + &"dd".repeat(32),
        hooks: addr("0x0000000000000000000000000000000000000000"),
        currency0_address: addr("0x000000000000000000000000000000000000000a"),
        currency1_address: addr("0x000000000000000000000000000000000000000b"),
        fee: 100,
        tick_spacing: 1,
    }];
    // wrong manager address → Decode error (mirrors Python `assert manager_in_db is not None`)
    let err = db
        .upsert_v4_pools(
            1,
            "0xcccccccccccccccccccccccccccccccccccccccc",
            1_000_000,
            &rows,
        )
        .unwrap_err();
    assert!(matches!(err, degenbot_db::DbError::Decode(_)));
}

#[test]
fn set_exchange_last_update_block_stamps_the_row() {
    let db = fresh_seeded_db();
    db.set_exchange_last_update_block(1, 1, 12345).unwrap();
    let conn = db.lock();
    let (block,): (i64,) = conn
        .query_row(
            "SELECT last_update_block FROM exchanges WHERE chain_id = 1 AND id = 1",
            [],
            |r| Ok((r.get(0)?,)),
        )
        .unwrap();
    assert_eq!(block, 12345);
}

#[test]
fn fetch_pool_manager_id_by_address_round_trips() {
    let db = fresh_seeded_db();
    let id = db
        .fetch_pool_manager_id_by_address(1, "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        .unwrap();
    assert_eq!(id, Some(1));
    let none = db
        .fetch_pool_manager_id_by_address(1, "0xcccccccccccccccccccccccccccccccccccccccc")
        .unwrap();
    assert_eq!(none, None);
}

#[test]
fn upsert_v2_reuses_existing_tokens_across_rows() {
    // Two pools sharing the same token pair → the token escalate must reuse
    // the existing `erc20_tokens` rows, not create duplicates.
    let db = fresh_seeded_db();
    let rows = vec![
        V2PoolRowInput {
            address: addr("0x1111111111111111111111111111111111111111"),
            token0_address: addr("0x000000000000000000000000000000000000000a"),
            token1_address: addr("0x000000000000000000000000000000000000000b"),
            fee_token0: 3,
            fee_token1: 3,
            stable: None,
        },
        V2PoolRowInput {
            address: addr("0x4444444444444444444444444444444444444444"),
            token0_address: addr("0x000000000000000000000000000000000000000a"),
            token1_address: addr("0x000000000000000000000000000000000000000b"),
            fee_token0: 3,
            fee_token1: 3,
            stable: None,
        },
    ];
    db.upsert_v2_pools(1, "uniswap_v2", 1, 1_000, &rows)
        .unwrap();
    let conn = db.lock();
    let token_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM erc20_tokens", [], |r| r.get(0))
        .unwrap();
    assert_eq!(token_count, 2); // not 4
    let pools_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM pools", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pools_count, 2);
}
