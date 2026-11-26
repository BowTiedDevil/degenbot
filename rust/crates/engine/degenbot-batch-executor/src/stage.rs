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
    assemble_batch, join_sim_result, AssembledBatch, AssemblyError, AssemblyInputs,
    JoinSimResultError, PathResolver,
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

/// The block facts one batch's fan-out simulates against.
///
/// `current_block` is the consumer's execution clock — executor targeting and
/// the batch records. `based_on` is the batch's SOLVE anchor (ADR-105 D10): the
/// exact block the simulation's chain-state reads are pinned to, which can sit
/// one block behind the clock. Carrying both as separate fields is what keeps a
/// read from being served at the clock.
fn simulation_block<'a>(provider: &'a AlloyProvider, work: &BatchWork) -> SimulationBlock<'a> {
    SimulationBlock {
        provider,
        base_fee_next: work.base_fee_next,
        current_block: work.execution_block,
        based_on: work.based_on,
        block_timestamp: work.block_timestamp,
        block_priority_fees: work.block_priority_fees.clone(),
    }
}

impl SimStage {
    /// Build the stage from the verdict-named policy and the sim-side handles:
    /// the ONE place the deployment registry and the encode posture are
    /// composed, so the production executor and a probe executor run the same
    /// stages over the same answers.
    ///
    /// `authority` is the account every deployment must name as its owner, for
    /// the constructions that may sign: the production conversion hands the
    /// session signer's address when the executor SUBMITS, and a probe hands the
    /// account it SIMULATES from (its binding's owner), because a probe holds no
    /// key at all.
    ///
    /// `None` states that the caller has no signing authority to offer: a
    /// non-submitting executor never leases a nonce, signs, or broadcasts, so a
    /// deployment may name an owner it does not hold — that owner is the
    /// transaction's simulated `from`, and it stays the deployment's own. The
    /// owner check therefore runs only for `Some`.
    ///
    /// # Errors
    ///
    /// [`degenbot_arbitrage::DeploymentRegistryError`] when the composed
    /// registry is empty or holds two deployments with one identity, or — for a
    /// `Some` authority — when a deployment's owner is not that account.
    ///
    /// # Panics
    ///
    /// When the verdict declares `simulation.inject_executor_code` but the
    /// runtime names no `injected_address`, or names an address other than the
    /// executor the built-in adapter encodes to.
    pub(crate) fn build(
        policy: &ExecutorPolicy,
        runtime: SimRuntime,
        authority: Option<alloy::primitives::Address>,
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
        if let Some(authority) = authority {
            registry.validate_signing_authority(authority)?;
        }
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
                    current_block: work.execution_block,
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
        let block = simulation_block(&provider, work);
        let fanout = degenbot_arbitrage::dispatch_admitted_across_deployments(
            std::mem::take(&mut batch.candidates),
            degenbot_arbitrage::FanOutInputs {
                deployments: &cfg.deployments,
                block: &block,
                current_block: work.execution_block,
                lineage: &work.lineage,
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

        let (mut attributed, mut submit_candidates) = attribute_results(
            &entered,
            &retired,
            &FanoutResults {
                profitable,
                unprofitable,
                failures,
            },
            &batch.path_info_by_id,
            work.execution_block,
        )
        .map_err(|refusal| refusal.to_string())?;
        batch.outcomes.append(&mut attributed);

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

impl SimStage {
    /// The **typed capital pre-flight** for one batch: the plan every lane of
    /// this stage will simulate under, composed once, with the refusal kept as
    /// its own value rather than flattened into the stage's detail string.
    ///
    /// A probe reports this directly ([`crate::probe::ProbeError::CapitalPlan`]),
    /// so a path whose capital cannot be overridden is refused as a typed
    /// planning outcome naming the asset — never as an opaque stage failure.
    /// The stage's own body composes the same plan per lane and refuses there
    /// too, so the production lane's `DispatchRefusal` contract is unchanged.
    ///
    /// It is a CONSERVATIVE pre-flight, never the assembly's answer: it reads
    /// the rows this stage can RESOLVE, so a row the resolver cannot find and an
    /// empty batch both produce no path at all. Those shapes are SKIPPED here and
    /// left to the ordinary assembly/outcome path, which already reports them
    /// (an unknown path, an unregistered deployment, a stale row). Refusing them
    /// as [`CapitalPlanError::NoPaths`] would replace that answer with a planning
    /// one — and simulating an unknown layout is never an option, so a
    /// non-production posture with nothing to compose is not an error either.
    ///
    /// # Errors
    ///
    /// [`CapitalPlanError`] from the first deployment whose plan cannot be
    /// composed over a NON-EMPTY path set, in registry order.
    pub(crate) fn probe_capital(
        &self,
        work: &BatchWork,
    ) -> Result<(), degenbot_simulation::CapitalPlanError> {
        for deployment in self.deployments.entries() {
            // A production deployment installs and clears nothing, so its plan
            // resolves no asset and needs no path.
            if deployment.capital() == ProbeCapital::Production {
                continue;
            }
            // The paths THIS deployment would simulate: the work's rows, bound to
            // its identity and resolved through the same resolver the assembly
            // reads. A row that does not resolve produces no candidate, and so no
            // capital requirement.
            let paths: Vec<degenbot_arbitrage::SimulatePath> = work
                .rows
                .iter()
                .filter_map(|row| {
                    let path_info = self.resolver.resolve(row.path_id)?;
                    let candidate = crate::assembly::build_raw_candidate(
                        row,
                        &path_info,
                        deployment.deployment_id(),
                    )
                    .ok()?;
                    Some(candidate.to_simulate_path())
                })
                .collect();
            if paths.is_empty() {
                continue;
            }
            deployment.override_params(&paths)?;
        }
        Ok(())
    }
}

/// The stage refused the WHOLE batch rather than emit records and rows it cannot
/// join honestly.
///
/// A per-row skip would be wrong here. A submit row's mutual-exclusion pool keys
/// derive from the candidate's own hops, and
/// [`degenbot_submission::Dispatcher::is_path_blocked`] reads an EMPTY set as
/// "touches no pool" — it can never block — so a row joined without its keys
/// would be admitted beside a candidate trading the same pool. Both arms name the
/// originating candidate so the refusal is attributable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimStageRefusal {
    /// The assembly's join map holds no path identity for this candidate.
    MissingPathIdentity {
        /// The candidate the fan-out re-emitted.
        candidate_id: CandidateId,
    },
    /// The candidate's resolved path has no hops, so [`join_sim_result`]
    /// refused its submit row.
    Join(JoinSimResultError),
}

impl std::fmt::Display for SimStageRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingPathIdentity { candidate_id } => write!(
                f,
                "candidate {candidate_id} was simulated without a resolved path \
                 identity: its pool-exclusion keys cannot be derived, so the batch \
                 is refused"
            ),
            Self::Join(error) => write!(f, "the batch is refused: {error}"),
        }
    }
}

impl std::error::Error for SimStageRefusal {}

/// The fan-out's per-candidate results, keyed by the identity the lane
/// simulated.
#[derive(Debug, Default)]
struct FanoutResults {
    profitable: HashMap<CandidateId, degenbot_arbitrage::SimResult>,
    unprofitable: HashMap<CandidateId, degenbot_arbitrage::SimResult>,
    failures: HashMap<CandidateId, degenbot_arbitrage::SimFailure>,
}

/// Stage 2's attribution: one record per entered candidate the selection policy
/// examined, plus one joined submit row per profitable one.
///
/// Every joined row's pool-exclusion keys derive from the candidate's own
/// resolved hops (see [`join_sim_result`]), so an entered candidate whose path
/// identity the assembly's join map cannot produce — or whose resolved path has
/// no hops for the join to key — proves the batch and the fan-out's results
/// diverged: the WHOLE batch is refused ([`SimStageRefusal`]) rather than
/// emitting a record with no display context and a submit row with no exclusion
/// set.
fn attribute_results(
    entered: &[(CandidateId, alloy::primitives::Address)],
    retired: &HashSet<CandidateId>,
    results: &FanoutResults,
    path_info_by_id: &HashMap<u64, PathInfo>,
    current_block: u64,
) -> Result<(Vec<BatchOutcome>, Vec<SubmitCandidate>), SimStageRefusal> {
    let mut outcomes = Vec::with_capacity(entered.len());
    let mut submit_candidates: Vec<degenbot_submission::SubmitCandidate> = Vec::new();
    for (candidate_id, target) in entered {
        if retired.contains(candidate_id) {
            continue;
        }
        // The path identity is looked up, never assumed: the assembly resolves
        // a path before it transports the candidate, so a miss here proves the
        // batch and the fan-out's results diverged. Refuse the whole batch —
        // the row's pool keys derive from these hops, and a submit row joined
        // without them would bypass the pool mutual exclusion.
        let Some(path_info) = path_info_by_id.get(&candidate_id.path_id()) else {
            return Err(SimStageRefusal::MissingPathIdentity {
                candidate_id: *candidate_id,
            });
        };
        let simulate = if let Some(r) = results.profitable.get(candidate_id) {
            // The transaction's `to` is the DEPLOYMENT's target, so a
            // heterogeneous fan-out routes each row to its own contract.
            let joined = join_sim_result(r, path_info, *target).map_err(SimStageRefusal::Join)?;
            submit_candidates.push(joined);
            Some(SimulateVerdict::Profitable(crate::record::SimReceipt {
                gross_profit: r.gross_profit,
                net_profit: r.net_profit,
                gas_used: r.gas_used,
                priority_fee: r.priority_fee,
            }))
        } else if let Some(r) = results.unprofitable.get(candidate_id) {
            Some(SimulateVerdict::GasUnprofitable(
                crate::record::SimReceipt {
                    gross_profit: r.gross_profit,
                    net_profit: r.net_profit,
                    gas_used: r.gas_used,
                    priority_fee: r.priority_fee,
                },
            ))
        } else if let Some(f) = results.failures.get(candidate_id) {
            Some(SimulateVerdict::Failed(Box::new(f.clone())))
        } else {
            // Accounted by neither bucket nor a failure record: the
            // fan-out counted a per-path exception (its
            // `return_exceptions=True` tolerance).
            Some(SimulateVerdict::Exception)
        };
        outcomes.push(BatchOutcome {
            candidate_id: *candidate_id,
            block: current_block,
            assembly: AssemblyVerdict::Assembled,
            simulate,
            submit: None,
            path_info: crate::record::PathInfoView::from(path_info),
        });
    }
    Ok((outcomes, submit_candidates))
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

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "tests assert on known-valid fixtures and the refusal is the expected outcome"
)]
mod tests {
    use super::*;

    use alloy::primitives::{address, Address, Bytes, U256};
    use degenbot_arbitrage::SimResult;
    use degenbot_execution::{HopInfo, V2HopInfo};

    const BLOCK: u64 = 100;
    const PATH_ID: u64 = 7;
    /// The lane's deployment `to`.
    const TARGET: Address = address!("0x00000000000000000000000000000000000000a1");
    /// The fixture path's one V2 pool.
    const POOL: Address = address!("0x1111111111111111111111111111111111111111");

    fn candidate_id() -> CandidateId {
        CandidateId::new(PATH_ID, DeploymentId::new(1))
    }

    fn v2_path() -> PathInfo {
        PathInfo::new(vec![HopInfo::V2(V2HopInfo {
            pool_address: POOL,
            token0_address: address!("0x2222222222222222222222222222222222222222"),
            token1_address: address!("0x3333333333333333333333333333333333333333"),
            fee: 30,
            zfo: true,
        })])
    }

    /// The gas-profitable result a lane re-emits for `candidate_id`.
    fn profitable(candidate_id: CandidateId) -> SimResult {
        SimResult {
            candidate_id,
            gross_profit: U256::from(6_000_000_000_u64),
            net_profit: U256::from(5_000_000_000_u64),
            gas_used: 300_000,
            priority_fee: 1,
            base_fee_next: 1_000_000_000,
            passed: true,
            execute_calldata: Bytes::from(vec![0xab, 0x58, 0x98, 0xe8]),
            value: U256::ZERO,
            access_list: None,
            captured_swaps: Vec::new(),
            hop_count: 1,
        }
    }

    fn profitable_results(candidate_id: CandidateId) -> FanoutResults {
        FanoutResults {
            profitable: std::iter::once((candidate_id, profitable(candidate_id))).collect(),
            ..FanoutResults::default()
        }
    }

    /// The batch's solve anchor rides the fan-out's block facts beside the
    /// consumer clock: an interleaved N/N+1 pair keeps two exact solve
    /// identities while both are executed at one clock block, and a batch with
    /// no exact anchor carries none (the fan-out's cold, unfenced read).
    #[tokio::test]
    async fn the_fan_out_block_facts_carry_the_batch_solve_anchor_beside_the_clock() {
        let provider = AlloyProvider::new("http://127.0.0.1:1", 0)
            .await
            .expect("a lazy provider constructs offline");
        let anchor = |number: u64, hash: [u8; 32]| {
            degenbot_core::canonical_head::ChainAnchor::new(
                1,
                number,
                alloy::primitives::B256::new(hash),
                alloy::primitives::B256::new([0x90; 32]),
            )
            .expect("the fixture anchor names its whole identity")
        };
        let work = |based_on| BatchWork {
            rows: Vec::new(),
            payloads: Vec::new(),
            based_on,
            execution_block: BLOCK,
            base_fee_next: 1,
            block_timestamp: 7,
            block_priority_fees: None,
            lineage: degenbot_bot::telemetry::BlockLineage::unattached(BLOCK),
        };

        let n = work(Some(anchor(BLOCK - 1, [0xaa; 32])));
        let n_plus_one = work(Some(anchor(BLOCK, [0xbb; 32])));
        let first = simulation_block(&provider, &n);
        let second = simulation_block(&provider, &n_plus_one);

        assert_eq!(
            first.current_block, BLOCK,
            "the execution clock stays the consumer's block"
        );
        assert_eq!(second.current_block, BLOCK);
        assert_eq!(
            first.based_on.map(|anchor| anchor.number()),
            Some(BLOCK - 1),
            "N's sim reads at N"
        );
        assert_eq!(
            second.based_on.map(|anchor| anchor.number()),
            Some(BLOCK),
            "N+1's sim reads at N+1"
        );
        assert_ne!(
            first.based_on, second.based_on,
            "the interleaved pair keeps two exact solve identities"
        );
        assert_eq!(
            first.block_timestamp, n.block_timestamp,
            "the environment's timestamp is the work's own fact"
        );

        let unanchored = simulation_block(&provider, &work(None));
        assert_eq!(
            unanchored.based_on, None,
            "a batch with no exact anchor carries none into the fan-out"
        );
        assert_eq!(unanchored.current_block, BLOCK);
    }

    /// A profitable result whose path identity the batch cannot recover is
    /// refused outright — never joined as a submit row with no pool-exclusion
    /// keys.
    #[test]
    fn a_profitable_result_without_its_path_identity_is_refused() {
        let id = candidate_id();
        let refusal = attribute_results(
            &[(id, TARGET)],
            &HashSet::new(),
            &profitable_results(id),
            &HashMap::new(),
            BLOCK,
        )
        .expect_err("a profitable row cannot be joined without its path identity");
        assert_eq!(
            refusal,
            SimStageRefusal::MissingPathIdentity { candidate_id: id }
        );
        assert!(
            refusal.to_string().contains(&id.to_string()),
            "the refusal names the originating candidate: {refusal}"
        );
    }

    /// A registered path with no hops reaches the same refusal through the
    /// stage: the join refuses the row, and the stage propagates that instead of
    /// panicking or emitting a submit row with no exclusion set.
    #[test]
    fn a_profitable_result_whose_path_has_no_hops_is_refused() {
        let id = candidate_id();
        let path_info_by_id: HashMap<u64, PathInfo> =
            std::iter::once((PATH_ID, PathInfo::new(Vec::new()))).collect();
        let refusal = attribute_results(
            &[(id, TARGET)],
            &HashSet::new(),
            &profitable_results(id),
            &path_info_by_id,
            BLOCK,
        )
        .expect_err("a hop-free path cannot yield a submit row");
        assert!(
            matches!(refusal, SimStageRefusal::Join(error) if error.candidate_id() == id),
            "the join refusal reaches the stage: {refusal:?}"
        );
        assert!(
            refusal.to_string().contains(&id.to_string()),
            "the refusal names the originating candidate: {refusal}"
        );
    }

    /// The same result joins its derived pool-exclusion set and its own lane
    /// target when the assembly's join map holds the path identity.
    #[test]
    fn a_profitable_result_with_its_path_identity_joins_the_exclusion_set() {
        let id = candidate_id();
        let path_info_by_id: HashMap<u64, PathInfo> =
            std::iter::once((PATH_ID, v2_path())).collect();
        let (outcomes, submits) = attribute_results(
            &[(id, TARGET)],
            &HashSet::new(),
            &profitable_results(id),
            &path_info_by_id,
            BLOCK,
        )
        .expect("the join map holds the entered candidate's path");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].candidate_id, id);
        assert_eq!(outcomes[0].path_info.path_type, "V2");
        assert_eq!(submits.len(), 1);
        assert_eq!(submits[0].candidate_id, id);
        assert_eq!(
            submits[0].path_pools,
            crate::assembly::derive_path_pools(&v2_path().hops)
        );
        assert!(!submits[0].path_pools.is_empty());
        assert_eq!(submits[0].execution_target, TARGET);
    }
}
