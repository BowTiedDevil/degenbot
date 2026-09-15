//! The driver's per-transaction processing stage: per-tx log grouping + the
//! per-tx apply loop (discount pre-pass → config dispatch → operations parse
//! → the chunk-event apply → the async GHO discount refresh), all on the
//! caller's `Transaction` connection.

use std::collections::{HashMap, HashSet};

use alloy::primitives::Address;
use alloy::rpc::types::Log;
use degenbot_db::DegenbotDb;
use degenbot_rpc::provider::AlloyProvider;
use rusqlite::Connection;

use super::{apply_chunk_events_on_conn, AaveChunkEvent, RunError};
use crate::config_dispatch::{build_discount_snapshot, dispatch_config_events};
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
/// the next tx's reads (GJQGKN per-tx apply — fixes the config-revision +
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
    chunk_end: u64,
) -> Result<ChunkCoreReport, RunError> {
    // GJQGKN: per-tx apply within `conn`. Two staleness surfaces fixed —
    //   (1) config: `vtoken_revision` is now re-resolved per-tx from `conn`
    //       (read-your-own-writes sees the prior tx's `Upgraded` write),
    //       instead of the chunk-start snapshot that masked an in-chunk bump.
    //   (2) ops balances: each tx's events are applied to `conn` BEFORE the
    //       next tx's `build_discount_snapshot` / `dispatch_config_events` /
    //       `process_transaction` read — so tx N+1 sees tx N's writes (the
    //       `ScaledTokenProcessor` is stateless; its balances come from `conn`
    //       lookups, so per-tx apply is the only seam that matches Python's
    //       per-tx ORM session apply).
    // The stamp advance stays LAST (end-of-chunk), preserving the §3.4
    // restart-invariant (on rollback the stamp does NOT advance + the whole
    // chunk reverts).
    let mut events_applied_total: usize = 0;
    let mut touched_user_addresses: HashSet<Address> = HashSet::new();

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

        // (0) Re-resolve the GHO asset from `conn` for THIS tx — sees the
        //     prior tx's `ReserveInitialized` write that set
        //     `aave_gho_tokens.v_token_id` (surface #3: the drive-startup
        //     snapshot had `v_token_address=None` when the GHO reserve wasn't
        //     yet initialized at coldboot, masking a mid-drive init + causing
        //     the GHO vToken's `Mint` to classify as plain `DebtMint` instead of
        //     `GhoDebtMint` → the borrow matcher found no DebtMint → NoMatch
        //     crash). Mirrors Python's lazy `tx_context.gho_vtoken_address`
        //     reload (re-reads the `v_token` relationship from the session on
        //     each tx's `_process_transaction` entry — line 81). The
        //     drive-startup `gho_asset`/addresses params are now only the
        //     coldboot seed; this per-tx fetch is authoritative.
        let gho_asset_tx = DegenbotDb::fetch_aave_gho_asset_on_conn(conn, chain_id)?;
        let gho_token_address_tx: Option<&str> = gho_asset_tx
            .as_ref()
            .and_then(|g| g.gho_token_address.as_deref());
        let gho_vtoken_address_tx: Option<&str> = gho_asset_tx
            .as_ref()
            .and_then(|g| g.v_token_address.as_deref());
        // C3.3 (C refresh): the chain's GHO discount-token (stkAAVE) address,
        //     re-resolved per-tx from `conn` (read-your-own-writes: a mid-run
        //     `DiscountTokenUpdated` bumps `v_gho_discount_token` here — the
        //     balanceOf must hit the NEW contract). `None` → the refresh is a
        //     no-op (no discount token configured).
        let discount_token_tx: Option<Address> = gho_asset_tx
            .as_ref()
            .and_then(|g| g.v_gho_discount_token.as_deref())
            .and_then(|s| s.parse().ok());

        // (a) Re-resolve the GHO vToken's revision from `conn` for THIS tx —
        //     sees the prior tx's in-chunk `Upgraded` write via
        //     read-your-own-writes (surface #1).
        let vtoken_revision: Option<u32> = match (gho_vtoken_address_tx, gho_asset_tx.as_ref()) {
            (Some(addr_str), Some(_)) => DegenbotDb::lookup_asset_by_token_address_on_conn(
                conn, market_id, addr_str, "v_token",
            )?
            .map(|row| row.v_token_revision),
            _ => None,
        };

        // (b) The discount pre-pass (RPC + the DB-cache path) — reads `conn`
        //     (sees prior txs' writes).
        let discounts = build_discount_snapshot(
            provider,
            &tx_hashes_refs,
            block_number,
            gho_vtoken_address_tx.and_then(|s| s.parse().ok()),
            vtoken_revision,
            market_id,
            conn,
        )
        .await?;

        // (c) The config-event dispatch (RPC + substrate lookups).
        let config_events = dispatch_config_events(
            provider,
            &tx_hashes_refs,
            market_id,
            chain_id,
            conn,
            pool_address,
            oracle_address,
            gho_asset_tx.as_ref(),
            block_number,
        )
        .await?;

        // (d) The config events were applied INTRA-dispatch (I2RHGP Fix 2c:
        //     `dispatch_config_events` applies each event to `conn` as it's
        //     dispatched, so a later config event's dispatch sees an earlier
        //     event's apply — e.g. `CollateralConfigurationChanged` sees the
        //     asset `ReserveInitialized` just created). By here the tx's config
        //     writes are already on `conn` (read-your-own-writes for the ops
        //     parser below). Matches Python's per-event apply order.
        events_applied_total += config_events.len();

        // (e) C3's operations parser (sync, substrate lookups) — reads `conn`
        //     (sees this tx's config writes + prior txs' writes) + uses the
        //     per-tx re-resolved GHO addresses (surface #3).
        let op_events = process_transaction(
            market_id,
            chain_id,
            pool_address,
            /* treasury_address */ None,
            gho_token_address_tx.and_then(|s| s.parse().ok()),
            gho_vtoken_address_tx.and_then(|s| s.parse().ok()),
            conn,
            &tx_hashes_refs,
            group.tx_hash,
            &discounts,
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

        // (f) Apply THIS tx's op events to `conn` — so tx N+1 sees them via
        //     read-your-own-writes (surface #2: the `Upgraded` revision bump +
        //     scaled-token balance deltas land before the next tx's reads).
        apply_chunk_events_on_conn(conn, market_id, &op_events)?;
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
                    _ => {}
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

    // (g) Stamp `last_update_block` as the LAST write (end-of-chunk). The
    //     per-tx applies above are durable only when the caller's
    //     `Transaction` commits; on rollback the whole chunk (events + stamp)
    //     reverts (§3.4 restart-invariant).
    let chunk_end_i64 = i64::try_from(chunk_end).unwrap_or(i64::MAX);
    DegenbotDb::set_market_last_update_block_on_conn(conn, market_id, chunk_end_i64)?;

    Ok(ChunkCoreReport {
        events_applied: events_applied_total,
        touched_user_addresses,
    })
}

/// A small carrier for the per-chunk core's outcome (the event count + a hook
/// for richer per-type accounting in a future revision).
#[derive(Debug, Clone, Default)]
pub(super) struct ChunkCoreReport {
    pub(super) events_applied: usize,
    /// User addresses touched by ANY event in the chunk (topics[1]/[2] of every
    /// log extracted as addresses — cheap `O(num_logs * 2)` scan). Drives the
    /// JGQHBX drive harness's per-chunk value-correctness gate (the verify fn
    /// accepts a touched-users filter; verifying only touched users per chunk
    /// keeps the per-chunk RPC count bounded — multicall3 batching is the
    /// market-wide extension, BE474R-full).
    pub(super) touched_user_addresses: HashSet<Address>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 6SWY4R-3: the `group_logs_by_tx` unit tests (the loop's pure
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
