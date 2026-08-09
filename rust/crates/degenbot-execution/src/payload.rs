//! The **Encode** part of an `ExecutionStrategy` (ADR-025 D2) — the
//! `PayloadComposer` seam.
//!
//! A solver result becomes payload `bytes` for ONE execution contract. Rust
//! users implement [`PayloadComposer`] directly; Python users supply a callable
//! that `degenbot-python` lifts into this same trait (`PyPayloadComposer`).
//!
//! **Decoupling note (ADR-025-a).** The canonical `cmd_executor` composer
//! (`encode_cmd_stream` over `degenbot_executor::composers::ComposerInputs`) is
//! the *developer's* adapter — its `ComposerInputs` carries
//! `executor_address` / `pool_manager_address` / `weth_address` + a
//! cmd-specific `opts: EncodeOptions`, all wedged to the Vyper contract. This
//! seam's [`ComposerInputs`] deliberately carries **only the solver-driven
//! amounts** (`optimal_input`, `hop_outputs`, `consumed_inputs`) + a
//! decoupled generic [`ComposeOptions`] — no protocol addresses, no
//! cmd-opcode knobs. A user's Encode part names its own contract; the
//! default-adapter internals (cmd addresses/opts) stay on the other side of
//! the seam.

use alloy::primitives::{Bytes, U256};

use degenbot_executor::composers::PathInfo;
use thiserror::Error;

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
/// these, exactly as the default adapter does today.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComposerInputs<'a> {
    /// The flash/optimal input amount (u128).
    pub optimal_input: u128,
    /// Per-hop output amounts. `hop_outputs[i]` = output after hop `i`
    /// (`[forward_out, final_output]` for a 2-hop path).
    pub hop_outputs: &'a [u128],
    /// Per-hop consumed input amounts (the CL-clamp swap-in). For a
    /// non-over-fed CL hop (and V2/Curve/Balancer/Solidly hops) this equals
    /// `hop_outputs[i-1]`; for an over-fed CL hop the clamp reduces it to
    /// `input_consumed − 1` (UO3JM4 / path-5000 EMPTY-HALT).
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

/// The **Encode** part of an `ExecutionStrategy`: solve result → payload
/// `bytes` for ONE execution contract (ADR-025 D2).
///
/// Rust users implement this trait; Python users supply a callable lifted into
/// it by `degenbot-python` (`PyPayloadComposer`). The canonical `cmd_executor`
/// encoder is the **default adapter** implementing this seam.
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
