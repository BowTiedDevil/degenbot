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
//! A declared [`ProbeSpec`] is an `eth_call`-shaped read: a transaction to
//! `spec.address` whose entire calldata is the 4-byte `spec.selector`. Declared
//! pre-reads replay in declared order before the opportunity's transaction and
//! post-reads after it, all inside ONE journaled sequence, so both edges see
//! the same accumulated state and the transaction sees the pre-reads' state.
//!
//! A read the engine could not turn into a figure is absent (`None`), never a
//! fabricated zero: a reverted or halted read, a read whose call returned no
//! data, and a pseudo-read declared with no callee (`address: None` — nothing
//! to call). The observation still carries one [`ProbeReading`] per declared
//! spec, in declared order.
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

use crate::sim::evm2::session::{
    Evm2CaptureMode, Evm2SimError, Evm2SimRun, Evm2SimSession, Evm2SimStep,
};
use crate::sim::RunVerdict;

/// The gas limit a declared probe read runs under.
///
/// A read is a view call: it must fit comfortably inside one call's budget, and
/// a read that runs out of gas is reported as an absent reading rather than
/// consuming the sequence's remaining budget.
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

/// One production simulation's product: the evidence the engine captured and
/// the verdict the adapter reached over it.
#[derive(Clone, Debug)]
pub struct ProductionSimulation {
    /// The typed evidence the adapter assessed.
    pub observation: ExecutionObservation,
    /// The adapter's assessment of that observation.
    pub result: ExecutionResult,
    /// The access list the executed transaction implied — engine-owned
    /// submission/diagnostic mechanics, never assessment evidence.
    pub access_list: alloy::rpc::types::AccessList,
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
    #[error("adapter assessment failed: {0}")]
    Assess(#[from] AssessError),
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

    let result = adapter.assess(&observation)?;
    Ok(ProductionSimulation {
        observation,
        result,
        access_list: sequence.capture.access_list,
    })
}

/// Queue one declared probe read as a bare (uncaptured) sequence step and
/// return its step index, or `None` when the spec names no callee.
fn push_probe_step(
    steps: &mut Vec<Evm2SimStep>,
    spec: &ProbeSpec,
    ctx: &ProductionSimContext,
    gas_price: u128,
) -> Option<usize> {
    let address = spec.address?;
    steps.push(Evm2SimStep::Transact(probe_envelope(
        address,
        spec.selector,
        ctx,
        gas_price,
    )));
    Some(steps.len() - 1)
}

/// The observation's readings: one entry per declared spec, in declared order,
/// each edge resolved from its own sequence step.
fn probe_readings(
    specs: &[ProbeSpec],
    before: &[Option<usize>],
    after: &[Option<usize>],
    calls: &[Evm2SimRun],
) -> Vec<ProbeReading> {
    specs
        .iter()
        .zip(before)
        .zip(after)
        .map(|((spec, before), after)| {
            ProbeReading::new(
                spec.clone(),
                read_figure(calls, *before),
                read_figure(calls, *after),
            )
        })
        .collect()
}

/// The figure one probe step reported, or `None` when the read did not yield
/// one (no callee, a revert/halt, or no return data).
fn read_figure(calls: &[Evm2SimRun], step: Option<usize>) -> Option<U256> {
    let call = calls.get(step?)?;
    if call.verdict != RunVerdict::Success {
        return None;
    }
    decode_figure(&call.output)
}

/// Decode a read-call's return bytes as a `uint256`: the ABI right-aligns the
/// value in the final 32 bytes, a shorter payload is left-padded, and an empty
/// payload carries no figure at all.
fn decode_figure(data: &[u8]) -> Option<U256> {
    match data.len() {
        0 => None,
        n if n >= 32 => Some(U256::from_be_slice(&data[n - 32..])),
        n => {
            let mut word = [0u8; 32];
            word[32 - n..].copy_from_slice(data);
            Some(U256::from_be_slice(&word))
        }
    }
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

/// Build a probe read's envelope: an argument-less call to the declared
/// address carrying only its selector, which reads and never funds.
fn probe_envelope(
    address: Address,
    selector: [u8; 4],
    ctx: &ProductionSimContext,
    gas_price: u128,
) -> ::evm2::ethereum::RecoveredTxEnvelope {
    build_envelope(
        address,
        &Bytes::copy_from_slice(&selector),
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
