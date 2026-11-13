//! The core probe executor: the ordinary assembly, production-encoding, and
//! EVM2 stage pass (stages 1-2, `SimStage`) behind a dedicated
//! NON-SUBMITTING, CORRELATED one-call interface.
//!
//! # Why this type exists
//!
//! An explicit execution probe asks ONE question about ONE caller-supplied
//! amount vector: "what does the production encoder + the EVM2 simulation say
//! about THIS vector at THIS block?" The ordered lane
//! ([`crate::executor::BatchExecutor`]) answers a different question — it runs
//! session batches in arrival order and publishes them on one shared drain —
//! so serving a probe through it forces the caller to correlate: it enqueues a
//! well-formed-looking batch and then finds its own record among whatever the
//! lane happened to drain next. Two concurrent callers can then take one
//! another's answers, and a cancelled caller leaves an outcome queued for its
//! successor (the temporary session-side lock and reap exist only to paper over
//! that).
//!
//! [`ProbeExecutor`] removes the correlation problem instead of managing it:
//!
//! * **Correlated by construction.** [`ProbeExecutor::probe`] spawns the stage
//!   pass for one call and hands the result through a `oneshot` channel whose
//!   receiver lives in that call's own future. There is no drain to consume, no
//!   shared order to reconstruct, and no way for a call to observe a result it
//!   did not produce. A caller that gives up drops its receiver AND the stage
//!   task it spawned, because the call owns that task's lifecycle through an
//!   abort-on-drop guard ([`StageTask`]): the abandoned result is discarded
//!   where it is produced — never forwarded to a later call — and no task is
//!   deliberately detached. (Aborting is not preemption; see
//!   [`ProbeExecutor::probe`]'s cancel-safety notes.)
//! * **Non-submitting by construction.** The type holds no [`SubmitLeaf`], no
//!   signer, no nonce lane, no relay fan-out, no receipt probe, and no
//!   dispatcher — those are [`crate::executor::ExecutorConfig`]'s submit half,
//!   and this construction boundary ([`ProbeConfig`] over
//!   [`crate::stage::SimRuntime`]) cannot name them at all. Non-submission is
//!   therefore a missing field, not a dry-run flag, a policy check, or a
//!   refused enqueue: there is no submit stage to reach.
//! * **One stage implementation.** The pass IS the production one: the same
//!   assembly (per-deployment lanes, the input-order cap, the early policy
//!   projection, the pre-selection staleness gate), the same production encoder
//!   (the fan-out's `join_sim_result` over the deployment's own adapter), and
//!   the same EVM2 simulation, driven through the SAME
//!   `SimStage` value type the production lane drives. No second
//!   pricing or encoder path exists for probes.
//!
//! # What the public adapter consumes
//!
//! The Python/operator surface consumes this seam directly (the `PyO3` shell's
//! `ProbeExecutor` / `ExecutionProbeRequest` / `ProbeReport`):
//!
//! 1. build ONE [`ProbeExecutor`] per capital posture with
//!    [`ProbeConfig::from_verdict`] over the session's sim-side handles
//!    (`SimRuntime` — no signer, no nonce lane, no relay URLs are needed, so the
//!    retired "build a submission-capable executor and refuse at the entry"
//!    dance has no replacement);
//! 2. hand it a [`BatchWork`] carrying the caller's one raw row (path id, fixed
//!    input, its hop outputs / consumed inputs, the block facts) and no payload
//!    rows;
//! 3. await [`ProbeExecutor::probe`] and project [`ProbeOutcome`] onto the typed
//!    report: `records` carry the assembly + simulate verdicts (profitable /
//!    gas-unprofitable / typed revert failure / exception) and the receipt, and
//!    `encoded` carries the planning evidence (calldata, native value, target).
//!
//! Because the call owns its outcome, the session lock and the abandoned-probe
//! reaping are unnecessary rather than merely correct: concurrent probes are
//! not serialized, nothing is shielded against cancellation for correctness,
//! and the ordered lane has no probe entry to refuse (`enqueue_probe`,
//! `ProbeRefusal`, and the shared-drain drain are gone).

use std::sync::Arc;

use tokio::sync::oneshot;

use degenbot_arbitrage::DeploymentRegistryError;
use degenbot_simulation::sim::evm2::bal_capability::BalCapabilityError;
use degenbot_submission::{SimLeaf, SubmitCandidate};

use crate::executor::BatchWork;
use crate::policy::ExecutorPolicy;
use crate::record::BatchOutcome;
use crate::stage::{SimRuntime, SimStage, SimStageOutput};

/// The probe executor's construction boundary: the verdict-named policy values
/// plus the sim-side handles, converted ONCE into the shared stage pass.
///
/// The struct holds no submit-side handle and no per-call state, so one value
/// can serve every probe of a session that shares its capital posture (the
/// capital posture is one of the `SimRuntime` values the stage build converts,
/// which is why an executor is per-posture).
pub struct ProbeConfig {
    /// The shared stage pass this probe runs (see `SimStage`).
    stage: SimStage,
}

impl ProbeConfig {
    /// Build the probe configuration from the installed verdict and the
    /// sim-side runtime handles.
    ///
    /// Every policy value converts through [`ExecutorPolicy::from`] — the same
    /// conversion the production executor uses, owning every default and clamp
    /// — and the stage build is `SimStage::build`, the same one the production
    /// lane's config uses. A probe therefore runs the session's exact
    /// assembly/encode/sim answers.
    ///
    /// # Errors
    ///
    /// [`DeploymentRegistryError`] when the composed deployment registry is
    /// empty or holds two deployments with one identity, or when a deployment's
    /// owner is not the account this probe simulates from. A probe has no key
    /// holder to name that account, so the authority is its binding's owner
    /// ([`degenbot_arbitrage::CmdExecutorBinding::owner`]) — the account the
    /// simulated transaction is sent from, which is the same account a session
    /// signer holds when a session has one. A deployment naming any other owner
    /// would be simulated from an account this construction cannot vouch for.
    ///
    /// # Panics
    ///
    /// When the verdict declares `simulation.inject_executor_code` but the
    /// runtime names no `injected_address`, or names an address other than the
    /// executor the built-in adapter encodes to (see `SimStage::build`).
    pub fn from_verdict(
        verdict: &degenbot_config::BotConfig,
        runtime: SimRuntime,
    ) -> Result<Self, DeploymentRegistryError> {
        let policy = ExecutorPolicy::from(verdict);
        let authority = runtime.binding.owner();
        Ok(Self {
            stage: SimStage::build(&policy, runtime, authority)?,
        })
    }
}

/// One probe call's OWN product: the records its rows produced and the encoder
/// rows the same pass composed for them.
///
/// Nothing in this value is shared, ordered, or queued: it is the return of one
/// call and only that call can observe it.
#[derive(Debug, Clone, Default)]
pub struct ProbeOutcome {
    /// The per-candidate records of the call's own rows — the assembly verdict
    /// (stage 1) and the simulate verdict (stage 2, with the receipt or the
    /// typed failure detail).
    ///
    /// `submit` is ALWAYS `None`: this executor has no submit stage to fill it,
    /// and the slot is kept only so the record vocabulary stays the one the
    /// renderers already read.
    pub records: Vec<BatchOutcome>,
    /// The production encoder's joined rows for the very candidates above
    /// (`execute_calldata`, native `value`, `execution_target`): the probe's
    /// planning evidence, produced by the ordinary encoder at no extra cost.
    ///
    /// **Nothing submits these.** The probe type carries no submit lane, signer,
    /// nonce lease, relay fan-out, or broadcast arm; the rows are here so a
    /// report can show what WOULD have been sent, not so anything can send it.
    pub encoded: Vec<SubmitCandidate>,
}

impl ProbeOutcome {
    /// The stage product as a probe outcome.
    fn from_stage(product: SimStageOutput) -> Self {
        Self {
            records: product.outcomes,
            encoded: product.submit_candidates,
        }
    }
}

/// Why a probe call produced no outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// The stages refused the batch — the same detail the production lane's
    /// loud-abort carries (a payload row naming an unregistered deployment, the
    /// fan-out's identity refusal, or a registry divergence mid-batch).
    Stage(String),
    /// The call ended with no result: the stage task panicked, or the runtime
    /// tore it down mid-probe. Nothing was queued and no other call can observe
    /// this call's work. A call that is itself DROPPED does not report this — it
    /// aborts its stage instead (see [`ProbeExecutor::probe`]).
    Abandoned,
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stage(detail) => write!(f, "the probe stage pass failed: {detail}"),
            Self::Abandoned => f.write_str(
                "the probe stage task ended without an outcome (it panicked, or the runtime tore down)",
            ),
        }
    }
}

impl std::error::Error for ProbeError {}

/// The owned lifecycle of ONE call's spawned stage task.
///
/// The spawned task must not outlive the call that commissioned it: a shared
/// queue would hide an abandoned batch, but a DETACHED task hides an abandoned
/// stage — the unreachable outcome is not the leak, the wedged EVM/RPC work
/// behind it is. So the call owns the handle and aborts it on drop: the runtime
/// then drops the stage future at its next cancellation point instead of the
/// task running on with nothing able to observe it.
///
/// Aborting is not preemption. `abort` takes effect where the stage next yields:
/// a stage body inside a synchronous, non-yielding span (the EVM run itself)
/// has already left its last await point, finishes that span, and has its future
/// dropped when the poll returns. The guarantee is that no task is deliberately
/// detached and its resources are released there — never that arbitrary
/// synchronous work can be force-stopped mid-call.
struct StageTask {
    /// The task's handle, or `None` once this value has been reaped (so the
    /// drop that follows a reap has nothing left to abort).
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl StageTask {
    /// Own the handle of the task just spawned.
    const fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    /// Await the task's terminal state after it failed to publish a result: the
    /// task already ended (it panicked, or the runtime tore it down), so this
    /// joins it and nothing is left unobserved.
    async fn reap(mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for StageTask {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // No result was observed, so the call was dropped before it settled.
            // Cancel the stage rather than detach it; `abort` on an already
            // finished task is a no-op, so the settling paths take this branch
            // harmlessly.
            handle.abort();
        }
    }
}

/// The core probe executor: one call in, that call's own outcome out.
///
/// Construct it with [`ProbeExecutor::new`] over a [`ProbeConfig`]. It owns the
/// sim stage and nothing else — no queue, no lane, no record stream, no
/// lifecycle to shut down, and (by construction) no submit side. Dropping the
/// value drops the stage; an in-flight call owns its own channel and its own
/// task, and aborts that task if the call itself is dropped (see
/// [`StageTask`]).
pub struct ProbeExecutor {
    /// The stage pass every call runs. A trait object so a test can drive the
    /// executor's correlation contract over a scripted stage while the
    /// production constructor hands it the real `SimStage`.
    stage: Arc<dyn SimLeaf<BatchWork, SimStageOutput>>,
}

impl ProbeExecutor {
    /// Build the probe executor over the ordinary stages.
    ///
    /// `async` for the same reason [`crate::executor::BatchExecutor::new`] is:
    /// the `simulation.delta_dispatch` capability probe is real provider I/O,
    /// and a probe runs the SAME simulation stage — so construction proves and
    /// arms the session's canonical-head BAL pipeline through the shared
    /// `arm_session_bal` rather than standing up a second one. A
    /// cache carrying no opt-in (the shipped posture) probes nothing.
    ///
    /// # Errors
    ///
    /// [`BalCapabilityError`] when the opt-in is ON and the endpoint cannot
    /// prove the capability (see [`crate::executor::BatchExecutor::new`] for the
    /// lineage/acquisition split).
    pub async fn new(config: ProbeConfig) -> Result<Self, BalCapabilityError> {
        crate::stage::arm_session_bal(config.stage.warm_cache.as_ref(), &config.stage.provider)
            .await?;
        Ok(Self::from_stage(Arc::new(config.stage)))
    }

    /// Wire an arbitrary stage pass into the executor: the same seam
    /// [`crate::executor::BatchExecutor`] exposes through `from_leaves`, so the
    /// correlation contract (identity, reverse-order completion, drop) is
    /// exercised at THIS boundary while the production constructor hands it the
    /// real stage.
    pub(crate) fn from_stage(stage: Arc<dyn SimLeaf<BatchWork, SimStageOutput>>) -> Self {
        Self { stage }
    }

    /// Run ONE probe call and return ITS OWN outcome.
    ///
    /// The stage pass for this call runs on the ambient Tokio runtime (the same
    /// runtime the ordered lane's sim tasks spawn on) and publishes through a
    /// `oneshot` whose receiver is owned by this future: the result can reach
    /// this call and no other. Concurrent calls are independent — nothing is
    /// ordered, shared, or drained — so two calls cannot take one another's
    /// outcomes however they interleave.
    ///
    /// A `BatchWork` with no rows (or a stage that reports nothing dispatchable)
    /// yields an empty [`ProbeOutcome`]; the caller's own amount vector is
    /// [`BatchWork::rows`].
    ///
    /// # Errors
    ///
    /// [`ProbeError::Stage`] when the stages refuse the batch (the detail is the
    /// production lane's), or [`ProbeError::Abandoned`] when the stage task
    /// ended without producing a result.
    ///
    /// # Cancel safety
    ///
    /// Dropping this future cancels the CALL, never a shared queue. The stage
    /// task belongs to this call through an abort-on-drop guard
    /// ([`StageTask`]): the dropped call aborts the task, and the runtime drops
    /// the stage future at its next cancellation point. Nothing is left queued
    /// for a successor to steal, no lock or reaping is needed to keep that true,
    /// and no task is deliberately detached — an abandoned stage cannot keep
    /// running with its EVM, RPC, and runtime resources unreachable.
    ///
    /// **Abort is not preemption.** `abort` takes effect where the stage next
    /// yields. A stage body inside a synchronous, non-yielding span (the EVM run
    /// itself) has already left its last await point: it finishes that span and
    /// has its future dropped when the poll returns. So an in-flight synchronous
    /// simulation cannot be force-stopped mid-call — what this seam guarantees is
    /// that the work is cancelled and released rather than detached.
    ///
    /// # Panics
    ///
    /// When polled outside a Tokio runtime (`tokio::spawn` has no reactor to
    /// place the stage task on), exactly like the ordered lane's enqueue.
    pub async fn probe(&self, work: BatchWork) -> Result<ProbeOutcome, ProbeError> {
        let stage = Arc::clone(&self.stage);
        // The correlation: this receiver is the ONLY handle that can observe
        // this call's result. It is created per call, so no ordering, no key,
        // and no drain is involved.
        let (tx, rx) = oneshot::channel();
        // The lifecycle: the call OWNS the task it spawns, so dropping this
        // future aborts the stage instead of detaching it (see `StageTask`).
        let task = StageTask::new(tokio::spawn(async move {
            let result = stage.simulate(&work).await;
            // A receiver dropped by a cancelled caller makes this send fail:
            // the outcome is discarded HERE and can never reach another call.
            let _ = tx.send(result);
        }));
        match rx.await {
            Ok(Ok(Some(product))) => Ok(ProbeOutcome::from_stage(product)),
            Ok(Ok(None)) => Ok(ProbeOutcome::default()),
            Ok(Err(detail)) => Err(ProbeError::Stage(detail)),
            Err(_) => {
                // The task ended without a result: it panicked, or the runtime
                // tore it down. Reap its terminal state so the failure belongs
                // to this call rather than an unobserved task.
                task.reap().await;
                Err(ProbeError::Abandoned)
            }
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests drive scripted fixtures and assert on the outcomes"
)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::time::Duration;

    use degenbot_execution::CandidateId;
    use degenbot_submission::SimFuture;

    use crate::executor::ExecutorConfig;
    use crate::record::{AssemblyVerdict, PathInfoView, SimReceipt, SimulateVerdict};

    /// A scripted stage: it sleeps for the delay its work's block names, records
    /// that it finished, and answers with a record whose identity IS the block —
    /// so any outcome's owner can be traced back to the call that produced it.
    struct ScriptedStage {
        delays: Vec<(u64, u64)>,
        finished: Arc<Mutex<Vec<u64>>>,
    }

    impl ScriptedStage {
        fn delay_ms(&self, block: u64) -> u64 {
            self.delays
                .iter()
                .find(|(delayed, _)| *delayed == block)
                .map_or(0, |(_, delay)| *delay)
        }
    }

    impl SimLeaf<BatchWork, SimStageOutput> for ScriptedStage {
        fn simulate<'a>(&'a self, work: &'a BatchWork) -> SimFuture<'a, SimStageOutput> {
            let block = work.current_block;
            let delay = self.delay_ms(block);
            let finished = Arc::clone(&self.finished);
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                finished.lock().unwrap().push(block);
                Ok(Some(SimStageOutput {
                    outcomes: vec![record(block)],
                    submit_candidates: vec![encoded(block)],
                }))
            })
        }
    }

    /// Holds a drop signal across a stage's await point and fires it when the
    /// future holding it is DROPPED. For a future that can never resolve, that is
    /// the only way the stage ends — so a signal here is proof of the abort, not
    /// of the work finishing.
    struct DropProof(Arc<Mutex<Vec<u64>>>, u64);

    impl Drop for DropProof {
        fn drop(&mut self) {
            self.0.lock().unwrap().push(self.1);
        }
    }

    /// A stage that answers immediately except for the blocks it wedges: those
    /// park forever while holding a [`DropProof`], so "the abandoned stage
    /// finished" can never be an explanation for anything the drop test
    /// observes.
    struct WedgeStage {
        wedged: Vec<u64>,
        entered: Arc<Mutex<Vec<u64>>>,
        dropped: Arc<Mutex<Vec<u64>>>,
    }

    impl SimLeaf<BatchWork, SimStageOutput> for WedgeStage {
        fn simulate<'a>(&'a self, work: &'a BatchWork) -> SimFuture<'a, SimStageOutput> {
            let block = work.current_block;
            let wedge = self.wedged.contains(&block);
            let entered = Arc::clone(&self.entered);
            let dropped = Arc::clone(&self.dropped);
            Box::pin(async move {
                entered.lock().unwrap().push(block);
                if wedge {
                    let _proof = DropProof(dropped, block);
                    std::future::pending::<()>().await;
                }
                Ok(Some(SimStageOutput {
                    outcomes: vec![record(block)],
                    submit_candidates: vec![encoded(block)],
                }))
            })
        }
    }

    /// Poll `condition` until it holds, failing the test on a deadline: the
    /// runtime performs an abort on its own schedule, so an abort's effects land
    /// on a later poll rather than inside `Drop::drop`.
    async fn await_until(mut condition: impl FnMut() -> bool, what: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("timed out waiting for {what}");
    }

    fn record(block: u64) -> BatchOutcome {
        BatchOutcome {
            candidate_id: CandidateId::new(block, ExecutorConfig::BUILTIN_DEPLOYMENT_ID),
            block,
            assembly: AssemblyVerdict::Assembled,
            simulate: Some(SimulateVerdict::Profitable(SimReceipt {
                gross_profit: alloy::primitives::U256::from(block),
                net_profit: alloy::primitives::U256::from(block),
                gas_used: 21_000,
                priority_fee: 0,
            })),
            submit: None,
            path_info: PathInfoView::empty(),
        }
    }

    fn encoded(block: u64) -> SubmitCandidate {
        SubmitCandidate {
            candidate_id: CandidateId::new(block, ExecutorConfig::BUILTIN_DEPLOYMENT_ID),
            gross_profit: alloy::primitives::U256::from(block),
            net_profit: alloy::primitives::U256::from(block),
            gas_used: 21_000,
            priority_fee: 0,
            base_fee_next: 1_000_000_000,
            execute_calldata: alloy::primitives::Bytes::from(block.to_be_bytes().to_vec()),
            value: alloy::primitives::U256::ZERO,
            execution_target: alloy::primitives::Address::ZERO,
            access_list: None,
            path_pools: HashSet::new(),
        }
    }

    fn work(block: u64) -> BatchWork {
        BatchWork {
            rows: Vec::new(),
            payloads: Vec::new(),
            current_block: block,
            base_fee_next: 1_000_000_000,
            block_timestamp: 0,
            block_priority_fees: None,
        }
    }

    fn executor(delays: Vec<(u64, u64)>, finished: &Arc<Mutex<Vec<u64>>>) -> ProbeExecutor {
        ProbeExecutor::from_stage(Arc::new(ScriptedStage {
            delays,
            finished: Arc::clone(finished),
        }))
    }

    /// Two concurrent calls finish in REVERSE order, and each still reads its
    /// OWN outcome: the correlation is per call, not per arrival, so the call
    /// that settles first cannot take the other's record.
    #[tokio::test]
    async fn concurrent_probes_correlate_across_reverse_completion_order() {
        let finished = Arc::new(Mutex::new(Vec::new()));
        let executor = executor(vec![(100, 60), (101, 10)], &finished);
        let (first, second) = tokio::join!(executor.probe(work(100)), executor.probe(work(101)));
        let first = first.expect("the first call has its own outcome");
        let second = second.expect("the second call has its own outcome");
        assert_eq!(
            *finished.lock().unwrap(),
            vec![101, 100],
            "the later-started call completed FIRST — the reverse of arrival order"
        );
        assert_eq!(first.records.len(), 1);
        assert_eq!(first.records[0].block, 100, "the first call's own row");
        assert_eq!(first.records[0].candidate_id.path_id(), 100);
        assert_eq!(second.records[0].block, 101, "the second call's own row");
        assert_eq!(second.records[0].candidate_id.path_id(), 101);
        assert_eq!(first.encoded[0].candidate_id.path_id(), 100);
        assert_eq!(second.encoded[0].candidate_id.path_id(), 101);
        for record in first.records.iter().chain(second.records.iter()) {
            assert!(
                record.submit.is_none(),
                "the probe executor has no submit stage to fill the slot"
            );
        }
    }

    /// Dropping a call ABORTS the stage it spawned instead of detaching it, and
    /// the call that follows still reads exactly its own outcome.
    ///
    /// The wedged stage's future never resolves, so the drop signal is the whole
    /// proof: the runtime only fires it by dropping that future, which is what
    /// the call's abort-on-drop guard causes. Nothing here waits for the
    /// abandoned work to finish — it has no finish to wait for.
    #[tokio::test]
    async fn a_dropped_probe_aborts_its_stage_future_instead_of_detaching_it() {
        let entered = Arc::new(Mutex::new(Vec::new()));
        let dropped = Arc::new(Mutex::new(Vec::new()));
        let executor = ProbeExecutor::from_stage(Arc::new(WedgeStage {
            wedged: vec![100],
            entered: Arc::clone(&entered),
            dropped: Arc::clone(&dropped),
        }));

        // Start the wedged call, then drop its future mid-flight (`select!`
        // drops the losing future) — the cancellation a caller's timeout
        // performs.
        tokio::select! {
            outcome = executor.probe(work(100)) => {
                panic!("the wedged stage cannot settle: {outcome:?}")
            }
            () = tokio::time::sleep(Duration::from_millis(5)) => {}
        }
        assert_eq!(
            *entered.lock().unwrap(),
            vec![100],
            "the stage really started, so the abort below cancels live work"
        );

        // The call dropped; the task it owns is aborted, and the runtime drops
        // the stage future at that cancellation point.
        await_until(
            || !dropped.lock().unwrap().is_empty(),
            "the dropped call's stage future to be dropped",
        )
        .await;
        assert_eq!(
            *dropped.lock().unwrap(),
            vec![100],
            "the wedged stage future was DROPPED — its task is not detached"
        );

        // The next call owns ITS outcome, never the abandoned one: the aborted
        // call left nothing queued for a successor.
        let next = executor.probe(work(200)).await.expect("its own outcome");
        assert_eq!(next.records[0].block, 200);
        assert_eq!(next.records[0].candidate_id.path_id(), 200);
        assert!(next.records[0].submit.is_none());
        assert_eq!(
            *entered.lock().unwrap(),
            vec![100, 200],
            "only the wedged call and its successor entered the stage"
        );
        assert_eq!(
            *dropped.lock().unwrap(),
            vec![100],
            "the successor's stage ran to its OWN result, so only the abandoned one was dropped"
        );
    }

    /// A stage refusal is THIS call's typed error — not a lane-wide terminal
    /// failure, and nothing the next call has to clear.
    #[tokio::test]
    async fn a_stage_refusal_is_the_callers_own_error() {
        struct RefusingStage;
        impl SimLeaf<BatchWork, SimStageOutput> for RefusingStage {
            fn simulate<'a>(&'a self, _work: &'a BatchWork) -> SimFuture<'a, SimStageOutput> {
                Box::pin(async move { Err("an unregistered payload deployment".to_string()) })
            }
        }
        let executor = ProbeExecutor::from_stage(Arc::new(RefusingStage));
        assert_eq!(
            executor.probe(work(100)).await.expect_err("the refusal"),
            ProbeError::Stage("an unregistered payload deployment".to_string())
        );
        // Per call, so the executor answers the very next probe too.
        assert!(executor.probe(work(101)).await.is_err());
    }

    /// A stage that dies without a result is reported as `Abandoned` instead of
    /// hanging the caller (the spawned task's panic is the stage's, not the
    /// test's).
    #[tokio::test]
    async fn a_stage_that_dies_reports_abandoned() {
        struct PanickingStage;
        impl SimLeaf<BatchWork, SimStageOutput> for PanickingStage {
            fn simulate<'a>(&'a self, _work: &'a BatchWork) -> SimFuture<'a, SimStageOutput> {
                Box::pin(async move { panic!("the stage blew up") })
            }
        }
        let executor = ProbeExecutor::from_stage(Arc::new(PanickingStage));
        // Silence the default hook: this unwind is the assertion's subject, not
        // a test failure in the log.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = executor.probe(work(100)).await;
        std::panic::set_hook(hook);
        assert_eq!(
            outcome.expect_err("no outcome survives the stage's death"),
            ProbeError::Abandoned
        );
    }

    /// A stage that reports nothing dispatchable yields an EMPTY outcome rather
    /// than an error or a borrowed record.
    #[tokio::test]
    async fn a_stage_reporting_nothing_yields_an_empty_outcome() {
        struct SilentStage;
        impl SimLeaf<BatchWork, SimStageOutput> for SilentStage {
            fn simulate<'a>(&'a self, _work: &'a BatchWork) -> SimFuture<'a, SimStageOutput> {
                Box::pin(async move { Ok(None) })
            }
        }
        let executor = ProbeExecutor::from_stage(Arc::new(SilentStage));
        let outcome = executor.probe(work(100)).await.expect("an empty outcome");
        assert!(outcome.records.is_empty());
        assert!(outcome.encoded.is_empty());
    }
}
