//! The batch executor: the value-configured module wrapping the ordered
//! [`SimSubmitPipeline`].
//!
//! A driver constructs [`BatchExecutor`] with an [`ExecutorConfig`] (the cap
//! as a plain count, the policy values, the relay posture — never
//! choreography code), calls [`BatchExecutor::enqueue`] with result rows, and
//! drains [`BatchOutcome`] records via [`BatchExecutor::next_outcome`]. The
//! ordered lane runs internally: bounded sim fan-out, FIFO submit order
//! (= nonce order), loud-abort re-raise in the driver's frame
//! ([`BatchExecutor::shutdown`] / [`BatchExecutor::raise_if_failed`]).
//!
//! No Python closures on the hot path: the `PyO3` seam is construction +
//! `next_outcome` + the typed record enums (ADR-032 naming).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use degenbot_arbitrage::{
    candidate_is_stale, dispatch_profitable_results, DispatchOutcome, FeeOnTransferRegistry,
    PoolDivergence, SimulateContext,
};
use degenbot_executor::composers::{EncodeOptions, PathInfo};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_submission::{
    dispatch_and_submit, Dispatcher, NonceLane, PathSuppression, PipelineFailure, ReceiptProbe,
    SimFuture, SimLeaf, SimSubmitPipeline, SubmissionTarget, SubmitLeaf, SubmitRecord, TxSigner,
};
use degenbot_substrate::state_lock::{LockSite, StateLock};
use degenbot_substrate::BotState;
use parking_lot::RwLock;

use crate::assembly::{
    assemble_batch, join_sim_result, AssemblyError, AssemblyInputs, PathResolver,
};
use crate::record::{fold_counters, AssemblyVerdict, BatchOutcome, SimulateVerdict, SubmitVerdict};
use crate::row::{PayloadRow, RawResult};

/// The session-static executor configuration — every value a driver injects.
///
/// Per-block facts (`base_fee_next`, `block_timestamp`, the priority-fee
/// percentiles) ride [`BatchWork`]; everything session-scoped lives here.
pub struct ExecutorConfig {
    /// The in-flight sim bound (a plain count, floor 1).
    pub sim_concurrency: usize,
    /// The per-batch sim cap (a plain count; clamped to the fan-out's
    /// `MAX_SIMULATE_CONCURRENT` so the drop happens once, where the dropped
    /// rows are known).
    pub max_candidates: usize,
    /// The thin-margin floor in bps (0 disables).
    pub min_profit_margin_bps: u64,
    /// The encode options stamped onto every assembled candidate.
    pub opts: EncodeOptions,
    /// The path resolver (the engine registry projection).
    pub resolver: Arc<dyn PathResolver>,
    /// The cross-block suppression registry (decision b — its own policy
    /// value, never merged with the payload-served set).
    pub suppression: Arc<Mutex<PathSuppression>>,
    /// The per-pool solver-divergence memo.
    pub divergence: Arc<Mutex<PoolDivergence>>,
    /// The per-token fee-on-transfer registry.
    pub fot: Arc<Mutex<FeeOnTransferRegistry>>,
    // ── Simulate-stage session values (the `SimulateContext` projection) ──
    /// The typed RPC provider (cold-miss fallback DB).
    pub provider: Arc<AlloyProvider>,
    /// The operator key's address (the `execute()` `from`).
    pub executor_owner: alloy::primitives::Address,
    /// The `cmd_executor` contract address (the `execute()` target + the join
    /// stamp).
    pub executor_address: alloy::primitives::Address,
    /// WETH9 contract address.
    pub weth_address: alloy::primitives::Address,
    /// The `Uniswap V4 PoolManager` contract address.
    pub pool_manager_address: alloy::primitives::Address,
    /// Multicall3 contract address.
    pub multicall3_address: alloy::primitives::Address,
    /// Whether to inject the executor runtime bytecode in-sim.
    pub inject_code: bool,
    /// The injected executor address (used when `inject_code`).
    pub injected_address: Option<alloy::primitives::Address>,
    /// The executor runtime bytecode.
    pub runtime_bytecode: alloy::primitives::Bytes,
    /// The simulation warmup slots.
    pub warmup: degenbot_executor::WarmupSlots,
    /// The engine's shared state owner (the staleness gate + the per-block
    /// EVM anchor). `None` only for empty-input callers.
    pub bot_state: Option<Arc<StateLock<BotState>>>,
    /// The cross-block warm-code cache.
    pub warm_cache: Option<Arc<RwLock<degenbot_simulation::WarmCodeCacheInner>>>,
    // ── Submit-stage session values (the `dispatch_and_submit` projection) ──
    /// The coordination state (pool mutual exclusion, monitors, block clock).
    pub dispatcher: Arc<Mutex<Dispatcher>>,
    /// The operator key holder (constructed ONCE; the key never leaves Rust).
    pub signer: Arc<TxSigner>,
    /// The receipt probe the spawned monitors poll.
    pub probe: Arc<dyn ReceiptProbe + Send + Sync>,
    /// The host-minted sign-time nonce lane.
    pub nonce_lane: Arc<NonceLane>,
    /// The live-submission skip (a safety policy value, S3).
    pub dry_run: bool,
    /// The submit-side `inject_code` guard (the injected contract doesn't
    /// exist on-chain — live submission is unsafe).
    pub inject_code_guard: bool,
    /// Additional broadcast providers fanned out alongside the read provider.
    pub extra_broadcast: Vec<Arc<AlloyProvider>>,
    /// Where the signed transaction is sent (the relay posture value).
    pub target: SubmissionTarget,
}

/// One batch's per-block facts + the rows to run through the pipeline.
#[derive(Debug, Clone)]
pub struct BatchWork {
    /// The raw solver rows.
    pub rows: Vec<RawResult>,
    /// The inline-sim payload rows (their path ids ARE the batch-local
    /// payload-served set — per-entry presence decides, so a mixed batch only
    /// degrades the payload-less entries).
    pub payloads: Vec<PayloadRow>,
    /// The block the batch is consumed at.
    pub current_block: u64,
    /// The base fee of the next block.
    pub base_fee_next: u128,
    /// The timestamp of `current_block` (the EVM `block.timestamp`).
    pub block_timestamp: u64,
    /// The latest priority-fee percentiles (p10/p50).
    pub block_priority_fees: Option<degenbot_rpc::BlockPriorityFees>,
}

/// The sim stage's product: the batch's records (stages 1-2 filled) + the
/// joined submit rows for stage 3.
struct SimStageOutput {
    outcomes: Vec<BatchOutcome>,
    submit_candidates: Vec<degenbot_submission::SubmitCandidate>,
}

/// One drained batch: the candidate outcome records + the RAW submit-lane
/// records (`Submitted`/`Skipped` with reason + detail) the Python drain
/// renders (the `[dispatch]` per-record lines + the silent-veto smoke FSM —
/// reads that live on the submit records, not inside the typed
/// `SubmitVerdict` slot).
#[derive(Debug, Clone)]
pub struct BatchOutcomeSet {
    /// The per-candidate outcome records, in submit order.
    pub records: Vec<BatchOutcome>,
    /// The submit lane's raw per-candidate records, in submit order.
    pub submit_records: Vec<SubmitRecord>,
}

/// The two production leaves over one shared config. The record sender is
/// held WEAK: the strong half lives on [`BatchExecutor`], which drops it at
/// `shutdown` so a drain-to-completion `next_outcome` loop terminates — a
/// strong copy pinned inside this Arc (which the pipeline holds for its
/// lifetime) would keep the channel open forever and hang the drain.
struct EngineLeaves {
    config: Arc<ExecutorConfig>,
    outcome_tx: mpsc::WeakUnboundedSender<BatchOutcomeSet>,
}

impl EngineLeaves {
    /// Stages 1 + 2: assemble, gate staleness, fan out, attribute, join.
    #[expect(
        clippy::too_many_lines,
        reason = "the leaf reads best as one ordered stage pass"
    )]
    fn run_stages_1_2(&self, work: &BatchWork) -> Result<SimStageOutput, String> {
        let cfg = &self.config;
        let served: HashSet<u64> = work.payloads.iter().map(|p| p.path_id).collect();

        // Stage 1 — assembly. The policy locks are held ONLY across this
        // synchronous span (the fan-out re-locks the same arcs at its own
        // bookends); guards drop before any later stage.
        let mut batch = {
            let mut suppression = cfg
                .suppression
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assemble_batch(&AssemblyInputs {
                rows: &work.rows,
                payloads: &work.payloads,
                resolver: cfg.resolver.as_ref(),
                opts: cfg.opts,
                payload_served: &served,
                suppression: &mut suppression,
                divergence: &cfg.divergence,
                fot: &cfg.fot,
                current_block: work.current_block,
                min_profit_margin_bps: cfg.min_profit_margin_bps,
                max_candidates: cfg.max_candidates,
                executor_address: cfg.executor_address,
            })
            .map_err(|e: AssemblyError| e.to_string())?
        };

        // Staleness gate at SIM time (as close to the fan-out as the core's
        // own step 3.5 sits): drop candidates whose any hop's state nonce
        // advanced past the solve snapshot. Their records are dropped too —
        // the same drain-invisible drop today's outcome applies (the Python
        // `MergedOutcome` carries no stale count; `candidate_count` excludes
        // them). The fan-out's re-check is a no-op: this span holds no
        // `.await` between the check and the call, so no state can advance.
        if let Some(arc) = &cfg.bot_state {
            let stale_ids: Vec<u64> = {
                let guard = arc.read_at(LockSite::Sim);
                batch
                    .candidates
                    .iter()
                    .filter(|c| candidate_is_stale(&guard, c))
                    .map(|c| c.path_id)
                    .collect()
            };
            if !stale_ids.is_empty() {
                batch.candidates.retain(|c| !stale_ids.contains(&c.path_id));
                batch
                    .outcomes
                    .retain(|o| !(o.simulate.is_none() && stale_ids.contains(&o.path_id)));
            }
        }

        // Stage 2 — the core fan-out. Its own pre-filters (suppression,
        // divergence, FoT, thin margin) are no-ops on the survivors: the
        // assembly applied each exactly once over the same registries within
        // this same no-await span, and the cap is already applied.
        let provider = Arc::clone(&cfg.provider);
        let ctx = SimulateContext {
            provider: &provider,
            executor_owner: cfg.executor_owner,
            executor_address: cfg.executor_address,
            weth_address: cfg.weth_address,
            pool_manager_address: cfg.pool_manager_address,
            multicall3_address: cfg.multicall3_address,
            inject_code: cfg.inject_code,
            injected_address: cfg.injected_address,
            runtime_bytecode: cfg.runtime_bytecode.clone(),
            warmup: cfg.warmup,
            base_fee_next: work.base_fee_next,
            current_block: work.current_block,
            block_timestamp: work.block_timestamp,
            block_priority_fees: work.block_priority_fees.clone(),
        };
        let outcome: DispatchOutcome = {
            // ONE Jaeger span per simulate fan-out (ADR-043), entered only
            // across the synchronous fan-out call.
            let span = degenbot_bot::telemetry::simulate_dispatch_span(
                work.current_block,
                batch.candidates.len(),
            );
            let _guard = span.enter();
            dispatch_profitable_results(
                std::mem::take(&mut batch.candidates),
                &ctx,
                &cfg.suppression,
                work.current_block,
                cfg.min_profit_margin_bps,
                &cfg.divergence,
                &cfg.fot,
                cfg.bot_state.clone(),
                cfg.warm_cache.clone(),
            )
        };

        // Attribution — per-candidate simulate verdicts in candidate order
        // (the records for skipped rows were already emitted by stage 1).
        let profitable: HashMap<u64, degenbot_arbitrage::SimResult> = outcome
            .gas_profitable
            .iter()
            .map(|r| (r.path_id, r.clone()))
            .collect();
        let unprofitable: HashMap<u64, degenbot_arbitrage::SimResult> = outcome
            .gas_unprofitable
            .iter()
            .map(|r| (r.path_id, r.clone()))
            .collect();
        let failures: HashMap<u64, degenbot_arbitrage::SimFailure> = outcome
            .failures
            .iter()
            .map(|f| (f.path_id, f.clone()))
            .collect();

        let mut submit_candidates: Vec<degenbot_submission::SubmitCandidate> = Vec::new();
        let empty_path = PathInfo::new(Vec::new());
        for c in std::mem::take(&mut batch.candidates) {
            // The lookup always hits: the core only re-emits path ids it was
            // handed (the join's `None`/empty fall-through keeps isolation —
            // an empty mutual-exclusion set — without fabricating hops).
            let path_info = batch.path_info_by_id.get(&c.path_id);
            let simulate = if let Some(r) = profitable.get(&c.path_id) {
                submit_candidates.push(join_sim_result(r, path_info, cfg.executor_address));
                Some(SimulateVerdict::Profitable(crate::record::SimReceipt {
                    gross_profit: r.gross_profit,
                    net_profit: r.net_profit,
                    gas_used: r.gas_used,
                    priority_fee: r.priority_fee,
                }))
            } else if let Some(r) = unprofitable.get(&c.path_id) {
                Some(SimulateVerdict::GasUnprofitable(
                    crate::record::SimReceipt {
                        gross_profit: r.gross_profit,
                        net_profit: r.net_profit,
                        gas_used: r.gas_used,
                        priority_fee: r.priority_fee,
                    },
                ))
            } else if let Some(f) = failures.get(&c.path_id) {
                Some(SimulateVerdict::Failed(Box::new(f.clone())))
            } else {
                // Accounted by neither bucket nor a failure record: the
                // fan-out counted a per-path exception (its
                // `return_exceptions=True` tolerance).
                Some(SimulateVerdict::Exception)
            };
            batch.outcomes.push(BatchOutcome {
                path_id: c.path_id,
                block: work.current_block,
                assembly: AssemblyVerdict::Assembled,
                simulate,
                submit: None,
                path_info: crate::record::PathInfoView::from(path_info.unwrap_or(&empty_path)),
            });
        }

        // The payload arm's profitable joins join the FFI survivors for the
        // single ordered submit (one lane, one nonce fetch per submit).
        submit_candidates.append(&mut batch.payload_submits);

        Ok(SimStageOutput {
            outcomes: batch.outcomes,
            submit_candidates,
        })
    }
}

impl SimLeaf<BatchWork, SimStageOutput> for EngineLeaves {
    fn simulate<'a>(&'a self, work: &'a BatchWork) -> SimFuture<'a, SimStageOutput> {
        Box::pin(async move { self.run_stages_1_2(work).map(Some) })
    }
}

/// Forensic capture (fork-replay): one INFO line per gate-clearing
/// candidate BEFORE broadcast — the exact calldata + candidate economics —
/// so any later tx can be replayed at its solve block. The `op_info!` field
/// expressions evaluate only when a sink enabled the `exec` target's INFO
/// level (hot-path cost flat: level check + no string building otherwise).
fn log_submit_arm(candidates: &[degenbot_submission::SubmitCandidate], solve_block: u64) {
    for c in candidates {
        degenbot_core::op_info!(
            domain = exec,
            "path={} solve_block={} net_wei={} gas={} calldata={}",
            c.path_id,
            solve_block,
            c.net_profit,
            c.gas_used,
            alloy::hex::encode(&c.execute_calldata),
        );
    }
}

impl SubmitLeaf<BatchWork, SimStageOutput> for EngineLeaves {
    fn submit<'a>(
        &'a self,
        work: &'a BatchWork,
        outcome: &'a SimStageOutput,
    ) -> degenbot_submission::SubmitFuture<'a> {
        let cfg = Arc::clone(&self.config);
        let tx = self.outcome_tx.clone();
        let outcomes = outcome.outcomes.clone();
        let candidates = outcome.submit_candidates.clone();
        let current_block = work.current_block;
        Box::pin(async move {
            // Forensic capture BEFORE broadcast: one line per gate-clearing
            // candidate, so any later tx can be replayed at its solve block.
            log_submit_arm(&candidates, current_block);

            // Stage 3 — the production submit orchestration (sorts net-desc,
            // mutual exclusion, nonce lease, fee finalize, access list,
            // sign, broadcast, monitor). Its per-candidate RPC failures are
            // tolerated as typed records; an Err is an unrecoverable signer
            // failure — stored by the lane and re-raised in the driver's
            // frame (the loud-abort rule).
            let submit_outcome = dispatch_and_submit(
                candidates,
                &cfg.dispatcher,
                &cfg.provider,
                &cfg.signer,
                Arc::clone(&cfg.probe),
                &cfg.nonce_lane,
                current_block,
                cfg.dry_run,
                cfg.inject_code_guard,
                &cfg.extra_broadcast,
                cfg.target.clone(),
            )
            .await
            .map_err(|e| e.to_string())?;

            let mut stamped = outcomes;
            for record in &submit_outcome.records {
                let (path_id, verdict) = match record {
                    SubmitRecord::Submitted { path_id, .. } => (*path_id, SubmitVerdict::Submitted),
                    SubmitRecord::Skipped { path_id, reason } => (
                        *path_id,
                        SubmitVerdict::Failed(crate::record::FailureKind::from_skip_reason(reason)),
                    ),
                };
                if let Some(slot) = stamped
                    .iter_mut()
                    .find(|o| o.path_id == path_id && o.submit.is_none())
                {
                    slot.submit = Some(verdict);
                }
            }
            // The record stream is the module's product: one Vec per batch,
            // published in the ordered lane's arrival order. After `shutdown`
            // dropped the executor's strong half the upgrade fails — the
            // records are dropped because the drain is gone too (the lane is
            // already torn down; no live reader exists).
            if let Some(tx) = tx.upgrade() {
                let _ = tx.send(BatchOutcomeSet {
                    records: stamped,
                    submit_records: submit_outcome.records.clone(),
                });
            }
            Ok(())
        })
    }
}

/// A drain handle over the record stream, shared with the executor's own
/// `next_outcome` (the unbounded receiver is not `Clone`, so both lock the
/// same mutex-guarded receiver — the `PyO3` seam's drain task owns one handle
/// and never touches the executor itself).
pub struct BatchDrain {
    rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<BatchOutcomeSet>>>,
}

impl BatchDrain {
    /// Await the next batch's outcome set (`None` when the lane is closed
    /// and drained).
    pub async fn next(&mut self) -> Option<BatchOutcomeSet> {
        self.rx.lock().await.recv().await
    }
}

/// The core batch executor.
pub struct BatchExecutor {
    pipeline: SimSubmitPipeline<BatchWork, SimStageOutput>,
    /// The record channel's strong half, behind interior mutability so
    /// `shutdown` takes `&self` (the shared-consumer seam drives the
    /// executor through an `Arc`). Dropped by `shutdown` after the lane
    /// drains so `next_outcome` returns `None` (the leaves hold only a weak
    /// sender — a strong copy living in the leaves would pin the channel
    /// open and hang any drain-to-completion loop).
    outcome_tx: std::sync::Mutex<Option<mpsc::UnboundedSender<BatchOutcomeSet>>>,
    outcome_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<BatchOutcomeSet>>>,
}

impl BatchExecutor {
    /// Build the executor: the ordered lane + the two production leaves.
    #[must_use]
    pub fn new(config: ExecutorConfig) -> Self {
        let (outcome_tx, outcome_rx) = mpsc::unbounded_channel();
        let config = Arc::new(config);
        let leaves = Arc::new(EngineLeaves {
            config: Arc::clone(&config),
            outcome_tx: outcome_tx.downgrade(),
        });
        Self::from_leaves(
            config.sim_concurrency,
            Arc::clone(&leaves) as Arc<dyn SimLeaf<BatchWork, SimStageOutput>>,
            Arc::clone(&leaves) as Arc<dyn SubmitLeaf<BatchWork, SimStageOutput>>,
            outcome_tx,
            outcome_rx,
        )
    }

    /// A drain handle over the record stream (see [`BatchDrain`]).
    #[must_use]
    pub fn drain_handle(&self) -> BatchDrain {
        BatchDrain {
            rx: Arc::clone(&self.outcome_rx),
        }
    }

    /// Wire one leaf pair + the record channel into the ordered lane. The
    /// production constructor passes [`EngineLeaves`]; the tests pass fakes
    /// over the SAME `BatchWork` / `SimStageOutput` types so the lane's
    /// ordering + loud-abort contract is exercised at the executor boundary.
    /// The strong record-sender half moves here (dropped at `shutdown`); the
    /// leaves receive a weak downgrade of it.
    fn from_leaves(
        concurrency: usize,
        sim: Arc<dyn SimLeaf<BatchWork, SimStageOutput>>,
        submit: Arc<dyn SubmitLeaf<BatchWork, SimStageOutput>>,
        outcome_tx: mpsc::UnboundedSender<BatchOutcomeSet>,
        outcome_rx: mpsc::UnboundedReceiver<BatchOutcomeSet>,
    ) -> Self {
        let pipeline = SimSubmitPipeline::new(concurrency, sim, submit);
        Self {
            pipeline,
            outcome_tx: std::sync::Mutex::new(Some(outcome_tx)),
            outcome_rx: Arc::new(tokio::sync::Mutex::new(outcome_rx)),
        }
    }

    /// Enqueue one batch: its rows enter the ordered lane (assembly, sim,
    /// and submit run internally). Returns immediately — the caller keeps
    /// advancing.
    pub fn enqueue(&self, work: BatchWork) {
        self.pipeline.enqueue(work);
    }

    /// Await the next batch's outcome set (`None` when the lane is closed
    /// and drained).
    pub async fn next_outcome(&self) -> Option<BatchOutcomeSet> {
        self.outcome_rx.lock().await.recv().await
    }

    /// Poll for the next batch's outcome set without awaiting.
    #[must_use]
    pub async fn try_next_outcome(&self) -> Option<BatchOutcomeSet> {
        self.outcome_rx.lock().await.try_recv().ok()
    }

    /// How many batches have been enqueued.
    #[must_use]
    pub fn enqueued(&self) -> u64 {
        self.pipeline.enqueued()
    }

    /// How many batches the ordered submitter has processed.
    #[must_use]
    pub fn submitted(&self) -> u64 {
        self.pipeline.submitted()
    }

    /// Re-raise the first stored leaf failure in the CALLER's frame (the
    /// loud-abort rule).
    ///
    /// # Errors
    ///
    /// [`PipelineFailure`] when a leaf (assembly corruption, sim, or submit)
    /// failed.
    pub fn raise_if_failed(&self) -> Result<(), PipelineFailure> {
        self.pipeline.raise_if_failed()
    }

    /// Graceful teardown: close the queue, drain in-flight work, await the
    /// submitter, then re-raise any stored failure.
    ///
    /// # Errors
    ///
    /// [`PipelineFailure`] when a leaf failed.
    pub async fn shutdown(&self) -> Result<(), PipelineFailure> {
        let result = self.pipeline.shutdown().await;
        // Release the record channel (see the field doc): the lane drained
        // every batch before this drop, so no send is lost, and a
        // drain-to-completion `next_outcome` loop now terminates.
        *self
            .outcome_tx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        result
    }
}

/// Fold a batch's records into the render counters (re-exported here for the
/// driver seams that count per drained batch).
#[must_use]
pub fn batch_counters(batch: &[BatchOutcome]) -> crate::record::BatchCounters {
    fold_counters(batch)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests drive known-valid fixtures and assert on the outcomes"
)]
mod tests {
    use super::*;
    use std::time::Duration;

    use tracing_subscriber::layer::SubscriberExt;

    use crate::record::{FailureDetail, PathInfoView, SimReceipt};

    /// The fake leaves tag batches by their block (one batch per block).
    const BASE_BLOCK: u64 = 100;

    fn work(block: u64) -> BatchWork {
        BatchWork {
            rows: Vec::new(),
            payloads: Vec::new(),
            current_block: block,
            base_fee_next: 0,
            block_timestamp: 0,
            block_priority_fees: None,
        }
    }

    fn receipt() -> SimReceipt {
        SimReceipt {
            gross_profit: alloy::primitives::U256::from(600_u64),
            net_profit: alloy::primitives::U256::from(500_u64),
            gas_used: 300_000,
            priority_fee: 2,
        }
    }

    /// A sim leaf that finishes LATER batches first (the sleep shrinks with
    /// the batch index), so sim-completion order disagrees with arrival
    /// order — the ordering the lane must NOT submit in.
    struct ReversedFinishSim {
        fail_block: Option<u64>,
    }

    impl SimLeaf<BatchWork, SimStageOutput> for ReversedFinishSim {
        fn simulate<'a>(&'a self, work: &'a BatchWork) -> SimFuture<'a, SimStageOutput> {
            let batch = work.current_block - BASE_BLOCK;
            let fail_block = self.fail_block;
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(40 - 10 * batch)).await;
                if fail_block == Some(work.current_block) {
                    return Err(format!("sim leaf failed at block {}", work.current_block));
                }
                let outcome = BatchOutcome {
                    path_id: batch,
                    block: work.current_block,
                    assembly: AssemblyVerdict::Assembled,
                    simulate: Some(SimulateVerdict::Profitable(receipt())),
                    submit: None,
                    path_info: PathInfoView::empty(),
                };
                Ok(Some(SimStageOutput {
                    outcomes: vec![outcome],
                    submit_candidates: Vec::new(),
                }))
            })
        }
    }

    /// A submit leaf that stamps every record `Submitted`, logs the batch
    /// (block) submit order, and publishes the stamped records.
    struct StampingSubmit {
        order: Arc<std::sync::Mutex<Vec<u64>>>,
        tx: mpsc::WeakUnboundedSender<BatchOutcomeSet>,
    }

    impl SubmitLeaf<BatchWork, SimStageOutput> for StampingSubmit {
        fn submit<'a>(
            &'a self,
            work: &'a BatchWork,
            outcome: &'a SimStageOutput,
        ) -> degenbot_submission::SubmitFuture<'a> {
            let order = Arc::clone(&self.order);
            let tx = self.tx.clone();
            let block = work.current_block;
            let mut records = outcome.outcomes.clone();
            Box::pin(async move {
                order.lock().unwrap().push(block);
                for record in &mut records {
                    record.submit = Some(SubmitVerdict::Submitted);
                }
                if let Some(tx) = tx.upgrade() {
                    let _ = tx.send(BatchOutcomeSet {
                        records,
                        submit_records: Vec::new(),
                    });
                }
                Ok(())
            })
        }
    }

    fn executor_with(
        sim: ReversedFinishSim,
        order: Arc<std::sync::Mutex<Vec<u64>>>,
    ) -> BatchExecutor {
        let (tx, rx) = mpsc::unbounded_channel();
        // The strong sender half moves into the executor (dropped at
        // `shutdown`); the fake holds the weak downgrade — the production
        // shape, so the drain-to-completion loop terminates.
        BatchExecutor::from_leaves(
            4,
            Arc::new(sim),
            Arc::new(StampingSubmit {
                order,
                tx: tx.downgrade(),
            }),
            tx,
            rx,
        )
    }

    /// Lane ordering: submission order is ARRIVAL (FIFO = nonce) order even
    /// when the sims complete in the reverse order — and the record stream
    /// the driver drains follows the same order.
    #[tokio::test]
    async fn submit_lane_runs_in_nonce_order_not_sim_completion_order() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let executor = executor_with(ReversedFinishSim { fail_block: None }, Arc::clone(&order));
        for batch in 0_u64..4 {
            executor.enqueue(work(BASE_BLOCK + batch));
        }
        let failure = executor.shutdown().await;
        assert!(failure.is_ok(), "no leaf failed: {failure:?}");
        assert_eq!(executor.submitted(), 4);

        // The submit leaf saw the batches in FIFO (nonce) order.
        assert_eq!(
            *order.lock().unwrap(),
            vec![BASE_BLOCK, BASE_BLOCK + 1, BASE_BLOCK + 2, BASE_BLOCK + 3]
        );

        // The record stream follows the same order: one Vec per batch, in
        // submit order.
        let mut drained = Vec::new();
        while let Some(records) = executor.next_outcome().await {
            drained.extend(records.records.iter().map(|r| r.block));
        }
        assert_eq!(
            drained,
            vec![BASE_BLOCK, BASE_BLOCK + 1, BASE_BLOCK + 2, BASE_BLOCK + 3]
        );
    }

    /// The loud-abort rule: the FIRST leaf failure is re-raised in the
    /// caller's frame (here via `shutdown`), and the lane stops — later
    /// batches never submit.
    #[tokio::test]
    async fn sim_leaf_failure_is_reraised_in_the_caller_frame() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let executor = executor_with(
            ReversedFinishSim {
                fail_block: Some(BASE_BLOCK),
            },
            Arc::clone(&order),
        );
        executor.enqueue(work(BASE_BLOCK));
        executor.enqueue(work(BASE_BLOCK + 1));

        let failure = executor.shutdown().await;
        let failure = failure.expect_err("the failing batch's detail is re-raised");
        assert_eq!(failure.detail, "sim leaf failed at block 100");
        // The submit leaf never saw a batch (batch 0 failed before submit).
        assert!(order.lock().unwrap().is_empty());
        assert_eq!(executor.submitted(), 0);
    }

    /// The same rule through `raise_if_failed` (the driver's between-batch
    /// poll): the stored failure surfaces exactly once.
    #[tokio::test]
    async fn raise_if_failed_surfaces_the_stored_failure_once() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let executor = executor_with(
            ReversedFinishSim {
                fail_block: Some(BASE_BLOCK + 1),
            },
            order,
        );
        executor.enqueue(work(BASE_BLOCK));
        executor.enqueue(work(BASE_BLOCK + 1));
        // Batch 0 submits; batch 1's sim failure stops the lane.
        let _ = executor.try_next_outcome().await;
        while executor.submitted() < 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let failure = executor.raise_if_failed().expect_err("stored failure");
        assert_eq!(failure.detail, "sim leaf failed at block 101");
        assert!(executor.raise_if_failed().is_ok());
        executor
            .shutdown()
            .await
            .expect("the failure was already re-raised");
    }

    /// The block-only drain (the Rust bot's `HeartbeatSink` shape): the
    /// drain reads ONLY `record.block` — the liveness heartbeat — and the
    /// record vocabulary compiles for a consumer that reads nothing else.
    #[tokio::test]
    async fn block_only_drain_reads_just_the_heartbeat() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let executor = executor_with(ReversedFinishSim { fail_block: None }, order);
        for batch in 0_u64..3 {
            executor.enqueue(work(BASE_BLOCK + batch));
        }
        executor.shutdown().await.expect("no failure");
        let mut heartbeats = Vec::new();
        while let Some(records) = executor.next_outcome().await {
            for record in records.records {
                heartbeats.push(record.block);
            }
        }
        assert_eq!(heartbeats, vec![BASE_BLOCK, BASE_BLOCK + 1, BASE_BLOCK + 2]);
    }

    /// The renderer-shaped drain (the Python renderers' shape): every stage
    /// and `path_info` are read, and the render counters fold EXACTLY over
    /// the drained records.
    #[expect(
        clippy::too_many_lines,
        reason = "the fixture spells every stage's record verbatim"
    )]
    #[tokio::test]
    async fn renderer_drain_reads_every_stage_and_folds_counters() {
        use degenbot_executor::composers::V2HopInfo;
        use std::collections::HashSet;

        struct MixedSim;
        impl SimLeaf<BatchWork, SimStageOutput> for MixedSim {
            fn simulate<'a>(&'a self, work: &'a BatchWork) -> SimFuture<'a, SimStageOutput> {
                Box::pin(async move {
                    let pool = degenbot_core::address_utils::parse_address(
                        "0x1111111111111111111111111111111111111111",
                    )
                    .unwrap();
                    let view =
                        PathInfoView::from(&degenbot_executor::composers::PathInfo::new(vec![
                            degenbot_executor::composers::HopInfo::V2(V2HopInfo {
                                pool_address: pool,
                                token0_address: pool,
                                token1_address: pool,
                                fee: 30,
                                zfo: true,
                            }),
                        ]));
                    let failure = FailureDetail {
                        path_id: 3,
                        bucket: "no-profit".to_string(),
                        fail_index: None,
                        revert_data: alloy::primitives::Bytes::new(),
                        reverting_frame: None,
                        captured_swaps: Vec::new(),
                        log_full_count: 0,
                        reverted_swaps: Vec::new(),
                        optimal_input: 0,
                        hop_outputs: Vec::new(),
                        call_trace: Vec::new(),
                        weth_before: 0,
                        weth_after: 0,
                        eth_before: 0,
                        eth_after: 0,
                        erc6909_before: 0,
                        erc6909_after: 0,
                    };
                    let outcomes = vec![
                        BatchOutcome {
                            path_id: 1,
                            block: work.current_block,
                            assembly: AssemblyVerdict::Assembled,
                            simulate: Some(SimulateVerdict::Profitable(receipt())),
                            submit: None,
                            path_info: view.clone(),
                        },
                        BatchOutcome {
                            path_id: 2,
                            block: work.current_block,
                            assembly: AssemblyVerdict::SkipSuppressed,
                            simulate: None,
                            submit: None,
                            path_info: PathInfoView::empty(),
                        },
                        BatchOutcome {
                            path_id: 3,
                            block: work.current_block,
                            assembly: AssemblyVerdict::Assembled,
                            simulate: Some(SimulateVerdict::Failed(Box::new(failure))),
                            submit: None,
                            path_info: view.clone(),
                        },
                        BatchOutcome {
                            path_id: 4,
                            block: work.current_block,
                            assembly: AssemblyVerdict::Assembled,
                            simulate: Some(SimulateVerdict::GasUnprofitable(receipt())),
                            submit: None,
                            path_info: view,
                        },
                    ];
                    Ok(Some(SimStageOutput {
                        outcomes,
                        submit_candidates: Vec::new(),
                    }))
                })
            }
        }

        /// Stamps path 1 `Submitted` and path 4 a typed RPC failure — the
        /// submit-lane surface the renderers read.
        struct PartialSubmit {
            tx: mpsc::WeakUnboundedSender<BatchOutcomeSet>,
        }
        impl SubmitLeaf<BatchWork, SimStageOutput> for PartialSubmit {
            fn submit<'a>(
                &'a self,
                _work: &'a BatchWork,
                outcome: &'a SimStageOutput,
            ) -> degenbot_submission::SubmitFuture<'a> {
                let tx = self.tx.clone();
                let mut records = outcome.outcomes.clone();
                Box::pin(async move {
                    for record in &mut records {
                        record.submit = match record.path_id {
                            1 => Some(SubmitVerdict::Submitted),
                            4 => Some(SubmitVerdict::Failed(crate::record::FailureKind::RpcFailed)),
                            _ => None,
                        };
                    }
                    if let Some(tx) = tx.upgrade() {
                        let _ = tx.send(BatchOutcomeSet {
                            records,
                            submit_records: Vec::new(),
                        });
                    }
                    Ok(())
                })
            }
        }

        let (tx, rx) = mpsc::unbounded_channel();
        let executor = BatchExecutor::from_leaves(
            1,
            Arc::new(MixedSim),
            Arc::new(PartialSubmit { tx: tx.downgrade() }),
            tx,
            rx,
        );
        executor.enqueue(work(BASE_BLOCK));
        executor.shutdown().await.expect("no failure");

        // ── The renderer-shaped drain: every stage + path_info ──
        let mut records = executor.next_outcome().await.expect("one batch").records;
        assert_eq!(records.len(), 4);
        let by_id: std::collections::HashMap<u64, BatchOutcome> =
            records.drain(..).map(|r| (r.path_id, r)).collect();

        let profitable = &by_id[&1];
        assert_eq!(profitable.assembly.label(), "assembled");
        let Some(SimulateVerdict::Profitable(sim)) = &profitable.simulate else {
            panic!("path 1 is profitable")
        };
        assert_eq!(sim.net_profit, alloy::primitives::U256::from(500_u64));
        assert_eq!(sim.gas_used, 300_000);
        assert_eq!(profitable.submit, Some(SubmitVerdict::Submitted));
        assert_eq!(profitable.path_info.path_type, "V2");
        assert_eq!(profitable.path_info.hops.len(), 1);

        assert_eq!(by_id[&2].assembly.label(), "skip-suppressed");
        assert!(by_id[&2].simulate.is_none() && by_id[&2].submit.is_none());

        let Some(SimulateVerdict::Failed(detail)) = &by_id[&3].simulate else {
            panic!("path 3 failed")
        };
        assert_eq!(detail.bucket, "no-profit");
        assert!(by_id[&3].submit.is_none());

        assert_eq!(
            by_id[&4].submit,
            Some(SubmitVerdict::Failed(crate::record::FailureKind::RpcFailed))
        );

        // ── The counters are exact FOLDS over the drained records ──
        let counters = batch_counters(&by_id.into_values().collect::<Vec<_>>());
        assert_eq!(counters.candidate_count, 3);
        assert_eq!(counters.profitable_count, 1);
        assert_eq!(counters.gas_unprofitable_count, 1);
        assert_eq!(counters.fail_count, 1);
        assert_eq!(counters.suppressed_count, 1);
        assert_eq!(counters.fail_buckets.get("no-profit"), Some(&1));
        assert_eq!(
            counters
                .fail_buckets
                .keys()
                .cloned()
                .collect::<HashSet<_>>(),
            std::iter::once("no-profit".to_string()).collect::<HashSet<_>>()
        );
    }

    /// A gate-clearing candidate fixture for the forensic emission: the
    /// economics the submit orchestration sorts on + the composed
    /// `execute()` calldata.
    fn forensic_candidate(
        path_id: u64,
        net_profit: u128,
        calldata: &[u8],
    ) -> degenbot_submission::SubmitCandidate {
        degenbot_submission::SubmitCandidate {
            path_id,
            gross_profit: alloy::primitives::U256::from(net_profit + 1_000_000_000_u128),
            net_profit: alloy::primitives::U256::from(net_profit),
            gas_used: 200_000,
            priority_fee: 1_000_000_000_u128,
            base_fee_next: 1_000_000_000_u128,
            execute_calldata: alloy::primitives::Bytes::copy_from_slice(calldata),
            executor_address: degenbot_core::address_utils::parse_address(
                "0x1111111111111111111111111111111111111111",
            )
            .unwrap(),
            access_list: None,
            path_pools: std::iter::once(degenbot_submission::PoolKey::new("0xpool")).collect(),
        }
    }

    /// Captures each event's (target, formatted message) — the forensic-line
    /// assertion surface (the same field visitation the Python log forwarder
    /// performs).
    struct EventCapture {
        lines: Arc<Mutex<Vec<(String, String)>>>,
    }

    struct MessageVisitor(String);

    impl tracing::field::Visit for MessageVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{value:?}");
            }
        }
    }

    impl<S> tracing_subscriber::Layer<S> for EventCapture
    where
        S: tracing::Subscriber,
    {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = MessageVisitor(String::new());
            event.record(&mut visitor);
            let message = visitor.0;
            let message = message
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(&message)
                .to_string();
            self.lines
                .lock()
                .unwrap()
                .push((event.metadata().target().to_string(), message));
        }
    }

    /// The submit-arm forensic emission: one INFO line per gate-clearing
    /// candidate BEFORE broadcast, carrying the fork-replay fields (path,
    /// solve block, net wei, gas, raw calldata hex) under the closed `exec`
    /// domain target.
    #[test]
    fn submit_arm_forensic_line_captures_calldata_and_economics_per_candidate() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(EventCapture {
            lines: Arc::clone(&lines),
        });
        let _guard = tracing::subscriber::set_default(subscriber);

        log_submit_arm(
            &[
                forensic_candidate(7, 123, &[0xde, 0xad]),
                forensic_candidate(9, 456, &[0xbe, 0xef]),
            ],
            4242,
        );

        let lines = lines.lock().unwrap().clone();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            (
                "degenbot::exec".to_string(),
                "path=7 solve_block=4242 net_wei=123 gas=200000 calldata=dead".to_string()
            )
        );
        assert_eq!(
            lines[1],
            (
                "degenbot::exec".to_string(),
                "path=9 solve_block=4242 net_wei=456 gas=200000 calldata=beef".to_string()
            )
        );
    }
}
