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
    candidate_is_stale, DeploymentRegistry, FeeOnTransferRegistry, PoolDivergence, SimulationBlock,
};
use degenbot_execution::{CandidateId, DeploymentId, PathInfo};
use degenbot_executor::composers::EncodeOptions;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_submission::{
    dispatch_and_submit, Dispatcher, EnqueueOutcome, PathSuppression, PipelineFailure,
    ReceiptProbe, SimFuture, SimLeaf, SimSubmitPipeline, SubmissionLedger, SubmissionTarget,
    SubmitLeaf, SubmitRecord, TxSigner,
};
use degenbot_substrate::nonce::NonceLane;
use degenbot_substrate::state_lock::{LockSite, StateLock};
use degenbot_substrate::BotState;
use parking_lot::RwLock;

use crate::assembly::{
    assemble_batch, join_sim_result, AssembledBatch, AssemblyError, AssemblyInputs, PathResolver,
};
use crate::policy::{ExecutorPolicy, ExecutorRuntime};
use crate::record::{AssemblyVerdict, BatchOutcome, SimulateVerdict, SubmitVerdict};
use crate::row::{PayloadRow, RawResult};

/// The session-static executor configuration — the resolved verdict's policy
/// values plus the injected runtime handles, assembled by
/// [`ExecutorConfig::from_verdict`] and nothing else.
///
/// Per-block facts (`base_fee_next`, `block_timestamp`, the priority-fee
/// percentiles) ride [`BatchWork`]; everything session-scoped lives here.
/// The fields are crate-private ON PURPOSE: the one construction boundary is
/// the verdict conversion ([`crate::policy`]), so a driver cannot assemble a
/// policy value a second authority named.
pub struct ExecutorConfig {
    /// The in-flight sim bound (a plain count, floor 1 — the conversion's
    /// clamp, `simulation.pipeline_concurrency`).
    pub(crate) sim_concurrency: usize,
    /// The per-batch sim cap (a plain count; `0` = uncapped, clamped at the
    /// boundary so the drop happens once, where the dropped rows are known).
    ///
    /// The cap bounds EACH registered deployment's lane, not the batch: a
    /// two-target session simulates at most `2 x cap` candidates, so one
    /// target's candidate volume can never starve another's.
    pub(crate) max_candidates: usize,
    /// The thin-margin floor in bps (0 disables;
    /// `dispatch.min_profit_margin_bps`).
    pub(crate) min_profit_margin_bps: u64,
    /// The path resolver (the engine registry projection).
    pub(crate) resolver: Arc<dyn PathResolver>,
    /// The cross-block suppression registry (decision b — its own policy
    /// value, never merged with the payload-served set).
    pub(crate) suppression: Arc<Mutex<PathSuppression>>,
    /// The per-pool solver-divergence memo.
    pub(crate) divergence: Arc<Mutex<PoolDivergence>>,
    /// The per-token fee-on-transfer registry.
    pub(crate) fot: Arc<Mutex<FeeOnTransferRegistry>>,
    // ── Simulate-stage session values (the `SimulateContext` projection) ──
    /// The typed RPC provider (cold-miss fallback DB).
    pub(crate) provider: Arc<AlloyProvider>,
    /// The session's execution deployments, in fan-out order: ONE entry for the
    /// built-in settlement session, more for a heterogeneous one. Every solved
    /// path becomes one candidate per entry, and each candidate's submit row
    /// carries its own deployment's target.
    pub(crate) deployments: Arc<DeploymentRegistry>,
    /// Which of those deployments each solved path is offered to
    /// ([`degenbot_arbitrage::TargetSelection`]): the default
    /// ordered-first-compatible walk, or the explicit all-target fan-out. A
    /// one-entry registry selects the same single deployment either way.
    pub(crate) target_selection: degenbot_arbitrage::TargetSelection,
    /// The engine's shared state owner (the staleness gate + the per-block
    /// EVM anchor). `None` only for empty-input callers.
    pub(crate) bot_state: Option<Arc<StateLock<BotState>>>,
    /// The cross-block warm-code cache.
    pub(crate) warm_cache: Option<Arc<RwLock<degenbot_simulation::WarmCodeCacheInner>>>,
    // ── Submit-stage session values (the `dispatch_and_submit` projection) ──
    /// The coordination state (pool mutual exclusion, monitors, block clock).
    pub(crate) dispatcher: Arc<Mutex<Dispatcher>>,
    /// The operator key holder (constructed ONCE; the key never leaves Rust).
    pub(crate) signer: Arc<TxSigner>,
    /// The receipt probe the spawned monitors poll.
    pub(crate) probe: Arc<dyn ReceiptProbe + Send + Sync>,
    /// The host-minted sign-time nonce lane.
    pub(crate) nonce_lane: Arc<NonceLane<SubmissionLedger>>,
    /// The live-submission skip (a safety policy value, S3 — the driver's
    /// run-mode stance).
    pub(crate) dry_run: bool,
    /// The submit-side `inject_code` guard (the injected contract doesn't
    /// exist on-chain — live submission is unsafe). The SAME declared inject
    /// stance as `inject_code`, converted once.
    pub(crate) inject_code_guard: bool,
    /// Additional broadcast providers fanned out alongside the read provider.
    pub(crate) extra_broadcast: Vec<Arc<AlloyProvider>>,
    /// Where the signed transaction is sent (the relay posture value).
    pub(crate) target: SubmissionTarget,
}

impl ExecutorConfig {
    /// The built-in settlement deployment's operator-assigned identity.
    ///
    /// [`ExecutorConfig::from_verdict`] registers it as the FIRST entry of the
    /// session's registry, so a session that names no additional deployment is
    /// the one-entry case with unchanged behavior. Additional deployments follow
    /// it in the order the runtime supplies, never downstream of this
    /// boundary.
    pub const BUILTIN_DEPLOYMENT_ID: degenbot_execution::DeploymentId =
        degenbot_execution::DeploymentId::new(1);

    /// The ONE construction boundary: `verdict` (the resolved values,
    /// `degenbot_config::BotConfig`) names every policy value — converted
    /// through [`ExecutorPolicy::from`], which owns every default and clamp
    /// — and `runtime` carries the injected handles the verdict cannot name.
    ///
    /// A driver never names a policy value itself: the twin clamps that used
    /// to live at the Python and FFI construction sites died with this
    /// boundary.
    ///
    /// # Errors
    ///
    /// [`degenbot_arbitrage::DeploymentRegistryError`] when the composed
    /// registry is empty or holds two deployments with one identity
    /// ([`DeploymentRegistry::new`]), or when a deployment's owner is not the
    /// account the configured signer holds
    /// ([`DeploymentRegistry::validate_signing_authority`]). Each is a
    /// composition error: an empty registry has no target to encode against, a
    /// duplicate identity makes every post-target row ambiguous, and an owner
    /// the signer does not hold would be simulated from — and submitted as — an
    /// account the session cannot sign for. The caller that assembled the
    /// configuration names it to its own consumer (the `PyO3` boundary raises
    /// `ValueError`).
    ///
    /// # Panics
    ///
    /// When the resolved verdict declares `simulation.inject_executor_code` but
    /// the injected runtime names no `injected_address`, or names an address
    /// other than the executor the built-in adapter encodes to — the declared
    /// override must name the very contract its transaction targets.
    pub fn from_verdict(
        verdict: &degenbot_config::BotConfig,
        runtime: ExecutorRuntime,
    ) -> Result<Self, degenbot_arbitrage::DeploymentRegistryError> {
        let policy = ExecutorPolicy::from(verdict);
        let max_candidates = if runtime.max_candidates == 0 {
            usize::MAX
        } else {
            runtime.max_candidates
        };
        // The signer's account is the one authority every deployment must name
        // as its owner; read before the signer handle moves into the config.
        let authority = runtime.signer.address();
        Ok(Self {
            sim_concurrency: policy.sim_concurrency,
            max_candidates,
            min_profit_margin_bps: policy.min_profit_margin_bps,
            deployments: {
                let opts = EncodeOptions {
                    erc6909_profit: policy.erc6909_profit,
                    use_v4_batch: runtime.use_v4_batch,
                    ..EncodeOptions::default()
                };
                let binding = runtime.binding.with_opts(opts);
                let builtin = if policy.inject_code {
                    #[expect(clippy::expect_used)] // the declared inject stance names its address
                    let address = runtime.injected_address.expect(
                        "inject_code is true but injected_address is None — supply the injection address",
                    );
                    #[expect(clippy::expect_used)] // the declared address must be the executor's
                    let deployment = binding
                        .injected(
                            Self::BUILTIN_DEPLOYMENT_ID,
                            address,
                            runtime.runtime_bytecode,
                        )
                        .expect(
                            "the declared injection address must be the executor address the                              adapter encodes to",
                        );
                    deployment
                } else {
                    binding.deployment(Self::BUILTIN_DEPLOYMENT_ID)
                };
                // The built-in deployment is the first entry; a heterogeneous
                // session appends its own deployments after it, in the order the
                // runtime names them. That order is the fan-out order.
                let mut entries = Vec::with_capacity(1 + runtime.additional_deployments.len());
                entries.push(Arc::new(builtin));
                entries.extend(runtime.additional_deployments);
                let registry = DeploymentRegistry::new(entries)?;
                registry.validate_signing_authority(authority)?;
                Arc::new(registry)
            },
            target_selection: runtime.target_selection,
            resolver: runtime.resolver,
            suppression: runtime.suppression,
            divergence: runtime.divergence,
            fot: runtime.fot,
            provider: runtime.provider,
            bot_state: runtime.bot_state,
            warm_cache: runtime.warm_cache,
            dispatcher: runtime.dispatcher,
            signer: runtime.signer,
            probe: runtime.probe,
            nonce_lane: runtime.nonce_lane,
            dry_run: runtime.dry_run,
            inject_code_guard: policy.inject_code_guard,
            extra_broadcast: runtime.extra_broadcast,
            target: runtime.target,
        })
    }
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
///
/// Crate-visible so the session consumer's own tests can drive the ordered lane
/// through the same [`BatchExecutor`] value the production path builds.
pub(crate) struct SimStageOutput {
    pub(crate) outcomes: Vec<BatchOutcome>,
    pub(crate) submit_candidates: Vec<degenbot_submission::SubmitCandidate>,
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
    #[expect(
        clippy::expect_used,
        reason = "the assembly mints candidates only for registered deployments"
    )]
    fn run_stages_1_2(&self, work: &BatchWork) -> Result<SimStageOutput, String> {
        let cfg = &self.config;
        // The batch-local payload-served set. `CandidateId` keys it, so a path
        // the engine simulated inline at ONE target skips only that target's raw
        // row: the same path at another deployment is a different candidate and
        // still needs its own simulation.
        let served: HashSet<CandidateId> = work.payloads.iter().map(|p| p.candidate_id).collect();

        // Payload rows are engine-born, so each names the deployment that
        // simulated it inline. A row naming an unregistered deployment has no
        // lane to join — refused before any lane runs, and never folded into
        // another target's lane.
        let mut payloads_by_deployment: HashMap<DeploymentId, Vec<PayloadRow>> =
            HashMap::with_capacity(cfg.deployments.len());
        for payload in &work.payloads {
            let deployment_id = payload.candidate_id.deployment_id();
            if cfg.deployments.get(deployment_id).is_none() {
                return Err(format!(
                    "payload row for path {} names deployment {deployment_id}, which this session \
                     does not execute",
                    payload.candidate_id.path_id()
                ));
            }
            payloads_by_deployment
                .entry(deployment_id)
                .or_default()
                .push(payload.clone());
        }

        // Stage 1 — ONE assembly lane per registered deployment, in registry
        // order. Every raw solver row is bound to EVERY deployment, so the
        // candidate set is `paths x deployments`, the per-batch cap bounds each
        // lane rather than the batch, and every pre-sim policy applies once per
        // lane against that lane's candidates. The policy locks are held ONLY
        // across this synchronous span (the fan-out re-locks the same arcs at
        // its own bookends); guards drop before any later stage.
        let mut batch = AssembledBatch::default();
        for deployment in cfg.deployments.entries() {
            let lane_payloads = payloads_by_deployment
                .get(&deployment.deployment_id())
                .map_or(&[][..], Vec::as_slice);
            let mut lane = {
                let mut suppression = cfg
                    .suppression
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                assemble_batch(&AssemblyInputs {
                    rows: &work.rows,
                    payloads: lane_payloads,
                    resolver: cfg.resolver.as_ref(),
                    payload_served: &served,
                    suppression: &mut suppression,
                    divergence: &cfg.divergence,
                    fot: &cfg.fot,
                    current_block: work.current_block,
                    min_profit_margin_bps: cfg.min_profit_margin_bps,
                    max_candidates: cfg.max_candidates,
                    deployment_id: deployment.deployment_id(),
                    target: deployment.target(),
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
                let stale_ids: Vec<CandidateId> = {
                    let guard = arc.read_at(LockSite::Sim);
                    lane.candidates
                        .iter()
                        .filter(|c| candidate_is_stale(&guard, c))
                        .map(|c| c.candidate_id)
                        .collect()
                };
                if !stale_ids.is_empty() {
                    lane.candidates
                        .retain(|c| !stale_ids.contains(&c.candidate_id));
                    lane.outcomes
                        .retain(|o| !(o.simulate.is_none() && stale_ids.contains(&o.candidate_id)));
                }
            }

            // Deployment-major merge: a lane's records, candidates, resolved
            // paths, and payload joins append in registry order, and that order
            // is what the whole batch's record set (and every submit row)
            // reports in.
            batch.outcomes.append(&mut lane.outcomes);
            batch.payload_submits.append(&mut lane.payload_submits);
            batch.candidates.append(&mut lane.candidates);
            batch.path_info_by_id.extend(lane.path_info_by_id);
        }

        // Stage 2 — the registry fan-out: one lane per registered deployment, in
        // registry order, and each candidate simulated ONLY against the
        // deployment its identity names. The leaf's own pre-filters (suppression,
        // divergence, FoT, thin margin) are no-ops on the survivors: this same
        // no-await span already applied each exactly once over the same
        // registries, and the cap is already applied.
        //
        // The pre-fan-out identities are kept beside the fan-out's results: a
        // candidate the leaf counted as a per-path exception is re-emitted by
        // neither bucket nor failure list, and its record must still appear.
        let entered: Vec<(CandidateId, alloy::primitives::Address)> = batch
            .candidates
            .iter()
            .map(|c| {
                let deployment = cfg
                    .deployments
                    .get(c.candidate_id.deployment_id())
                    .expect("assembly mints candidates only for registered deployments");
                (c.candidate_id, deployment.target())
            })
            .collect();
        let provider = Arc::clone(&cfg.provider);
        let block = SimulationBlock {
            provider: &provider,
            base_fee_next: work.base_fee_next,
            current_block: work.current_block,
            block_timestamp: work.block_timestamp,
            block_priority_fees: work.block_priority_fees.clone(),
        };
        let fanout = degenbot_arbitrage::dispatch_across_deployments(
            std::mem::take(&mut batch.candidates),
            &block,
            &cfg.deployments,
            cfg.target_selection,
            &cfg.suppression,
            work.current_block,
            cfg.min_profit_margin_bps,
            &cfg.divergence,
            &cfg.fot,
            cfg.bot_state.as_ref(),
            cfg.warm_cache.as_ref(),
        )
        .map_err(|error| error.to_string())?;

        // Attribution — per-candidate simulate verdicts, deployment-major (the
        // order the lanes and their assemblies produced). Every map keys on
        // `CandidateId`, so two targets on one path stay two rows.
        // The candidates the selection policy never examined (a path whose
        // earlier target accepted composition). They were never composed or
        // simulated, so they earn no record and no submit row: this batch's
        // record set names exactly the candidates that entered execution.
        let retired: HashSet<CandidateId> = fanout.retired.iter().copied().collect();
        let mut profitable: HashMap<CandidateId, degenbot_arbitrage::SimResult> = HashMap::new();
        let mut unprofitable: HashMap<CandidateId, degenbot_arbitrage::SimResult> = HashMap::new();
        let mut failures: HashMap<CandidateId, degenbot_arbitrage::SimFailure> = HashMap::new();
        for lane in fanout.lanes {
            profitable.extend(
                lane.outcome
                    .gas_profitable
                    .into_iter()
                    .map(|r| (r.candidate_id, r)),
            );
            unprofitable.extend(
                lane.outcome
                    .gas_unprofitable
                    .into_iter()
                    .map(|r| (r.candidate_id, r)),
            );
            failures.extend(
                lane.outcome
                    .failures
                    .into_iter()
                    .map(|f| (f.candidate_id, f)),
            );
        }

        let mut submit_candidates: Vec<degenbot_submission::SubmitCandidate> = Vec::new();
        let empty_path = PathInfo::new(Vec::new());
        for (candidate_id, target) in entered {
            if retired.contains(&candidate_id) {
                continue;
            }
            // The lookup always hits: the core only re-emits path ids it was
            // handed (the join's `None`/empty fall-through keeps isolation —
            // an empty mutual-exclusion set — without fabricating hops).
            let path_info = batch.path_info_by_id.get(&candidate_id.path_id());
            let simulate = if let Some(r) = profitable.get(&candidate_id) {
                // The transaction's `to` is the DEPLOYMENT's target, so a
                // heterogeneous fan-out routes each row to its own contract.
                submit_candidates.push(join_sim_result(r, path_info, target));
                Some(SimulateVerdict::Profitable(crate::record::SimReceipt {
                    gross_profit: r.gross_profit,
                    net_profit: r.net_profit,
                    gas_used: r.gas_used,
                    priority_fee: r.priority_fee,
                }))
            } else if let Some(r) = unprofitable.get(&candidate_id) {
                Some(SimulateVerdict::GasUnprofitable(
                    crate::record::SimReceipt {
                        gross_profit: r.gross_profit,
                        net_profit: r.net_profit,
                        gas_used: r.gas_used,
                        priority_fee: r.priority_fee,
                    },
                ))
            } else if let Some(f) = failures.get(&candidate_id) {
                Some(SimulateVerdict::Failed(Box::new(f.clone())))
            } else {
                // Accounted by neither bucket nor a failure record: the
                // fan-out counted a per-path exception (its
                // `return_exceptions=True` tolerance).
                Some(SimulateVerdict::Exception)
            };
            batch.outcomes.push(BatchOutcome {
                candidate_id,
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
            "path={} deployment={} solve_block={} net_wei={} gas={} calldata={}",
            c.candidate_id.path_id(),
            c.candidate_id.deployment_id(),
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
                let (candidate_id, verdict) = match record {
                    SubmitRecord::Submitted { candidate_id, .. } => {
                        (*candidate_id, SubmitVerdict::Submitted)
                    }
                    SubmitRecord::Skipped {
                        candidate_id,
                        reason,
                    } => (
                        *candidate_id,
                        SubmitVerdict::Failed(crate::record::FailureKind::from_skip_reason(reason)),
                    ),
                };
                if let Some(slot) = stamped
                    .iter_mut()
                    .find(|o| o.candidate_id == candidate_id && o.submit.is_none())
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
    ///
    /// # Cancel safety
    ///
    /// **Cancel-safe**: no message can be lost. `UnboundedReceiver::recv` is
    /// documented cancel-safe by Tokio — dropping this future leaves any
    /// un-consumed message queued on the receiver, so a retry observes it.
    ///
    /// **Fairness caveat**: cancelling the `lock()` future (before it
    /// resolves) forfeits the place in the mutex's wait queue — a cancelled
    /// waiter re-queues on the next call. This seam has two consumers (the
    /// executor's own [`BatchExecutor::next_outcome`] and this `PyO3`-owned
    /// drain handle), so interleaved drains may interleave outcome sets. That
    /// interleaving is the sharing intent documented on [`BatchDrain`], not a
    /// defect: the stream is a single ordered lane and either consumer may
    /// take the next set.
    ///
    /// **The fence**: this lock guards ONLY a channel receiver. It must NOT be
    /// extended to invariant-bearing state — a `tokio::sync::Mutex` held
    /// across an `.await` while the guarded state is temporarily invalid is
    /// the Oxide RFD 397 bug class (see the workspace `await_holding_lock`
    /// deny, which covers the `std`/`parking_lot` guards and for which this
    /// seam is not the exception).
    pub async fn next(&mut self) -> Option<BatchOutcomeSet> {
        self.rx.lock().await.recv().await
    }
}

/// The core batch executor.
pub struct BatchExecutor {
    /// The deployment the session's ENGINE-BORN rows name — the built-in
    /// settlement deployment, whose inline sim produces the payload rows a
    /// batch carries. A heterogeneous session fans the batch's raw rows out to
    /// every registered deployment instead; this identity is the inline arm's,
    /// not the fan-out's.
    inline_deployment_id: DeploymentId,
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
    ///
    /// Construction also ARMS the session's canonical-head BAL pipeline
    /// (`WarmCodeCacheInner::arm_bal_heads`): this is the one place in either
    /// composition where the provider, the session's warm cache and the
    /// `simulation.delta_dispatch` opt-in already meet, so a session that
    /// constructs no executor has no consumer and requests nothing. Arming is a
    /// no-op when the cache carries no opt-in (the shipped posture), and it
    /// fetches nothing — it only hands the pipeline the driver it will use.
    #[must_use]
    pub fn new(config: ExecutorConfig) -> Self {
        if let Some(cache) = config.warm_cache.as_ref() {
            cache.read().arm_bal_heads(&config.provider);
        }
        let (outcome_tx, outcome_rx) = mpsc::unbounded_channel();
        let config = Arc::new(config);
        let leaves = Arc::new(EngineLeaves {
            config: Arc::clone(&config),
            outcome_tx: outcome_tx.downgrade(),
        });
        Self::from_leaves(
            config.sim_concurrency,
            ExecutorConfig::BUILTIN_DEPLOYMENT_ID,
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
    pub(crate) fn from_leaves(
        concurrency: usize,
        inline_deployment_id: DeploymentId,
        sim: Arc<dyn SimLeaf<BatchWork, SimStageOutput>>,
        submit: Arc<dyn SubmitLeaf<BatchWork, SimStageOutput>>,
        outcome_tx: mpsc::UnboundedSender<BatchOutcomeSet>,
        outcome_rx: mpsc::UnboundedReceiver<BatchOutcomeSet>,
    ) -> Self {
        let pipeline = SimSubmitPipeline::new(concurrency, sim, submit);
        Self {
            inline_deployment_id,
            pipeline,
            outcome_tx: std::sync::Mutex::new(Some(outcome_tx)),
            outcome_rx: Arc::new(tokio::sync::Mutex::new(outcome_rx)),
        }
    }

    /// The deployment the session's engine-born rows name — the built-in
    /// settlement deployment whose inline sim produces a batch's payload rows.
    ///
    /// The session consumer stamps this identity on each payload row it
    /// derives; a raw solver row carries no deployment and is bound to EVERY
    /// registered deployment by the assembly instead.
    #[must_use]
    pub const fn inline_deployment_id(&self) -> DeploymentId {
        self.inline_deployment_id
    }

    /// Enqueue one batch: its rows enter the ordered lane (assembly, sim,
    /// and submit run internally). Returns immediately — the caller keeps
    /// advancing.
    ///
    /// The lane's typed [`EnqueueOutcome`] is passed through unchanged: a
    /// declined batch never entered the lane (nothing was counted, spawned, or
    /// queued), and the caller decides what a stopped lane means for it.
    pub fn enqueue(&self, work: BatchWork) -> EnqueueOutcome {
        self.pipeline.enqueue(work)
    }

    /// Await the next batch's outcome set (`None` when the lane is closed
    /// and drained).
    ///
    /// # Cancel safety
    ///
    /// **Cancel-safe**: no message can be lost. `UnboundedReceiver::recv` is
    /// documented cancel-safe by Tokio — dropping this future leaves any
    /// un-consumed message queued on the receiver, so a retry observes it.
    ///
    /// **Fairness caveat**: cancelling the `lock()` future (before it
    /// resolves) forfeits the place in the mutex's wait queue — a cancelled
    /// waiter re-queues on the next call. This receiver is shared with the
    /// `PyO3` seam's drain handle (see [`BatchDrain`], the sharing intent
    /// documented on the struct), so interleaved drains may interleave outcome
    /// sets. That interleaving is the seam's intent, not a defect: the stream
    /// is a single ordered lane and either consumer may take the next set.
    ///
    /// **The fence**: this lock guards ONLY a channel receiver. It must NOT be
    /// extended to invariant-bearing state — a `tokio::sync::Mutex` held
    /// across an `.await` while the guarded state is temporarily invalid is
    /// the Oxide RFD 397 bug class (see the workspace `await_holding_lock`
    /// deny, which covers the `std`/`parking_lot` guards and for which this
    /// seam is not the exception).
    pub async fn next_outcome(&self) -> Option<BatchOutcomeSet> {
        self.outcome_rx.lock().await.recv().await
    }

    /// Poll for the next batch's outcome set without awaiting.
    ///
    /// # Cancel safety
    ///
    /// **Cancel-safe**: no message can be lost — `try_recv` never suspends
    /// and consumes only on `Ok`, so a dropped future leaves the queue
    /// untouched.
    ///
    /// **Fairness caveat**: cancelling the `lock()` future (before it
    /// resolves) forfeits the place in the mutex's wait queue — a cancelled
    /// waiter re-queues on the next call. The receiver is shared with the
    /// `PyO3` seam's drain handle (see [`BatchDrain`]), so a poll may observe
    /// a set the other consumer interleaved; that is the seam's sharing
    /// intent, not a defect.
    ///
    /// **The fence**: this lock guards ONLY a channel receiver. It must NOT be
    /// extended to invariant-bearing state — a `tokio::sync::Mutex` held
    /// across an `.await` while the guarded state is temporarily invalid is
    /// the Oxide RFD 397 bug class (see the workspace `await_holding_lock`
    /// deny, which covers the `std`/`parking_lot` guards and for which this
    /// seam is not the exception).
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

    /// The lane's authoritative failure, reported or not (`None` while the
    /// lane is healthy). This read observes nothing.
    #[must_use]
    pub fn failure(&self) -> Option<PipelineFailure> {
        self.pipeline.failure()
    }

    /// Re-raise the first stored leaf failure in the CALLER's frame (the
    /// loud-abort rule) — once.
    ///
    /// Reporting is not recovery: the lane stays terminal (every later enqueue
    /// is declined) and the failure stays readable through [`Self::failure`].
    ///
    /// # Errors
    ///
    /// [`PipelineFailure`] the first time a leaf (assembly corruption, sim, or
    /// submit) failed.
    pub fn raise_if_failed(&self) -> Result<(), PipelineFailure> {
        self.pipeline.raise_if_failed()
    }

    /// Graceful teardown: close the queue, drain in-flight work, await the
    /// submitter, release the record stream, then report the stored failure.
    ///
    /// Idempotent: the sender, the submitter handle, and the record stream are
    /// released exactly once, and while the lane carries a failure every call
    /// reports that same failure.
    ///
    /// # Errors
    ///
    /// [`PipelineFailure`] when a leaf failed — on every call while the lane
    /// carries it.
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

    /// The fixtures' one deployment identity.
    fn test_deployment_id() -> degenbot_execution::DeploymentId {
        degenbot_execution::DeploymentId::new(1)
    }

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
                    candidate_id: CandidateId::new(batch, test_deployment_id()),
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

    /// Enqueue one batch and assert the lane accepted it.
    fn accepted(executor: &BatchExecutor, work: BatchWork) {
        assert_eq!(executor.enqueue(work), EnqueueOutcome::Accepted);
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
            test_deployment_id(),
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
            accepted(&executor, work(BASE_BLOCK + batch));
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
        accepted(&executor, work(BASE_BLOCK));
        accepted(&executor, work(BASE_BLOCK + 1));

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
        accepted(&executor, work(BASE_BLOCK));
        accepted(&executor, work(BASE_BLOCK + 1));
        // Batch 0 submits; batch 1's sim failure stops the lane.
        let _ = executor.try_next_outcome().await;
        while executor.submitted() < 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let failure = executor.raise_if_failed().expect_err("stored failure");
        assert_eq!(failure.detail, "sim leaf failed at block 101");
        assert!(executor.raise_if_failed().is_ok());

        // Reporting did not recover the lane: a new batch is declined, the
        // failure stays readable, and the teardown keeps reporting it rather
        // than a fresh success contradicting the terminal state.
        assert_eq!(
            executor.enqueue(work(BASE_BLOCK + 2)),
            EnqueueOutcome::Declined(degenbot_submission::EnqueueDecline::Failed)
        );
        assert_eq!(
            executor.failure().map(|failure| failure.detail),
            Some("sim leaf failed at block 101".to_string())
        );
        assert_eq!(
            executor
                .shutdown()
                .await
                .expect_err("the failure is still the verdict")
                .detail,
            "sim leaf failed at block 101"
        );
    }

    /// The block-only drain (the Rust bot's `HeartbeatSink` shape): the
    /// drain reads ONLY `record.block` — the liveness heartbeat — and the
    /// record vocabulary compiles for a consumer that reads nothing else.
    #[tokio::test]
    async fn block_only_drain_reads_just_the_heartbeat() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let executor = executor_with(ReversedFinishSim { fail_block: None }, order);
        for batch in 0_u64..3 {
            accepted(&executor, work(BASE_BLOCK + batch));
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
    /// and `path_info` are read off the drained records.
    #[expect(
        clippy::too_many_lines,
        reason = "the fixture spells every stage's record verbatim"
    )]
    #[tokio::test]
    async fn renderer_drain_reads_every_stage() {
        use degenbot_execution::V2HopInfo;

        struct MixedSim;
        impl SimLeaf<BatchWork, SimStageOutput> for MixedSim {
            fn simulate<'a>(&'a self, work: &'a BatchWork) -> SimFuture<'a, SimStageOutput> {
                Box::pin(async move {
                    let pool = degenbot_core::address_utils::parse_address(
                        "0x1111111111111111111111111111111111111111",
                    )
                    .unwrap();
                    let view = PathInfoView::from(&degenbot_execution::PathInfo::new(vec![
                        degenbot_execution::HopInfo::V2(V2HopInfo {
                            pool_address: pool,
                            token0_address: pool,
                            token1_address: pool,
                            fee: 30,
                            zfo: true,
                        }),
                    ]));
                    let failure = FailureDetail {
                        candidate_id: CandidateId::new(3, test_deployment_id()),
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
                    };
                    let outcomes = vec![
                        BatchOutcome {
                            candidate_id: CandidateId::new(1, test_deployment_id()),
                            block: work.current_block,
                            assembly: AssemblyVerdict::Assembled,
                            simulate: Some(SimulateVerdict::Profitable(receipt())),
                            submit: None,
                            path_info: view.clone(),
                        },
                        BatchOutcome {
                            candidate_id: CandidateId::new(2, test_deployment_id()),
                            block: work.current_block,
                            assembly: AssemblyVerdict::SkipSuppressed,
                            simulate: None,
                            submit: None,
                            path_info: PathInfoView::empty(),
                        },
                        BatchOutcome {
                            candidate_id: CandidateId::new(3, test_deployment_id()),
                            block: work.current_block,
                            assembly: AssemblyVerdict::Assembled,
                            simulate: Some(SimulateVerdict::Failed(Box::new(failure))),
                            submit: None,
                            path_info: view.clone(),
                        },
                        BatchOutcome {
                            candidate_id: CandidateId::new(4, test_deployment_id()),
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
                        record.submit = match record.candidate_id.path_id() {
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
            test_deployment_id(),
            Arc::new(MixedSim),
            Arc::new(PartialSubmit { tx: tx.downgrade() }),
            tx,
            rx,
        );
        accepted(&executor, work(BASE_BLOCK));
        executor.shutdown().await.expect("no failure");

        // ── The renderer-shaped drain: every stage + path_info ──
        let mut records = executor.next_outcome().await.expect("one batch").records;
        assert_eq!(records.len(), 4);
        let by_id: std::collections::HashMap<u64, BatchOutcome> = records
            .drain(..)
            .map(|r| (r.candidate_id.path_id(), r))
            .collect();

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
        assert!(matches!(
            by_id[&4].simulate,
            Some(SimulateVerdict::GasUnprofitable(_))
        ));
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
            candidate_id: CandidateId::new(path_id, test_deployment_id()),
            gross_profit: alloy::primitives::U256::from(net_profit + 1_000_000_000_u128),
            net_profit: alloy::primitives::U256::from(net_profit),
            gas_used: 200_000,
            priority_fee: 1_000_000_000_u128,
            base_fee_next: 1_000_000_000_u128,
            execute_calldata: alloy::primitives::Bytes::copy_from_slice(calldata),
            value: alloy::primitives::U256::ZERO,
            execution_target: degenbot_core::address_utils::parse_address(
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

    impl<S> tracing_subscriber::Layer<S> for EventCapture
    where
        S: tracing::Subscriber,
    {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let message =
                degenbot_core::log_capture::MessageVisitor::capture(event).unwrap_or_default();
            self.lines
                .lock()
                .unwrap()
                .push((event.metadata().target().to_string(), message));
        }
    }

    /// The submit-arm forensic emission: one INFO line per gate-clearing
    /// candidate BEFORE broadcast, carrying the fork-replay fields (path, the
    /// deployment that would execute it, solve block, net wei, gas, raw
    /// calldata hex) under the closed `exec` domain target.
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
                "path=7 deployment=1 solve_block=4242 net_wei=123 gas=200000 calldata=dead"
                    .to_string()
            )
        );
        assert_eq!(
            lines[1],
            (
                "degenbot::exec".to_string(),
                "path=9 deployment=1 solve_block=4242 net_wei=456 gas=200000 calldata=beef"
                    .to_string()
            )
        );
    }

    /// Shutdown is idempotent at the executor boundary: the first call drains
    /// the lane and releases the record stream, and a repeat call joins
    /// nothing, publishes nothing, and leaves the closed stream closed.
    #[tokio::test]
    async fn shutdown_is_idempotent_and_closes_the_outcome_stream_once() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let executor = executor_with(ReversedFinishSim { fail_block: None }, order);
        for batch in 0_u64..2 {
            accepted(&executor, work(BASE_BLOCK + batch));
        }
        executor.shutdown().await.expect("no failure");

        let mut drained = Vec::new();
        while let Some(records) = executor.next_outcome().await {
            drained.extend(records.records.iter().map(|r| r.block));
        }
        assert_eq!(drained, vec![BASE_BLOCK, BASE_BLOCK + 1]);

        // A repeated teardown returns (rather than awaiting a join twice) and
        // cannot reopen the record stream.
        tokio::time::timeout(Duration::from_secs(1), executor.shutdown())
            .await
            .expect("the repeat shutdown returns")
            .expect("no failure");
        assert!(
            executor.try_next_outcome().await.is_none(),
            "the stream stays closed"
        );
        assert_eq!(executor.submitted(), 2, "no batch ran twice");
    }
}
