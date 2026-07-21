//! Distinct Python exception types for the `ArbitrageEngine` surface.
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
    degenbot._ffi,
    VerificationMismatchError,
    pyo3::exceptions::PyRuntimeError,
    "A verification mismatch: the engine's tick data does not match on-chain state."
);
create_exception!(
    degenbot._ffi,
    VerificationRpcError,
    pyo3::exceptions::PyRuntimeError,
    "An RPC/transport error during on-chain verification (e.g. provider construction failed)."
);

// V4 pool-admission refusals (Plan 102, slice 2). The Rust core refuses
// amount-modifying-hook and dynamic-fee pools as a *correctness floor*
// (the solver's V3-CL math assumes no hook intervention + a fixed fee).
// Per ADR-005 that floor must protect a standalone Rust consumer, so the
// refusal lives in `BotState::register_v4_pool` and surfaces here as typed
// exceptions. All the pool-registration admission refusals subclass
// `PoolRegistrationError` (which subclasses `PyValueError`) — F2EVV6 unified
// the family so `except PoolRegistrationError:` catches every admission
// refusal across V2/V3/V4 (already-registered + spec violation + V4 hook +
// V4 dynamic fee), and the older V4-specific names (`HookedPoolRejectedError` /
// `DynamicFeePoolRejectedError`) reparent under `PoolRegistrationError` so a
// broader catch keeps working AND classification is by type.
//
// The unified hierarchy:
//
//   ValueError
//   └─ PoolRegistrationError                       (F2EVV6 base)
//      ├─ HookedPoolRejectedError                    (V4 admission —
//      │                                              amount-modifying hook)
//      ├─ DynamicFeePoolRejectedError                (V4 admission — dynamic fee)
//      ├─ PoolAlreadyRegisteredError                (V2/V3/V4 — duplicate
//      │                                              address at registration)
//      └─ SpecViolationError                        (V2/V3/V4 — out-of-spec
//                                                     field: sqrt/tick/fee/
//                                                     tickSpacing/reserve)
create_exception!(
    degenbot._ffi,
    PoolRegistrationError,
    pyo3::exceptions::PyValueError,
    "A pool was refused at registration (duplicate address, out-of-spec field, V4 amount-modifying hook, or V4 dynamic fee). Subclasses classify the specific admission reason so build_paths skips rejected pools by type, not string matching."
);
create_exception!(
    degenbot._ffi,
    HookedPoolRejectedError,
    crate::bot::engine::PoolRegistrationError,
    "A V4 pool with an amount-modifying hook was rejected at registration: the solver's CL math assumes no hook intervention."
);
create_exception!(
    degenbot._ffi,
    DynamicFeePoolRejectedError,
    crate::bot::engine::PoolRegistrationError,
    "A V4 pool with a dynamic fee was rejected at registration: the solver assumes a fixed fee."
);
create_exception!(
    degenbot._ffi,
    PoolAlreadyRegisteredError,
    crate::bot::engine::PoolRegistrationError,
    "A pool at this address is already registered. Subclasses PoolRegistrationError (a wiring/programming error surfaced at admission time, distinct from per-field spec violations / V4 admission categories)."
);
create_exception!(
    degenbot._ffi,
    SpecViolationError,
    crate::bot::engine::PoolRegistrationError,
    "A field on the pool registration params violates its on-chain Solidity bound (e.g. V2 reserve > uint112, V3/V4 sqrtPriceX96 / tick / fee / tickSpacing out of range). The message identifies the offending field, its value, and the bound it violates."
);
