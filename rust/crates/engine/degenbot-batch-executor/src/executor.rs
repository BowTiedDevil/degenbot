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

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use degenbot_execution::DeploymentId;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::bal_capability::BalCapabilityError;
use degenbot_submission::{
    dispatch_and_submit, Dispatcher, EnqueueOutcome, PipelineFailure, ReceiptProbe, SimFuture,
    SimLeaf, SimSubmitPipeline, SubmissionLedger, SubmissionTarget, SubmitLeaf, SubmitRecord,
    TxSigner,
};
use degenbot_substrate::nonce::NonceLane;

use crate::policy::{ExecutorPolicy, ExecutorRuntime, SubmissionPolicy};
use crate::record::{BatchOutcome, SubmitVerdict};
use crate::row::{PayloadRow, RawResult};
use crate::stage::SimStage;

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
    // ── The sim side: stages 1-2, shared with the probe executor ──
    /// The assembly + production-encoding + EVM2 stage pass, built ONCE from
    /// the verdict policy and the sim-side handles ([`crate::stage::SimRuntime`])
    /// and driven by this lane's [`SimLeaf`]. A probe executor
    /// ([`crate::probe::ProbeExecutor`]) runs the very same value: there is no
    /// second assembly, encoder, or simulation path in this crate.
    pub(crate) sim: SimStage,
    // ── The lane + submit side: what a probe executor does NOT have ──
    /// The in-flight sim bound (a plain count, floor 1 — the conversion's
    /// clamp, `simulation.pipeline_concurrency`).
    ///
    /// This sizes the ordered LANE; a probe call is not queued behind it, so
    /// the probe executor never carries this value.
    pub(crate) sim_concurrency: usize,
    // ── Submit-stage session values (the `dispatch_and_submit` projection) ──
    /// The coordination state (pool mutual exclusion, monitors, block clock).
    pub(crate) dispatcher: Arc<Mutex<Dispatcher>>,
    /// The operator key holder (constructed ONCE; the key never leaves Rust).
    pub(crate) signer: Arc<TxSigner>,
    /// The receipt probe the spawned monitors poll.
    pub(crate) probe: Arc<dyn ReceiptProbe + Send + Sync>,
    /// The host-minted sign-time nonce lane.
    pub(crate) nonce_lane: Arc<NonceLane<SubmissionLedger>>,
    /// The submission policy: whether the submit arm may lease a nonce, sign,
    /// and broadcast. Converted ONCE from the driver's run-mode stance
    /// ([`SubmissionPolicy::from_dry_run`]) — the submit leaf's dry-run flag and
    /// the probe entry's refusal both read this value, never a second stance.
    pub(crate) submission: SubmissionPolicy,
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
        // The runtime is ONE value carrying both halves; this destructuring is
        // the ONE place they separate. A probe construction has no submit half
        // at all, so it hands the same sim-side struct
        // ([`crate::stage::SimRuntime`]) to the same [`SimStage::build`] and
        // keeps none of the handles below.
        let ExecutorRuntime {
            max_candidates,
            use_v4_batch,
            probe_capital,
            dry_run,
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
            dispatcher,
            signer,
            probe,
            nonce_lane,
            extra_broadcast,
            target,
        } = runtime;
        // The signer's account is the one authority every deployment must name
        // as its owner; read before the signer handle moves into the config.
        let authority = signer.address();
        let sim = SimStage::build(
            &policy,
            crate::stage::SimRuntime {
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
            },
            authority,
        )?;
        Ok(Self {
            sim,
            sim_concurrency: policy.sim_concurrency,
            dispatcher,
            signer,
            probe,
            nonce_lane,
            submission: SubmissionPolicy::from_dry_run(dry_run),
            inject_code_guard: policy.inject_code_guard,
            extra_broadcast,
            target,
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

/// The sim stage's product, re-exported from its ONE owner
/// ([`crate::stage`]): the batch's records (stages 1-2 filled) + the joined
/// encoder rows for stage 3.
///
/// Crate-visible so the session consumer's own tests can drive the ordered lane
/// through the same [`BatchExecutor`] value the production path builds.
pub(crate) use crate::stage::SimStageOutput;

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

impl SimLeaf<BatchWork, SimStageOutput> for EngineLeaves {
    /// Stages 1 + 2 — the shared [`crate::stage::SimStage`] pass, the same one
    /// a probe executor runs (this leaf adds nothing but the lane's contract).
    fn simulate<'a>(&'a self, work: &'a BatchWork) -> SimFuture<'a, SimStageOutput> {
        Box::pin(async move { self.config.sim.run(work).map(Some) })
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
                &cfg.sim.provider,
                &cfg.signer,
                Arc::clone(&cfg.probe),
                &cfg.nonce_lane,
                current_block,
                cfg.submission.dry_run(),
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
    /// This executor's submission policy (see [`BatchExecutor::submission`]).
    submission: SubmissionPolicy,
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
    /// fetches nothing — it only hands the pipeline the driver it will use. The
    /// arming itself is `stage::arm_session_bal`, shared with
    /// [`crate::probe::ProbeExecutor::new`] so the two constructions cannot
    /// drift into two BAL postures.
    ///
    /// # The ON startup capability probe
    ///
    /// With the opt-in ON, construction first proves the endpoint can serve the
    /// CURRENT canonical head's block access list AND that this tree fully
    /// decodes it (`probe_bal_capability`). Only a proven capability arms the
    /// pipeline: a node serving neither official EIP-7928 representation would
    /// otherwise turn every head `bal-unreadable` and quietly collapse the flag
    /// to the legacy lane, so an unusable endpoint is a CONSTRUCTION error
    /// instead. The probe reads the warm cache's opt-in under its guard and
    /// releases it before any provider I/O; nothing is fetched while a lock is
    /// held. OFF never probes: no header read and no BAL request, ever.
    ///
    /// # Errors
    ///
    /// [`BalCapabilityError`] when the opt-in is ON and the probe fails — an
    /// unreadable head, an unreadable lineage, no COMPLETE canonical lineage at
    /// the head number (no block there, or a header missing its hash or parent
    /// hash), or a list that cannot be acquired or decoded. The error names the
    /// remediation its own stage can act on: check the node's health /
    /// canonical-header availability for a lineage failure; enable EIP-7928 BAL
    /// RPC support on the endpoint or disable `simulation.delta_dispatch` for an
    /// acquisition/decode failure.
    ///
    /// # Async
    ///
    /// Construction is `async` because the probe is real provider I/O: a
    /// pure-Rust caller awaits it on its own runtime, whatever that runtime's
    /// flavor. A SYNCHRONOUS caller (the Python adapter) drives it through
    /// `degenbot_core::runtime::bridged_block_on`, the one home of the
    /// sync-over-async boundary — the constructor itself never blocks a runtime.
    pub async fn new(config: ExecutorConfig) -> Result<Self, BalCapabilityError> {
        // The ONE arming site, shared with the probe executor's construction
        // (`crate::stage::arm_session_bal`): a probe runs the same sim stage
        // over the same warm cache, so it reads the same armed evidence rather
        // than a second BAL pipeline.
        crate::stage::arm_session_bal(config.sim.warm_cache.as_ref(), &config.sim.provider).await?;
        let (outcome_tx, outcome_rx) = mpsc::unbounded_channel();
        let config = Arc::new(config);
        let leaves = Arc::new(EngineLeaves {
            config: Arc::clone(&config),
            outcome_tx: outcome_tx.downgrade(),
        });
        Ok(Self::from_leaves(
            config.sim_concurrency,
            ExecutorConfig::BUILTIN_DEPLOYMENT_ID,
            config.submission,
            Arc::clone(&leaves) as Arc<dyn SimLeaf<BatchWork, SimStageOutput>>,
            Arc::clone(&leaves) as Arc<dyn SubmitLeaf<BatchWork, SimStageOutput>>,
            outcome_tx,
            outcome_rx,
        ))
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
    /// leaves receive a weak downgrade of it. The `submission` policy is the
    /// caller's: the production constructor reads it off the config, and a
    /// fake-leaf lane carries the non-submitting one.
    pub(crate) fn from_leaves(
        concurrency: usize,
        inline_deployment_id: DeploymentId,
        submission: SubmissionPolicy,
        sim: Arc<dyn SimLeaf<BatchWork, SimStageOutput>>,
        submit: Arc<dyn SubmitLeaf<BatchWork, SimStageOutput>>,
        outcome_tx: mpsc::UnboundedSender<BatchOutcomeSet>,
        outcome_rx: mpsc::UnboundedReceiver<BatchOutcomeSet>,
    ) -> Self {
        let pipeline = SimSubmitPipeline::new(concurrency, sim, submit);
        Self {
            submission,
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

    /// This executor's submission policy — the ONE value the submit leaf reads
    /// before it leases a nonce, signs, or broadcasts.
    ///
    /// It is a read surface only: this executor has no probe entry to refuse,
    /// because an execution probe is [`crate::probe::ProbeExecutor`]'s, not the
    /// ordered lane's.
    #[must_use]
    pub const fn submission(&self) -> SubmissionPolicy {
        self.submission
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
    use std::collections::HashSet;
    use std::time::Duration;

    use tracing_subscriber::layer::SubscriberExt;

    use degenbot_execution::{CandidateId, PathInfo};
    use degenbot_substrate::state_lock::StateLock;
    use degenbot_substrate::BotState;

    use crate::assembly::{assemble_batch, AssembledBatch, AssemblyInputs, PathResolver};
    use crate::record::{
        AssemblyVerdict, FailureDetail, PathInfoView, SimReceipt, SimulateVerdict,
    };

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
            SubmissionPolicy::NonSubmitting,
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
            SubmissionPolicy::NonSubmitting,
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

    // ── The caller's pre-selection staleness gate ────────────────────────

    const LANE_POOL: alloy::primitives::Address =
        alloy::primitives::address!("a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1");
    const LANE_TOKEN_IN: alloy::primitives::Address =
        alloy::primitives::address!("2222222222222222222222222222222222222222");
    const LANE_TOKEN_OUT: alloy::primitives::Address =
        alloy::primitives::address!("3333333333333333333333333333333333333333");
    const LANE_TARGET: alloy::primitives::Address =
        alloy::primitives::address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    /// The fixture path every row resolves to: one V2 hop through `LANE_POOL`.
    fn lane_path() -> PathInfo {
        PathInfo::new(vec![degenbot_execution::HopInfo::V2(
            degenbot_execution::V2HopInfo {
                pool_address: LANE_POOL,
                token0_address: LANE_TOKEN_IN,
                token1_address: LANE_TOKEN_OUT,
                fee: 30,
                zfo: true,
            },
        )])
    }

    struct LaneResolver(PathInfo);

    impl PathResolver for LaneResolver {
        fn resolve(&self, _path_id: u64) -> Option<PathInfo> {
            Some(self.0.clone())
        }
    }

    /// One raw solver row: one hop, snapshotting `state_nonce`.
    fn lane_row(path_id: u64, state_nonce: u64, profit: u128) -> RawResult {
        RawResult {
            path_id,
            optimal_input: 1_000_000,
            profit,
            hop_outputs: vec![profit],
            consumed_inputs: vec![1_000_000],
            solve_block: BASE_BLOCK,
            state_nonces: vec![state_nonce],
        }
    }

    /// A `BotState` with the fixture pool registered as a V2 pair.
    fn lane_state() -> BotState {
        let mut core = BotState::new();
        core.register_v2_pool(&degenbot_pools::RegisterV2PoolParams {
            address: LANE_POOL,
            token0: LANE_TOKEN_IN,
            token1: LANE_TOKEN_OUT,
            reserve0: alloy::primitives::aliases::U112::from(1_000_000u64),
            reserve1: alloy::primitives::aliases::U112::from(2_000_000u64),
            fee_token0: degenbot_core::fee::FeeRatio::new(997, 1_000).unwrap(),
            fee_token1: degenbot_core::fee::FeeRatio::new(997, 1_000).unwrap(),
            factory: alloy::primitives::Address::ZERO,
            deployer: alloy::primitives::Address::ZERO,
            init_hash: alloy::primitives::B256::ZERO,
            update_block: 1,
            variant: degenbot_pools::DexVariant::UniswapV2,
            stable_swap: false,
        })
        .unwrap();
        core
    }

    /// The live `state_nonce` of the fixture pool — the value a fresh row
    /// snapshots.
    fn lane_nonce(core: &BotState) -> u64 {
        let pid = core
            .pool_id_by_address(&LANE_POOL)
            .expect("the fixture pool is registered");
        core.pool_state_nonce(pid)
    }

    /// Assemble one lane at the fixture block: the batch's own early admission
    /// projection, exactly as the sim leaf builds it.
    fn assemble_lane(
        rows: &[RawResult],
        p: &LanePolicy,
        max_candidates: usize,
        min_profit_margin_bps: u64,
        bot_state: Option<&StateLock<BotState>>,
    ) -> AssembledBatch {
        let mut suppression = p
            .suppression
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assemble_batch(&AssemblyInputs {
            rows,
            payloads: &[],
            resolver: &p.resolver,
            payload_served: &HashSet::new(),
            suppression: &mut suppression,
            divergence: &p.divergence,
            fot: &p.fot,
            bot_state,
            current_block: BASE_BLOCK,
            min_profit_margin_bps,
            max_candidates,
            target: LANE_TARGET,
            deployment_id: test_deployment_id(),
        })
        .expect("the fixture rows are well formed")
    }

    /// The three policy registries + resolver one assembled lane reads.
    struct LanePolicy {
        suppression: std::sync::Mutex<degenbot_submission::PathSuppression>,
        divergence: std::sync::Mutex<degenbot_arbitrage::PoolDivergence>,
        fot: std::sync::Mutex<degenbot_arbitrage::FeeOnTransferRegistry>,
        resolver: LaneResolver,
    }

    impl LanePolicy {
        fn fresh() -> Self {
            Self {
                suppression: std::sync::Mutex::new(degenbot_submission::PathSuppression::new()),
                divergence: std::sync::Mutex::new(degenbot_arbitrage::PoolDivergence::new()),
                fot: std::sync::Mutex::new(degenbot_arbitrage::FeeOnTransferRegistry::new()),
                resolver: LaneResolver(lane_path()),
            }
        }
    }

    /// The ids one assembled lane transports, in order.
    fn lane_ids(lane: &AssembledBatch) -> Vec<CandidateId> {
        lane.candidates
            .iter()
            .map(|admitted| admitted.candidate().candidate_id)
            .collect()
    }

    /// Cap 1 and `[stale, fresh]`: the caller's cap runs in INPUT order and
    /// BEFORE the projection's staleness gate, so the kept row is the STALE one.
    /// The gate then drops it and the fresh sibling never had a slot — nobody
    /// simulates, and the stale row earns no record (the drain-invisible drop).
    /// One slot more and the fresh sibling is what rides, which is what makes the
    /// cap's position observable here.
    #[test]
    fn a_stale_kept_row_starves_a_fresh_sibling_at_cap_one() {
        let p = LanePolicy::fresh();
        let core = lane_state();
        let fresh = lane_nonce(&core);
        let rows = [
            lane_row(1, fresh + 1, 10_000), // stale
            lane_row(2, fresh, 10_000),     // fresh
        ];
        let state = StateLock::new(core);

        let at_cap_one = assemble_lane(&rows, &p, 1, 0, Some(&state));
        assert!(
            at_cap_one.candidates.is_empty(),
            "the cap's one slot went to the stale row, which the gate then dropped"
        );
        assert!(
            at_cap_one.outcomes.is_empty(),
            "a stale row is drain-invisible — no record"
        );

        let with_a_slot_for_both = assemble_lane(&rows, &p, 2, 0, Some(&state));
        assert_eq!(
            lane_ids(&with_a_slot_for_both),
            vec![CandidateId::new(2, test_deployment_id())],
            "the fresh sibling rides when the cap leaves it a slot"
        );
    }

    /// The gate drops only the stale rows: a sibling's stage-1 record (here a
    /// thin-margin drop) survives it untouched, and the fresh survivor is what
    /// the fan-out receives.
    #[test]
    fn the_staleness_gate_keeps_a_siblings_stage_one_record() {
        let p = LanePolicy::fresh();
        let core = lane_state();
        let fresh = lane_nonce(&core);
        let rows = [
            lane_row(1, fresh, 1),          // thin at a 50 bps floor
            lane_row(2, fresh + 1, 10_000), // stale
            lane_row(3, fresh, 10_000),     // fresh
        ];
        let state = StateLock::new(core);
        let lane = assemble_lane(&rows, &p, 50, 50, Some(&state));

        assert_eq!(
            lane_ids(&lane),
            vec![CandidateId::new(3, test_deployment_id())],
            "only the stale row left the transport"
        );
        assert_eq!(
            lane.outcomes.len(),
            1,
            "the sibling's thin-margin record is not a stale record"
        );
        assert_eq!(
            lane.outcomes[0].candidate_id,
            CandidateId::new(1, test_deployment_id())
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
