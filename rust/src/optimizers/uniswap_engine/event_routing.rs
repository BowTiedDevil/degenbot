//! Log event routing: apply live and backfill events to sub-engines.

use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;

use crate::bot_core::V3SwapUpdate;
use crate::bot_core::V4SwapUpdate;

use super::{BlockMetadata, HashSet, UniswapEngine};

impl UniswapEngine {
    /// Route a single WS log to the appropriate sub-engine.
    ///
    /// Decodes the topic and dispatches to the V2, V3, or V4 engine.
    /// Marks affected pool keys as dirty for subsequent `solve_dirty()`.
    ///
    /// The topic0 re-match below (`*topic == V2_SYNC_TOPIC`, etc.) is
    /// **intentional and defensive**, not redundant with the pump's `Log`-arm
    /// topic pre-filter. `apply_log` must remain safe to call with
    /// **unfiltered** inputs — backfill, tests, and any future caller may
    /// pass logs whose topic0 is outside `RELEVANT_TOPICS`, and those must
    /// no-op here rather than dispatch to a wrong decoder. The pre-filter is
    /// a lock-avoidance fast path; this re-check is the correctness floor.
    pub fn apply_log(&mut self, log: &Log, block_number: u64) {
        let Some(topic) = log.topics().first() else {
            return;
        };

        if *topic == crate::optimizers::v2_sync_decoder::V2_SYNC_TOPIC {
            if let Some(event) = crate::optimizers::v2_sync_decoder::decode_sync_log(log) {
                // ADR-003: V2 state lives in BotState. apply under the core lock
                // (nested inside the engine lock the pump already holds —
                // engine-then-core ordering). The returned single pool_id marks
                // every path using this pool in EITHER direction dirty.
                if let Some(pool_id) = self.core.write().apply_v2_sync(
                    event.pool_address,
                    event.reserve0,
                    event.reserve1,
                    block_number,
                ) {
                    self.dirty_v2.insert(pool_id);
                }
            }
        } else if *topic == crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC {
            if let Some(event) = crate::bot_core::v3_swap_decoder::decode_v3_swap_log(log) {
                // ADR-003: V3 state lives in BotState; apply under the core lock
                // (nested inside the engine lock the pump already holds).
                if let Some(pool_id) = self.core.write().apply_v3_swap(
                    event.pool_address,
                    event.sqrt_price_x96,
                    event.liquidity.to::<u128>(),
                    event.tick,
                    block_number,
                    &[],
                ) {
                    self.dirty_v3.insert(pool_id);
                }
            }
        } else if *topic == crate::bot_core::v3_mint_burn_decoder::V3_MINT_TOPIC {
            if let Some(event) = crate::bot_core::v3_mint_burn_decoder::decode_v3_mint_log(log) {
                if let Some(pool_id) = self.core.write().apply_v3_liquidity_update(
                    event.pool_address,
                    event.tick_lower,
                    event.tick_upper,
                    event.amount.cast_signed(),
                    block_number,
                ) {
                    self.dirty_v3.insert(pool_id);
                }
            }
        } else if *topic == crate::bot_core::v3_mint_burn_decoder::V3_BURN_TOPIC {
            if let Some(event) = crate::bot_core::v3_mint_burn_decoder::decode_v3_burn_log(log) {
                if let Some(pool_id) = self.core.write().apply_v3_liquidity_update(
                    event.pool_address,
                    event.tick_lower,
                    event.tick_upper,
                    -(event.amount.cast_signed()),
                    block_number,
                ) {
                    self.dirty_v3.insert(pool_id);
                }
            }
        } else if *topic == crate::bot_core::v4_swap_decoder::V4_SWAP_TOPIC {
            if let Some(event) = crate::bot_core::v4_swap_decoder::decode_v4_swap_log(log) {
                if let Some(pool_id) = self.core.write().apply_v4_swap(
                    &V4SwapUpdate {
                        pool_manager: log.address(),
                        pool_id: event.pool_id,
                        sqrt_price_x96: event.sqrt_price_x96,
                        liquidity: event.liquidity.to::<u128>(),
                        tick: event.tick,
                        tick_priors: vec![],
                    },
                    block_number,
                ) {
                    self.dirty_v4.insert(pool_id);
                }
            }
        } else if *topic == crate::bot_core::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC
        {
            if let Some(event) =
                crate::bot_core::v4_modify_liquidity_decoder::decode_v4_modify_liquidity_log(log)
            {
                if let Some(pool_id) = self.core.write().apply_v4_liquidity_update(
                    log.address(),
                    event.pool_id,
                    event.tick_lower,
                    event.tick_upper,
                    event.liquidity_delta,
                    block_number,
                ) {
                    self.dirty_v4.insert(pool_id);
                }
            }
        }
    }

    /// Mark `pool_id` dirty, classifying it into the V2/V3/V4 dirty set by
    /// consulting the shared `BotState`'s `PoolEntry` variant (ADR-006 D4).
    ///
    /// This is the subscriber-facing entry point: the `EngineSubscriber`
    /// adapter calls this from `on_pool_state_updated` (after the `BotState`
    /// write guard is released), taking only the engine `Mutex`. If `pool_id`
    /// isn't registered in `core`, the call is a no-op (the event was for a
    /// pool no path references).
    pub(crate) fn insert_dirty(&mut self, pool_id: u64) {
        let core = self.core.read();
        if core.get_v2_pool_state(pool_id).is_some() {
            drop(core);
            self.dirty_v2.insert(pool_id);
        } else if core.get_v3_pool(pool_id).is_some() {
            drop(core);
            self.dirty_v3.insert(pool_id);
        } else if core.get_v4_pool(pool_id).is_some() {
            drop(core);
            self.dirty_v4.insert(pool_id);
        }
        // Unregistered pool_id → no-op (no path references it).
    }

    /// call, but do NOT send a result batch to Python.
    ///
    /// The pump calls this eagerly after each WS log to keep engine state
    /// current. The actual batch send is triggered by the pump's debounce
    /// timer or block boundary logic.
    pub fn solve_dirty(&mut self, block_number: u64, metadata: &BlockMetadata) {
        // Expire stale buffered events in the V3/V4 buffers (ADR-003: both
        // now live on BotState).
        self.core.write().expire_v3_buffered(block_number);
        self.core.write().expire_v4_buffered(block_number);

        // Take ownership of dirty sets to avoid borrow conflict
        let dirty_v2 = std::mem::take(&mut self.dirty_v2);
        let dirty_v3 = std::mem::take(&mut self.dirty_v3);
        let dirty_v4 = std::mem::take(&mut self.dirty_v4);

        // Re-solve only paths containing updated pools (no batch send)
        self.rebuild_and_solve_affected(&dirty_v2, &dirty_v3, &dirty_v4, block_number, metadata);

        // dirty sets are already cleared by std::mem::take
        self.last_processed_block = Some(block_number);
    }

    /// Compute the incremental diff and send a result batch to Python.
    ///
    /// Called by the pump when the debounce timer fires (mid-block) or
    /// when a block boundary is detected. Results must already be
    /// up-to-date (via `solve_dirty`) before calling this.
    pub fn send_result_batch(&mut self, metadata: &BlockMetadata) {
        self.compute_diff_and_send(metadata);
    }

    /// Returns `true` if there are unsolved dirty pool keys from `apply_log`
    /// calls that haven't been followed by `solve_dirty` yet.
    #[must_use]
    pub fn has_dirty_paths(&self) -> bool {
        !self.dirty_v2.is_empty() || !self.dirty_v3.is_empty() || !self.dirty_v4.is_empty()
    }

    /// Process a block: apply all logs then solve affected paths.
    /// Does NOT send a result batch — the pump controls dispatch.
    pub fn process_block(&mut self, logs: &[Log], block_number: u64, metadata: &BlockMetadata) {
        for log in logs {
            self.apply_log(log, block_number);
        }
        self.solve_dirty(block_number, metadata);
    }

    /// Process a block, solve, and send result batch to Python.
    /// Used for empty-block notifications where the pump doesn't go
    /// through the debounce path.
    pub fn process_block_and_send(
        &mut self,
        logs: &[Log],
        block_number: u64,
        metadata: &BlockMetadata,
    ) {
        self.process_block(logs, block_number, metadata);
        self.compute_diff_and_send(metadata);
    }

    /// Finalize the current block: if dirty paths accumulated since the last
    /// solve would be left behind by a block advance, solve them and send a
    /// result batch carrying `metadata`. Otherwise, if logs were observed but
    /// touched no registered pools, send an empty block-boundary batch so
    /// Python sees the advance.
    ///
    /// This is the engine-side logic behind `UniswapEnginePump::finalize_if_dirty`.
    /// Holding it on the engine (rather than the pump) keeps it next to its
    /// siblings (`solve_dirty`, `send_result_batch`, `process_block_and_send`)
    /// and makes the metadata-threading contract unit-testable without a live
    /// WS connection. The pump passes its real `current_metadata` so that any
    /// batch emitted here carries genuine fees/gas/timestamp (previously this
    /// path sent `BlockMetadata::default()`, which would make the Python
    /// consumer compute `base_fee_next = 0` and broadcast underpriced txs —
    /// see `rust/CONTEXT.md` `ResultBatch` entry).
    ///
    /// The `block > *last_solved_block` guard is load-bearing: the pump runs
    /// `solve_dirty` at the top of every loop iteration before awaiting the
    /// next event, so this normally only sends on a genuine block advance.
    pub fn finalize_block(
        &mut self,
        block: u64,
        metadata: &BlockMetadata,
        last_solved_block: &mut u64,
        has_logs_this_block: &mut bool,
    ) {
        if block > *last_solved_block {
            if self.has_dirty_paths() {
                self.solve_dirty(block, metadata);
                self.send_result_batch(metadata);
            } else if *has_logs_this_block {
                self.process_block_and_send(&[], block, metadata);
            }
            *last_solved_block = block;
            *has_logs_this_block = false;
        }
    }

    /// Process pre-decoded updates for testing.
    pub fn process_updates(
        &mut self,
        v2_updates: &[(Address, U256, U256)],
        v3_updates: &[V3SwapUpdate],
        block_number: u64,
        metadata: &BlockMetadata,
    ) {
        // Apply V2+V3 updates to BotState and collect affected pool ids (ADR-003)
        let mut v2_affected = HashSet::new();
        let mut v3_affected = HashSet::new();
        {
            let mut core = self.core.write();
            for &(addr, r0, r1) in v2_updates {
                if let Some(pool_id) = core.apply_v2_sync(addr, r0, r1, block_number) {
                    v2_affected.insert(pool_id);
                }
            }
            for update in v3_updates {
                if let Some(pool_id) = core.apply_v3_swap(
                    update.pool_address,
                    update.sqrt_price_x96,
                    update.liquidity,
                    update.tick,
                    block_number,
                    &update.tick_priors,
                ) {
                    v3_affected.insert(pool_id);
                }
            }
        }

        // Re-solve only paths containing updated pools
        self.rebuild_and_solve_affected(
            &v2_affected,
            &v3_affected,
            &HashSet::new(),
            block_number,
            metadata,
        );
        self.last_processed_block = Some(block_number);
    }

    /// Process pre-decoded V4 updates.
    pub fn process_v4_updates(
        &mut self,
        v4_updates: &[V4SwapUpdate],
        block_number: u64,
        metadata: &BlockMetadata,
    ) {
        let mut v4_affected = HashSet::new();
        {
            let mut core = self.core.write();
            for update in v4_updates {
                if let Some(pool_id) = core.apply_v4_swap(update, block_number) {
                    v4_affected.insert(pool_id);
                }
            }
        }
        self.rebuild_and_solve_affected(
            &HashSet::new(),
            &HashSet::new(),
            &v4_affected,
            block_number,
            metadata,
        );
    }

    /// Process all updates at once (V2 + V3 + V4).
    pub fn process_all_updates(
        &mut self,
        v2_updates: &[(Address, U256, U256)],
        v3_updates: &[V3SwapUpdate],
        v4_updates: &[V4SwapUpdate],
        block_number: u64,
        metadata: &BlockMetadata,
    ) {
        let mut v2_affected = HashSet::new();
        let mut v3_affected = HashSet::new();
        let mut v4_affected = HashSet::new();
        {
            let mut core = self.core.write();
            for &(addr, r0, r1) in v2_updates {
                if let Some(pool_id) = core.apply_v2_sync(addr, r0, r1, block_number) {
                    v2_affected.insert(pool_id);
                }
            }
            for update in v3_updates {
                if let Some(pool_id) = core.apply_v3_swap(
                    update.pool_address,
                    update.sqrt_price_x96,
                    update.liquidity,
                    update.tick,
                    block_number,
                    &update.tick_priors,
                ) {
                    v3_affected.insert(pool_id);
                }
            }
            for update in v4_updates {
                if let Some(pool_id) = core.apply_v4_swap(update, block_number) {
                    v4_affected.insert(pool_id);
                }
            }
        }
        self.rebuild_and_solve_affected(
            &v2_affected,
            &v3_affected,
            &v4_affected,
            block_number,
            metadata,
        );
        self.last_processed_block = Some(block_number);
    }

    /// Apply backfill logs to the V3/V4 sub-engines.
    ///
    /// Unlike `apply_log`, liquidity updates are buffered rather than
    /// applied immediately. Swap updates are applied directly.
    /// After all logs are processed, expired buffers are purged and
    /// the sub-engines rebuild their solve states.
    ///
    /// `chunk_end` is the chunk boundary (the highest block in this paginated
    /// `eth_getLogs` batch) — used as the buffer-expiry cutoff and to advance
    /// `last_processed_block`. Each applied log is stamped with ITS OWN
    /// `block_number` (decoded from the RPC log), NOT `chunk_end`. Pre-fix
    /// every log in the chunk was journaled at `chunk_end`, so same-block
    /// replacement collapsed the whole chunk into one journal delta and a
    /// mid-chunk reorg couldn't restore a per-block landed-at state.
    pub fn process_backfill_logs(&mut self, logs: &[Log], chunk_end: u64) {
        use crate::bot_core::v3_mint_burn_decoder::{decode_v3_burn_log, decode_v3_mint_log};
        use crate::bot_core::v3_swap_decoder::decode_v3_swap_log;
        use crate::bot_core::v4_modify_liquidity_decoder::decode_v4_modify_liquidity_log;
        use crate::bot_core::v4_swap_decoder::decode_v4_swap_log;

        let mut v3_touched = false;
        let mut v4_touched = false;

        for log in logs {
            // Stamp this log with its own block number. A backfill log should
            // always carry `block_number`; fall back to `chunk_end` only for a
            // malformed log so apply never sees block 0.
            let log_block = log.block_number.unwrap_or(chunk_end);

            let Some(topic0) = log.topic0() else {
                continue;
            };

            if *topic0 == crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC {
                if let Some(event) = decode_v3_swap_log(log) {
                    self.core.write().apply_v3_swap(
                        event.pool_address,
                        event.sqrt_price_x96,
                        event.liquidity.to::<u128>(),
                        event.tick,
                        log_block,
                        &[],
                    );
                    v3_touched = true;
                }
            } else if *topic0 == crate::bot_core::v3_mint_burn_decoder::V3_MINT_TOPIC {
                if let Some(event) = decode_v3_mint_log(log) {
                    self.core.write().buffer_backfill_v3_liquidity_update(
                        event.pool_address,
                        event.tick_lower,
                        event.tick_upper,
                        event.amount.cast_signed(),
                        log_block,
                    );
                    v3_touched = true;
                }
            } else if *topic0 == crate::bot_core::v3_mint_burn_decoder::V3_BURN_TOPIC {
                if let Some(event) = decode_v3_burn_log(log) {
                    self.core.write().buffer_backfill_v3_liquidity_update(
                        event.pool_address,
                        event.tick_lower,
                        event.tick_upper,
                        -(event.amount.cast_signed()),
                        log_block,
                    );
                    v3_touched = true;
                }
            } else if *topic0 == crate::bot_core::v4_swap_decoder::V4_SWAP_TOPIC {
                if let Some(event) = decode_v4_swap_log(log) {
                    self.core.write().apply_v4_swap(
                        &V4SwapUpdate {
                            pool_manager: log.address(),
                            pool_id: event.pool_id,
                            sqrt_price_x96: event.sqrt_price_x96,
                            liquidity: event.liquidity.to::<u128>(),
                            tick: event.tick,
                            tick_priors: vec![],
                        },
                        log_block,
                    );
                    v4_touched = true;
                }
            } else if *topic0
                == crate::bot_core::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC
            {
                if let Some(event) = decode_v4_modify_liquidity_log(log) {
                    self.core.write().buffer_backfill_v4_liquidity_update(
                        log.address(),
                        event.pool_id,
                        event.tick_lower,
                        event.tick_upper,
                        event.liquidity_delta,
                        log_block,
                    );
                    v4_touched = true;
                }
            }
        }

        if v3_touched {
            self.core.write().expire_v3_buffered(chunk_end);
        }
        if v4_touched {
            self.core.write().expire_v4_buffered(chunk_end);
            // NOTE: the orphan `rebuild_and_solve` that previously solved
            // V4-engine-local paths here is removed (ADR-003 S3). The
            // unified engine re-derives affected paths on the next
            // `solve_dirty`; V4-engine-local paths/results are deleted as
            // duplicate dead code (the unified `register_path`/`solve_path`
            // handles V4 paths via `HopType::V4`).
        }

        self.last_processed_block = Some(chunk_end);
    }
}
