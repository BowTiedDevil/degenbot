//! Distinct Python exception types for the `UniswapArbEngine` surface.
//!
//! Split out of the former monolithic `py_binding.rs` (ergo UG6FKN task
//! 74W2Z6). `create_exception!` registers the Rust type in *this* module
//! (`crate::bot::engine::errors`); `engine::mod` re-exports them so `c_api`
//! and the sibling concern files reference them as `crate::bot::engine::*`.

#[allow(unused_imports)]
use pyo3::create_exception;

// Distinct Python exception types for the two verification failure
// categories (TODO-53b7453b / 7SSOJX). Both subclass `RuntimeError` so existing
// `except RuntimeError` handlers keep catching them, but they let callers
// classify by *type* instead of fragile string matching on "tick data
// mismatch". `build_paths` previously swallowed any `RuntimeError` lacking
// that substring — masking RPC/transport errors that should also surface
// loudly (an unverifiable pool is no safer to operate on than a mismatched
// one).
//
// - `VerificationMismatchError`: the engine's tick data does NOT match
//   on-chain. Fatal — the bot must shut down rather than trade on stale data.
// - `VerificationRpcError`: the verification RPC could not be performed
//   (provider construction failure OR a per-call RPC transport failure —
//   VP42BP). Not safe to silently skip; surfaced as a distinct type so the
//   caller can choose retry/backoff vs abort without re-introducing the
//   swallowing bug.
//
// VP42BP: per-call RPC transport failures inside `liquidity_verifier` are now
// `LiquidityVerifyError::Rpc` (VP42BP), mapped here to `VerifyError::Rpc` →
// `VerificationRpcError` (NOT flattened to `Snapshot`). The distinction here
// covers the `VerifyError::Provider` (provider-construction) category AND the
// `VerifyError::Rpc` (per-call transport) category the seam now routes to the
// retryable `VerificationRpcError`.
create_exception!(
    degenbot_rs,
    VerificationMismatchError,
    pyo3::exceptions::PyRuntimeError,
    "A verification mismatch: the engine's tick data does not match on-chain state."
);
create_exception!(
    degenbot_rs,
    VerificationRpcError,
    pyo3::exceptions::PyRuntimeError,
    "An RPC/transport error during on-chain verification (e.g. provider construction failed)."
);

// V4 pool-admission refusals (Plan 102, slice 2). The Rust core refuses
// amount-modifying-hook and dynamic-fee pools as a *correctness floor*
// (the solver's V3-CL math assumes no hook intervention + a fixed fee).
// Per ADR-005 that floor must protect a standalone Rust consumer, so the
// refusal lives in `BotState::register_v4_pool` and surfaces here as typed
// exceptions. Both subclass `PyValueError` so existing broad
// `except ValueError` handlers (which skip rejected pools one path at a
// time) keep working — Python now classifies by type, not string matching,
// mirroring the TODO-53b7453b verification pattern.
create_exception!(
    degenbot_rs,
    HookedPoolRejectedError,
    pyo3::exceptions::PyValueError,
    "A V4 pool with an amount-modifying hook was rejected at registration: the solver's CL math assumes no hook intervention."
);
create_exception!(
    degenbot_rs,
    DynamicFeePoolRejectedError,
    pyo3::exceptions::PyValueError,
    "A V4 pool with a dynamic fee was rejected at registration: the solver assumes a fixed fee."
);
