//! The **Encode** part of an `ExecutionAdapter` (ADR-025 D2) — the
//! `PayloadComposer` seam, plus the [`PayloadAdapter`] binding that turns bare
//! calldata into a complete transaction.
//!
//! A solver result becomes payload `bytes` for ONE execution contract. Rust
//! users implement [`PayloadComposer`] directly; Python users supply a callable
//! that `degenbot-python` lifts into this same trait.
//!
//! **Bytes alone do not carry a target or a value.** The production boundary is
//! [`crate::TransactionRequest`] (`to` + `data` + native `value`), so an
//! encoding-only composer is bound to its target explicitly with
//! [`PayloadAdapter`] — the binding supplies `to` / `value` / probes /
//! assessment defaults, and the adapter it produces is a full
//! `ExecutionAdapter`. A composer that needs to pick a target or a value per
//! opportunity implements `ExecutionAdapter::encode` itself.
//!
//! **Decoupling note (ADR-025-a).** The built-in `cmd_executor` adapter is a
//! concrete production composition in `degenbot-strategy`; it captures the
//! command-executor addresses and accepts its per-call `EncodeOptions` there.
//! This seam's [`ComposerInputs`] deliberately carries **only the solver-driven
//! amounts** (`optimal_input`, `hop_outputs`, `consumed_inputs`) + a decoupled
//! generic [`ComposeOptions`] — no protocol addresses, no cmd-opcode knobs. A
//! user's Encode part names its own contract; the built-in adapter keeps its
//! command-specific policy on the other side of the seam.

use alloy::primitives::{Address, Bytes, U256};

use thiserror::Error;

use crate::gate::{AssessOptions, FeePolicy, ProbeSpecs};
use crate::path::PathInfo;

/// Generic, adapter-agnostic encode knobs (ADR-025 D2/D5).
///
/// Deliberately **not** `degenbot_executor::composers::EncodeOptions` (which
/// carries `erc6909_profit` / `use_v4_batch` — cmd_executor-specific opcode
/// toggles). This is an empty, defaulted placeholder a foreign composer may
/// ignore; extend it if a future user contract needs its own encode-time
/// switches. Decoupled from `cmd_executor` opcodes by construction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ComposeOptions;

/// The per-path solver-driven amount bundle handed to [`PayloadComposer::compose`].
///
/// Mirrors the *shape* of `degenbot_executor::composers::ComposerInputs`
/// (the `optimal_input` / `hop_outputs` / `consumed_inputs` / `opts` fields)
/// while dropping the cmd-specific `executor/pool_manager/weth` addresses and
/// the cmd-opcode `EncodeOptions`, per ADR-025 D5.
///
/// Amounts are **integer fixed-point u128** (the `cmd_executor` int128
/// convention) — decimal place matters, so they are never floats. The
/// solve-result view ([`crate::SolveResult`]) carries the same amounts as
/// `U256`; the caller narrows via `fits_int128`-style checks when building
/// these, exactly as the production command adapter does today.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComposerInputs<'a> {
    /// The flash/optimal input amount (u128).
    pub optimal_input: u128,
    /// Per-hop output amounts. `hop_outputs[i]` = output after hop `i`
    /// (`[forward_out, final_output]` for a 2-hop path).
    pub hop_outputs: &'a [u128],
    /// Per-hop consumed input amounts (the concentrated-liquidity-clamp swap-in). For a
    /// non-over-fed concentrated liquidity hop (and V2/Curve/Balancer/Solidly hops) this equals
    /// `hop_outputs[i-1]`; for an over-fed concentrated liquidity hop the clamp reduces it to
    /// `input_consumed − 1` (path-5000 EMPTY-HALT).
    pub consumed_inputs: &'a [u128],
    /// Adapter-agnostic encode knobs (decoupled from `cmd_executor` opcodes).
    pub opts: ComposeOptions,
}

/// Failure modes for the Encode part (payload composition).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ComposeError {
    /// The path/arity/amount combination is unsupported by this composer.
    #[error("unsupported path or hop mix: {0}")]
    Unsupported(String),
    /// A primitive encoding step failed (e.g. int128 overflow).
    #[error("encode failure: {0}")]
    Encode(String),
}

/// A custom failure factory for user composers.
impl ComposeError {
    /// Wrap an arbitrary message as an [`ComposeError::Unsupported`].
    #[must_use]
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    /// Wrap an arbitrary message as an [`ComposeError::Encode`].
    #[must_use]
    pub fn encode(msg: impl Into<String>) -> Self {
        Self::Encode(msg.into())
    }
}

/// The **Encode** part of an `ExecutionAdapter`: solve result → payload
/// `bytes` for ONE execution contract (ADR-025 D2).
///
/// Rust users implement this trait; Python users supply a callable lifted into
/// it by `degenbot-python` (`PyPayloadComposer`). The canonical
/// `cmd_executor` encoder is the concrete production `CmdExecutorAdapter` in
/// `degenbot-strategy`; it is not a foreign `PayloadComposer` implementation
/// and does not widen this generic seam.
pub trait PayloadComposer {
    /// Turn a solved path (`PathInfo` hop descriptors + [`ComposerInputs`]
    /// solver-driven amounts) into the `bytes` payload for the composer's
    /// execution contract.
    ///
    /// # Errors
    ///
    /// Returns [`ComposeError`] if the path/amounts cannot be encoded.
    fn compose(&self, path: &PathInfo, inputs: &ComposerInputs<'_>) -> Result<Bytes, ComposeError>;
}

// Blanket so a caller can satisfy `&dyn PayloadComposer` with any sized impl.
impl<T: PayloadComposer + ?Sized> PayloadComposer for &T {
    #[inline]
    fn compose(&self, path: &PathInfo, inputs: &ComposerInputs<'_>) -> Result<Bytes, ComposeError> {
        (*self).compose(path, inputs)
    }
}

/// An encoding-only [`PayloadComposer`] bound to the transaction it executes.
///
/// The binding is explicit by construction: a composer produces calldata, and
/// this value supplies the pieces calldata cannot carry — the target contract,
/// the native `value`, the declared probe reads, and the assessment defaults.
/// The result satisfies the full [`crate::ExecutionAdapter`] seam, so an
/// encoding-only consumer still meets the production interface; it does not
/// become a second simulation implementation.
///
/// A composer whose target or value varies per opportunity (a multi-deployment
/// operator, a contract that sizes `msg.value` from the solution) implements
/// `ExecutionAdapter::encode` directly instead of binding a constant here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadAdapter<C> {
    composer: C,
    to: Address,
    value: U256,
    probes: ProbeSpecs,
    options: AssessOptions,
    fee: FeePolicy,
}

impl<C> PayloadAdapter<C> {
    /// Bind a composer to its target contract; the value starts at zero and the
    /// probe/assessment halves at their defaults.
    #[must_use]
    pub fn new(composer: C, to: Address) -> Self {
        Self {
            composer,
            to,
            value: U256::ZERO,
            probes: Vec::new(),
            options: AssessOptions::default(),
            fee: FeePolicy::default(),
        }
    }

    /// Set the native value (wei) sent with every transaction this binding
    /// produces.
    #[must_use]
    pub const fn with_value(mut self, value: U256) -> Self {
        self.value = value;
        self
    }

    /// Declare the pre/post read-calls the engine snapshots.
    #[must_use]
    pub fn with_probes(mut self, probes: ProbeSpecs) -> Self {
        self.probes = probes;
        self
    }

    /// Select the built-in assessment shape and minimum net profit.
    #[must_use]
    pub fn with_assess_options(mut self, options: AssessOptions) -> Self {
        self.options = options;
        self
    }

    /// Override the defaulted pricing half of Assess.
    #[must_use]
    pub const fn with_fee_policy(mut self, fee: FeePolicy) -> Self {
        self.fee = fee;
        self
    }

    /// The bound composer.
    #[must_use]
    pub const fn composer(&self) -> &C {
        &self.composer
    }

    /// The bound target contract.
    #[must_use]
    pub const fn to(&self) -> Address {
        self.to
    }

    /// The bound native value (wei).
    #[must_use]
    pub const fn value(&self) -> U256 {
        self.value
    }

    /// The declared probe reads.
    #[must_use]
    pub fn probes(&self) -> &ProbeSpecs {
        &self.probes
    }

    /// The assessment defaults this binding reports.
    #[must_use]
    pub const fn options(&self) -> &AssessOptions {
        &self.options
    }

    /// The pricing policy this binding reports.
    #[must_use]
    pub const fn fee(&self) -> FeePolicy {
        self.fee
    }
}

/// A convenience `U256` → `u128` narrowing for composing payloads from the
/// solve-result view's amounts, mirroring the `cmd_executor` `fits_int128`
/// guard's non-negative acceptance.
///
/// Returns `None` when `value` does not fit in a signed 128-bit integer.
#[must_use]
#[inline]
pub fn narrow_u256_to_u128(value: U256) -> Option<u128> {
    u128::try_from(value)
        .ok()
        .filter(|v| *v <= i128::MAX as u128)
}
