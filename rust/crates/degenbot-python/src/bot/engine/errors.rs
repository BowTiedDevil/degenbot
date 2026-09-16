//! Distinct Python exception types for the engine-wrapper surface.
//!
//! `create_exception!` registers the Rust type in *this* module
//! (`crate::bot::engine::errors`); `engine::mod` re-exports them so `c_api`
//! and the sibling concern files reference them as `crate::bot::engine::*`.

use pyo3::{create_exception, PyErr};

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
// per-call RPC transport failures inside `liquidity_verifier` are now
// `LiquidityVerifyError::Rpc`, mapped here to `VerifyError::Rpc` →
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
// amount-modifying-hook, dynamic-fee, and high-static-fee pools as a
// *correctness floor* (the solver's V3-CL math assumes no hook
// intervention + a fixed fee; the cmd_executor encodes fee as u16 so
// fees > 65535 are un-encodable). Per ADR-005 that floor must protect a
// standalone Rust consumer, so the refusal lives in
// `BotState::register_v4_pool` and surfaces here as typed exceptions. All the pool-registration admission refusals subclass
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
//      ├─ HighFeePoolRejectedError                   (V4 admission — static
//      │                                              fee > 65535, DPODAZ)
//      ├─ PoolAlreadyRegisteredError                (V2/V3/V4 — duplicate
//      │                                              address at registration)
//      └─ SpecViolationError                        (V2/V3/V4 — out-of-spec
//                                                     field: sqrt/tick/fee/
//                                                     tickSpacing/reserve)
create_exception!(
    degenbot._ffi,
    PoolRegistrationError,
    pyo3::exceptions::PyValueError,
    "A pool was refused at registration (duplicate address, out-of-spec field, V4 amount-modifying hook, V4 dynamic fee, or V4 high static fee > 65535). Subclasses classify the specific admission reason so build_paths skips rejected pools by type, not string matching."
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
    HighFeePoolRejectedError,
    crate::bot::engine::PoolRegistrationError,
    "A V4 pool whose static fee exceeds the cmd_executor's 2-byte encoding limit (fee > 65535) was rejected at registration: the executor encodes fee as u16 in both V4_SWAP_COMPACT and V4_SWAP_DYNAMIC, so such pools cannot be encoded. They are also unprofitable (32%+ per swap)."
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

// ADR-037: hooked V4 pools are admitted but their simulations use
// standard CL math that hooks may invalidate — ported from the archived
// Python `PossibleInaccurateResult` (archive/main-20260721
// src/degenbot/exceptions/liquidity_pool.py:94). Raised by the pool-handle
// sim seams with the APPROXIMATE consumed/delivered magnitudes attached so
// callers can decide whether a possibly-wrong number is usable.
create_exception!(
    degenbot._ffi,
    PossibleInaccurateResult,
    pyo3::exceptions::PyValueError,
    "The simulated swap crosses a pool whose amount-modifying hook may have invalidated the result; the attached amounts are the standard-math approximation."
);

// PRG-4 / IRUMXD: the registered-path cap refusal. A BENIGN stop signal, not
// an error: the crawl catches it and stops discovery (it replaces the Python
// DiscoveryCrawlComplete pre-count unwind). Deliberately NOT under
// PoolRegistrationError — a full registry is a state, not a pool-admission
// refusal.
create_exception!(
    degenbot._ffi,
    PathRegistryFullError,
    pyo3::exceptions::PyValueError,
    "The engine path registry is at its configured registered-path cap. Benign stop: discovery must stop offering new candidate paths."
);

// FF-T1 (BPHR6F, FLEETFLOOR): the fleet boot refusal is a TYPED error —
// the library never aborts the host process on the boot-refusal arm. The
// 2026-09-11 CI failures made the gap concrete: on 4-vCPU runners the
// fleet budget refusal (fractional quota below the pinned-role floor)
// reached the seat-host materializer's abort and SIGABRT'd pytest-xdist
// workers inside the extension. Both refusal families surface HERE as
// the same exception: the budget floor family (BudgetError's
// QuotaTooSmallForPinnedRoles / Oversubscribed / TooFewSolverCpus) and
// the boot invariants (SlotLayout's dead-station refusals). Reuse, not a
// new mechanism: the Rust carriers are the workers' existing BootError
// family; this is the pyo3 surface. The message carries the detected
// budget, the floor, and ONE operator hint. The degenbot binary maps
// this exception to its loud named fail-fast exit — fail-fast stays
// BINARY-only, never a library abort.
create_exception!(
    degenbot._ffi,
    BootRefused,
    pyo3::exceptions::PyRuntimeError,
    "The fleet host refused to boot: the detected CPU budget is below the pinned-role floor, or a boot invariant failed. The library never aborts the host process on this arm; the message carries the detected budget, the floor, and one operator hint."
);

// TB4QGX T6 (spike S2): the Faulted intake drain. The sticky lane-death
// latch resolved parked intake units terminally; the receipt re-raises this
// typed error. Distinct from the fatal verification errors so the driver can
// tell a fleet fault from a bad pool; subclassing RuntimeError keeps broad
// handlers working.
create_exception!(
    degenbot._ffi,
    FleetIntakeFaultedError,
    pyo3::exceptions::PyRuntimeError,
    "The fleet registration intake faulted: the sticky lane-death latch resolved held intake units terminally (they were never executed). Sticky until a fresh process."
);

/// Map the bot-side typed `IntakeFault` onto the exception (ONE owner of the
/// wording, mirroring `boot_refused`).
pub(crate) fn intake_faulted(fault: degenbot_bot::fleet_intake::IntakeFault) -> PyErr {
    FleetIntakeFaultedError::new_err(format!(
        "fleet registration intake faulted ({}): {} held unit(s) resolved terminally and were never executed; the lane is dead and the cordon is sticky until a fresh process",
        fault.cause, fault.held
    ))
}

/// FF-T1: map the workers' typed `BootError` family onto the
/// `BootRefused` exception — the ONE owner of the message shape (detected
/// budget + floor + one operator hint), so the wording cannot drift per
/// seam. Every pyo3 surface that can surface a fleet boot refusal maps
/// through here.
pub(crate) fn boot_refused(err: degenbot_workers::dispatcher::BootError) -> PyErr {
    use degenbot_workers::budget::BudgetError;
    use degenbot_workers::dispatcher::BootError;
    match err {
        BootError::Budget(BudgetError::QuotaTooSmallForPinnedRoles { quota, required }) => {
            BootRefused::new_err(format!(
                "fleet boot refused: detected CPU budget {quota:.2} cores is below the pinned-role floor of {required} cores — give the host at least {required} usable cores (cgroup quota / CPU affinity); the serial binding for 2-5-core hosts is pending (FF-T4, FLEETFLOOR)"
            ))
        }
        BootError::Budget(BudgetError::Oversubscribed { quota, floor, declared }) => {
            BootRefused::new_err(format!(
                "fleet boot refused: declared peak shares {declared} cores exceed floor(quota {quota:.2}) = {floor} — lower the fleet.* share overrides; oversubscription is a configuration bug, refused at boot"
            ))
        }
        BootError::Budget(BudgetError::BelowHostFloor { quota }) => BootRefused::new_err(
            format!(
                "fleet boot refused: detected CPU budget {quota:.2} cores is below the 2-core host floor (one core for I/O work, one core for solve work) — no binding can host the fleet there; give the host at least 2 usable cores (cgroup quota / CPU affinity)"
            ),
        ),
        BootError::Budget(BudgetError::IoWorkersOutOfBounds { requested, binding }) => {
            BootRefused::new_err(format!(
                "fleet boot refused: runtime.io_workers override {requested} is out of bounds for the {binding} binding (pinned: A >= 1, the SMTH6M ambient floor; serial: exactly one ambient I/O lane) — fix or drop the override, never a silent clamp"
            ))
        }
        BootError::Budget(BudgetError::TooFewSolverCpus { solver, min }) => {
            BootRefused::new_err(format!(
                "fleet boot refused: the Solver share derives to {solver} cores, below the {min}-core minimum — raise the quota or lower the other fleet.* shares"
            ))
        }
        other => BootRefused::new_err(format!(
            "fleet boot refused: {other} — the fleet cannot host this configuration; check the fleet.* overrides"
        )),
    }
}
