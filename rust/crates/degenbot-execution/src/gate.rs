//! The **Probe**/**Assess**/**Fee** parts of an `ExecutionStrategy` (ADR-025
//! D2) — declared *data* + value types, pyo3-free.
//!
//! Probe is declared data (which pre/post read-calls to snapshot); Assess is a
//! gate rule (how deltas → gross and pass/fail); Fee is the *defaulted pricing
//! half of Assess* — `net = gross − gas×(base_fee_next + priority_fee)` is
//! defined *in terms of* the pricing policy, so it is not independently
//! orderable. A built-in market-percentile default (TARGET_PROFIT_RATIO /
//! age-decay) is provided; a foreign searcher may override it.

use alloy::primitives::{Address, U256};

/// A declared pre/post read-call to snapshot: `(label, addr, selector)`.
///
/// The engine runs the read (`eth_call` on the address with the selector),
/// warms the cache + access list, and returns the decoded deltas. `address` is
/// `None` for native-ETH reads (e.g. `getEthBalance` reads a zero address);
/// `selector` is the 4-byte function selector.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProbeSpec {
    /// A human label for diagnostics (`"WETH"`, `"ETH"`, `"USDC"`, …).
    pub label: String,
    /// The contract to read. `None` for the native-ETH pseudo-read.
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

/// A built-in gate shape for **Assess** (ADR-025 D2).
///
/// The engine runs the declared probes and returns per-probe deltas; the gate
/// rule turns those deltas into gross profit + pass/fail. A foreign searcher
/// may instead supply a tiny user interpreter (the `ExecutionStrategy::assess`
/// hook), but the built-in shapes cover the common cases.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AssessRule {
    /// `gross = Σ deltas` over the declared probes (`sum-of-deltas`).
    ///
    /// The default — mirrors the settlement-arbitrage strategy's WETH/ETH/ERC6909 balance
    /// arithmetic generalized to any probe list.
    #[default]
    SumOfDeltas,
    /// `gross = return value` of one probe (`return-value`) — for an executor
    /// contract whose `execute()` returns its own profit figure.
    ReturnValue,
}

/// Tuning knobs for the **Assess** gate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssessOptions {
    /// The built-in gate shape, or `None` if the user supplies a custom
    /// interpreter via the strategy's `assess` hook.
    pub rule: Option<AssessRule>,
    /// A minimum net-profit threshold (wei). `pass = net_profit ≥ threshold`
    /// when the built-in gate applies.
    pub min_net_profit: U256,
}

impl Default for AssessOptions {
    fn default() -> Self {
        Self {
            rule: Some(AssessRule::SumOfDeltas),
            min_net_profit: U256::ZERO,
        }
    }
}

/// The **defaulted pricing half of Assess** (ADR-025 D2) — NOT a fifth seam.
///
/// `net = gross − gas×(base_fee_next + priority_fee)` is defined in terms of
/// the pricing policy, so pricing cannot be ordered independently of Assess. A
/// built-in market-percentile default (the equivalent of the settlement-arbitrage
/// strategy's `compute_priority_fee`) is provided; a foreign searcher may
/// override it with a fixed absolute priority fee.
///
/// The market-percentile *computation itself* lives strategy-side (ADR-019);
/// this value type only names *whether* to use that default and what absolute
/// override (if any) to substitute. Calling code (the engine's Probe/Assess
/// primitives, or a strategy's own `assess`) resolves the final `priority_fee`.
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

/// The outcome of running a strategy's Assess/Fee parts over a sim — the gross
/// → net profit resolution plus the gate verdict.
///
/// This is the value type both the Rust `ExecutionStrategy` and a Python
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
    /// Whether the gate passed (`net_profit ≥ min_net_profit` on the built-in
    /// rule, or the user interpreter's verdict).
    pub passed: bool,
    /// Per-probe deltas (signed), the raw material the gate consumed.
    pub deltas: Vec<i128>,
}

impl ExecutionResult {
    /// `gross = Σ deltas` over the probe state (after − before) — the built-in
    /// sum-of-deltas Assess shape (ADR-025 D2). Negative `after−before`
    /// deltas reduce gross; the built-in gate passes only when the resolved
    /// net profit clears the threshold.
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
