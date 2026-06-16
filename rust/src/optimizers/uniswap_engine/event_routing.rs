//! Log event routing: apply live and backfill events to sub-engines.

use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;

use crate::optimizers::v3_block_engine::V3SwapUpdate;
use crate::optimizers::v4_block_engine::V4SwapUpdate;

use super::{UniswapEngine, BlockMetadata, HashSet};

impl UniswapEngine {
    /// Route a single WS log to the appropriate sub-engine.
    ///
    /// Decodes the topic and dispatches to the V2, V3, or V4 engine.
    /// Marks affected pool keys as dirty for subsequent `solve_dirty()`.
    pub fn apply_log(&mut self, log: &Log, block_number: u64) {
        let Some(topic) = log.topics().first() else {
            return;
        };

        if *topic == crate::optimizers::v2_sync_decoder::V2_SYNC_TOPIC {
            if let Some(event) = crate::optimizers::v2_sync_decoder::decode_sync_log(log) {
                for key in self.v2_engine.apply_sync(
                    event.pool_address,
                    event.reserve0,
                    event.reserve1,
                ).iter() {
                    self.dirty_v2.insert(key);
                }
            }
        } else if *topic == crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC {
            if let Some(event) = crate::bot_core::v3_swap_decoder::decode_v3_swap_log(log) {
                for key in self.v3_engine.apply_swap(
                    event.pool_address,
                    event.sqrt_price_x96,
                    event.liquidity.to::<u128>(),
                    event.tick,
                    block_number,
                    &[],
                ).iter() {
                    self.dirty_v3.insert(key);
                }
            }
        } else if *topic == crate::bot_core::v3_mint_burn_decoder::V3_MINT_TOPIC {
            if let Some(event) = crate::bot_core::v3_mint_burn_decoder::decode_v3_mint_log(log) {
                for key in self.v3_engine.apply_liquidity_update(
                    event.pool_address,
                    event.tick_lower,
                    event.tick_upper,
                    event.amount.cast_signed(),
                    block_number,
                ).iter() {
                    self.dirty_v3.insert(key);
                }
            }
        } else if *topic == crate::bot_core::v3_mint_burn_decoder::V3_BURN_TOPIC {
            if let Some(event) = crate::bot_core::v3_mint_burn_decoder::decode_v3_burn_log(log) {
                for key in self.v3_engine.apply_liquidity_update(
                    event.pool_address,
                    event.tick_lower,
                    event.tick_upper,
                    -(event.amount.cast_signed()),
                    block_number,
                ).iter() {
                    self.dirty_v3.insert(key);
                }
            }
        } else if *topic == crate::bot_core::v4_swap_decoder::V4_SWAP_TOPIC {
            if let Some(event) = crate::bot_core::v4_swap_decoder::decode_v4_swap_log(log) {
                for key in self.v4_engine.apply_swap(
                    &V4SwapUpdate {
                        pool_manager: log.address(),
                        pool_id: event.pool_id,
                        sqrt_price_x96: event.sqrt_price_x96,
                        liquidity: event.liquidity.to::<u128>(),
                        tick: event.tick,
                        tick_priors: vec![],
                    },
                    block_number,
                ).iter() {
                    self.dirty_v4.insert(key);
                }
            }
        } else if *topic == crate::bot_core::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC {
            if let Some(event) = crate::bot_core::v4_modify_liquidity_decoder::decode_v4_modify_liquidity_log(log) {
                for key in self.v4_engine.apply_liquidity_update(
                    log.address(),
                    event.pool_id,
                    event.tick_lower,
                    event.tick_upper,
                    event.liquidity_delta,
                    block_number,
                ).iter() {
                    self.dirty_v4.insert(key);
                }
            }
        }
    }

    /// Solve all paths affected by logs applied since the last `solve_dirty`
    /// call, but do NOT send a result batch to Python.
    ///
    /// The pump calls this eagerly after each WS log to keep engine state
    /// current. The actual batch send is triggered by the pump's debounce
    /// timer or block boundary logic.
    pub fn solve_dirty(&mut self, block_number: u64, metadata: &BlockMetadata) {
        // Expire stale buffered events in V3/V4 sub-engines
        self.v3_engine.expire_buffered_events(block_number);
        self.v4_engine.expire_buffered_events(block_number);

        // Take ownership of dirty sets to avoid borrow conflict
        let dirty_v2 = std::mem::take(&mut self.dirty_v2);
        let dirty_v3 = std::mem::take(&mut self.dirty_v3);
        let dirty_v4 = std::mem::take(&mut self.dirty_v4);

        // Re-solve only paths containing updated pools (no batch send)
        self.rebuild_and_solve_affected(
            &dirty_v2,
            &dirty_v3,
            &dirty_v4,
            block_number,
            metadata,
        );

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
    pub fn process_block_and_send(&mut self, logs: &[Log], block_number: u64, metadata: &BlockMetadata) {
        self.process_block(logs, block_number, metadata);
        self.compute_diff_and_send(metadata);
    }

    /// Process pre-decoded updates for testing.
    pub fn process_updates(
        &mut self,
        v2_updates: &[(Address, U256, U256)],
        v3_updates: &[V3SwapUpdate],
        block_number: u64,
        metadata: &BlockMetadata,
    ) {
        // Apply updates to sub-engines and collect affected pool keys
        let v2_affected = self.v2_engine.apply_sync_updates(v2_updates);
        let v3_affected = self.v3_engine.apply_swap_updates(v3_updates, block_number);

        // Re-solve only paths containing updated pools
        self.rebuild_and_solve_affected(&v2_affected, &v3_affected, &HashSet::new(), block_number, metadata);
        self.last_processed_block = Some(block_number);
    }

    /// Process pre-decoded V4 updates.
    pub fn process_v4_updates(
        &mut self,
        v4_updates: &[V4SwapUpdate],
        block_number: u64,
        metadata: &BlockMetadata,
    ) {
        let v4_affected = self.v4_engine.apply_swap_updates(v4_updates, block_number);
        self.rebuild_and_solve_affected(&HashSet::new(), &HashSet::new(), &v4_affected, block_number, metadata);
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
        let v2_affected = self.v2_engine.apply_sync_updates(v2_updates);
        let v3_affected = self.v3_engine.apply_swap_updates(v3_updates, block_number);
        let v4_affected = self.v4_engine.apply_swap_updates(v4_updates, block_number);
        self.rebuild_and_solve_affected(&v2_affected, &v3_affected, &v4_affected, block_number, metadata);
        self.last_processed_block = Some(block_number);
    }

    /// Apply backfill logs to the V3/V4 sub-engines.
    ///
    /// Unlike `apply_log`, liquidity updates are buffered rather than
    /// applied immediately. Swap updates are applied directly.
    /// After all logs are processed, expired buffers are purged and
    /// the sub-engines rebuild their solve states.
    pub fn process_backfill_logs(&mut self, logs: &[Log], block_number: u64) {
        use crate::bot_core::v3_swap_decoder::decode_v3_swap_log;
        use crate::bot_core::v3_mint_burn_decoder::{decode_v3_mint_log, decode_v3_burn_log};
        use crate::bot_core::v4_swap_decoder::decode_v4_swap_log;
        use crate::bot_core::v4_modify_liquidity_decoder::decode_v4_modify_liquidity_log;

        let mut v3_touched = false;
        let mut v4_touched = false;

        for log in logs {
            let Some(topic0) = log.topic0() else {
                continue;
            };

            if *topic0 == crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC {
                if let Some(event) = decode_v3_swap_log(log) {
                    self.v3_engine.apply_swap(
                        event.pool_address,
                        event.sqrt_price_x96,
                        event.liquidity.to::<u128>(),
                        event.tick,
                        block_number,
                        &[],
                    );
                    v3_touched = true;
                }
            } else if *topic0 == crate::bot_core::v3_mint_burn_decoder::V3_MINT_TOPIC {
                if let Some(event) = decode_v3_mint_log(log) {
                    self.v3_engine.buffer_backfill_liquidity_update(
                        event.pool_address,
                        event.tick_lower,
                        event.tick_upper,
                        event.amount.cast_signed(),
                        block_number,
                    );
                    v3_touched = true;
                }
            } else if *topic0 == crate::bot_core::v3_mint_burn_decoder::V3_BURN_TOPIC {
                if let Some(event) = decode_v3_burn_log(log) {
                    self.v3_engine.buffer_backfill_liquidity_update(
                        event.pool_address,
                        event.tick_lower,
                        event.tick_upper,
                        -(event.amount.cast_signed()),
                        block_number,
                    );
                    v3_touched = true;
                }
            } else if *topic0 == crate::bot_core::v4_swap_decoder::V4_SWAP_TOPIC {
                if let Some(event) = decode_v4_swap_log(log) {
                    self.v4_engine.apply_swap(
                        &V4SwapUpdate {
                            pool_manager: log.address(),
                            pool_id: event.pool_id,
                            sqrt_price_x96: event.sqrt_price_x96,
                            liquidity: event.liquidity.to::<u128>(),
                            tick: event.tick,
                            tick_priors: vec![],
                        },
                        block_number,
                    );
                    v4_touched = true;
                }
            } else if *topic0 == crate::bot_core::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC {
                if let Some(event) = decode_v4_modify_liquidity_log(log) {
                    self.v4_engine.buffer_backfill_liquidity_update(
                        log.address(),
                        event.pool_id,
                        event.tick_lower,
                        event.tick_upper,
                        event.liquidity_delta,
                        block_number,
                    );
                    v4_touched = true;
                }
            }
        }

        if v3_touched {
            self.v3_engine.expire_buffered_events(block_number);
            self.v3_engine.rebuild_and_solve(block_number);
        }
        if v4_touched {
            self.v4_engine.expire_buffered_events(block_number);
            self.v4_engine.rebuild_and_solve(block_number);
        }

        self.last_processed_block = Some(block_number);
    }
}
