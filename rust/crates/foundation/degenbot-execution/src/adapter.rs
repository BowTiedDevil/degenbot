//! The `ExecutionAdapter` trait — the full four-part seam (ADR-025 D2,
//! ADR-098 D2).
//!
//! A strategy decomposes into:
//!
//! - **Encode** — [`Self::encode`]: solve result → one complete
//!   [`TransactionRequest`] (`to` + `data` + native `value`) for the
//!   strategy's own execution contract.
//! - **Probe** — declared data: which pre/post read-calls to snapshot
//!   ([`ProbeSpec`]). The engine runs the reads / warms the cache + access
//!   list and reports the readings on the observation.
//! - **Assess** — [`Self::assess`]: the engine's typed
//!   [`ExecutionObservation`] → economic success, gross profit, diagnostics.
//!   The built-in shapes are defaults; an adapter with its own evidence shape
//!   overrides `assess` and reuses [`ExecutionResult::price`], which enforces
//!   the terminal-status invariant (a reverted observation is always a zero-gross
//!   decline) on every assessment.
//! - **Fee** — the **defaulted pricing half of Assess** ([`FeePolicy`]), not a
//!   fifth seam.
//!
//! This crate holds only the generic seam; it has no built-in
//! `cmd_executor` policy and no simulation implementation. The concrete
//! production `CmdExecutorAdapter` lives in `degenbot-strategy`; a foreign
//! searcher implements this trait (or supplies a Python callable lifted into it
//! by `degenbot-python`) for their own contract.
//!
//! The seam is **one opportunity → one transaction** (ADR-098 D2): `encode`
//! returns exactly one transaction, and there is no consumer-owned simulation
//! loop or multi-transaction choreography.

use crate::gate::{AssessError, AssessOptions, ExecutionResult, FeePolicy, ProbeSpecs};
use crate::observation::ExecutionObservation;
use crate::path::PathInfo;
use crate::payload::{
    ComposeError, ComposeOptions, ComposerInputs, PayloadAdapter, PayloadComposer,
};
use crate::solve_result::SolveResult;
use crate::transaction::TransactionRequest;

/// The full four-part `ExecutionAdapter` seam (ADR-025 D2).
///
/// Implement this in a foreign searcher's own crate for their own execution
/// contract — the exact same trait. The built-in `cmd_executor` composition is
/// the concrete `CmdExecutorAdapter` in `degenbot-strategy`, not a generic
/// foreign-adapter implementation.
///
/// Only **Encode** and the adapter's own **Assess** interpretation are user
/// code. **Probe** is declared data; **Assess** and **Fee** have built-in
/// defaults (sum-of-deltas / return-data gate + market-percentile pricing) a
/// foreign searcher may override — they are *not* independent fifth seams
/// (pricing is folded into Assess).
pub trait ExecutionAdapter {
    /// **Encode** — turn a solved path into ONE complete transaction for this
    /// strategy's contract.
    ///
    /// `path` carries the hop descriptors; `inputs` carries the solver-driven
    /// amounts (`optimal_input` / `hop_outputs` / `consumed_inputs`). A
    /// foreign Encode part builds calldata for *its own* contract, names its
    /// own target, and sets whatever native `value` its funding model needs —
    /// it is not wedged onto `cmd_executor` (ADR-025 D5).
    ///
    /// # Errors
    ///
    /// Returns [`ComposeError`] when the path/amounts cannot be encoded.
    fn encode(
        &self,
        path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError>;

    /// **Probe** — declared data: which pre/post read-calls to snapshot
    /// `(label, addr, selector)`.
    ///
    /// The engine runs these reads, warms the cache + access list, and reports
    /// the readings on [`ExecutionObservation::probe_reads`]. This is *declared
    /// data*, not user code.
    fn probe_spec(&self) -> &ProbeSpecs;

    /// The built-in assessment shape + minimum net profit the default
    /// [`Self::assess`] applies.
    fn assess_options(&self) -> AssessOptions {
        AssessOptions::default()
    }

    /// **Fee** — the defaulted pricing half of Assess (NOT a fifth seam).
    ///
    /// Defaults to the built-in market-percentile policy; a foreign searcher
    /// may override it. Pricing folds into [`Self::assess`]:
    /// `net = gross − gas×(base_fee_next + priority_fee)` is defined *in terms
    /// of* this policy, so it cannot be ordered independently.
    fn fee_policy(&self) -> FeePolicy {
        FeePolicy::default()
    }

    /// **Assess** — interpret the engine's typed observation: economic
    /// success, gross profit, and (on failure) the reason.
    ///
    /// The default is the built-in gate over the declared probe readings
    /// ([`AssessOptions`]) priced by [`Self::fee_policy`]. Override it when
    /// your contract reports profit in its own shape — its return data, an
    /// event, its own receipt struct — and reuse [`ExecutionResult::price`] so
    /// the pricing half stays the seam's.
    ///
    /// Reusing that helper also inherits the terminal-status invariant: a
    /// reverted observation prices to zero gross, zero net, and
    /// `passed = false` no matter what your decoder produced, with the
    /// observed gas and fee facts preserved. Check
    /// [`ExecutionObservation::status`] only to *skip* decoding, never to
    /// permit a pass on a revert.
    ///
    /// You are never asked to own the sim loop (ADR-019): the engine executes
    /// the transaction and hands you evidence. `Ok(passed = false)` is an
    /// economic decline; [`AssessError`] means the evidence was absent or
    /// undecodable, so no verdict was reached.
    ///
    /// # Errors
    ///
    /// Returns [`AssessError`] when the observation lacks the evidence this
    /// assessment needs.
    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        self.assess_options()
            .assess(observation, &self.fee_policy())
    }

    /// Convenience: encode from the sealed [`SolveResult`] view when the
    /// strategy only needs that view as its input (most foreign strategies).
    ///
    /// This is the Encode-part entry point the [`PayloadAdapter`]
    /// binding mirrors: `(path, SolveResult)` → one complete transaction. The
    /// default forwards to [`Self::encode`] after projecting the view; a
    /// strategy whose Encode wants the sealed view instead of `path`/`inputs`
    /// may override.
    ///
    /// # Errors
    ///
    /// Returns [`ComposeError`] when the payload cannot be encoded.
    fn compose_view(
        &self,
        path: &PathInfo,
        result: &SolveResult,
    ) -> Result<TransactionRequest, ComposeError> {
        // `ComposerInputs` is a borrowed bundle over the view's u128-narrowed
        // amounts; the generic seam narrows the same way (fits_int128).
        let optimal_input = crate::payload::narrow_u256_to_u128(result.optimal_input)
            .ok_or_else(|| ComposeError::encode("optimal_input does not fit int128"))?;
        let hop_outputs: Vec<u128> = result
            .hop_outputs
            .iter()
            .map(|v| {
                crate::payload::narrow_u256_to_u128(*v)
                    .ok_or_else(|| ComposeError::encode("hop_output does not fit int128"))
            })
            .collect::<Result<_, _>>()?;
        let consumed_inputs: Vec<u128> = result
            .consumed_inputs
            .iter()
            .map(|v| {
                crate::payload::narrow_u256_to_u128(*v)
                    .ok_or_else(|| ComposeError::encode("consumed_input does not fit int128"))
            })
            .collect::<Result<_, _>>()?;
        let inputs = ComposerInputs {
            optimal_input,
            hop_outputs: &hop_outputs,
            consumed_inputs: &consumed_inputs,
            opts: ComposeOptions,
        };
        self.encode(path, &inputs)
    }
}

// `PayloadAdapter` implements the full seam for an encoding-only composer; the
// target and native value are its explicit binding, so bare bytes never
// pretend to carry them.
impl<C: PayloadComposer> ExecutionAdapter for PayloadAdapter<C> {
    fn encode(
        &self,
        path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        let data = self.composer().compose(path, inputs)?;
        Ok(TransactionRequest::new(self.to(), data, self.value()))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        self.probes()
    }

    fn assess_options(&self) -> AssessOptions {
        self.options().clone()
    }

    fn fee_policy(&self) -> FeePolicy {
        self.fee()
    }
}
