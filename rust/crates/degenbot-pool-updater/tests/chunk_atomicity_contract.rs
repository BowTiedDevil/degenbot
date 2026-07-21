//! Contract artifact: chunk interrupt -> full rollback -> restart clean (epic `2SFL6I`, task `BHF5BC`).
//!
//! THE regression test gating the §1 atomicity + restart invariants
//! (`docs/migration-guides/pool-updater-chunk-atomicity.md` §1). Reproduces
//! the user's original bug — a chunk loop that committed pool rows mid-chunk
//! but rolled back the `last_update_block` stamp, so a restart re-fetched the
//! same range + hit `UNIQUE constraint failed: pools.address, pools.chain`.
//! Proves the fix: with [`apply_chunk_writes_on_conn`], an interrupt (dropping
//! the transaction before commit) leaves NO partial-chunk state — the pool
//! writes + the stamp are a single all-or-nothing unit — so a restart
//! re-processes the chunk from the stale `last_update_block` with NO
//! duplicates.
//!
//! Mirrors `tests/cli/test_database_heal_boundary.py`'s contract-artifact
//! discipline: a journey narrative asserting the rugpull-protection invariant
//! (rollback -> no pools + no stamp; restart re-processes cleanly because the
//! rollback left no partial state). The testable seam is the D1 write-half
//! split [`apply_chunk_writes_on_conn`] (Task `CKXCOB` 3c) — pure-sync, takes
//! a `&Connection` + a [`ChunkInputs`], NO mock RPC needed (the RPC fetch
//! half is covered by `degenbot-rpc`'s 88 tests; this test is about the
//! transaction/rollback semantics).
//!
//! # The journey
//!
//! 1. **Chunk [A,B] writes pools + stages the stamp — but the run is
//!    interrupted before `tx.commit()`** (a SIGINT mid-chunk, a panic, a
//!    power loss). The transaction is dropped -> `SQLite` rolls back EVERY
//!    write in the chunk. Assert: ZERO pool rows + `last_update_block` STILL
//!    at `A-1` (the pre-chunk cursor). This is `§1.1` atomicity: no
//!    intermediate state is observable across an interrupt.
//! 2. **RESTART re-processes chunk [A,B]** (the stamp is still `A-1`, so the
//!    chunk is the next unprocessed range). The pool rows are GONE (rolled
//!    back in step 1), so the `INSERT`s succeed — NO `UNIQUE constraint
//!    failed`. Assert: pools ARE present + `last_update_block == B`. This is
//!    the `§1.3` restart-invariant + THE bug-fix proof: with the bug (pools
//!    committed mid-chunk + stamp stale), this restart would hit
//!    `UNIQUE constraint failed: pools.address, pools.chain` on the
//!    duplicate insert. With the fix (full rollback), the restart succeeds.
//! 3. **A third chunk [B+1, C] with NEW pools** -> succeeds, stamp -> C. The
//!    committed chunk [A,B] is NOT re-processed (the stamp is B, so the
//!    restart continues from B+1). This pins `§1.2`: `last_update_block` is a
//!    strict upper bound on committed work, so a restart never re-applies
//!    committed work.

use std::collections::HashMap;

use alloy::primitives::{Address, B256};
use degenbot_db::{DegenbotDb, V3PoolRowInput};
use degenbot_pool_updater::{
    apply_chunk_writes_on_conn, ChunkInputs, ExchangeSpec, PoolCreationToWrite, PoolFamily,
};
use rusqlite::Connection;
use tempfile::TempDir;

const CHAIN: i64 = 1;
const BLOCK_A: u64 = 1_000; // chunk start
const BLOCK_B: u64 = 2_000; // chunk end
const BLOCK_C: u64 = 3_000; // second chunk end

/// An on-disk writable DB with the head schema + one seeded `uniswap_v3`
/// exchange whose `last_update_block` is `Some(BLOCK_A - 1)` (the pre-chunk
/// restart cursor). Returns the DB + the exchange's `ExchangeSpec`.
fn seeded_db_with_v3_exchange() -> (DegenbotDb, ExchangeSpec) {
    let tmp = TempDir::new().unwrap();
    let (db, _state) = DegenbotDb::open_for_writes(&tmp.path().join("chunk_atomicity.db")).unwrap();
    let factory = Address::from([0x1f; 20]);
    let exchange = db
        .upsert_exchange(CHAIN, "uniswap_v3", factory, None)
        .unwrap();
    // Seed the restart cursor at BLOCK_A - 1 (the chunk [A,B] is the next
    // unprocessed range).
    {
        let guard = db.lock();
        DegenbotDb::set_exchange_last_update_block_on_conn(
            &guard,
            CHAIN,
            exchange.id,
            i64::try_from(BLOCK_A - 1).unwrap(),
        )
        .unwrap();
    }
    let spec = ExchangeSpec {
        id: exchange.id,
        chain_id: CHAIN,
        name: "uniswap_v3".to_string(),
        factory,
        family: PoolFamily::V3,
        event_topic: B256::ZERO,
        fee_denominator: 1_000_000,
        v2_fee_token: None,
        rpc_fee_call: None,
        last_update_block: Some(i64::try_from(BLOCK_A - 1).unwrap()),
    };
    (db, spec)
}

/// Build `ChunkInputs` carrying one V3 pool-creation row at `addr` (a pool
/// "discovered" in the chunk). Mirrors the in-crate `run.rs` fixture pattern.
fn chunk_inputs_with_v3_pool(exchange_id: i64, addr: Address) -> ChunkInputs {
    ChunkInputs {
        pool_creations: vec![PoolCreationToWrite::V3 {
            exchange_id,
            row: V3PoolRowInput {
                address: addr,
                token0_address: Address::from([0x22; 20]),
                token1_address: Address::from([0x33; 20]),
                fee: 500,
                tick_spacing: 10,
            },
        }],
        v3_liquidity: HashMap::new(),
        v4_liquidity: HashMap::new(),
        pool_manager_chain: CHAIN,
        v4_manager_addresses: HashMap::new(),
    }
}

/// Count pool rows for `CHAIN` (the substrate-agnostic read for the assertions).
fn count_pools(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM pools WHERE chain = ?1",
        rusqlite::params![CHAIN],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn chunk_interrupt_rolls_back_and_restart_is_clean() {
    let (db, spec) = seeded_db_with_v3_exchange();
    let specs = [spec.clone()];
    let pool_addr = Address::from([0x11; 20]);
    let inputs = chunk_inputs_with_v3_pool(spec.id, pool_addr);

    // ============================================================
    // 1. INTERRUPT before commit: drop the transaction mid-chunk.
    //    (The pool was written inside the tx, but tx.commit() never ran —
    //    simulating a SIGINT/panic/power-loss BEFORE the chunk boundary.)
    // ============================================================
    {
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        let report =
            apply_chunk_writes_on_conn(&tx, CHAIN, &specs, BLOCK_B, &inputs, None).unwrap();
        // The fn returned Ok (no injected failure) — the writes are staged in
        // the tx's buffer, visible to this connection, NOT yet durable.
        assert_eq!(
            report.pools_written, 1,
            "the chunk staged one pool write inside the transaction",
        );
        // INTERRUPT: drop the tx WITHOUT commit -> `SQLite` rolls back EVERY
        // write in the chunk (the pool row + the stamp advance).
        drop(tx);
    }

    // §1.1 atomicity: no intermediate state observable across the interrupt.
    let stamp_after_interrupt = db
        .fetch_exchange(spec.id)
        .unwrap()
        .unwrap()
        .last_update_block;
    assert_eq!(
        stamp_after_interrupt,
        Some(i64::try_from(BLOCK_A - 1).unwrap()),
        "rolled-back chunk's stamp advance must NOT be durable (still at A-1)",
    );
    let pools_after_interrupt = {
        let guard = db.lock();
        count_pools(&guard)
    };
    assert_eq!(
        pools_after_interrupt, 0,
        "rolled-back chunk's pool writes must NOT be durable (zero rows)",
    );

    // ============================================================
    // 2. RESTART re-processes chunk [A,B] (the stamp is still A-1).
    //    THE BUG-FIX PROOF: with the bug (pools committed mid-chunk + stamp
    //    stale), this restart would hit
    //    `UNIQUE constraint failed: pools.address, pools.chain` on the
    //    duplicate INSERT. With the fix (step 1's full rollback), the pool
    //    table is empty -> the INSERT succeeds -> pools + stamp commit.
    //    §1.3 restart-invariant: re-processes only uncommitted work -> no
    //    duplicates.
    // ============================================================
    {
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        let report =
            apply_chunk_writes_on_conn(&tx, CHAIN, &specs, BLOCK_B, &inputs, None).unwrap();
        assert_eq!(
            report.pools_written, 1,
            "restart re-processes the rolled-back chunk + the INSERT succeeds (no UNIQUE failure)",
        );
        tx.commit().unwrap();
    }

    let stamp_after_restart = db
        .fetch_exchange(spec.id)
        .unwrap()
        .unwrap()
        .last_update_block;
    assert_eq!(
        stamp_after_restart,
        Some(i64::try_from(BLOCK_B).unwrap()),
        "restart chunk committed + advanced the stamp to B (no stale-cursor stall)",
    );
    let pools_after_restart = {
        let guard = db.lock();
        count_pools(&guard)
    };
    assert_eq!(
        pools_after_restart, 1,
        "restart re-processes the chunk cleanly (one pool, no duplicate)",
    );

    // ============================================================
    // 3. A THIRD chunk [B+1, C] with a NEW pool -> succeeds.
    //    `§1.2`: `last_update_block == B` is a strict upper bound on committed
    //    work, so the restart continues from B+1 (does NOT re-process [A,B]).
    //    A re-discovery of `pool_addr` here would be a genuine duplicate
    //    (the pool was created at block A, re-emitting is impossible), so
    //    this chunk carries a DIFFERENT pool address.
    // ============================================================
    let new_pool_addr = Address::from([0x44; 20]);
    let inputs_chunk2 = chunk_inputs_with_v3_pool(spec.id, new_pool_addr);
    {
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        let report =
            apply_chunk_writes_on_conn(&tx, CHAIN, &specs, BLOCK_C, &inputs_chunk2, None).unwrap();
        assert_eq!(
            report.pools_written, 1,
            "the second chunk staged one new pool"
        );
        tx.commit().unwrap();
    }

    let stamp_after_chunk2 = db
        .fetch_exchange(spec.id)
        .unwrap()
        .unwrap()
        .last_update_block;
    assert_eq!(
        stamp_after_chunk2,
        Some(i64::try_from(BLOCK_C).unwrap()),
        "second chunk advanced the stamp to C",
    );
    let pools_after_chunk2 = {
        let guard = db.lock();
        count_pools(&guard)
    };
    assert_eq!(
        pools_after_chunk2, 2,
        "second chunk added a second pool (two rows total, no duplicates)",
    );

    // ============================================================
    // 4. FK integrity + no orphans (mirrors the heal-boundary sibling's
    //    rugpull-protection tail assertion).
    // ============================================================
    let fk_ok = {
        let guard = db.lock();
        let mut stmt = guard.prepare("PRAGMA foreign_key_check").unwrap();
        let rows = stmt.query_map([], |_| Ok(())).unwrap();
        rows.count()
    };
    assert_eq!(fk_ok, 0, "no orphaned FK rows (foreign_key_check empty)");
}
