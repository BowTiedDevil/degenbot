//! The driver's per-transaction processing stage: per-tx log grouping + the
//! per-tx apply loop (discount pre-pass → config dispatch → operations parse
//! → the chunk-event apply → the async GHO discount refresh), all on the
//! caller's `Transaction` connection.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use alloy::rpc::types::Log;
use degenbot_db::DegenbotDb;
use degenbot_rpc::provider::AlloyProvider;
use rusqlite::Connection;

use super::substrate::{candidate_addresses, ChunkSubstrate};
use super::{AaveChunkEvent, RunError};
use crate::config_dispatch::{ChunkContext, ChunkSpan};
use crate::transaction_processor::process_transaction;

/// One transaction's grouped logs. Sorted by `(block_number, first log_index)`
/// (the fetcher's `(block_number, log_index)` sort is preserved; the grouping
/// is stable). Mirrors the Python `_build_transaction_contexts` (utils.py:82).
#[derive(Debug)]
pub(super) struct TxGroup<'a> {
    tx_hash: [u8; 32],
    block_number: u64,
    logs: Vec<&'a Log>,
}

/// Group a chunk's logs by `transactionHash`, preserving the
/// `(block_number, log_index)` sort. Mirrors the Python
/// `_build_transaction_contexts` (utils.py:82) — the events are pre-sorted,
/// then bucketed by `tx_hash`; the groups are emitted in first-seen order
/// (sorted by the group's first log's `(block_number, log_index)`, which the
/// fetcher's sort guarantees).
pub(super) fn group_logs_by_tx(logs: &[Log]) -> Vec<TxGroup<'_>> {
    // The fetcher already sorted by (block_number, log_index), so a stable
    // insertion-order HashMap preserves chronological group order.
    let mut order: Vec<[u8; 32]> = Vec::new();
    let mut groups: HashMap<[u8; 32], TxGroup<'_>> = HashMap::new();
    for log in logs {
        let Some(tx_hash_b256) = log.transaction_hash else {
            // A log with no tx_hash — shouldn't happen for fetched logs. Treat
            // it as its own singleton group keyed by zero (mirrors the Python
            // which would KeyError on `event["transactionHash"]`).
            continue;
        };
        let tx_hash = tx_hash_b256.0;
        let block_number = log.block_number.unwrap_or(0);
        let entry = groups.entry(tx_hash).or_insert_with(|| {
            order.push(tx_hash);
            TxGroup {
                tx_hash,
                block_number,
                logs: Vec::new(),
            }
        });
        entry.logs.push(log);
    }
    order
        .into_iter()
        .map(|k| {
            #[expect(clippy::expect_used)] // `order` yields group keys known to `groups`
            groups.remove(&k).expect("present in order")
        })
        .collect()
}

/// The per-chunk processing core (the Transaction-borrowed, RPC-interspersed
/// body). Per tx group: re-resolve the GHO vToken revision, run the discount
/// pre-pass + the config-event dispatch + C3's `process_transaction`, then
/// apply THAT tx's events to `conn` via [`apply_chunk_events_on_conn`] BEFORE
/// the next tx's reads (per-tx apply — fixes the config-revision +
/// ops-balance staleness surfaces; matches Python's per-tx ORM session apply).
/// The `last_update_block` stamp is the LAST write (end-of-chunk). Held
/// inside the caller's `Transaction`; on `Err` the caller drops the tx
/// (rollback).
///
/// Extracted from [`run_aave_update`] to keep the loop fn readable + to
/// localize the `await_holding_lock` allow (the `&Transaction` borrow across
/// `.await` — safe under `block_on`'s single-thread poll).
#[expect(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) async fn process_chunk_on_conn(
    conn: &Connection,
    provider: &AlloyProvider,
    market_id: i64,
    chain_id: i64,
    pool_address: Address,
    oracle_address: Option<Address>,
    tx_groups: &[TxGroup<'_>],
    chunk_start: u64,
    chunk_end: u64,
) -> Result<ChunkCoreReport, RunError> {
    // Per-tx apply within `conn`. Two staleness surfaces fixed —
    //   (1) config: `vtoken_revision` is now re-resolved per-tx from `conn`
    //       (read-your-own-writes sees the prior tx's `Upgraded` write),
    //       instead of the chunk-start snapshot that masked an in-chunk bump.
    //   (2) ops balances: each tx's events are applied to `conn` BEFORE the
    //       next tx's `build_discount_snapshot` / `dispatch_config_events` /
    //       `process_transaction` read — so tx N+1 sees tx N's writes (the
    //       `ScaledTokenProcessor` is stateless; its balances come from `conn`
    //       lookups, so per-tx apply is the only seam that matches Python's
    //       per-tx ORM session apply).
    // The stamp advance stays LAST (end-of-chunk), preserving the
    // restart-invariant (on rollback the stamp does NOT advance + the whole
    // chunk reverts).
    let mut events_applied_total: usize = 0;
    let mut touched_user_addresses: HashSet<Address> = HashSet::new();
    // Perf C: the chunk-level substrate cache — a handful of set-shaped
    // `SELECT ... IN` prefetches over the chunk's touched entity sets (the
    // topic-derived candidate users + their positions/configs, the market's
    // assets/contracts, the chain's GHO row), then in-memory map hits for the
    // per-event substrate reads, with EVERY cache-visible write landing an
    // overlay update (the apply arms in `apply.rs` + the config dispatch).
    // The read-your-own-writes contract: reads for log-transaction N
    // consult the cache ONLY where its state reflects all writes of
    // transactions < N — the overlay guarantees it (see
    // `substrate.rs`'s module doc). An empty chunk skips the prefetch (its
    // only writes are the end-of-chunk cleanup + stamp).
    let mut substrate = if tx_groups.is_empty() {
        ChunkSubstrate::lazy(chain_id)
    } else {
        let all_logs: Vec<&Log> = tx_groups
            .iter()
            .flat_map(|g| g.logs.iter().copied())
            .collect();
        let (user_candidates, asset_candidates) = candidate_addresses(&all_logs);
        ChunkSubstrate::prefetch_on_conn(
            conn,
            market_id,
            chain_id,
            &user_candidates,
            &asset_candidates,
        )?
    };
    // The replay-bench stage spans (plain `Instant`; the telemetry chunk lands its
    // own spans later): the per-tx decode+compute block (the GHO/revision
    // reads, the discount pre-pass + config dispatch - BOTH hold the write
    // lock across their RPC reads) and the per-tx + end-of-chunk SQL apply.
    let mut decode_compute_time = Duration::ZERO;
    let mut apply_time = Duration::ZERO;
    // Perf D: the per-chunk dispatch context — ONE owner holding the
    // chunk-scoped dispatch constants + the `RevisionMemo`, so the 4 revision
    // reads (`ATOKEN_REVISION()` / `DEBT_TOKEN_REVISION()` / `POOL_REVISION()` /
    // `CONFIGURATOR_REVISION()`) dedupe per `(implementation, selector,
    // block)`. Chunks partition the block range, so chunk scope is the full
    // read span the dispatch can observe; the block-pinned key keeps the
    // memo safe across an in-chunk `Upgraded` (the upgrade's own read sits
    // at the upgrade block; every later read sits at a later block — a
    // distinct key — so no pre-upgrade value can leak across the boundary).
    // The context borrows the chunk span, so it cannot outlive the chunk.
    let chunk_span = ChunkSpan::new(chunk_start, chunk_end);
    let mut ctx = ChunkContext::for_chunk(
        &chunk_span,
        provider.clone(),
        market_id,
        chain_id,
        pool_address,
        oracle_address,
    );

    for group in tx_groups {
        let block_number = group.block_number;
        let tx_hashes_refs: Vec<&Log> = group.logs.clone();

        for log in &group.logs {
            let topics = log.topics();
            if let Some(t1) = topics.get(1) {
                touched_user_addresses.insert(Address::from_slice(&t1.as_slice()[12..]));
            }
            if let Some(t2) = topics.get(2) {
                touched_user_addresses.insert(Address::from_slice(&t2.as_slice()[12..]));
            }
        }

        // The per-tx decode+compute stage span (the Jaeger twin of the
        // `compute_started` anchor): the GHO/revision reads, the discount
        // pre-pass + the config dispatch — all under the chunk's write lock.
        // The guard rides the driver future's single-thread poll (the
        // `await_holding_lock` soundness argument covers the span context
        // exactly as it covers the `&Transaction` borrow). One span per tx:
        // Jaeger grain answers WHICH transaction is slow; the chunk-total
        // metric still rides the ChunkProgress boundary (one observation per
        // committed chunk).
        let compute_span = tracing::info_span!(
            "degenbot.updater.aave.compute",
            tx.block = block_number,
            rpc.round_trips = tracing::field::Empty,
        );
        let compute_rt_start = degenbot_rpc::provider::rpc_round_trips();
        let compute_guard = compute_span.enter();
        let compute_started = Instant::now();
        // (0..3) The overlay-mediated read + dispatch stages, in the ONE order
        //     the read-your-own-writes contract requires: re-resolve the GHO
        //     row (a prior tx's `ReserveInitialized` sets `v_token_id`; the
        //     drive-startup snapshot is only the coldboot seed), re-resolve the
        //     GHO vToken revision (a prior tx's `Upgraded` overwrote the cached
        //     revision in place), build the discount snapshot, then dispatch the
        //     config events (each applied intra-dispatch). The seam owns the
        //     order; `ChunkContext::dispatch_transaction` carries the rationale
        //     + the Python-mirror notes for each stage.
        let dispatch = ctx
            .dispatch_transaction(conn, &mut substrate, &tx_hashes_refs, block_number)
            .await?;
        let discounts = dispatch.discounts;
        let config_events = dispatch.config_events;
        let discount_token_tx = dispatch.stk_aave_address;
        // (d) The config events were applied INTRA-dispatch: by here the tx's
        //     config writes are already on `conn` (read-your-own-writes for
        //     the ops parser below). Matches Python's per-event apply order.
        events_applied_total += config_events.len();

        // (e) C3's operations parser (sync, substrate lookups) — reads `conn`
        //     (sees this tx's config writes + prior txs' writes) + uses the
        //     overlay-resolved GHO addresses (surface #3).
        let op_events = process_transaction(
            market_id,
            chain_id,
            pool_address,
            /* treasury_address */ None,
            dispatch.gho_underlying_address,
            dispatch.gho_vtoken_address,
            conn,
            &tx_hashes_refs,
            group.tx_hash,
            &discounts,
            &mut substrate,
        )
        .map_err(|e| {
            #[expect(clippy::print_stderr)] // auditable stderr parse-fail line
            {
                eprintln!(
                    "AAVE-PARSE-FAIL block={block_number} tx=0x{} err={e}",
                    alloy::hex::encode(group.tx_hash)
                );
            }
            e
        })?;

        decode_compute_time += compute_started.elapsed();
        drop(compute_guard);
        compute_span.record(
            "rpc.round_trips",
            degenbot_rpc::provider::rpc_round_trips().saturating_sub(compute_rt_start),
        );
        // (f) Apply THIS tx's op events to `conn` — so tx N+1 sees them via
        //     read-your-own-writes (surface #2: the `Upgraded` revision bump +
        //     scaled-token balance deltas land before the next tx's reads).
        // The per-tx apply stage span (the Jaeger twin of the
        // `apply_started` anchor): this tx's op-event applies to `conn` —
        // read-your-own-writes for tx N+1's reads. SQL-only.
        let apply_span = tracing::info_span!(
            "degenbot.updater.aave.apply",
            apply.kind = "tx",
            events.n = tracing::field::Empty,
        );
        let apply_guard = apply_span.enter();
        let apply_started = Instant::now();
        ctx.apply_events(conn, &op_events, &mut substrate)?;
        apply_time += apply_started.elapsed();
        drop(apply_guard);
        apply_span.record(
            "events.n",
            u64::try_from(op_events.len()).unwrap_or(u64::MAX),
        );
        events_applied_total += op_events.len();

        // (f.4) DEBUG: per-tx touched-position trace (env-gated). When
        //     `the aave tx trace=1`, emit one JSONL line per touched
        //     `position_id` to stderr, reading the POST-APPLY
        //     `(balance, last_index)` from `conn`. The per-tx differential-
        //     narrowing tool: a divergent (user, asset)'s trajectory pinpoints
        //     the exact tx where Rust's value first takes its final divergent
        //     value (e.g. the burn that should zero but leaves a ±1 residual).
        //     `position_id` is a stable surrogate; map it to (user, asset) via
        //     the final DB + the end-of-chunk compare's divergent list. This
        //     is the per-tx sibling of the per-tx apply (step f) — narrowing
        //     the comparison inside the chunk, one tx at a time, instead of
        //     deferring it to end-of-chunk. The end-of-chunk GREEN compare
        //     (exact-zero) remains the rigorous gate; this trace is the
        //     narrowing tool, not the gate.
        if ::tracing::enabled!(::tracing::Level::DEBUG) {
            let mut seen: std::collections::HashSet<(bool, i64)> = std::collections::HashSet::new();
            for ev in &op_events {
                match ev {
                    AaveChunkEvent::ScaledTokenMint {
                        position,
                        position_id,
                        ..
                    }
                    | AaveChunkEvent::ScaledTokenBurn {
                        position,
                        position_id,
                        ..
                    } => {
                        seen.insert((
                            matches!(*position, degenbot_db::ScaledTokenPosition::Debt),
                            *position_id,
                        ));
                    }
                    AaveChunkEvent::DebtPositionReset { position_id, .. } => {
                        seen.insert((true, *position_id));
                    }
                    AaveChunkEvent::ScaledTokenTransfer {
                        from_position_id,
                        to_position_id,
                        ..
                    } => {
                        seen.insert((false, *from_position_id));
                        if let Some(to) = to_position_id {
                            seen.insert((false, *to));
                        }
                    }
                    _ => {} // ExpectedAbsent: only position-bearing events feed this trace.
                }
            }
            let txhex = alloy::hex::encode(group.tx_hash);
            let mut ordered: Vec<(bool, i64)> = seen.into_iter().collect();
            ordered.sort_unstable();
            for (is_debt, pid) in ordered {
                let table = if is_debt {
                    "aave_v3_debt_positions"
                } else {
                    "aave_v3_collateral_positions"
                };
                let q = match table {
                    "aave_v3_debt_positions" => {
                        "SELECT balance, last_index FROM aave_v3_debt_positions WHERE id = ?1"
                    }
                    "aave_v3_collateral_positions" => {
                        "SELECT balance, last_index FROM aave_v3_collateral_positions WHERE id = ?1"
                    }
                    _ => continue,
                };
                let row: Option<(Option<String>, Option<String>)> = conn
                    .query_row(q, rusqlite::params![pid], |r| {
                        Ok((
                            r.get::<_, Option<String>>(0)?,
                            r.get::<_, Option<String>>(1)?,
                        ))
                    })
                    .ok();
                if let Some((bal, idx)) = row {
                    degenbot_core::diag!(
                        domain = aave,
                        block = block_number,
                        tx = %txhex,
                        kind = %table,
                        pos = pid,
                        bal = %bal.unwrap_or_default(),
                        idx = %idx.unwrap_or_default(),
                        "AAVE-TXTRACE"
                    );
                }
            }
        }

        // (f.5) C3.3 (C refresh): the async POST-APPLY discount-refresh pass.
        //     For each `GhoRefreshDiscount` signal this tx emitted (V1-V3 GHO
        //     mints/burns), recompute the user's `gho_discount` from the
        //     POST-APPLY debt balance + the user's stkAAVE balance
        //     (`balanceOf` `eth_call` at block-1 if `stk_aave_balance` is
        //     None). The refresh reads the POST-APPLY values (the apply above
        //     just landed them) + has the provider (this loop is async).
        //     Mirrors Python's `_refresh_discount_rate` +
        //     `get_or_init_stk_aave_balance` after a GHO borrow/accrual.
        for ev in &op_events {
            if let AaveChunkEvent::GhoRefreshDiscount { position_id } = ev {
                crate::config_dispatch::refresh_gho_discount(
                    provider,
                    conn,
                    market_id,
                    *position_id,
                    block_number,
                    discount_token_tx,
                )
                .await?;
            }
        }
    }

    // (g) End-of-chunk zero-balance cleanup, then the `last_update_block`
    //     stamp as the LAST write (end-of-chunk). The cleanup deletes the
    //     burned-down collateral + debt rows the chunk zeroed (the ported
    //     Python `cleanup_zero_balance_positions`); the per-tx applies above +
    //     the cleanup are durable only when the caller's `Transaction`
    //     commits; on rollback the whole chunk (events + cleanup + stamp)
    //     reverts (the restart invariant).
    // The end-of-chunk cleanup + stamp span (the Jaeger twin of this
    // `apply_started` anchor): the deferred `ReserveDataUpdated` flush, the
    // zero-balance cleanup, + the stamp — the chunk's remaining
    // in-transaction SQL. The stamp stays the LAST write (chunk atomicity).
    let cleanup_span = tracing::info_span!("degenbot.updater.aave.apply", apply.kind = "cleanup");
    let cleanup_guard = cleanup_span.enter();
    let apply_started = Instant::now();
    // Perf C: flush the deferred `ReserveDataUpdated` writes (one sorted
    // multi-row UPDATE — the liquidity_updater deterministic-order idiom)
    // BEFORE the cleanup + stamp: the stamp stays the LAST write (chunk atomicity), and
    // the flush sits inside the chunk's transaction (a rollback reverts it
    // with the chunk; the in-memory buffer drops with the substrate). The
    // post-commit verification gate reads the flushed values.
    substrate.flush_reserve_data_updates(conn)?;
    DegenbotDb::delete_zero_balance_positions_on_conn(conn, market_id)?;
    let chunk_end_i64 = i64::try_from(chunk_end).unwrap_or(i64::MAX);
    DegenbotDb::set_market_last_update_block_on_conn(conn, market_id, chunk_end_i64)?;
    apply_time += apply_started.elapsed();
    drop(cleanup_guard);

    Ok(ChunkCoreReport {
        events_applied: events_applied_total,
        touched_user_addresses,
        decode_compute_time,
        apply_time,
    })
}

/// A small carrier for the per-chunk core's outcome (the event count + a hook
/// for richer per-type accounting in a future revision).
#[derive(Debug, Clone, Default)]
pub(super) struct ChunkCoreReport {
    pub(super) events_applied: usize,
    /// Wall time of the per-tx decode+compute block (the GHO/revision conn
    /// reads, the discount pre-pass + the config-event dispatch - both hold
    /// the write lock across their RPC reads).
    pub(super) decode_compute_time: Duration,
    /// Wall time of the per-tx event apply + the end-of-chunk cleanup/stamp
    /// (the chunk's remaining in-transaction SQL).
    pub(super) apply_time: Duration,
    /// User addresses touched by ANY event in the chunk (topics[1]/[2] of every
    /// log extracted as addresses — cheap `O(num_logs * 2)` scan). Drives the
    /// drive harness's per-chunk value-correctness gate (the verify fn
    /// accepts a touched-users filter; verifying only touched users per chunk
    /// keeps the per-chunk RPC count bounded — multicall3 batching is the
    /// market-wide extension).
    pub(super) touched_user_addresses: HashSet<Address>,
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]
mod tests {
    use super::*;

    // ── the `group_logs_by_tx` unit tests (the loop's pure
    //    tx-grouping seam — mirrors the Python `_build_transaction_contexts`).

    /// Build a minimal `alloy::rpc::types::Log` with the given tx hash,
    /// block number, + log index (the grouping key).
    fn make_tx_log(tx_hash: [u8; 32], block_number: u64, log_index: u64) -> Log {
        use alloy::primitives::{Bytes, Log as AlloyLog, B256};
        let inner = AlloyLog::new_unchecked(Address::ZERO, vec![], Bytes::new());
        Log {
            inner,
            block_hash: None,
            block_number: Some(block_number),
            block_timestamp: None,
            transaction_hash: Some(B256::from(tx_hash)),
            transaction_index: None,
            log_index: Some(log_index),
            removed: false,
        }
    }

    #[test]
    fn process_chunk_clears_zero_balance_positions_at_end_of_chunk() {
        // The live chunk path's end-of-chunk zero-balance cleanup (the ported
        // Python cleanup_zero_balance_positions) - the same substrate call the
        // batched entrypoint makes, riding the chunk's uncommitted transaction
        // so a rollback reverts it too. With an EMPTY chunk (no logs) the only
        // observable writes are the cleanup + the stamp.
        let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
                 VALUES (1, 1, 'mainnet', 1, NULL)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (1, 1, '0xu1')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_assets \
                    (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                     v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                     borrow_index, borrow_rate) \
                 VALUES (1, 1, 1, 1, 1, 1, 1, '0', '0', '1', '0')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_users \
                    (id, market_id, address, e_mode, gho_discount, stk_aave_balance, \
                     isolation_mode_collateral_asset_id, isolation_mode_debt) \
                 VALUES (1, 1, '0xuser1', 0, 0, NULL, NULL, '0')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_users \
                    (id, market_id, address, e_mode, gho_discount, stk_aave_balance, \
                     isolation_mode_collateral_asset_id, isolation_mode_debt) \
                 VALUES (2, 1, '0xuser2', 0, 0, NULL, NULL, '0')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_collateral_positions \
                    (id, user_id, asset_id, balance, last_index) VALUES (1, 1, 1, '0', NULL)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_collateral_positions \
                    (id, user_id, asset_id, balance, last_index) VALUES (2, 2, 1, '42', NULL)",
                [],
            )
            .unwrap();
        }

        // A dead-URL provider: the empty chunk never RPCs (mirrors the
        // dummy-provider precedent in config_dispatch.rs).
        let provider = degenbot_core::runtime::get_runtime()
            .block_on(AlloyProvider::new("http://127.0.0.1:1", 1))
            .expect("a dead-URL provider constructs without contact");

        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            degenbot_core::runtime::get_runtime()
                .block_on(process_chunk_on_conn(
                    &tx,
                    &provider,
                    1,
                    1,
                    Address::ZERO,
                    None,
                    &[],
                    3_000,
                    3_000,
                ))
                .unwrap();
            tx.commit().unwrap();
        }

        {
            let conn = db.lock();
            let count = |pid: i64| -> i64 {
                conn.query_row(
                    "SELECT COUNT(*) FROM aave_v3_collateral_positions WHERE id = ?1",
                    rusqlite::params![pid],
                    |r| r.get(0),
                )
                .unwrap()
            };
            assert_eq!(count(1), 0, "the zero-balance position is cleared");
            assert_eq!(count(2), 1, "the nonzero position stays");
            let stamp: Option<i64> = conn
                .query_row(
                    "SELECT last_update_block FROM aave_v3_markets WHERE id = 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(stamp, Some(3_000), "the chunk stamp still advances");
        }
    }

    #[test]
    fn group_logs_by_tx_empty_returns_empty() {
        let logs: Vec<Log> = vec![];
        let groups = group_logs_by_tx(&logs);
        assert!(groups.is_empty());
    }

    #[test]
    fn group_logs_by_tx_single_tx_buckets_all_logs() {
        let tx = [0xaa; 32];
        let logs = vec![
            make_tx_log(tx, 100, 0),
            make_tx_log(tx, 100, 1),
            make_tx_log(tx, 100, 2),
        ];
        let groups = group_logs_by_tx(&logs);
        assert_eq!(groups.len(), 1, "one tx → one group");
        assert_eq!(groups[0].tx_hash, tx);
        assert_eq!(groups[0].logs.len(), 3);
        assert_eq!(groups[0].block_number, 100);
    }

    #[test]
    fn group_logs_by_tx_multi_tx_preserves_chronological_order() {
        // The fetcher pre-sorts by (block_number, log_index); the grouping is
        // stable → groups emitted in first-seen (chronological) order.
        let tx_a = [0x11; 32];
        let tx_b = [0x22; 32];
        let tx_c = [0x33; 32];
        let logs = vec![
            // block 100: tx_a's two logs, then tx_b's one.
            make_tx_log(tx_a, 100, 0),
            make_tx_log(tx_a, 100, 1),
            make_tx_log(tx_b, 100, 2),
            // block 101: tx_c's one log.
            make_tx_log(tx_c, 101, 0),
        ];
        let groups = group_logs_by_tx(&logs);
        assert_eq!(groups.len(), 3);
        // Chronological order: tx_a (block 100, log 0), tx_b (block 100, log 2),
        // tx_c (block 101, log 0).
        assert_eq!(groups[0].tx_hash, tx_a);
        assert_eq!(groups[0].block_number, 100);
        assert_eq!(groups[0].logs.len(), 2);
        assert_eq!(groups[1].tx_hash, tx_b);
        assert_eq!(groups[1].block_number, 100);
        assert_eq!(groups[1].logs.len(), 1);
        assert_eq!(groups[2].tx_hash, tx_c);
        assert_eq!(groups[2].block_number, 101);
    }

    #[test]
    fn group_logs_by_tx_skips_logs_without_tx_hash() {
        // A log with no transaction_hash — shouldn't happen for fetched logs,
        // but the grouping is defensive (mirrors the Python which would
        // KeyError on `event["transactionHash"]`).
        let tx = [0xaa; 32];
        let log_with_tx = make_tx_log(tx, 100, 0);
        let mut log_no_tx = make_tx_log([0x00; 32], 100, 1);
        log_no_tx.transaction_hash = None;
        let logs = vec![log_with_tx.clone(), log_no_tx, log_with_tx.clone()];
        let groups = group_logs_by_tx(&logs);
        assert_eq!(groups.len(), 1, "the no-tx-hash log is skipped");
        assert_eq!(groups[0].logs.len(), 2);
    }
}
