//! Result batch channel management and incremental diff computation.

use alloy::primitives::U256;
use tokio::sync::mpsc;

use super::{BlockMetadata, ResultBatch, SolvePathResult, UniswapEngine};

impl UniswapEngine {
    /// Set the sender for the result batch channel.
    pub fn set_result_channel(&mut self, tx: mpsc::UnboundedSender<ResultBatch>) {
        self.result_tx = Some(tx);
    }

    /// Set the profit thresholds for the result batch channel.
    ///
    /// Only paths with `profit > min_profit` and `profit < max_profit`
    /// appear in batch `fresh` / `updated` entries. Paths outside
    /// this range are excluded from `delivered` and batches.
    pub const fn set_profit_thresholds(&mut self, min_profit: U256, max_profit: U256) {
        self.min_profit = min_profit;
        self.max_profit = max_profit;
    }

    /// De-register a path from the engine.
    ///
    /// Removes the path from `paths`, `pool_to_paths` reverse index,
    /// `results`, `delivered`, and `pending_new_paths`. The path's
    /// pools are **not** removed from the sub-engines — other paths
    /// may still reference them.
    ///
    /// The de-registered path ID is recorded and included in the next
    /// batch's `removed` field.
    ///
    /// Returns `true` if the path existed and was removed.
    pub fn deregister_path(&mut self, path_id: u64) -> bool {
        // Remove from path_pools and get the pool refs to clean up reverse index
        let removed = self.path_pools.remove(&path_id);
        let existed = removed.is_some();
        if let Some(path) = removed {
            // Remove from pool_to_paths reverse index
            for pool_ref in &path.pools {
                if let Some(path_ids) = self
                    .pool_to_paths
                    .get_mut(&(pool_ref.hop_type, pool_ref.pool_key))
                {
                    path_ids.retain(|id| *id != path_id);
                }
            }
        }

        // Remove from path_resolved
        self.path_resolved.remove(&path_id);

        // Remove from results
        self.results.remove(&path_id);

        // Remove from delivered
        self.delivered.remove(&path_id);

        // Remove from pending_new_paths
        self.pending_new_paths.remove(&path_id);

        // Record for the next batch
        if existed {
            self.deregistered.push(path_id);
        }

        existed
    }

    /// Compute the incremental diff between `delivered` and `new_results`,
    /// then advance `delivered` to the above-threshold subset.
    ///
    /// If `result_tx` is set, sends the batch.
    /// If the channel is full, the batch is dropped — the next one
    /// will carry a correct cumulative diff.
    pub(super) fn compute_diff_and_send(&mut self, metadata: &BlockMetadata) {
        // Compute incremental diff directly from results and delivered HashMaps.
        // No intermediate collections needed — iterate both once.

        // Fresh: above-threshold in results, not in delivered
        let fresh: Vec<(u64, SolvePathResult)> = self
            .results
            .iter()
            .filter(|(_, r)| r.profit > self.min_profit && r.profit < self.max_profit)
            .filter(|(id, _)| !self.delivered.contains_key(id))
            .map(|(&id, r)| (id, r.clone()))
            .collect();

        // Updated: above-threshold in both, values differ
        let updated: Vec<(u64, SolvePathResult)> = self
            .results
            .iter()
            .filter(|(_, r)| r.profit > self.min_profit && r.profit < self.max_profit)
            .filter(|(id, new)| matches!(self.delivered.get(id), Some(old) if old != *new))
            .map(|(&id, r)| (id, r.clone()))
            .collect();

        // Expired: in delivered but not above-threshold in results
        let expired: Vec<u64> = self
            .delivered
            .keys()
            .filter(|id| {
                !self
                    .results
                    .get(id)
                    .is_some_and(|r| r.profit > self.min_profit && r.profit < self.max_profit)
            })
            .copied()
            .collect();

        // Removed: de-registered since last batch
        let removed: Vec<u64> = self.deregistered.drain(..).collect();

        // Advance `delivered` to the above-threshold subset of current
        // `results` (ADR-003: this is what makes reorg `expired` diffs real —
        // a path that was profitable but rolled back must leave `delivered`).
        //   1. retain only paths still above-threshold in current results
        //      (drops expired entries — `removed` is handled separately via
        //      `deregistered`);
        //   2. insert/overwrite current values so `updated` paths stop
        //      re-firing every batch once their new value is delivered.
        self.delivered.retain(|id, _| {
            self.results
                .get(id)
                .is_some_and(|r| r.profit > self.min_profit && r.profit < self.max_profit)
        });
        for (&id, r) in &self.results {
            if r.profit > self.min_profit && r.profit < self.max_profit {
                self.delivered.insert(id, r.clone());
            }
        }

        // Send if channel is available and there's anything to report
        if let Some(ref tx) = self.result_tx {
            // Always send a batch even if empty — Python needs the block
            // metadata and solve_block to drive its main loop.
            let batch = ResultBatch {
                solve_block: self.results_block,
                timestamp: metadata.timestamp,
                base_fee_per_gas: metadata.base_fee_per_gas,
                gas_used: metadata.gas_used,
                gas_limit: metadata.gas_limit,
                fresh,
                updated,
                expired,
                removed,
            };
            let _ = tx.send(batch);
        }
    }
}
