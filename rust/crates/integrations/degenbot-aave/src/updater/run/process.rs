//! The driver's per-transaction processing stage: per-tx log grouping + the
//! per-tx loop (discount pre-pass → configuration dispatch → the operation
//! parse, which reduces against the chunk's projection view and advances it →
//! delta persistence → the async GHO discount refresh), all on the caller's
//! `Transaction` connection.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use alloy::rpc::types::Log;
use degenbot_core::canonical_head::{ChainAnchor, TxRef};
use degenbot_db::{CursorAnchor, DegenbotDb, ProjectionCursor, ProjectionUnit};
use rusqlite::Connection;

use super::resolver::ChunkResolution;
use super::substrate::ChunkSubstrate;
use super::RunError;
use crate::updater::aave_fetch::{ChunkLogFacts, LogIdentityRefusal};

/// One transaction's grouped logs, carrying the exact coordinates the group
/// occupies: its block (height AND hash) and its transaction index.
///
/// The coordinates are the GROUP's identity, not its first log's: a group is one
/// transaction of one block, and a transaction reference is built from these facts
/// plus the anchor the caller is filling (ADR-105 D1, D2). Groups are emitted in
/// first-seen order, which the fetcher's `(block_number, log_index)` sort makes
/// chronological.
#[derive(Debug)]
pub(super) struct TxGroup<'a> {
    pub(super) tx_hash: [u8; 32],
    pub(super) block_number: u64,
    block_hash: B256,
    transaction_index: u64,
    pub(super) logs: Vec<&'a Log>,
}

impl TxGroup<'_> {
    /// The group's exact transaction position in `anchor`'s block.
    ///
    /// # Errors
    ///
    /// [`LogIdentityRefusal::AnchorMismatch`] when the group's block is not the
    /// anchor's — a same-height branch, or a group lifted from another block.
    pub(super) fn tx_ref(&self, anchor: ChainAnchor) -> Result<TxRef, LogIdentityRefusal> {
        if anchor.number() != self.block_number || anchor.hash() != self.block_hash {
            return Err(LogIdentityRefusal::AnchorMismatch {
                block_number: self.block_number,
                block_hash: self.block_hash,
                anchor_number: anchor.number(),
                anchor_hash: anchor.hash(),
            });
        }
        TxRef::new(anchor, B256::from(self.tx_hash), self.transaction_index).map_err(|_| {
            LogIdentityRefusal::MissingTransactionHash {
                block_number: self.block_number,
            }
        })
    }
}

/// Group a chunk's logs by `transactionHash`, preserving the
/// `(block_number, log_index)` sort: the events are pre-sorted, then bucketed by
/// `tx_hash`; the groups are emitted in first-seen order (chronological, which the
/// fetcher's sort guarantees).
///
/// Every log must carry its exact position: a log with no transaction hash, no block
/// hash, no transaction index, or no block-wide log index has no coordinate to group
/// by, and a group whose logs disagree about their block or transaction is
/// contradictory evidence. One block-wide `(block_number, log_index)` also names ONE
/// log for the WHOLE chunk: a second transaction claiming it is contradictory evidence,
/// while the same log stated twice is the duplicate/order refusal. Every variant is
/// raised before the chunk's writes begin (ADR-105 D1).
///
/// # Errors
///
/// One [`LogIdentityRefusal`] per log that cannot be placed, plus the set-consistency
/// variants ([`LogIdentityRefusal::ConflictingCoordinates`],
/// [`LogIdentityRefusal::ForeignBlockInGroup`],
/// [`LogIdentityRefusal::ForeignTransactionInGroup`],
/// [`LogIdentityRefusal::LogsOutOfOrder`]).
pub(super) fn group_logs_by_tx(logs: &[Log]) -> Result<Vec<TxGroup<'_>>, LogIdentityRefusal> {
    let mut order: Vec<[u8; 32]> = Vec::new();
    let mut groups: HashMap<[u8; 32], TxGroup<'_>> = HashMap::new();
    let mut previous: HashMap<[u8; 32], u64> = HashMap::new();
    let mut claimed: HashMap<(u64, u64), ChunkLogFacts> = HashMap::new();
    for log in logs {
        let facts = ChunkLogFacts::of(log)?;
        let position = facts.position;
        let coordinate = (position.block_number, position.log_index);
        if let Some(existing) = claimed.get(&coordinate) {
            if *existing != facts {
                return Err(LogIdentityRefusal::ConflictingCoordinates {
                    block_number: position.block_number,
                    log_index: position.log_index,
                });
            }
            return Err(LogIdentityRefusal::LogsOutOfOrder {
                previous: position.log_index,
                actual: position.log_index,
            });
        }
        claimed.insert(coordinate, facts);
        let tx_hash = position.transaction_hash.0;
        let entry = groups.entry(tx_hash).or_insert_with(|| {
            order.push(tx_hash);
            TxGroup {
                tx_hash,
                block_number: position.block_number,
                block_hash: position.block_hash,
                transaction_index: position.transaction_index,
                logs: Vec::new(),
            }
        });
        if entry.block_number != position.block_number {
            return Err(LogIdentityRefusal::ForeignBlockInGroup {
                block_number: entry.block_number,
                actual: position.block_number,
            });
        }
        if entry.block_hash != position.block_hash
            || entry.transaction_index != position.transaction_index
        {
            return Err(LogIdentityRefusal::ForeignTransactionInGroup {
                transaction_hash: position.transaction_hash,
            });
        }
        if let Some(previous_index) = previous.insert(tx_hash, position.log_index) {
            if position.log_index <= previous_index {
                return Err(LogIdentityRefusal::LogsOutOfOrder {
                    previous: previous_index,
                    actual: position.log_index,
                });
            }
        }
        entry.logs.push(log);
    }
    Ok(order
        .into_iter()
        .map(|k| {
            #[expect(clippy::expect_used)] // `order` yields group keys known to `groups`
            groups.remove(&k).expect("present in order")
        })
        .collect())
}

/// Prove every group emitted at `terminal`'s height belongs to the terminal anchor's
/// own block, so each one states an exact transaction position under the run's
/// resolved anchor rather than a height that could have been re-resolved.
///
/// This is the per-transaction face of the terminal fence: the height's LOGS are
/// already checked against the anchor (`place_logs_at`), and this check extends that
/// identity to the transactions the parsed chunk is about to write, before any of it
/// touches the database.
///
/// # Errors
///
/// [`LogIdentityRefusal::AnchorMismatch`] for a terminal-height group whose block hash
/// is not the anchor's, or [`LogIdentityRefusal::MissingTransactionHash`] when the
/// group's transaction cannot be named.
pub(super) fn guard_terminal_transaction_identity(
    terminal: ChainAnchor,
    groups: &[TxGroup<'_>],
) -> Result<(), LogIdentityRefusal> {
    for group in groups {
        if group.block_number == terminal.number() {
            let _ = group.tx_ref(terminal)?;
        }
    }
    Ok(())
}

/// Persist one chunk's resolved deltas under the caller's write transaction,
/// then flush the deferred reserve-data writes, clean the zeroed positions and
/// stamp the cursor last (the restart invariant).
///
/// This is the chunk's whole write surface: no acquisition, no decode, no
/// identity resolution (ADR-105 D4).
///
/// # Errors
///
/// Returns [`RunError::Db`] on any write failure; the caller drops the
/// transaction, rolling the whole chunk back.
pub(super) fn persist_chunk(
    conn: &Connection,
    resolution: &mut ChunkResolution,
    market_id: i64,
    chain_id: i64,
    chunk_end: u64,
    anchor: CursorAnchor,
) -> Result<ChunkCoreReport, RunError> {
    let apply_started = Instant::now();
    let mut substrate =
        std::mem::replace(&mut resolution.substrate, ChunkSubstrate::lazy(chain_id));
    crate::run::apply_canonical_deltas_on_conn(conn, &resolution.deltas, &mut substrate)?;
    substrate.flush_reserve_data_updates(conn)?;
    DegenbotDb::delete_zero_balance_positions_on_conn(conn, market_id)?;
    let projection_cursor = commit_projection_unit_on_conn(
        conn,
        market_id,
        chain_id,
        chunk_end,
        anchor,
        &resolution.deltas,
        resolution.checkpoint.as_ref(),
    )?;
    let chunk_end_i64 = i64::try_from(chunk_end).unwrap_or(i64::MAX);
    DegenbotDb::set_market_last_update_block_on_conn(conn, market_id, chunk_end_i64)?;
    let apply_time = apply_started.elapsed();
    resolution.substrate = substrate;
    Ok(ChunkCoreReport {
        events_applied: resolution.deltas.len(),
        touched_user_addresses: resolution.touched_user_addresses.clone(),
        deltas: resolution.deltas.clone(),
        decode_compute_time: resolution.decode_compute_time,
        apply_time,
        projection_cursor,
    })
}

/// Journal the chunk's canonical deltas and advance the market's exact cursor
/// under the caller's transaction (ADR-105 D8).
///
/// A chunk that journaled nothing claims no exact anchor: it has no position of
/// its own to establish or continue, so it leaves the cursor untouched and only
/// the numeric range claim (the `last_update_block` stamp) advances. A chunk
/// with deltas commits its journal rows and its exact cursor together.
///
/// # Errors
///
/// [`degenbot_db::DbError::Projection`] for any refusal — the caller drops the
/// transaction, so the chunk's state rows, its journal, and its cursor all roll
/// back.
pub(super) fn commit_projection_unit_on_conn(
    conn: &Connection,
    market_id: i64,
    chain_id: i64,
    chunk_end: u64,
    anchor: CursorAnchor,
    deltas: &[crate::projection::CanonicalDelta],
    checkpoint: Option<&crate::projection::CheckpointState>,
) -> Result<Option<ProjectionCursor>, degenbot_db::DbError> {
    let entries = crate::projection::journal_entries(deltas);
    let anchor = match anchor {
        CursorAnchor::First { .. } if entries.is_empty() => CursorAnchor::Unchanged,
        other => other,
    };
    let checkpoint = checkpoint
        .filter(|_| !entries.is_empty())
        .map(crate::projection::CheckpointState::wire);
    let unit = ProjectionUnit {
        market_id,
        chain_id,
        encoding_version: crate::projection::DELTA_ENCODING_VERSION,
        entries: &entries,
        range_end_block: chunk_end,
        anchor,
        checkpoint,
    };
    Ok(degenbot_db::write_projection_unit_on_conn(conn, &unit)?)
}

/// The per-chunk core's outcome: the applied event count, the two stage
/// timings, and the chunk's touched users.
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
    /// Every canonical delta this chunk reduced, in emission order (per
    /// transaction: the configuration family as it was dispatched, then the
    /// operation family) — the chunk's write product. Each delta states its
    /// exact position, so a consumer can order the stream itself (the replay
    /// bench and the cross-chunk equality gate read it; a journal consumes it
    /// later).
    pub(super) deltas: Vec<crate::projection::CanonicalDelta>,
    /// User addresses touched by ANY event in the chunk (topics[1]/[2] of every
    /// log extracted as addresses — cheap `O(num_logs * 2)` scan). Drives the
    /// drive harness's per-chunk value-correctness gate (the verify fn
    /// accepts a touched-users filter; verifying only touched users per chunk
    /// keeps the per-chunk RPC count bounded — multicall3 batching is the
    /// market-wide extension).
    pub(super) touched_user_addresses: HashSet<Address>,
    /// The market's exact projection cursor after this chunk, when it has one.
    pub(super) projection_cursor: Option<ProjectionCursor>,
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
    //    tx-grouping seam).

    /// Build a minimal `alloy::rpc::types::Log` with the given tx hash,
    /// block number, + log index (the grouping key).
    fn make_tx_log(tx_hash: [u8; 32], block_number: u64, log_index: u64) -> Log {
        use alloy::primitives::{Bytes, Log as AlloyLog, B256, U256};
        let inner = AlloyLog::new_unchecked(Address::ZERO, vec![], Bytes::new());
        Log {
            inner,
            block_hash: Some(B256::from(U256::from(block_number))),
            block_number: Some(block_number),
            block_timestamp: None,
            transaction_hash: Some(B256::from(tx_hash)),
            transaction_index: Some(0),
            log_index: Some(log_index),
            removed: false,
        }
    }

    #[test]
    fn process_chunk_clears_zero_balance_positions_at_end_of_chunk() {
        // The live chunk path's end-of-chunk zero-balance cleanup - the same
        // substrate call the
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
            .block_on(degenbot_rpc::provider::AlloyProvider::new(
                "http://127.0.0.1:1",
                1,
            ))
            .expect("a dead-URL provider constructs without contact");

        let mut resolution = degenbot_core::runtime::get_runtime()
            .block_on(crate::updater::run::resolver::ChunkResolver::new().resolve(
                &provider,
                crate::updater::run::seed::ChunkSeed::empty(1),
                1,
                1,
                Address::ZERO,
                &[],
                3_000,
                3_000,
            ))
            .unwrap();
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            persist_chunk(&tx, &mut resolution, 1, 1, 3_000, CursorAnchor::Unchanged).unwrap();
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
        let groups = group_logs_by_tx(&logs).expect("an empty chunk has no log to refuse");
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
        let groups = group_logs_by_tx(&logs).expect("the fixture logs carry exact coordinates");
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
        let groups = group_logs_by_tx(&logs).expect("the fixture logs carry exact coordinates");
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

    /// The terminal fence extends to transactions: a group at the anchor's height
    /// must state an exact position under that anchor, and a same-height foreign
    /// group is refused before the chunk is written.
    #[test]
    fn the_terminal_fence_covers_terminal_height_transactions() {
        use degenbot_core::canonical_head::ChainAnchor;

        let tx = [0xaa; 32];
        let logs = vec![make_tx_log(tx, 100, 0)];
        let groups = group_logs_by_tx(&logs).expect("the fixture log carries its coordinates");
        let anchor = ChainAnchor::new(
            1,
            100,
            alloy::primitives::B256::from(alloy::primitives::U256::from(100u64)),
            alloy::primitives::B256::repeat_byte(0xaa),
        )
        .unwrap();

        assert_eq!(guard_terminal_transaction_identity(anchor, &groups), Ok(()));

        let elsewhere = ChainAnchor::new(
            1,
            100,
            alloy::primitives::B256::repeat_byte(0xcd),
            alloy::primitives::B256::repeat_byte(0xaa),
        )
        .unwrap();
        assert!(matches!(
            guard_terminal_transaction_identity(elsewhere, &groups).unwrap_err(),
            crate::updater::aave_fetch::LogIdentityRefusal::AnchorMismatch { .. }
        ));
    }

    /// A group's block is its own exact identity: the same-height branch a
    /// number-keyed grouping cannot see is a refusal, not a misplaced group.
    #[test]
    fn a_group_cannot_be_attributed_to_another_block_at_its_height() {
        use degenbot_core::canonical_head::ChainAnchor;

        let tx = [0xaa; 32];
        let logs = vec![make_tx_log(tx, 100, 0), make_tx_log(tx, 100, 1)];
        let groups = group_logs_by_tx(&logs).expect("the fixture logs carry exact coordinates");
        let group = &groups[0];

        let same_height_other_branch = ChainAnchor::new(
            1,
            100,
            alloy::primitives::B256::repeat_byte(0xbb),
            alloy::primitives::B256::repeat_byte(0xaa),
        )
        .unwrap();
        assert!(matches!(
            group.tx_ref(same_height_other_branch).unwrap_err(),
            crate::updater::aave_fetch::LogIdentityRefusal::AnchorMismatch { .. }
        ));

        let anchor = ChainAnchor::new(
            1,
            100,
            alloy::primitives::B256::from(alloy::primitives::U256::from(100u64)),
            alloy::primitives::B256::repeat_byte(0xaa),
        )
        .unwrap();
        let reference = group.tx_ref(anchor).expect("the group is this block's");
        assert_eq!(reference.hash(), alloy::primitives::B256::from(tx));
        assert_eq!(reference.index(), 0);
        assert_eq!(reference.anchor(), &anchor);
    }

    /// Two logs claiming one coordinate with different block hashes are
    /// contradictory evidence inside one chunk: the merge refuses instead of
    /// de-duplicating a same-height foreign log away.
    #[test]
    fn a_conflicting_coordinate_is_not_a_duplicate() {
        use alloy::primitives::B256;

        let tx = [0xaa; 32];
        let held = make_tx_log(tx, 100, 7);
        let mut stranger = held.clone();
        stranger.block_hash = Some(B256::repeat_byte(0xbb));
        let mut logs = vec![held];

        assert_eq!(
            crate::updater::aave_fetch::merge_chunk_log_extension(&mut logs, vec![stranger])
                .unwrap_err(),
            crate::updater::aave_fetch::LogIdentityRefusal::ConflictingCoordinates {
                block_number: 100,
                log_index: 7,
            }
        );
        assert_eq!(logs.len(), 1, "the held log is untouched");
    }

    /// One coordinate holds ONE log: a repeat whose content differs is a second log
    /// claiming the same position, so the merge refuses and leaves the held set
    /// untouched rather than silently taking the first.
    #[test]
    fn mutated_content_at_one_coordinate_is_not_a_duplicate() {
        let tx = [0xaa; 32];
        let held = make_tx_log(tx, 100, 7);
        let mut mutated = held.clone();
        mutated.inner = alloy::primitives::Log::new_unchecked(
            Address::repeat_byte(0x42),
            vec![alloy::primitives::B256::repeat_byte(0x11)],
            alloy::primitives::Bytes::from(vec![0x01]),
        );
        let mut logs = vec![held.clone()];

        assert_eq!(
            crate::updater::aave_fetch::merge_chunk_log_extension(&mut logs, vec![mutated])
                .unwrap_err(),
            crate::updater::aave_fetch::LogIdentityRefusal::ConflictingCoordinates {
                block_number: 100,
                log_index: 7,
            }
        );
        assert_eq!(logs.len(), 1, "the held log is untouched");
        assert_eq!(logs[0].address(), held.address());
    }

    /// A removed log is not canonical evidence: it describes a log the chain no longer
    /// states, so it has no position and is refused at the coordinate seam — before
    /// grouping, merging, or any write.
    #[test]
    fn a_removed_log_has_no_coordinate() {
        use crate::updater::aave_fetch::{ChunkLogPosition, LogIdentityRefusal};

        let tx = [0xaa; 32];
        let mut log = make_tx_log(tx, 100, 7);
        log.removed = true;

        assert_eq!(
            ChunkLogPosition::of(&log).unwrap_err(),
            LogIdentityRefusal::RemovedLog {
                block_number: 100,
                log_index: 7,
            }
        );
        assert_eq!(
            group_logs_by_tx(&[log]).unwrap_err(),
            LogIdentityRefusal::RemovedLog {
                block_number: 100,
                log_index: 7,
            }
        );
    }

    /// The same log answered twice is one log: the exact coordinate collapses it.
    #[test]
    fn an_answered_twice_log_is_one_log() {
        let tx = [0xaa; 32];
        let mut logs = vec![make_tx_log(tx, 100, 7)];

        crate::updater::aave_fetch::merge_chunk_log_extension(
            &mut logs,
            vec![make_tx_log(tx, 100, 7)],
        )
        .expect("an identical coordinate is the same log");

        assert_eq!(logs.len(), 1);
    }

    /// One block-wide index names ONE log in the whole chunk: two transactions
    /// claiming the same (block, index) is contradictory evidence, and a number-only or
    /// per-transaction check cannot see it.
    #[test]
    fn two_transactions_cannot_claim_one_block_wide_index() {
        use crate::updater::aave_fetch::LogIdentityRefusal;

        let first = make_tx_log([0xaa; 32], 100, 7);
        let mut other_transaction = make_tx_log([0xbb; 32], 100, 7);
        let mut other_block_hash = other_transaction.clone();
        other_block_hash.block_hash = Some(alloy::primitives::B256::repeat_byte(0xbb));

        for second in [&mut other_transaction, &mut other_block_hash] {
            let logs = vec![first.clone(), second.clone()];
            assert_eq!(
                group_logs_by_tx(&logs).unwrap_err(),
                LogIdentityRefusal::ConflictingCoordinates {
                    block_number: 100,
                    log_index: 7,
                },
                "one block-wide index, two claims: {second:?}"
            );
        }
    }

    /// The same log repeated at its own coordinate is still the order refusal: it is
    /// one transaction claiming one position twice, not two different logs.
    #[test]
    fn a_repeated_log_in_one_transaction_is_out_of_order() {
        use crate::updater::aave_fetch::LogIdentityRefusal;

        let log = make_tx_log([0xaa; 32], 100, 7);

        assert_eq!(
            group_logs_by_tx(&[log.clone(), log]).unwrap_err(),
            LogIdentityRefusal::LogsOutOfOrder {
                previous: 7,
                actual: 7,
            }
        );
    }

    /// The held set is validated even when no supplemental batch arrives: two logs of
    /// one coordinate with different content are contradictory evidence, and the merge
    /// refuses without touching the held logs.
    #[test]
    fn a_held_set_conflict_is_refused_without_an_extension() {
        use crate::updater::aave_fetch::{merge_chunk_log_extension, LogIdentityRefusal};

        let held = make_tx_log([0xaa; 32], 100, 7);
        let mut stranger = held.clone();
        stranger.inner = alloy::primitives::Log::new_unchecked(
            Address::repeat_byte(0x42),
            vec![alloy::primitives::B256::repeat_byte(0x11)],
            alloy::primitives::Bytes::from(vec![0x01]),
        );
        let mut logs = vec![held.clone(), stranger];

        assert_eq!(
            merge_chunk_log_extension(&mut logs, Vec::new()).unwrap_err(),
            LogIdentityRefusal::ConflictingCoordinates {
                block_number: 100,
                log_index: 7,
            }
        );
        assert_eq!(logs.len(), 2, "the held set is untouched");
        assert_eq!(logs[0].address(), held.address());
    }

    #[test]
    fn group_logs_by_tx_refuses_logs_without_tx_hash() {
        // A fetched log always carries a tx hash; one without a hash has no
        // transaction identity to group by, so the chunk is refused rather than
        // silently parsed without that log.
        let tx = [0xaa; 32];
        let log_with_tx = make_tx_log(tx, 100, 0);
        let mut log_no_tx = make_tx_log([0x00; 32], 100, 1);
        log_no_tx.transaction_hash = None;
        let logs = vec![log_with_tx.clone(), log_no_tx, log_with_tx];
        assert_eq!(
            group_logs_by_tx(&logs).unwrap_err(),
            crate::updater::aave_fetch::LogIdentityRefusal::MissingTransactionHash {
                block_number: 100,
            }
        );
    }
}
