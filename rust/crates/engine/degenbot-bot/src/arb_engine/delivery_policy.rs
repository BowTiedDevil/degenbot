//! Delivery policy — deciding what is worth publishing and pushing it over
//! an async channel, decoupled from the engine's solve output .
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
use super::admission_floor::AdmissionSeam;
use super::delivery_lifecycle::DeliveryLifecycle;
use super::{ArbitrageEngine, BlockMetadata, ResultBatch};
use crate::arb_engine::inline_sim::SimulatedPathResult;
use ::degenbot_pools::solve::SolvePathResult;
use alloy::primitives::U256;
use degenbot_core::channel_lifecycle::ChannelDecline;
use degenbot_core::op_warn;
use hashbrown::{HashMap, HashSet};
use tokio::sync::mpsc;
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
    ///
    /// This is the STATIC half of the window: the effective floor at a block is
    /// [`super::admission_floor::admission_floor`], the static minimum raised by
    /// the path's own pre-sim gas floor.
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
    /// Attach the result-batch channel sender.
    ///
    /// # Errors
    ///
    /// [`ChannelDecline::AlreadyAttached`] when one is already attached, and
    /// [`ChannelDecline::AttachedAfterClose`] after [`DeliveryLifecycle::close`].
    pub fn set_result_channel(
        &mut self,
        tx: mpsc::UnboundedSender<ResultBatch>,
    ) -> Result<(), ChannelDecline> {
        self.lifecycle.attach(tx)
    }
    /// Forward a `newHeads` block tick onto the block-notification channel
    /// . A no-op when no block channel is attached (no-pyo3
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

    /// Admit-or-floor one solved result at this block: the max cap, then the
    /// shared gas floor ([`super::admission_floor::admit`]). A candidate outside
    /// the max cap is excluded as it always was.
    ///
    /// `owner` is the cycle's owning seam ([`AdmissionSeam::of_cycle`]). The
    /// window EXCLUDES every candidate under the floor either way — it is the
    /// second gate — but when the walk's inline gate owns the count this floor
    /// is that same decision re-derived at publish, so the window's echo is not
    /// counted again.
    fn admit(
        &self,
        owner: AdmissionSeam,
        path_id: u64,
        result: &SolvePathResult,
        metadata: &BlockMetadata,
    ) -> bool {
        if result.profit > self.max_profit {
            return false;
        }
        super::admission_floor::admit(
            owner.window_count(),
            path_id,
            result,
            self.min_profit,
            metadata.base_fee_per_gas,
        )
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
    ///
    /// `anchored` is handed in by the engine from the block cursor
    /// (`BlockCursor::is_anchored`) — the policy consumes the cursor's
    /// predicate instead of re-deriving it from the raw `results_block`.
    ///
    /// `owner` is the seam that OWNS the gas floor's count for these candidates
    /// ([`AdmissionSeam::of_cycle`] over the engine's inline-gate state): the
    /// floor still EXCLUDES every candidate under it — the window is the second
    /// gate — but a candidate the walk's inline admission already floored is one
    /// decision, counted at that seam, so the window's echo is not counted
    /// again.
    pub fn diff_and_send(
        &mut self,
        results: &HashMap<u64, SolvePathResult>,
        results_block: u64,
        anchored: bool,
        metadata: &BlockMetadata,
        inline_payloads: &HashMap<u64, SimulatedPathResult>,
        owner: AdmissionSeam,
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
        if !anchored && !results.is_empty() {
            op_warn!(
                domain = solver,
                results = results.len(),
                "delivery: deferring {} candidate(s) — no solve anchor yet (results_block=0)",
                results.len(),
            );
        }
        // The admission decision, taken ONCE per result at this block: the
        // static window raised by the path's own gas floor. Every filter below
        // (fresh / updated / expired / the `delivered` advance) reads this one
        // set, so a floored candidate is counted and named once per batch and
        // `delivered` advances on exactly the set the batch publishes.
        let admitted: HashSet<u64> = if anchored {
            results
                .iter()
                .filter(|(&id, r)| self.admit(owner, id, r, metadata))
                .map(|(&id, _)| id)
                .collect()
        } else {
            HashSet::new()
        };

        // Fresh: admitted in results, not in delivered
        let fresh: Vec<(u64, SolvePathResult)> = results
            .iter()
            .filter(|(&id, _)| admitted.contains(&id))
            .filter(|(&id, _)| !self.delivered.contains_key(&id))
            .map(|(&id, r)| (id, r.clone()))
            .collect();
        // Updated: admitted in both, values differ
        let updated: Vec<(u64, SolvePathResult)> = results
            .iter()
            .filter(|(&id, _)| admitted.contains(&id))
            .filter(|(&id, new)| matches!(self.delivered.get(&id), Some(old) if old != *new))
            .map(|(&id, r)| (id, r.clone()))
            .collect();
        // Expired: in delivered but not admitted in results
        let expired: Vec<u64> = self
            .delivered
            .keys()
            .filter(|&&id| !admitted.contains(&id))
            .copied()
            .collect();
        // Removed: de-registered since last batch
        let removed: Vec<u64> = std::mem::take(&mut self.deregistered);
        // Advance `delivered` to the admitted subset of current `results`
        // (ADR-003: this is what makes reorg `expired` diffs real — a path that
        // was profitable but rolled back must leave `delivered`).
        //   1. retain only paths still admitted in current results (drops
        //      expired entries — `removed` is handled separately via
        //      `deregistered`);
        //   2. insert/overwrite current values so `updated` paths stop
        //      re-firing every batch once their new value is delivered.
        if anchored {
            self.delivered.retain(|&id, _| admitted.contains(&id));
            for (&id, r) in results {
                if admitted.contains(&id) {
                    self.delivered.insert(id, r.clone());
                }
            }
        }
        // Always send a batch even if empty — Python needs the block
        // metadata and solve_block to drive its main loop. Quiet no-op when
        // no channel is open (standalone consumer) or after close.
        // SIMPIPE2 T3: ship the payload for every delivered entry; stale
        // payload ids (expired/removed, or a path that re-solved without a
        // payload) drop here — presence decides per entry Python-side.
        let payloads: HashMap<u64, SimulatedPathResult> = fresh
            .iter()
            .chain(updated.iter())
            .filter_map(|(id, _)| inline_payloads.get(id).map(|p| (*id, p.clone())))
            .collect();
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
            payloads,
        };
        self.lifecycle.send_batch(batch);
    }
}
impl DeliveryPolicy {
    /// T3: `DEGENBOT_STREAMING_DELIVERY` — emit ONE above
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
        payload: Option<&SimulatedPathResult>,
        owner: AdmissionSeam,
    ) {
        let anchored = results_block != 0;
        let admitted = self.admit(owner, path_id, result, metadata);
        let mut fresh: Vec<(u64, SolvePathResult)> = Vec::new();
        let mut updated: Vec<(u64, SolvePathResult)> = Vec::new();
        if anchored && admitted {
            if !self.delivered.contains_key(&path_id) {
                fresh.push((path_id, result.clone()));
                self.delivered.insert(path_id, result.clone());
            } else if self.delivered.get(&path_id) != Some(result) {
                updated.push((path_id, result.clone()));
                self.delivered.insert(path_id, result.clone());
            }
        }
        // SIMPIPE2 T3: a streamed entry carries its own payload.
        let payloads: HashMap<u64, SimulatedPathResult> = payload
            .map(|p| HashMap::from([(path_id, p.clone())]))
            .unwrap_or_default();
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
            payloads,
        };
        self.lifecycle.send_batch(batch);
    }
}
/// Set the sender for the result batch channel (machine-direct free-function
/// route). The solve itself is channel-free — this only attaches the optional
/// delivery sink.
pub(crate) fn set_result_channel(
    engine: &mut ArbitrageEngine,
    tx: mpsc::UnboundedSender<ResultBatch>,
) {
    // Test seam: the wiring is attach-once, so a decline here is a caller bug
    // the test that made it will surface on its own assertions.
    let _ = engine.delivery.set_result_channel(tx);
}
/// Set the profit thresholds for the result batch channel (machine-direct
/// free-function route).
pub(crate) fn set_profit_thresholds(
    engine: &mut ArbitrageEngine,
    min_profit: U256,
    max_profit: U256,
) {
    engine
        .delivery
        .set_profit_thresholds(min_profit, max_profit);
}
/// Compute the incremental diff and send a batch to Python (machine-direct
/// free-function route). The [`DeliveryPolicy`] consumes the solve output
/// (`latest_results`) as its input.
///
/// The cold-start `results_block` anchor is established by the pump's
/// `set_solve_anchor(current_block)` seed at resume (the settled resume
/// boundary — a completed, fully-applied block within the backfill window).
/// The [`DeliveryPolicy`] refuses to publish at block 0 (safety net) if no
/// anchor has been seeded yet.
pub(crate) fn compute_diff_and_send(engine: &mut ArbitrageEngine, metadata: &BlockMetadata) {
    let results_block = engine.cycle.cursor.coordinates().results_block;
    // f701ccd3 bridge: capture the settle-entered block span as the
    // propagation parent for `results_block` — the Python simulate seam
    // re-attaches it (telemetry::simulate_dispatch_span) so the whole block
    // chain renders as one Jaeger trace.
    crate::telemetry::publish_block_context(results_block);
    let results_snapshot: HashMap<u64, SolvePathResult> = engine
        .cycle
        .results
        .iter()
        .map(|r| (*r.key(), r.value().clone()))
        .collect();
    // SIMPIPE2 T3: drain the worker-resolved inline payloads for this publish;
    // the delivery ships the entries for the delivered paths and drops the
    // rest.
    let inline_payloads: HashMap<u64, crate::arb_engine::inline_sim::SimulatedPathResult> = engine
        .cycle
        .inline_payloads
        .iter()
        .map(|e| (*e.key(), e.value().clone()))
        .collect();
    engine.cycle.inline_payloads.clear();
    // the anchored gate comes from the block cursor.
    let anchored = engine.cycle.cursor.coordinates().is_anchored();
    // The floor's counting seam for this batch: the walk's inline gate owns the
    // count when it is armed — its suppressions are the ones that prevented the
    // sims, and the window's re-floor of those candidates is the same decision.
    let owner = AdmissionSeam::of_cycle(
        engine.cycle.inline_sim_enabled,
        engine.cycle.inline_sim.is_some(),
    );
    engine.delivery.diff_and_send(
        &results_snapshot,
        results_block,
        anchored,
        metadata,
        &inline_payloads,
        owner,
    );
}
/// De-register a path from the engine (machine-direct free-function route).
///
/// Removes the path from the solve state (`path_pools`, `pool_to_paths`
/// reverse index, `path_resolved`, `results`, `pending_new_paths`) and hands
/// the delivery bookkeeping (`delivered` / `deregistered`) to the
/// [`DeliveryPolicy`]. The path's pools are **not** removed from the
/// sub-engines — other paths may still reference them.
///
/// The de-registered path ID is recorded and included in the next batch's
/// `removed` field.
///
/// Returns `true` if the path existed and was removed.
pub(crate) fn deregister_path(engine: &mut ArbitrageEngine, path_id: u64) -> bool {
    // Remove from the registry: drops the path, prunes the reverse index,
    // and clears the dedup signature.
    let removed = engine.registry.remove(path_id);
    let existed = removed.is_some();
    // ADR-045 T4: the path-deregistration coupling (resolve companions,
    // results, pending-new carry) is the cycle's `forget`.
    engine.cycle.forget(path_id);
    // Record for the next batch (delivery-policy half)
    engine.delivery.on_path_deregistered(path_id, existed);
    existed
}
#[expect(clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::arb_engine::admission_floor;
    use ::degenbot_pools::solve::{HopGasProfile, MEASURED_GAS_FLOOR};
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
            ..Default::default()
        }
    }
    /// Core claim: the delivery policy is a **pure consumer** of the
    /// solve output — feed it a hand-built results map (no `ArbitrageEngine`
    /// involved) and it computes the true incremental diff against what Python
    /// has already seen, then advances `delivered`.
    #[test]
    fn diff_and_send_consumes_solve_output_independent_of_engine() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut policy = DeliveryPolicy::default();
        let _ = policy.set_result_channel(tx);
        let mut results: HashMap<u64, SolvePathResult> = HashMap::new();
        results.insert(1, solve_result(500));
        results.insert(2, solve_result(0)); // below threshold (min=0 is strict `>`)
        results.insert(3, solve_result(900));
        // Pretend Python already saw path 3 at an older value.
        policy.delivered.insert(3, solve_result(700));
        policy.diff_and_send(
            &results,
            42,
            true,
            &BlockMetadata::default(),
            &HashMap::default(),
            AdmissionSeam::DeliveryWindow,
        );
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
        policy.diff_and_send(
            &results,
            7,
            true,
            &BlockMetadata::default(),
            &HashMap::default(),
            AdmissionSeam::DeliveryWindow,
        );
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
        let _ = policy.set_result_channel(tx);
        let mut results: HashMap<u64, SolvePathResult> = HashMap::new();
        results.insert(1, solve_result(500)); // above threshold, would be fresh if anchored
                                              // results_block == 0 (cold start, no solve yet): batch is EMPTY.
        policy.diff_and_send(
            &results,
            0,
            false,
            &BlockMetadata::default(),
            &HashMap::default(),
            AdmissionSeam::DeliveryWindow,
        );
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
        policy.diff_and_send(
            &results,
            42,
            true,
            &BlockMetadata::default(),
            &HashMap::default(),
            AdmissionSeam::DeliveryWindow,
        );
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

    /// A solved result that carries the census the floor prices: two V2 hops
    /// (a flat leg each) with the given margin.
    fn solve_result_with_gas(profit: u64) -> SolvePathResult {
        SolvePathResult {
            optimal_input: U256::from(1_000u64),
            profit: U256::from(profit),
            hop_outputs: Vec::new(),
            consumed_inputs: Vec::new(),
            state_nonces: Vec::new(),
            solver_pool_states: Vec::new(),
            gas_hops: vec![HopGasProfile::V2, HopGasProfile::V2],
            ..Default::default()
        }
    }

    fn metadata_at_base_fee(base_fee: u64) -> BlockMetadata {
        BlockMetadata {
            base_fee_per_gas: Some(base_fee),
            ..Default::default()
        }
    }

    /// The window's floor for that result at that block, from the same table
    /// the policy reads.
    fn floor_wei(profit: u64, base_fee: u64) -> U256 {
        let result = solve_result_with_gas(profit);
        MEASURED_GAS_FLOOR
            .floor_wei(&result.gas_hops, u128::from(base_fee))
            .expect("a V2 path is priced")
    }

    /// The delivery window with a batch of above/below-floor results ships
    /// exactly the above-floor set: the gas floor (not a static constant) is
    /// what separates them, a result sitting exactly ON the floor is excluded
    /// (the min bound stays strict), and `delivered` advances to that same
    /// set so the next batch does not re-publish the floored candidate.
    #[test]
    fn the_window_ships_exactly_the_above_floor_set() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut policy = DeliveryPolicy::default();
        let _ = policy.set_result_channel(tx);

        let base_fee = 12_000_000_000u64;
        let metadata = metadata_at_base_fee(base_fee);

        // The floor is DATA: the path's own shape priced at this block's base
        // fee (two V2 legs + per-hop transfers + the settlement frame).
        let expected_floor = U256::from(176_364u64) * U256::from(base_fee);
        assert_eq!(floor_wei(0, base_fee), expected_floor);

        let mut results: HashMap<u64, SolvePathResult> = HashMap::new();
        // Below the floor (the phantom class: a real margin that cannot pay
        // the path's gas).
        results.insert(
            1,
            solve_result_with_gas(
                u64::try_from(expected_floor / U256::from(2u64)).expect("fixture fits u64"),
            ),
        );
        // Exactly ON the floor — excluded, the min bound is strict.
        results.insert(
            2,
            solve_result_with_gas(u64::try_from(expected_floor).expect("fixture fits u64")),
        );
        // One wei over the floor — the cheapest admitted candidate.
        results.insert(
            3,
            solve_result_with_gas(
                u64::try_from(expected_floor + U256::ONE).expect("fixture fits u64"),
            ),
        );
        // Comfortably above the floor.
        results.insert(
            4,
            solve_result_with_gas(
                u64::try_from(expected_floor * U256::from(4u64)).expect("fixture fits u64"),
            ),
        );

        policy.diff_and_send(
            &results,
            42,
            true,
            &metadata,
            &HashMap::default(),
            AdmissionSeam::DeliveryWindow,
        );

        let batch = rx.try_recv().expect("an anchored batch sends");
        let mut shipped: Vec<u64> = batch.fresh.iter().map(|(id, _)| *id).collect();
        shipped.sort_unstable();
        assert_eq!(
            shipped,
            vec![3, 4],
            "only the candidates strictly above the gas floor ship"
        );
        assert!(!batch.expired.iter().any(|id| [1, 2].contains(id)));

        // The floored candidates never entered `delivered`; the admitted ones
        // did, so a second identical batch is a quiet update, not a re-ship.
        assert!(policy.delivered.contains_key(&3));
        assert!(policy.delivered.contains_key(&4));
        assert!(!policy.delivered.contains_key(&1));
        assert!(!policy.delivered.contains_key(&2));

        policy.diff_and_send(
            &results,
            43,
            true,
            &metadata,
            &HashMap::default(),
            AdmissionSeam::DeliveryWindow,
        );
        let second = rx.try_recv().expect("the metadata batch still sends");
        assert!(
            second.fresh.is_empty(),
            "admitted candidates do not re-ship"
        );
        assert!(second.updated.is_empty(), "unchanged values do not re-ship");
    }

    /// The window stays the SECOND GATE when the walk's inline gate owns the
    /// count: a candidate the inline seam already floored is still excluded from
    /// the batch (the base fee can move between the solve and the publish, and
    /// `delivered` must not advance on a candidate the batch will not carry) —
    /// only the COUNT moves upstream, so the window's echo is not counted twice.
    #[test]
    fn an_armed_inline_gate_keeps_the_windows_gate() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut policy = DeliveryPolicy::default();
        let _ = policy.set_result_channel(tx);
        let base_fee = 12_000_000_000u64;
        let metadata = metadata_at_base_fee(base_fee);
        let floor = floor_wei(0, base_fee);
        let mut results: HashMap<u64, SolvePathResult> = HashMap::new();
        results.insert(
            1,
            solve_result_with_gas(u64::try_from(floor / U256::from(2u64)).expect("fixture fits")),
        );
        results.insert(
            2,
            solve_result_with_gas(u64::try_from(floor + U256::ONE).expect("fixture fits")),
        );
        let owner = AdmissionSeam::of_cycle(true, true);
        assert_eq!(owner, AdmissionSeam::InlineSim);
        policy.diff_and_send(&results, 42, true, &metadata, &HashMap::default(), owner);
        let batch = rx.try_recv().expect("an anchored batch sends");
        assert_eq!(
            batch.fresh.iter().map(|(id, _)| *id).collect::<Vec<u64>>(),
            vec![2],
            "the window still excludes the floored candidate"
        );
        assert!(
            !policy.delivered.contains_key(&1),
            "a floored candidate never enters the delivered set"
        );
        assert!(policy.delivered.contains_key(&2));
    }

    /// The floor is PER BLOCK: the same result that ships at a low base fee is
    /// floored (and expires) at a high one — the window is data, so a gas-price
    /// move alone changes admission.
    #[test]
    fn a_higher_base_fee_floors_a_previously_admitted_candidate() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut policy = DeliveryPolicy::default();
        let _ = policy.set_result_channel(tx);

        let profit =
            u64::try_from(floor_wei(0, 5_000_000_000u64) + U256::ONE).expect("fixture fits u64");
        let mut results: HashMap<u64, SolvePathResult> = HashMap::new();
        results.insert(7, solve_result_with_gas(profit));

        policy.diff_and_send(
            &results,
            10,
            true,
            &metadata_at_base_fee(5_000_000_000),
            &HashMap::default(),
            AdmissionSeam::DeliveryWindow,
        );
        let batch = rx.try_recv().expect("batch at the low base fee");
        assert_eq!(batch.fresh.len(), 1, "cheap-gas block admits the candidate");

        // The same candidate at a base fee whose floor exceeds its margin.
        policy.diff_and_send(
            &results,
            11,
            true,
            &metadata_at_base_fee(20_000_000_000),
            &HashMap::default(),
            AdmissionSeam::DeliveryWindow,
        );
        let batch = rx.try_recv().expect("batch at the high base fee");
        assert!(batch.fresh.is_empty(), "nothing fresh at the higher fee");
        assert_eq!(
            batch.expired,
            vec![7],
            "the now-floored candidate expires out of the delivered set"
        );
        assert!(!policy.delivered.contains_key(&7));
    }

    /// A result whose solve reported no hop projection (a family with no
    /// measured row, or a hand-built result) has no gas floor: the window
    /// falls back to the static minimum rather than guessing a cost.
    #[test]
    fn a_result_without_a_gas_projection_uses_the_static_minimum() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut policy = DeliveryPolicy::default();
        policy.set_profit_thresholds(U256::from(100u64), U256::MAX);
        let _ = policy.set_result_channel(tx);

        let metadata = metadata_at_base_fee(30_000_000_000u64);
        let mut results: HashMap<u64, SolvePathResult> = HashMap::new();
        results.insert(1, solve_result(50)); // below min_profit
        results.insert(2, solve_result(101)); // above min_profit
        assert!(results.get(&2).is_some_and(|r| r.gas_hops.is_empty()));

        policy.diff_and_send(
            &results,
            42,
            true,
            &metadata,
            &HashMap::default(),
            AdmissionSeam::DeliveryWindow,
        );
        let batch = rx.try_recv().expect("batch sends");
        assert_eq!(
            batch.fresh.iter().map(|(id, _)| *id).collect::<Vec<u64>>(),
            vec![2],
            "an unpriced path keeps the pre-existing static window"
        );
    }

    /// The gas term only ever RAISES the floor: a static minimum above the gas
    /// floor stays the effective floor (the operator's knob is a floor, not a
    /// replacement).
    #[test]
    fn the_static_minimum_and_the_gas_floor_compose_by_max() {
        let mut policy = DeliveryPolicy::default();
        let metadata = metadata_at_base_fee(1u64);
        let gas_floor = floor_wei(0, 1);
        let result =
            solve_result_with_gas(u64::try_from(gas_floor + U256::ONE).expect("fixture fits u64"));

        assert_eq!(
            admission_floor::admission_floor(&result, policy.min_profit, metadata.base_fee_per_gas),
            gas_floor,
            "with min_profit at zero the gas floor is the floor"
        );

        policy.set_profit_thresholds(gas_floor * U256::from(2u64), U256::MAX);
        assert_eq!(
            admission_floor::admission_floor(&result, policy.min_profit, metadata.base_fee_per_gas),
            gas_floor * U256::from(2u64),
            "a static minimum above the gas floor wins"
        );
        assert!(
            !policy.admit(AdmissionSeam::DeliveryWindow, 1, &result, &metadata),
            "a candidate between the gas floor and the raised static minimum is excluded"
        );
    }
}
