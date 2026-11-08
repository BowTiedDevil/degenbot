//! The **Probe**/**Assess**/**Fee** parts of an `ExecutionAdapter` (ADR-025
//! D2) — declared *data* + value types, pyo3-free.
//!
//! Probe is declared data (which pre/post read-calls to snapshot). Assess is
//! the adapter's interpretation of the engine's typed
//! [`ExecutionObservation`]: economic success, gross profit, and diagnostics.
//! Fee is the *defaulted pricing half of Assess* — `net = gross −
//! gas × (base_fee_next + priority_fee)` is defined *in terms of* the pricing
//! policy, so it is not independently orderable. A built-in market-percentile
//! default is provided; a foreign searcher may override it.
//!
//! The built-in assessment shapes ([`AssessRule::SumOfDeltas`] over declared
//! probe readings, [`AssessRule::ReturnData`] over the transaction's first
//! return word) are **defaults an adapter may ignore**. An adapter whose
//! contract reports profit another way — a bespoke event, its own receipt
//! struct, a credit-ledger read — overrides `ExecutionAdapter::assess` and
//! reuses [`ExecutionResult::price`] for the pricing half. That helper also
//! pins the terminal-status invariant, so a reverted observation prices to zero
//! gross and `passed = false` whatever the adapter decoded.

use alloy::primitives::{Address, U256};
use thiserror::Error;

use crate::observation::{ExecutionObservation, ExecutionStatus};

/// A declared pre/post read-call to snapshot: `(label, addr, selector)`.
///
/// The engine runs the read (`eth_call` on the address with the selector),
/// warms the cache + access list, and returns the decoded readings. `address`
/// is `None` for native-ETH reads (e.g. a balance read against no contract);
/// `selector` is the 4-byte function selector.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProbeSpec {
    /// A human label for diagnostics (`"CREDIT"`, `"ETH"`, `"USDC"`, …).
    pub label: String,
    /// The contract to read. `None` for a pseudo-read with no callee.
    pub address: Option<Address>,
    /// The 4-byte function selector.
    pub selector: [u8; 4],
}

impl ProbeSpec {
    /// Build a probe spec from `(label, address, selector)`.
    #[must_use]
    pub const fn new(label: String, address: Option<Address>, selector: [u8; 4]) -> Self {
        Self {
            label,
            address,
            selector,
        }
    }
}

/// The declared probe list for a strategy.
pub type ProbeSpecs = Vec<ProbeSpec>;

/// A built-in assessment shape (ADR-025 D2).
///
/// These are defaults, not requirements: an adapter with its own evidence
/// shape overrides `ExecutionAdapter::assess` and never selects a rule here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AssessRule {
    /// `gross = Σ probe deltas` over the declared readings (`sum-of-deltas`).
    ///
    /// Every declared probe must carry a complete before/after reading; an
    /// unavailable read is [`AssessError::Evidence`], never a zero delta.
    #[default]
    SumOfDeltas,
    /// `gross = first 32-byte word of the transaction's return data`
    /// (`return-data`) — for a contract that reports its own profit figure.
    ReturnData,
}

/// Tuning knobs for the **Assess** gate: the built-in shape (if any) and the
/// minimum net profit a pass requires.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssessOptions {
    /// The built-in assessment shape the default `assess` applies.
    pub rule: AssessRule,
    /// A minimum net-profit threshold (wei). `pass = net_profit ≥ threshold`.
    pub min_net_profit: U256,
}

impl Default for AssessOptions {
    fn default() -> Self {
        Self {
            rule: AssessRule::SumOfDeltas,
            min_net_profit: U256::ZERO,
        }
    }
}

impl AssessOptions {
    /// The built-in assessment: resolve an observation's evidence into gross
    /// profit with [`Self::rule`], then price it with `fee_policy` and the
    /// declared threshold.
    ///
    /// A reverted observation declines (`passed = false`, zero gross) while
    /// preserving the observed gas and base fee — the revert is evidence on the
    /// observation, not an assessment error. The short-circuit keeps a revert
    /// from failing on evidence the selected rule would otherwise require; the
    /// zero-gross/`passed = false` verdict itself is enforced by
    /// [`ExecutionResult::price`] for every assessment, custom ones included.
    /// Evidence the selected rule needs but the observation does not carry is
    /// [`AssessError::Evidence`].
    ///
    /// # Errors
    ///
    /// Returns [`AssessError`] when the observation lacks the evidence the
    /// selected rule requires.
    pub fn assess(
        &self,
        observation: &ExecutionObservation,
        fee_policy: &FeePolicy,
    ) -> Result<ExecutionResult, AssessError> {
        if observation.status == ExecutionStatus::Revert {
            return Ok(ExecutionResult::price(
                U256::ZERO,
                Vec::new(),
                observation,
                self.min_net_profit,
                fee_policy,
            ));
        }
        match self.rule {
            AssessRule::SumOfDeltas => {
                let deltas = observation
                    .probe_reads
                    .iter()
                    .map(|reading| {
                        reading.delta().ok_or_else(|| {
                            AssessError::evidence(format!(
                                "probe `{}` has no complete before/after reading",
                                reading.spec.label
                            ))
                        })
                    })
                    .collect::<Result<Vec<i128>, AssessError>>()?;
                Ok(ExecutionResult::price(
                    ExecutionResult::deltas_to_gross(&deltas),
                    deltas,
                    observation,
                    self.min_net_profit,
                    fee_policy,
                ))
            }
            AssessRule::ReturnData => {
                let gross = observation.first_return_word().ok_or_else(|| {
                    AssessError::evidence(
                        "the return-data rule needs the transaction's first 32-byte return word",
                    )
                })?;
                Ok(ExecutionResult::price(
                    gross,
                    Vec::new(),
                    observation,
                    self.min_net_profit,
                    fee_policy,
                ))
            }
        }
    }
}

/// The **defaulted pricing half of Assess** (ADR-025 D2) — NOT a fifth seam.
///
/// `net = gross − gas×(base_fee_next + priority_fee)` is defined in terms of
/// the pricing policy, so pricing cannot be ordered independently of Assess. A
/// built-in market-percentile default is provided; a foreign searcher may
/// override it with a fixed absolute priority fee.
///
/// The market-percentile *computation itself* lives strategy-side (ADR-019);
/// this value type only names *whether* to use that default and what absolute
/// override (if any) to substitute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeePolicy {
    /// Use the built-in market-percentile default when `true`.
    pub market_percentile: bool,
    /// A fixed absolute priority-fee override (wei/gas), used when
    /// `market_percentile == false`.
    pub override_priority_fee: u128,
}

impl Default for FeePolicy {
    fn default() -> Self {
        Self {
            market_percentile: true,
            override_priority_fee: 0,
        }
    }
}

impl FeePolicy {
    /// The built-in market-percentile priority-fee default (wei/gas).
    ///
    /// This is the *constant default* backing the market-percentile policy
    /// (a stand-in for the strategy-side `compute_priority_fee`, which owns
    /// the real TARGET_PROFIT_RATIO / age-decay math — ADR-019 D4).
    #[must_use]
    pub const fn builtin_priority_fee() -> u128 {
        1_000_000_000 // 1 gwei — conservative market-percentile floor
    }

    /// Resolve the effective priority fee (wei/gas) this policy selects.
    #[must_use]
    pub const fn priority_fee(&self) -> u128 {
        if self.market_percentile {
            Self::builtin_priority_fee()
        } else {
            self.override_priority_fee
        }
    }
}

/// Failure modes for Assess — the adapter could not interpret the observation.
///
/// This is *not* an economic decline: a decline is `Ok` with `passed = false`.
/// An error means the evidence an adapter's assessment requires is absent or
/// undecodable, so no verdict was reached. The engine treats it as a decline
/// carrying the reason; it never crashes the bot.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AssessError {
    /// The observation does not carry the evidence this assessment needs.
    #[error("observation lacks required evidence: {0}")]
    Evidence(String),
    /// The evidence is present but could not be decoded.
    #[error("assessment decode failure: {0}")]
    Decode(String),
}

impl AssessError {
    /// Wrap an arbitrary message as an [`AssessError::Evidence`].
    #[must_use]
    pub fn evidence(msg: impl Into<String>) -> Self {
        Self::Evidence(msg.into())
    }

    /// Wrap an arbitrary message as an [`AssessError::Decode`].
    #[must_use]
    pub fn decode(msg: impl Into<String>) -> Self {
        Self::Decode(msg.into())
    }
}

/// The outcome of running a strategy's Assess/Fee parts over an observation —
/// the gross → net profit resolution plus the gate verdict.
///
/// This is the value type both the Rust `ExecutionAdapter` and a Python
/// consumer observe. **Amounts are integer fixed-point wei (never floats).**
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionResult {
    /// Gross profit (wei) before gas pricing.
    pub gross_profit: U256,
    /// Net profit (wei) after `gross − gas×(base_fee_next + priority_fee)`.
    pub net_profit: U256,
    /// Gas used by the execute call (un-inflated).
    pub gas_used: u64,
    /// The base fee of the next block (wei/gas).
    pub base_fee_next: u128,
    /// The priority fee (wei/gas) from the pricing policy.
    pub priority_fee: u128,
    /// Whether the gate passed (`net_profit ≥ min_net_profit`).
    pub passed: bool,
    /// Per-probe deltas (signed), the raw material a sum-of-deltas assessment
    /// consumed. Empty when the adapter's assessment used other evidence.
    pub deltas: Vec<i128>,
}

impl ExecutionResult {
    /// Price a gross-profit figure against the observation's gas and base fee
    /// with the fee policy — the pricing half every assessment shares,
    /// including a custom adapter-owned `assess`.
    ///
    /// # Terminal-status invariant
    ///
    /// A reverted observation prices to `gross_profit = 0`, `net_profit = 0`,
    /// and `passed = false`, whatever gross figure or evidence the caller
    /// decoded: a revert moved no value, so no payload bytes or emitted event
    /// can be profit. The observed `gas_used`, `base_fee_next`, and
    /// `priority_fee` facts are preserved for diagnostics. This lives in the
    /// shared helper — not in each assessment — so a custom `assess` cannot
    /// reintroduce an economic pass on a reverted transaction.
    ///
    /// On a successful observation, a genuine loss (gross below gas cost) is a
    /// *fail* with zero net, not a zero-profit pass: `checked_sub` keeps an
    /// underflow distinct from a true break-even (`net == 0`), which the default
    /// zero threshold passes.
    #[must_use]
    pub fn price(
        gross_profit: U256,
        deltas: Vec<i128>,
        observation: &ExecutionObservation,
        min_net_profit: U256,
        fee_policy: &FeePolicy,
    ) -> Self {
        let gas_used = observation.gas_used;
        let base_fee_next = observation.base_fee_next;
        let priority_fee = fee_policy.priority_fee();
        if !observation.succeeded() {
            return Self {
                gross_profit: U256::ZERO,
                net_profit: U256::ZERO,
                gas_used,
                base_fee_next,
                priority_fee,
                passed: false,
                deltas,
            };
        }
        let gas_cost =
            U256::from(gas_used) * (U256::from(base_fee_next) + U256::from(priority_fee));
        let (net_profit, passed) = match gross_profit.checked_sub(gas_cost) {
            Some(net) => (net, net >= min_net_profit),
            None => (U256::ZERO, false),
        };
        Self {
            gross_profit,
            net_profit,
            gas_used,
            base_fee_next,
            priority_fee,
            passed,
            deltas,
        }
    }

    /// `gross = Σ deltas` over the probe readings (after − before) — the
    /// built-in sum-of-deltas shape (ADR-025 D2). Negative deltas reduce gross;
    /// a net-negative sum floors at zero.
    #[must_use]
    pub fn deltas_to_gross(deltas: &[i128]) -> U256 {
        let sum: i128 = deltas.iter().sum();
        if sum >= 0 {
            U256::from(sum.unsigned_abs())
        } else {
            U256::ZERO
        }
    }
}
