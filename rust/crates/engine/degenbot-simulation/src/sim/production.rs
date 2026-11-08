//! The production simulation leaf: one adapter transaction → engine execution
//! and declared probe reads → typed [`ExecutionObservation`] → the adapter's
//! own Assess (ADR-098 D2/D3).
//!
//! The engine owns mechanics here: the declared pre/post probe reads, the
//! transaction execution, the measured gas, the return data, the captured
//! events, and the access list. The adapter owns Encode (one complete
//! [`TransactionRequest`]) and Assess (its interpretation of the observation).
//! Nothing in this module selects an adapter, and it knows no settlement token,
//! pool manager, command stream, or deployment version/release policy — the
//! same mechanics serve a foreign contract and the built-in executor
//! (ADR-098 D6).
//!
//! # Scope: one opportunity → one transaction
//!
//! [`simulate_opportunity`] encodes exactly one transaction, executes exactly
//! that one, and returns the adapter's verdict. There is no simulation loop
//! hook and no multi-transaction choreography: a contract that must batch does
//! so internally (ADR-098 D2).
//!
//! # Probe reads
//!
//! A declared [`ProbeSpec`] is one of two neutral executable forms, and the
//! leaf executes whichever was declared:
//!
//! - [`ProbeSpec::Call`] — an `eth_call`-shaped read: a call to the declared
//!   callee carrying the declared calldata **verbatim**, so a read is as
//!   complete as its declaration (a 4-byte selector for an argument-less view,
//!   or selector + ABI words for `balanceOf(account)` /
//!   `balanceOf(account, id)`). The return data must be **exactly one ABI word**
//!   (32 bytes); that word is the figure.
//! - [`ProbeSpec::NativeBalance`] — the declared account's own native (ETH)
//!   balance, read from the journaled state layer at that step. No transaction
//!   is built and nothing is spent on the account's behalf: the account *is*
//!   the thing observed, so the read costs no gas and cannot revert.
//!
//! Declared pre-reads replay in declared order before the opportunity's
//! transaction and post-reads after it, all inside ONE journaled sequence, so
//! both edges see the same accumulated state and the transaction sees the
//! pre-reads' state.
//!
//! A read the engine could not turn into a figure is absent (`None`), never a
//! fabricated zero: a reverted or halted call read, a call that returned no
//! data, and a call whose return data is not exactly one word (a short payload
//! is not padded into a value and a multi-word payload's fields are not chosen
//! between). The observation still carries one [`ProbeReading`] per declared spec,
//! in declared order. A native-account read has no such failure mode — the
//! state layer either answers (an account it holds no record of holds zero,
//! which is a fact, not an absence) or the sequence refuses with
//! [`ProductionSimError::Engine`].
//!
//! # Evidence honesty
//!
//! - `transaction` is the adapter's transaction, unmodified, including a
//!   nonzero native `value` — the engine executes exactly what Encode returned.
//! - `status` is [`ExecutionStatus::Success`] only for a completed call; a
//!   revert and a halt (out of gas, invalid opcode) both report
//!   [`ExecutionStatus::Revert`], which is the two-way status the observation
//!   models.
//! - `gas_used` is the engine's measured gas for the top-level call — never
//!   inflated by a submission safety margin and never the transaction's gas
//!   limit.
//! - `base_fee_next` is the base fee of the block env the session replays
//!   under, i.e. the fee the transaction actually ran under.
//! - `return_data` is the call's own output (a successful call that returned
//!   nothing reports `Some` with zero bytes; a revert reports its payload).
//! - `events` are the committed logs the transaction emitted, in execution
//!   order, with their index within the transaction.
//!
//! # Engine mechanics beside the assessment evidence
//!
//! [`ProductionSimulation::capture`] carries what the engine owns and the
//! adapter does not assess: the per-step settlements (so a caller can name the
//! read or the call that failed), the call trace, the captured (and
//! reverted-frame) swap events, the log count, and the access list. That is the
//! same set a strategy needs for diagnostics and submission — and none of it is
//! required to reach an economic verdict, so an adapter never sees it.
//!
//! # Failure classes
//!
//! The leaf never panics the bot. It reports a [`ProductionSimError`]: the
//! adapter declined to encode, the engine refused the sequence, the engine
//! returned no result for the executed step, or the adapter's Assess found the
//! observation lacked the evidence it needs. An economic decline is NOT an
//! error: it is `Ok` with `result.passed == false`, and a revert always prices
//! to zero gross through the shared [`ExecutionResult::price`] invariant.

use alloy::primitives::{Address, Bytes, TxKind, U256};
use degenbot_execution::{
    AssessError, CapturedEvent, ComposeError, ExecutionAdapter, ExecutionObservation,
    ExecutionResult, ExecutionStatus, PathInfo, ProbeReading, ProbeSpec, SolveResult,
    TransactionRequest,
};
use thiserror::Error;

use crate::sim::capture::CallTrace;
use crate::sim::evm2::session::{
    Evm2CaptureMode, Evm2SequenceCapture, Evm2SimError, Evm2SimRun, Evm2SimSession, Evm2SimStep,
};
use crate::sim::{CapturedSwap, RunVerdict};

/// The gas limit a declared contract-call probe read runs under.
///
/// A call read is a view call: it must fit comfortably inside one call's
/// budget, and a read that runs out of gas is reported as an absent reading
/// rather than consuming the sequence's remaining budget. A native-account read
/// builds no call and executes no code, so no gas limit applies to it.
pub const PROBE_CALL_GAS_LIMIT: u64 = 100_000;

/// The deployment- and block-scoped facts the leaf needs beyond the adapter.
///
/// These are the facts the composition root owns per simulated block: who sends
/// the transaction and how much gas it may burn. Fee pricing is the adapter's
/// (`FeePolicy` — the defaulted half of Assess), and the base fee is the
/// session's own block env, not a second copy here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProductionSimContext {
    /// The simulated transaction's sender — the account whose balance funds a
    /// nonzero `value` and whose identity a contract may key on.
    pub caller: Address,
    /// The top-level transaction's gas limit.
    pub gas_limit: u64,
}

/// One executed step's settled facts — the verdict, the return (or revert)
/// bytes, and the gas the step burned.
///
/// Steps are reported in the sequence's own order: the declared pre-reads, the
/// opportunity's transaction, then the declared post-reads. A caller that
/// declared `n` probes therefore finds its transaction at index `n`, and the
/// first step whose verdict is not [`RunVerdict::Success`] names the read or
/// call that failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepReading {
    /// How the step ended.
    pub verdict: RunVerdict,
    /// The step's return bytes, its revert payload, or empty for a halt and
    /// for a native-balance read.
    pub output: Bytes,
    /// The step's gas usage (zero for a native-balance read, which runs no
    /// code).
    pub gas_used: u64,
}

impl StepReading {
    /// Whether the step failed (reverted or halted).
    #[must_use]
    pub const fn failed(&self) -> bool {
        !matches!(self.verdict, RunVerdict::Success)
    }
}

/// The engine-owned mechanics a simulation produces beside the assessment
/// evidence: the per-step settlements, the call trace, the captured swap
/// events, and the access list.
///
/// None of this is assessment evidence — an adapter assesses the
/// [`ExecutionObservation`] — and none of it is adapter policy either. It is
/// engine mechanics a caller uses for diagnostics (which step failed, what the
/// trace saw), for submission (the access list), and for its own strategy
/// bookkeeping (the swap capture).
#[derive(Clone, Debug, Default)]
pub struct SimulationCapture {
    /// The access list the executed transaction implied.
    pub access_list: alloy::rpc::types::AccessList,
    /// The call trace, when the capture mode collected one (the production
    /// seam mode derives no trace; the diagnostic inspector mode does).
    pub trace: CallTrace,
    /// The committed swap events, in execution order.
    pub swaps: Vec<CapturedSwap>,
    /// The swap events emitted inside reverted frames (the divergence
    /// diagnostic).
    pub reverted_swaps: Vec<CapturedSwap>,
    /// Every `log` hook invocation the capture saw, including non-swap and
    /// reverted-frame logs.
    pub log_count: usize,
    /// One reading per executed step, in step order.
    pub steps: Vec<StepReading>,
}

impl SimulationCapture {
    /// The index of the first step that failed (reverted or halted), if any.
    ///
    /// This is the sequence-position attribution: with `n` declared probes the
    /// opportunity's own transaction sits at index `n`, so a caller can name the
    /// failing read or the call itself from the declaration it made.
    #[must_use]
    pub fn failing_step(&self) -> Option<usize> {
        self.steps.iter().position(StepReading::failed)
    }

    /// The reading for one step, if the sequence reached it.
    #[must_use]
    pub fn step(&self, index: usize) -> Option<&StepReading> {
        self.steps.get(index)
    }

    /// Drain the sequence's capture into this value's mechanics.
    fn from_sequence(capture: Evm2SequenceCapture, calls: &[Evm2SimRun]) -> Self {
        Self {
            access_list: capture.access_list,
            trace: capture.trace,
            swaps: capture.swaps,
            reverted_swaps: capture.reverted_swaps,
            log_count: capture.log_count,
            steps: calls
                .iter()
                .map(|call| StepReading {
                    verdict: call.verdict,
                    output: call.output.clone(),
                    gas_used: call.gas_used,
                })
                .collect(),
        }
    }
}

/// One production simulation's product: the evidence the engine captured, the
/// verdict the adapter reached over it, and the engine mechanics beside them.
#[derive(Clone, Debug)]
pub struct ProductionSimulation {
    /// The typed evidence the adapter assessed.
    pub observation: ExecutionObservation,
    /// The adapter's assessment of that observation.
    pub result: ExecutionResult,
    /// The engine mechanics: per-step settlements, trace, swaps, access list.
    pub capture: SimulationCapture,
}

/// The leaf's typed failures.
///
/// An economic decline is not one of these (it is `Ok` with
/// `result.passed == false`). Each variant names a part of the seam that could
/// not deliver, so a caller can categorize without parsing a message.
#[derive(Debug, Error)]
pub enum ProductionSimError {
    /// The adapter declined to encode this opportunity (Encode failed).
    #[error("adapter encode declined: {0}")]
    Encode(#[from] ComposeError),
    /// The engine refused the journaled sequence carrying the probes and the
    /// transaction (`Evm2SimError::step_index` names the refusing step).
    #[error("engine refused the simulation: {0}")]
    Engine(#[from] Evm2SimError),
    /// The engine returned no result for the executed step — the sequence's
    /// step bookkeeping and the observation disagree, so no evidence exists to
    /// assess.
    #[error("engine returned no result for the executed transaction")]
    MissingExecution,
    /// The adapter's Assess could not interpret the observation: the evidence
    /// it needs is absent or undecodable.
    ///
    /// The engine's evidence still rides the refusal, so a caller can categorise
    /// the failure against the same facts the assessment declined to rule on.
    #[error("adapter assessment failed: {source}")]
    Assess {
        /// The assessment's typed refusal.
        source: AssessError,
        /// The observation the assessment refused.
        observation: Box<ExecutionObservation>,
        /// The mechanics the executed sequence produced.
        capture: Box<SimulationCapture>,
    },
}

/// Encode one opportunity with `adapter`, execute exactly that transaction
/// alongside the adapter's declared probe reads, and return the adapter's
/// assessment of the captured evidence.
///
/// # Errors
///
/// [`ProductionSimError`] when the adapter declines to encode, the engine
/// refuses the sequence, the executed step has no result, or the adapter's
/// Assess needs evidence the observation lacks. A reverted transaction is a
/// successful run: its observation reports the revert and its assessment
/// declines.
pub fn simulate_opportunity(
    session: &mut Evm2SimSession<'_, '_>,
    adapter: &dyn ExecutionAdapter,
    path: &PathInfo,
    solved: &SolveResult,
    ctx: &ProductionSimContext,
) -> Result<ProductionSimulation, ProductionSimError> {
    let transaction = adapter.compose_view(path, solved)?;
    simulate_transaction(session, adapter, transaction, ctx)
}

/// Execute one already-encoded transaction alongside the adapter's declared
/// probe reads, and return the adapter's assessment of the captured evidence.
///
/// The split exists because Encode and execution are independent acts: an
/// encoder-only consumer can hand the leaf the transaction it built while the
/// same adapter still supplies Probe and Assess.
///
/// # Errors
///
/// [`ProductionSimError`] when the engine refuses the sequence, the executed
/// step has no result, or the adapter's Assess needs evidence the observation
/// lacks.
pub fn simulate_transaction(
    session: &mut Evm2SimSession<'_, '_>,
    adapter: &dyn ExecutionAdapter,
    transaction: TransactionRequest,
    ctx: &ProductionSimContext,
) -> Result<ProductionSimulation, ProductionSimError> {
    let base_fee_next = session.block_env().basefee.saturating_to::<u128>();
    // The transaction's fee cap clears the block's fee floor (a legacy envelope
    // pays the base fee; the priority fee is pricing policy, not execution).
    let gas_price = base_fee_next.max(1);
    let specs = adapter.probe_spec();

    let mut steps = Vec::with_capacity(2 * specs.len() + 1);
    let mut before = Vec::with_capacity(specs.len());
    for spec in specs {
        before.push(push_probe_step(&mut steps, spec, ctx, gas_price));
    }
    let transaction_index = steps.len();
    steps.push(Evm2SimStep::Inspect(envelope(&transaction, ctx, gas_price)));
    let mut after = Vec::with_capacity(specs.len());
    for spec in specs {
        after.push(push_probe_step(&mut steps, spec, ctx, gas_price));
    }

    let sequence = session.run_sequence(&steps, Evm2CaptureMode::Seam)?;
    let Some(executed) = sequence.calls.get(transaction_index) else {
        return Err(ProductionSimError::MissingExecution);
    };

    let observation = ExecutionObservation::new(
        transaction,
        status_of(executed.verdict),
        executed.gas_used,
        base_fee_next,
    )
    .with_return_data(executed.output.clone())
    .with_probe_reads(probe_readings(specs, &before, &after, &sequence.calls))
    .with_events(captured_events(&sequence.capture.logs));

    let capture = SimulationCapture::from_sequence(sequence.capture, &sequence.calls);
    let result = adapter
        .assess(&observation)
        .map_err(|source| ProductionSimError::Assess {
            source,
            observation: Box::new(observation.clone()),
            capture: Box::new(capture.clone()),
        })?;
    Ok(ProductionSimulation {
        observation,
        result,
        capture,
    })
}

/// Queue one declared probe read as a sequence step and return its step index.
///
/// A contract call becomes a bare (uncaptured) transaction carrying the
/// declared calldata; a native-account read becomes a read step at this
/// position, so the account is observed directly rather than through a
/// fabricated call.
fn push_probe_step(
    steps: &mut Vec<Evm2SimStep>,
    spec: &ProbeSpec,
    ctx: &ProductionSimContext,
    gas_price: u128,
) -> usize {
    steps.push(match spec {
        ProbeSpec::Call { to, data, .. } => {
            Evm2SimStep::Transact(probe_envelope(*to, data, ctx, gas_price))
        }
        ProbeSpec::NativeBalance { account, .. } => Evm2SimStep::ReadNativeBalance(*account),
    });
    steps.len() - 1
}

/// The observation's readings: one entry per declared spec, in declared order,
/// each edge resolved from its own sequence step.
fn probe_readings(
    specs: &[ProbeSpec],
    before: &[usize],
    after: &[usize],
    calls: &[Evm2SimRun],
) -> Vec<ProbeReading> {
    specs
        .iter()
        .zip(before)
        .zip(after)
        .map(|((spec, before), after)| {
            ProbeReading::new(
                spec.clone(),
                read_figure(spec, calls, *before),
                read_figure(spec, calls, *after),
            )
        })
        .collect()
}

/// The figure one probe step reported for its declaration, or `None` when the
/// read did not yield one.
///
/// A call read reports the one ABI word a completed call returned; a revert, a
/// halt, and a successful call whose return data is empty, short, or multi-word
/// are all absent.
/// A native-account read reports the balance the step observed (the step always
/// completes; a state-layer refusal fails the whole sequence instead).
fn read_figure(spec: &ProbeSpec, calls: &[Evm2SimRun], step: usize) -> Option<U256> {
    let call = calls.get(step)?;
    match spec {
        ProbeSpec::NativeBalance { .. } => call.native_balance,
        ProbeSpec::Call { .. } => {
            if call.verdict != RunVerdict::Success {
                return None;
            }
            decode_figure(&call.output)
        }
    }
}

/// Decode a read-call's return bytes as a `uint256`: EXACTLY one ABI word (32
/// bytes) decodes to a figure, and any other length yields none.
///
/// A short payload is not left-padded into a value and a multi-word payload's
/// words are not chosen between: either would report a figure the call never
/// returned, and a two-word response's words are different fields. A read whose
/// callee answers in another shape is therefore an absent reading, not evidence.
fn decode_figure(data: &[u8]) -> Option<U256> {
    let Ok(word) = <[u8; 32]>::try_from(data) else {
        return None;
    };
    Some(U256::from_be_bytes(word))
}

/// The events the transaction committed, in execution order.
fn captured_events(logs: &[alloy::primitives::Log]) -> Vec<CapturedEvent> {
    logs.iter()
        .enumerate()
        .map(|(index, log)| CapturedEvent {
            address: log.address,
            topics: log.data.topics().to_vec(),
            data: log.data.data.clone(),
            log_index: u32::try_from(index).unwrap_or(u32::MAX),
        })
        .collect()
}

/// The observation's two-way status: only a completed call succeeded; a revert
/// and a halt both moved no value.
const fn status_of(verdict: RunVerdict) -> ExecutionStatus {
    match verdict {
        RunVerdict::Success => ExecutionStatus::Success,
        RunVerdict::Reverted | RunVerdict::Halted => ExecutionStatus::Revert,
    }
}

/// Build the envelope for the opportunity's transaction: the adapter's target,
/// calldata, and native value, sent by the context's caller.
fn envelope(
    transaction: &TransactionRequest,
    ctx: &ProductionSimContext,
    gas_price: u128,
) -> ::evm2::ethereum::RecoveredTxEnvelope {
    build_envelope(
        transaction.to,
        &transaction.data,
        transaction.value,
        ctx.gas_limit,
        ctx.caller,
        gas_price,
    )
}

/// Build a probe read's envelope: a call to the declared callee carrying the
/// declared calldata verbatim, which reads and never funds.
fn probe_envelope(
    to: Address,
    data: &Bytes,
    ctx: &ProductionSimContext,
    gas_price: u128,
) -> ::evm2::ethereum::RecoveredTxEnvelope {
    build_envelope(
        to,
        data,
        U256::ZERO,
        PROBE_CALL_GAS_LIMIT,
        ctx.caller,
        gas_price,
    )
}

fn build_envelope(
    to: Address,
    data: &Bytes,
    value: U256,
    gas_limit: u64,
    sender: Address,
    gas_price: u128,
) -> ::evm2::ethereum::RecoveredTxEnvelope {
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(to),
            input: data.clone(),
            value,
            gas_limit,
            gas_price,
            ..Default::default()
        }),
        sender,
    )
}
