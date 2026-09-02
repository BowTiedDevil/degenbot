//! Delivery policy — deciding what is worth publishing and pushing it over
//! an async channel, decoupled from the engine's solve output (ergo BI7UZV).
//!
//! The engine's solve produces a canonical, channel-independent result map
//! ([`ArbitrageEngine::latest_results`]). This module is the **optional
//! delivery sink** that consumes that solve output: it filters by the profit
//! window, tracks what Python has already seen (`delivered`), and batches the
//! incremental diff over a `tokio` channel. The pub-sub transport, profit
//! thresholds, and diff bookkeeping live here — NOT fused into the solver.
//!
//! # tokio stays a hard dependency of `degenbot-bot`
//!
//! The task asked whether `tokio` could become optional so a pure-solver build
//! avoids it. It cannot: `degenbot-bot`'s pump (`block_pump.rs`, 60+ uses),
//! block clock, liquidity verifier, and registration lifecycle are all
//! `tokio`-based production functionality. Making `tokio` optional here would
//! be crate-wide surgery, not a delivery-channel isolation. This struct keeps
//! every `tokio` use local to the delivery surface, so when the solver is
//! eventually pulled into its own crate (ADR-018 trigger: a second engine
//! family), `DeliveryPolicy` and its channels can travel intact.

use hashbrown::HashMap;

use alloy::primitives::U256;
use tokio::sync::mpsc;

use super::delivery_lifecycle::DeliveryLifecycle;
use super::{ArbitrageEngine, BlockMetadata, ResultBatch};
use ::degenbot_solvers::mixed::SolvePathResult;

/// The delivery policy: filters the engine's solve output by the profit
/// window, tracks what Python has already received, and pushes an incremental
/// diff over the result channel.
///
/// It is the **only** owner of the diff bookkeeping (`delivered`/`deregistered`)
/// and the profit thresholds. It does not solve anything — it consumes the
/// engine's [`ArbitrageEngine::latest_results`] output via
/// [`DeliveryPolicy::diff_and_send`].
///
/// The *transport* half — channel open/send/close and the end-of-stream
/// contract — lives on the embedded [`DeliveryLifecycle`]; this struct owns
/// the *policy* half (per-engine: thresholds + diff bookkeeping).
///
/// The fields are `pub(crate)` so the in-crate unit tests (`arb_engine/tests.rs`)
/// can assert on `delivered` directly; no external crate can reach them, and the
/// engine exposes only the delegating methods below.
pub(crate) struct DeliveryPolicy {
    /// The above-threshold results that have been **actually delivered to
    /// Python** via the result channel. Used to compute incremental diffs.
    ///
    /// # Invariant
    ///
    /// `delivered` is advanced **only** by [`DeliveryPolicy::diff_and_send`],
    /// and only after building a `ResultBatch` for the current above-threshold
    /// subset of the engine's `results`. It must stay **empty before the first
    /// pump-driven send** (e.g. during cold-start / `solve_all_paths`), since
    /// Python has not yet received anything. Advancing it without a live
    /// channel would poison `fresh`/`expired` computation for the next real
    /// send.
    pub(crate) delivered: HashMap<u64, SolvePathResult>,
    /// Path IDs that have been de-registered since the last batch.
    /// Drained into the next batch's `removed` field.
    pub(crate) deregistered: Vec<u64>,
    /// Minimum profit (in wei) for a result to appear in the batch channel.
    /// Paths below this threshold are excluded from `delivered` and batches.
    pub(crate) min_profit: U256,
    /// Maximum profit (in wei) for a result to appear in the batch channel.
    /// Paths above this are likely solver defects or scam tokens.
    pub(crate) max_profit: U256,
    /// The transport half: channel open/send/close + the end-of-stream
    /// contract (see the type's docs).
    pub(crate) lifecycle: DeliveryLifecycle,
}

impl Default for DeliveryPolicy {
    fn default() -> Self {
        Self {
            delivered: HashMap::new(),
            deregistered: Vec::new(),
            min_profit: U256::ZERO,
            max_profit: U256::MAX,
            lifecycle: DeliveryLifecycle::default(),
        }
    }
}

impl DeliveryPolicy {
    /// Set the sender for the result batch channel. Delegates to the
    /// embedded [`DeliveryLifecycle`].
    pub fn set_result_channel(&mut self, tx: mpsc::UnboundedSender<ResultBatch>) {
        self.lifecycle.set_result_channel(tx);
    }

    /// Forward a `newHeads` block tick onto the block-notification channel
    /// (epic 6W35AI). A no-op when no block channel is attached (no-pyo3
    /// tests / standalone).
    /// Set the profit thresholds for the result batch channel.
    ///
    /// Only paths with `profit > min_profit` and `profit <= max_profit`
    /// appear in batch `fresh` / `updated` entries. Paths outside
    /// this range are excluded from `delivered` and batches.
    ///
    /// The max bound is inclusive so that `max_profit = U256::MAX` (the
    /// default) opens the cap fully — profits can exceed `u64::MAX` for
    /// 18-decimal tokens with large reserves, and the V4 `int128` overflow
    /// guard allows up to `2^127-1`. The min bound remains strict (`>`).
    pub const fn set_profit_thresholds(&mut self, min_profit: U256, max_profit: U256) {
        self.min_profit = min_profit;
        self.max_profit = max_profit;
    }

    /// Record a path de-registration in the delivery bookkeeping: drop it from
    /// `delivered` and (only when it actually existed) queue it for the next
    /// batch's `removed` field.
    ///
    /// This is the delivery-policy half of `ArbitrageEngine::deregister_path`.
    pub fn on_path_deregistered(&mut self, path_id: u64, existed: bool) {
        self.delivered.remove(&path_id);
        if existed {
            self.deregistered.push(path_id);
        }
    }

    /// Compute the incremental diff between the engine's solve output
    /// (`results` / `results_block`) and what Python has already received
    /// (`delivered`), advance `delivered` to the above-threshold subset, and
    /// — if a result channel is attached — send the batch.
    ///
    /// The solve output is passed in, not read from the policy; the policy is
    /// a pure consumer of the solve (`ArbitrageEngine::latest_results` is the
    /// canonical, channel-independent surface).
    ///
    /// If the channel is full, the batch is dropped — the next one will carry
    /// a correct cumulative diff.
    pub fn diff_and_send(
        &mut self,
        results: &HashMap<u64, SolvePathResult>,
        results_block: u64,
        metadata: &BlockMetadata,
    ) {
        // A non-zero `results_block` is the invariant that a batch's candidates
        // are dispatchable: the strategy sims each candidate at its
        // `solve_block` (= `results_block`), so a 0 anchor would execute every
        // tracked pool as an EOA at block 0 (`eth_getCode(0)` → `KECCAK_EMPTY`
        // → the Sim DB invariant panic). `results_block` is only advanced by a
        // real solve (`rebuild_and_solve_affected` / `solve_all_paths`);
        // registration-time eager solves (`register_and_solve_path`) and
        // backfill (Backfilled-phase invariant: no solve) leave it 0 until the
        // first live `on_drain` solve. Until anchored, publish an EMPTY batch
        // (metadata only) and do NOT commit these candidates to `delivered` —
        // they are re-delivered once the first real solve anchors them at a
        // valid block.
        let anchored = results_block != 0;
        if !anchored && !results.is_empty() {
            tracing::warn!(
                results = results.len(),
                "delivery: deferring {} candidate(s) — no solve anchor yet (results_block=0)",
                results.len(),
            );
        }

        // Fresh: above-threshold in results, not in delivered
        let fresh: Vec<(u64, SolvePathResult)> = if anchored {
            results
                .iter()
                .filter(|(_, r)| r.profit > self.min_profit && r.profit <= self.max_profit)
                .filter(|(&id, _)| !self.delivered.contains_key(&id))
                .map(|(&id, r)| (id, r.clone()))
                .collect()
        } else {
            Vec::new()
        };

        // Updated: above-threshold in both, values differ
        let updated: Vec<(u64, SolvePathResult)> = if anchored {
            results
                .iter()
                .filter(|(_, r)| r.profit > self.min_profit && r.profit <= self.max_profit)
                .filter(|(&id, new)| matches!(self.delivered.get(&id), Some(old) if old != *new))
                .map(|(&id, r)| (id, r.clone()))
                .collect()
        } else {
            Vec::new()
        };

        // Expired: in delivered but not above-threshold in results
        let expired: Vec<u64> = self
            .delivered
            .keys()
            .filter(|&&id| {
                !results
                    .get(&id)
                    .is_some_and(|r| r.profit > self.min_profit && r.profit <= self.max_profit)
            })
            .copied()
            .collect();

        // Removed: de-registered since last batch
        let removed: Vec<u64> = std::mem::take(&mut self.deregistered);

        // Advance `delivered` to the above-threshold subset of current
        // `results` (ADR-003: this is what makes reorg `expired` diffs real —
        // a path that was profitable but rolled back must leave `delivered`).
        //   1. retain only paths still above-threshold in current results
        //      (drops expired entries — `removed` is handled separately via
        //      `deregistered`);
        //   2. insert/overwrite current values so `updated` paths stop
        //      re-firing every batch once their new value is delivered.
        if anchored {
            self.delivered.retain(|&id, _| {
                results
                    .get(&id)
                    .is_some_and(|r| r.profit > self.min_profit && r.profit <= self.max_profit)
            });
            for (&id, r) in results {
                if r.profit > self.min_profit && r.profit <= self.max_profit {
                    self.delivered.insert(id, r.clone());
                }
            }
        }

        // Always send a batch even if empty — Python needs the block
        // metadata and solve_block to drive its main loop. Quiet no-op when
        // no channel is open (standalone consumer) or after close.
        let batch = ResultBatch {
            solve_block: results_block,
            timestamp: metadata.timestamp,
            base_fee_per_gas: metadata.base_fee_per_gas,
            gas_used: metadata.gas_used,
            gas_limit: metadata.gas_limit,
            fresh,
            updated,
            expired,
            removed,
        };
        self.lifecycle.send_batch(batch);
    }
}

impl DeliveryPolicy {
    /// T3 (epic BXUSGL): DEGENBOT_STREAMING_DELIVERY — emit ONE above
    /// -threshold result as an immediate single-entry batch, advancing the
    /// per-entry diff bookkeeping. Per-entry deltas compose with the regular
    /// debounce sweep (which still owns `expired`/`removed` and the metadata
    /// -only end-of-cycle batch): a streaming-delivered entry is already in
    /// `delivered`, so the sweep's fresh/updated filters skip it.
    /// Semantics mirror [`Self::diff_and_send`]: anchor gate + profit window
    /// and up-to-date discrimination. No-op without a live channel and
    /// after engine close (the lifecycle batch contract).
    pub fn emit_single_result_batch(
        &mut self,
        results_block: u64,
        metadata: &BlockMetadata,
        path_id: u64,
        result: &SolvePathResult,
    ) {
        let anchored = results_block != 0;
        let above_threshold = result.profit > self.min_profit && result.profit <= self.max_profit;
        let mut fresh: Vec<(u64, SolvePathResult)> = Vec::new();
        let mut updated: Vec<(u64, SolvePathResult)> = Vec::new();
        if anchored && above_threshold {
            if !self.delivered.contains_key(&path_id) {
                fresh.push((path_id, result.clone()));
                self.delivered.insert(path_id, result.clone());
            } else if self.delivered.get(&path_id) != Some(result) {
                updated.push((path_id, result.clone()));
                self.delivered.insert(path_id, result.clone());
            }
        }

        let batch = ResultBatch {
            solve_block: results_block,
            timestamp: metadata.timestamp,
            base_fee_per_gas: metadata.base_fee_per_gas,
            gas_used: metadata.gas_used,
            gas_limit: metadata.gas_limit,
            fresh,
            updated,
            expired: Vec::new(),
            removed: Vec::new(),
        };
        self.lifecycle.send_batch(batch);
    }
}

impl ArbitrageEngine {
    /// Set the sender for the result batch channel. Delegates to the
    /// [`DeliveryPolicy`]. The solve itself is channel-free — this only
    /// attaches the optional delivery sink.
    pub fn set_result_channel(&mut self, tx: mpsc::UnboundedSender<ResultBatch>) {
        self.delivery.set_result_channel(tx);
    }

    /// Set the profit thresholds for the result batch channel. Delegates to
    /// the [`DeliveryPolicy`].
    pub const fn set_profit_thresholds(&mut self, min_profit: U256, max_profit: U256) {
        self.delivery.set_profit_thresholds(min_profit, max_profit);
    }

    /// Compute the incremental diff and send a batch to Python. Delegates to
    /// the [`DeliveryPolicy`], which consumes the solve output
    /// (`latest_results`) as its input.
    ///
    /// The cold-start `results_block` anchor is established by the pump's
    /// `set_solve_anchor(current_block)` seed at resume (the settled resume
    /// boundary — a completed, fully-applied block within the backfill
    /// window). The [`DeliveryPolicy`] refuses to publish at block 0 (safety
    /// net) if no anchor has been seeded yet.
    pub fn compute_diff_and_send(&mut self, metadata: &BlockMetadata) {
        let results_block = self.results_block;
        let results_snapshot: HashMap<u64, SolvePathResult> = self
            .results
            .iter()
            .map(|r| (*r.key(), r.value().clone()))
            .collect();
        self.delivery
            .diff_and_send(&results_snapshot, results_block, metadata);
    }

    /// De-register a path from the engine.
    ///
    /// Removes the path from the solve state (`path_pools`, `pool_to_paths`
    /// reverse index, `path_resolved`, `results`, `pending_new_paths`) and
    /// hands the delivery bookkeeping (`delivered` / `deregistered`) to the
    /// [`DeliveryPolicy`]. The path's pools are **not** removed from the
    /// sub-engines — other paths may still reference them.
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

        // Remove from pending_new_paths
        self.pending_new_paths.remove(&path_id);

        // Record for the next batch (delivery-policy half)
        self.delivery.on_path_deregistered(path_id, existed);

        existed
    }
}

#[expect(clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    /// Build a minimal `SolvePathResult` with the given profit (other fields
    /// are irrelevant to the delivery-policy diff logic).
    fn solve_result(profit: u64) -> SolvePathResult {
        SolvePathResult {
            optimal_input: U256::from(1000),
            profit: U256::from(profit),
            hop_outputs: Vec::new(),
            consumed_inputs: Vec::new(),
            state_nonces: Vec::new(),
            solver_pool_states: Vec::new(),
        }
    }

    /// BI7UZV core claim: the delivery policy is a **pure consumer** of the
    /// solve output — feed it a hand-built results map (no `ArbitrageEngine`
    /// involved) and it computes the true incremental diff against what Python
    /// has already seen, then advances `delivered`.
    #[test]
    fn diff_and_send_consumes_solve_output_independent_of_engine() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut policy = DeliveryPolicy::default();
        policy.set_result_channel(tx);

        let mut results: HashMap<u64, SolvePathResult> = HashMap::new();
        results.insert(1, solve_result(500));
        results.insert(2, solve_result(0)); // below threshold (min=0 is strict `>`)
        results.insert(3, solve_result(900));

        // Pretend Python already saw path 3 at an older value.
        policy.delivered.insert(3, solve_result(700));

        policy.diff_and_send(&results, 42, &BlockMetadata::default());

        let batch = rx.try_recv().expect("diff_and_send with a channel sends");
        assert_eq!(batch.solve_block, 42);
        // 1 is fresh (never delivered); 3 changed → updated (not fresh).
        assert!(batch.fresh.iter().any(|(id, _)| *id == 1));
        assert!(batch.updated.iter().any(|(id, _)| *id == 3));
        assert!(
            !batch.fresh.iter().any(|(id, _)| *id == 2),
            "below-threshold profit must be excluded from fresh"
        );
        // Delivered advances to the above-threshold subset only.
        assert!(policy.delivered.contains_key(&1));
        assert!(policy.delivered.contains_key(&3));
        assert!(
            !policy.delivered.contains_key(&2),
            "below-threshold path must not be delivered"
        );
    }

    /// Standalone contract: with NO `result_tx` attached, `diff_and_send`
    /// must not panic and must still advance `delivered` — exactly the
    /// engine-without-channel path a standalone Rust consumer exercises.
    #[test]
    fn diff_and_send_without_channel_advances_delivered_without_sending() {
        let mut policy = DeliveryPolicy::default();
        let mut results: HashMap<u64, SolvePathResult> = HashMap::new();
        results.insert(1, solve_result(500));
        policy.diff_and_send(&results, 7, &BlockMetadata::default());
        assert!(policy.delivered.contains_key(&1));
    }

    /// Solve-anchor guard (`bot_run.log` 0x841820 code-less panic): a batch
    /// whose candidates are published with `solve_block = results_block = 0`
    /// makes the strategy sim every tracked pool at block 0 (EOA → `KECCAK_EMPTY`
    /// → Sim DB invariant panic). Registration-time eager solves populate
    /// `results` before any real solve has advanced `results_block`. So a 0
    /// anchor must (a) publish an EMPTY batch — no fresh/updated candidates —
    /// and (b) NOT commit those candidates to `delivered`, so the first real
    /// solve re-delivers them at a valid block.
    #[test]
    fn diff_and_send_with_zero_anchor_defers_candidates_and_does_not_commit() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut policy = DeliveryPolicy::default();
        policy.set_result_channel(tx);

        let mut results: HashMap<u64, SolvePathResult> = HashMap::new();
        results.insert(1, solve_result(500)); // above threshold, would be fresh if anchored

        // results_block == 0 (cold start, no solve yet): batch is EMPTY.
        policy.diff_and_send(&results, 0, &BlockMetadata::default());
        let batch = rx
            .try_recv()
            .expect("zero-anchor still sends metadata batch");
        assert!(
            batch.fresh.is_empty(),
            "no candidates at an un-anchored solve"
        );
        assert!(
            batch.updated.is_empty(),
            "no candidates at an un-anchored solve"
        );
        assert_eq!(batch.solve_block, 0);
        assert!(
            !policy.delivered.contains_key(&1),
            "un-anchored candidates must stay pending, not be marked delivered"
        );

        // First real solve advances results_block to 42: the deferred candidate
        // is now delivered as fresh at a valid anchor.
        policy.diff_and_send(&results, 42, &BlockMetadata::default());
        let batch = rx.try_recv().expect("anchored solve delivers");
        assert!(
            batch.fresh.iter().any(|(id, _)| *id == 1),
            "candidate re-delivered once anchored"
        );
        assert_eq!(batch.solve_block, 42);
        assert!(policy.delivered.contains_key(&1));
    }

    /// De-registration bookkeeping: drop from `delivered` and queue `removed`
    /// only for paths that actually existed.
    #[test]
    fn on_path_deregistered_removes_from_delivered_and_queues_removed() {
        let mut policy = DeliveryPolicy::default();
        policy.delivered.insert(1, solve_result(500));
        policy.on_path_deregistered(1, true);
        assert!(!policy.delivered.contains_key(&1));
        assert_eq!(policy.deregistered, vec![1]);
        // A path that never existed must NOT be queued for `removed`.
        policy.on_path_deregistered(99, false);
        assert_eq!(policy.deregistered, vec![1]);
    }
}
