//! The shared assembly + encoding + simulation stage (stages 1 and 2).
//!
//! ONE owner for the work both consumers of this crate need: the production
//! lane ([`crate::executor::BatchExecutor`], which runs it inside its
//! [`degenbot_submission::SimLeaf`] and then hands the survivors to the ordered
//! submit arm) and the probe executor
//! ([`crate::probe::ProbeExecutor`], which runs it and returns the product to
//! the caller that asked). Neither re-implements a hop of it: the assembly
//! (`crate::assembly::assemble_batch`), the production encoder
//! ([`degenbot_execution::ExecutionAdapter`] via the fan-out + `join_sim_result`),
//! and the EVM2 simulation (`degenbot_arbitrage`'s fan-out) are driven ONCE,
//! here.
//!
//! Stage 1 binds every raw row to every registered deployment (the per-lane
//! input-order cap, the early policy projection, the pre-selection staleness
//! gate) and stage 2 fans the pre-admitted survivors out to their own
//! deployment's lane, then attributes each result. The product is
//! [`SimStageOutput`]: the per-candidate records plus the encoder's joined
//! rows.
//!
//! The stage owns NO submission anything: no signer, no nonce lane, no relay
//! fan-out, no broadcast arm, and no dispatcher. A caller that submits reads
//! the joined rows; a caller that only observes (a probe) reads the records and
//! may render the rows as planning evidence, but nothing in this module can
//! send them.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use degenbot_arbitrage::{
    DeploymentRegistry, FeeOnTransferRegistry, PoolDivergence, SimulationBlock,
};
use degenbot_execution::{CandidateId, DeploymentId, PathInfo};
use degenbot_executor::composers::EncodeOptions;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_submission::{PathSuppression, SimFuture, SimLeaf, SubmitCandidate};
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;
use parking_lot::RwLock;

use crate::assembly::{
    assemble_batch, join_sim_result, AssembledBatch, AssemblyError, AssemblyInputs, PathResolver,
};
use crate::executor::{BatchWork, ExecutorConfig};
use crate::policy::{ExecutorPolicy, ProbeCapital};
use crate::record::{AssemblyVerdict, BatchOutcome, SimulateVerdict};
use crate::row::PayloadRow;

/// The sim-side injected handles: everything stages 1-2 read, and nothing the
/// submit arm holds.
///
/// This is the slice of [`crate::policy::ExecutorRuntime`] a caller needs to run
/// the stages at all, and deliberately excludes the submit side — a probe
/// construction names no key holder, no nonce authority, no broadcast target,
/// and no receipt probe, so an execution probe cannot reach a signature even by
/// mistake. The production conversion
/// ([`ExecutorConfig::from_verdict`]) destructures its runtime into this slice
/// plus the submit handles, so both constructions share ONE stage build.
pub struct SimRuntime {
    /// The per-batch sim cap (a plain count; `0` = uncapped, clamped once here
    /// at the boundary so the drop happens where the dropped rows are known).
    pub max_candidates: usize,
    /// The v4-batch encode axis (no declared key yet — a runtime value).
    pub use_v4_batch: bool,
    /// Capital posture for the probe's own encoding: `Production` installs and
    /// strips nothing, `SelfFund` installs the known asset set with the
    /// self-funded encoding, `ZeroBalance` strips it with the in-path-flash
    /// encoding.
    pub probe_capital: ProbeCapital,
    /// The path resolver (the engine registry projection).
    pub resolver: Arc<dyn PathResolver>,
    /// The cross-block suppression registry.
    pub suppression: Arc<Mutex<PathSuppression>>,
    /// The per-pool solver-divergence memo.
    pub divergence: Arc<Mutex<PoolDivergence>>,
    /// The per-token fee-on-transfer registry.
    pub fot: Arc<Mutex<FeeOnTransferRegistry>>,
    /// The typed RPC provider (the sim leaf's cold-miss fallback DB).
    pub provider: Arc<AlloyProvider>,
    /// The built-in settlement deployment binding: the executor / WETH /
    /// `PoolManager` identities, the owner, and the encode policy.
    pub binding: degenbot_arbitrage::CmdExecutorBinding,
    /// The session's ADDITIONAL execution deployments, registered after the
    /// built-in one and in this order.
    pub additional_deployments: Vec<Arc<degenbot_arbitrage::SimulationDeployment>>,
    /// How a solved path chooses among those deployments
    /// ([`degenbot_arbitrage::TargetSelection`]).
    pub target_selection: degenbot_arbitrage::TargetSelection,
    /// The declared-code override address (used when the inject stance is on).
    pub injected_address: Option<alloy::primitives::Address>,
    /// The executor runtime bytecode (installed by the declared override).
    pub runtime_bytecode: alloy::primitives::Bytes,
    /// The engine's shared state owner (`None` only for empty-input callers).
    pub bot_state: Option<Arc<StateLock<BotState>>>,
    /// The cross-block warm-code cache.
    pub warm_cache: Option<Arc<RwLock<degenbot_simulation::WarmCodeCacheInner>>>,
}

/// The stage's own state: the policy values and the runtime arcs stages 1-2
/// read, built ONCE by [`SimStage::build`].
///
/// Crate-visible: the two consumers live in this crate and construct it through
/// [`SimRuntime`]; no driver assembles one directly.
pub(crate) struct SimStage {
    /// The per-batch sim cap per registered deployment's lane.
    pub(crate) max_candidates: usize,
    /// The thin-margin floor in bps (0 disables).
    pub(crate) min_profit_margin_bps: u64,
    /// The path resolver (the engine registry projection).
    pub(crate) resolver: Arc<dyn PathResolver>,
    /// The cross-block suppression registry.
    pub(crate) suppression: Arc<Mutex<PathSuppression>>,
    /// The per-pool solver-divergence memo.
    pub(crate) divergence: Arc<Mutex<PoolDivergence>>,
    /// The per-token fee-on-transfer registry.
    pub(crate) fot: Arc<Mutex<FeeOnTransferRegistry>>,
    /// The typed RPC provider.
    pub(crate) provider: Arc<AlloyProvider>,
    /// The session's execution deployments, in fan-out order.
    pub(crate) deployments: Arc<DeploymentRegistry>,
    /// Which deployments each solved path is offered to.
    pub(crate) target_selection: degenbot_arbitrage::TargetSelection,
    /// The engine's shared state owner (the staleness gate + EVM anchor).
    pub(crate) bot_state: Option<Arc<StateLock<BotState>>>,
    /// The cross-block warm-code cache.
    pub(crate) warm_cache: Option<Arc<RwLock<degenbot_simulation::WarmCodeCacheInner>>>,
}

impl SimStage {
    /// Build the stage from the verdict-named policy and the sim-side handles:
    /// the ONE place the deployment registry and the encode posture are
    /// composed, so the production executor and a probe executor run the same
    /// stages over the same answers.
    ///
    /// `authority` is the account every deployment must name as its owner. The
    /// production conversion hands the session signer's address; a probe hands
    /// the account it SIMULATES from (its binding's owner), because a probe
    /// holds no key at all.
    ///
    /// # Errors
    ///
    /// [`degenbot_arbitrage::DeploymentRegistryError`] when the composed
    /// registry is empty or holds two deployments with one identity, or when a
    /// deployment's owner is not `authority`.
    ///
    /// # Panics
    ///
    /// When the verdict declares `simulation.inject_executor_code` but the
    /// runtime names no `injected_address`, or names an address other than the
    /// executor the built-in adapter encodes to.
    pub(crate) fn build(
        policy: &ExecutorPolicy,
        runtime: SimRuntime,
        authority: alloy::primitives::Address,
    ) -> Result<Self, degenbot_arbitrage::DeploymentRegistryError> {
        let SimRuntime {
            max_candidates,
            use_v4_batch,
            probe_capital,
            resolver,
            suppression,
            divergence,
            fot,
            provider,
            binding,
            additional_deployments,
            target_selection,
            injected_address,
            runtime_bytecode,
            bot_state,
            warm_cache,
        } = runtime;
        let max_candidates = if max_candidates == 0 {
            usize::MAX
        } else {
            max_candidates
        };
        // The non-capital encode axes are the session's; the capital posture is
        // the ONE value that supplies the `funding` / zero-balance pair AND the
        // deployment's override plan, so the boundary converts it exactly once,
        // by handing it over.
        let opts = EncodeOptions {
            erc6909_profit: policy.erc6909_profit,
            use_v4_batch,
            ..EncodeOptions::default()
        };
        let binding = binding.with_opts(opts).with_capital(probe_capital);
        let builtin = if policy.inject_code {
            #[expect(clippy::expect_used)] // the declared inject stance names its address
            let address = injected_address.expect(
                "inject_code is true but injected_address is None — supply the injection address",
            );
            #[expect(clippy::expect_used)] // the declared address must be the executor's
            binding
                .injected(
                    ExecutorConfig::BUILTIN_DEPLOYMENT_ID,
                    address,
                    runtime_bytecode,
                )
                .expect(
                    "the declared injection address must be the executor address the                              adapter encodes to",
                )
        } else {
            binding.deployment(ExecutorConfig::BUILTIN_DEPLOYMENT_ID)
        };
        // The built-in deployment is the first entry; a heterogeneous session
        // appends its own deployments after it, in the order the runtime names
        // them. That order is the fan-out order.
        let mut entries = Vec::with_capacity(1 + additional_deployments.len());
        entries.push(Arc::new(builtin));
        entries.extend(additional_deployments);
        let registry = DeploymentRegistry::new(entries)?;
        registry.validate_signing_authority(authority)?;
        Ok(Self {
            max_candidates,
            min_profit_margin_bps: policy.min_profit_margin_bps,
            resolver,
            suppression,
            divergence,
            fot,
            provider,
            deployments: Arc::new(registry),
            target_selection,
            bot_state,
            warm_cache,
        })
    }

    /// Stages 1 + 2: assemble (which projects the early policy, the
    /// pre-selection staleness gate included), fan out, attribute, join.
    ///
    /// Unchanged from the day the production lane owned it: this body IS the
    /// production stage pass, moved — never rewritten — so a probe and a session
    /// batch cannot drift apart.
    #[expect(
        clippy::too_many_lines,
        reason = "the stage reads best as one ordered pass"
    )]
    #[expect(
        clippy::expect_used,
        reason = "the assembly mints candidates only for registered deployments"
    )]
    pub(crate) fn run(&self, work: &BatchWork) -> Result<SimStageOutput, String> {
        // The body reads its policy values and runtime arcs off `cfg` — the name
        // this pass has carried since the production lane owned it.
        let cfg = self;
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
                    // The state owner rides the ASSEMBLY: the projection's
                    // PRE-SELECTION staleness gate runs inside it, after the
                    // caller's input-order cap and before the survivors reach
                    // the target-selection walk — that is what keeps a stale
                    // candidate from being composed, accepted, and thereby
                    // retiring a fresh sibling. The pre-admitted leaf's
                    // finalization re-reads the same nonces afterwards, so a
                    // state write past this gate still drops the candidate.
                    bot_state: cfg.bot_state.as_deref(),
                    current_block: work.current_block,
                    min_profit_margin_bps: cfg.min_profit_margin_bps,
                    max_candidates: cfg.max_candidates,
                    deployment_id: deployment.deployment_id(),
                    target: deployment.target(),
                })
                .map_err(|e: AssemblyError| e.to_string())?
            };

            // Deployment-major merge: a lane's records, candidates, resolved
            // paths, and payload joins append in registry order, and that order
            // is what the whole batch's record set (and every submit row)
            // reports in.
            batch.outcomes.append(&mut lane.outcomes);
            batch.payload_submits.append(&mut lane.payload_submits);
            batch.candidates.append(&mut lane.candidates);
            batch.path_info_by_id.extend(lane.path_info_by_id);
        }

        // Stage 2 — the registry fan-out over the PRE-ADMITTED transport: one
        // lane per registered deployment, in registry order, and each candidate
        // simulated ONLY against the deployment its identity names. Assembly
        // PROJECTED the early half of the policy (`project_early_admission`);
        // the leaf's finalization then re-reads the same registries
        // authoritatively at simulation time (stamping suppression, divergence,
        // fee-on-transfer, staleness) and never re-runs the thin-margin floor,
        // so the projection is evidence, not a promise, and the batch carries
        // one implementation of the common policy rather than two.
        //
        // The pre-fan-out identities are kept beside the fan-out's results: a
        // candidate the leaf counted as a per-path exception is re-emitted by
        // neither bucket nor failure list, and its record must still appear.
        let entered: Vec<(CandidateId, alloy::primitives::Address)> = batch
            .candidates
            .iter()
            .map(|admitted| {
                let candidate_id = admitted.candidate().candidate_id;
                let deployment = cfg
                    .deployments
                    .get(candidate_id.deployment_id())
                    .expect("assembly mints candidates only for registered deployments");
                (candidate_id, deployment.target())
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
        let fanout = degenbot_arbitrage::dispatch_admitted_across_deployments(
            std::mem::take(&mut batch.candidates),
            degenbot_arbitrage::FanOutInputs {
                deployments: &cfg.deployments,
                block: &block,
                current_block: work.current_block,
                target_selection: cfg.target_selection,
                path_suppression: &cfg.suppression,
                pool_divergence: &cfg.divergence,
                fot_registry: &cfg.fot,
                bot_state: cfg.bot_state.as_ref(),
                warm_cache: cfg.warm_cache.as_ref(),
            },
        )
        .map_err(|error| error.to_string())?;

        // Attribution — per-candidate simulate verdicts, deployment-major (the
        // order the lanes and their assemblies produced). Every map keys on
        // `CandidateId`, so two targets on one path stay two rows.
        //
        // These maps cannot silently overwrite a LIVE duplicate: the fan-out
        // refuses a repeated identity before any lane runs
        // (`degenbot_arbitrage::identity`), so one key here is one candidate.
        // The `entered` walk below relies on the same fact — one identity is one
        // submit row, never two rows the submit lane's mutual exclusion would
        // have to hide.
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

impl SimLeaf<BatchWork, SimStageOutput> for SimStage {
    fn simulate<'a>(&'a self, work: &'a BatchWork) -> SimFuture<'a, SimStageOutput> {
        Box::pin(async move { self.run(work).map(Some) })
    }
}

/// The sim stage's product: the batch's records (stages 1-2 filled) + the
/// joined submit rows for stage 3.
///
/// Crate-visible so the session consumer's own tests can drive the ordered lane
/// through the same [`crate::executor::BatchExecutor`] value the production path
/// builds, and so the probe executor can hand the same product to its own
/// caller.
pub(crate) struct SimStageOutput {
    pub(crate) outcomes: Vec<BatchOutcome>,
    pub(crate) submit_candidates: Vec<SubmitCandidate>,
}

/// Arm the session's canonical-head BAL pipeline over `provider` + `warm_cache`
/// when the cache carries the `simulation.delta_dispatch` opt-in, proving the
/// endpoint's capability first.
///
/// This is the one place the provider, the session's warm cache, and the
/// opt-in already meet, so it is shared by BOTH constructions
/// ([`crate::executor::BatchExecutor::new`] and
/// [`crate::probe::ProbeExecutor::new`]): a probe runs the same sim stage and
/// therefore reads the same armed evidence rather than a second pipeline.
/// Arming is a no-op when the cache carries no opt-in (the shipped posture) and
/// fetches nothing — it only hands the pipeline the driver it will use. OFF
/// never probes: no header read and no BAL request, ever.
///
/// # Errors
///
/// [`degenbot_simulation::sim::evm2::bal_capability::BalCapabilityError`] when
/// the opt-in is ON and the capability probe fails — an unreadable head, an
/// unreadable lineage, no COMPLETE canonical lineage at the head number, or a
/// list that cannot be acquired or decoded. The error names the remediation its
/// own stage can act on.
pub(crate) async fn arm_session_bal(
    warm_cache: Option<&Arc<RwLock<degenbot_simulation::WarmCodeCacheInner>>>,
    provider: &Arc<AlloyProvider>,
) -> Result<(), degenbot_simulation::sim::evm2::bal_capability::BalCapabilityError> {
    let Some(cache) = warm_cache else {
        return Ok(());
    };
    // The opt-in is a flag read: take it under the guard, then drop the guard
    // for the probe's I/O.
    let on = cache.read().delta_dispatch().is_some();
    if !on {
        return Ok(());
    }
    // Awaited on the caller's runtime: no bridge, no block-in-place, so a
    // current-thread runtime is a supported caller.
    degenbot_simulation::sim::evm2::bal_capability::probe_bal_capability(provider).await?;
    cache.read().arm_bal_heads(provider);
    Ok(())
}
