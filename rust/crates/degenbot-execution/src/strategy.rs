//! The `ExecutionStrategy` trait — the full four-part seam (ADR-025 D2).
//!
//! A strategy decomposes into:
//!
//! - **Encode** — [`PayloadComposer`]: solve result → payload `bytes` for the
//!   strategy's own execution contract.
//! - **Probe** — declared data: which pre/post read-calls to snapshot
//!   ([`ProbeSpec`]). The engine runs the reads / warms the cache + access
//!   list.
//! - **Assess** — a gate rule: how deltas → gross and pass/fail (built-in
//!   shapes in [`AssessRule`], or a user override of the default assessor).
//! - **Fee** — the **defaulted pricing half of Assess** ([`FeePolicy`]), not a
//!   fifth seam.
//!
//! This crate holds **no default strategy** — `degenbot-backrun-strategy`
//! implements this trait as the default adapter; a foreign searcher implements
//! it (or supplies a Python callable lifted into it by `degenbot-python`) for
//! their own contract.

use alloy::primitives::{Bytes, U256};

use degenbot_executor::composers::PathInfo;

use crate::gate::{AssessRule, FeePolicy, ProbeSpecs};
use crate::payload::{ComposeError, ComposeOptions, ComposerInputs, PayloadComposer};
use crate::solve_result::SolveResult;

/// The full four-part `ExecutionStrategy` seam (ADR-025 D2).
///
/// Implement this (a) as the default adapter (`degenbot-backrun-strategy`, the
/// canonical `cmd_executor` path), or (b) in a foreign searcher's own crate for
/// their own execution contract — the exact same trait. A Python consumer
/// instead supplies a callable + probe/assess spec, lifted into this seam by
/// `degenbot-python` (`PyPayloadComposer` / `PyExecutionStrategy`).
///
/// Only **Encode** is unconditionally user code. **Probe** is declared data;
/// **Assess** and **Fee** have built-in defaults (sum-of-deltas gate +
/// market-percentile pricing) a foreign searcher may override — they are *not*
/// independent fifth seams (pricing is folded into Assess; see
/// [`Self::assess`]).
pub trait ExecutionStrategy {
    /// **Encode** — turn a solved path into payload `bytes` for this
    /// strategy's contract.
    ///
    /// `path` carries the hop descriptors; `inputs` carries the solver-driven
    /// amounts (`optimal_input` / `hop_outputs` / `consumed_inputs`). A
    /// foreign Encode part builds calldata for *its own* contract and names
    /// its own addresses — it is not wedged onto `cmd_executor` (ADR-025 D5).
    ///
    /// # Errors
    ///
    /// Returns [`ComposeError`] when the path/amounts cannot be encoded.
    fn encode(&self, path: &PathInfo, inputs: &ComposerInputs<'_>) -> Result<Bytes, ComposeError>;

    /// **Probe** — declared data: which pre/post read-calls to snapshot
    /// `(label, addr, selector)`.
    ///
    /// The engine runs these reads, warms the cache + access list, and feeds
    /// the deltas to [`Self::assess`]. This is *declared data*, not user code.
    fn probe_spec(&self) -> &ProbeSpecs;

    /// **Assess** — the default gate rule over the probe deltas.
    ///
    /// `None` means the strategy overrides [`Self::assess`] with its own
    /// interpreter (the optional tiny user gate). `Some(rule)` selects a
    /// built-in shape (sum-of-deltas / return-value); the default is
    /// [`AssessRule::SumOfDeltas`].
    fn assess_rule(&self) -> Option<AssessRule> {
        Some(AssessRule::SumOfDeltas)
    }

    /// The minimum net profit (wei) the gate requires for a pass.
    fn min_net_profit(&self) -> U256 {
        U256::ZERO
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

    /// Resolve the probe deltas into an [`ExecutionResult`], applying the
    /// strategy's Assess rule + Fee policy: `gross = Σ deltas`, then
    /// `net = gross − gas×(base_fee_next + priority_fee)`,
    /// `passed = net ≥ min_net_profit`.
    ///
    /// This is the **built-in default** assessor a foreign searcher may
    /// override (e.g. the backrun strategy's richer balance-decode gate, or a
    /// user's tiny interpreter reading a single return value). It is engine-
    /// free: the caller supplies the deltas, gas, and base fee; the strategy
    /// owns the interpretation + pricing. You are never asked to own the sim
    /// loop (ADR-019).
    fn assess(
        &self,
        deltas: &[i128],
        gas_used: u64,
        base_fee_next: u128,
    ) -> crate::gate::ExecutionResult {
        let gross_profit = crate::gate::ExecutionResult::deltas_to_gross(deltas);
        let priority_fee = self.fee_policy().priority_fee();
        let gas_cost =
            (U256::from(gas_used)) * (U256::from(base_fee_next) + U256::from(priority_fee));
        // A genuine loss (gross < gas cost) can never clear the gate — it is a
        // *fail*, not a zero-profit pass. `checked_sub` keeps that distinct from
        // a true break-even (net == 0), which the default zero threshold passes.
        let (net_profit, passed) = match gross_profit.checked_sub(gas_cost) {
            Some(net) => (net, net >= self.min_net_profit()),
            None => (U256::ZERO, false),
        };
        crate::gate::ExecutionResult {
            gross_profit,
            net_profit,
            gas_used,
            base_fee_next,
            priority_fee,
            passed,
            deltas: deltas.to_vec(),
        }
    }

    /// Convenience: decode + assess in one call when the strategy only needs
    /// the sealed [`SolveResult`] view as its input (most foreign strategies).
    ///
    /// This is the Encode-part entry point the `PayloadComposer` blanket
    /// mirrors: `(path, `SolveResult`) → payload bytes`. The default forwards
    /// to [`Self::encode`] after projecting the view; a strategy whose Encode
    /// wants the sealed view instead of `path`/`inputs` may override.
    ///
    /// # Errors
    ///
    /// Returns [`ComposeError`] when the payload cannot be encoded.
    fn compose_view(&self, path: &PathInfo, result: &SolveResult) -> Result<Bytes, ComposeError> {
        // `ComposerInputs` is a borrowed bundle over the view's u128-narrowed
        // amounts; the default adapter narrows the same way (fits_int128).
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

/// Blanket so a `PayloadComposer` (Encode-only, ADR-025 D2) satisfies the full
/// `ExecutionStrategy` seam with the built-in Probe/Assess/Fee defaults —
/// implementing just the Encode blob is enough for the common case.
impl<P: PayloadComposer> ExecutionStrategy for P {
    fn encode(&self, path: &PathInfo, inputs: &ComposerInputs<'_>) -> Result<Bytes, ComposeError> {
        PayloadComposer::compose(self, path, inputs)
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        static EMPTY: ProbeSpecs = Vec::new();
        &EMPTY
    }
}
