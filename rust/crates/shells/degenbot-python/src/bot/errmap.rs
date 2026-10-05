//! Core-error → historical-Python-exception translation for the bot shell.
//!
//! One home for every mapper the `PyBot` / `PyArbEngine` / `PyLiquidityPool`
//! pymethods raise through. The historical Python message vocabulary is a
//! behavioral contract: each mapper's output text is pinned by the tests in
//! this module, and `build_paths`' exception classification rides the typed
//! hierarchy these constructors produce.
//!
//! Address parsing shares the same contract:
//! [`degenbot_bot::bot_core::registration::parse_address_str`] owns the
//! `Invalid address '<input>': <source>` vocabulary; the shell only wraps
//! its error in the historical `ValueError`.
//!
//! Other domains keep their own mappers: `db::db_err_to_py`, `abi/*`,
//! `fork/*`, and `engine/strategy.rs` are out of scope here.

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use degenbot_bot::arb_engine::DriverError;
use degenbot_bot::bot_core::liquidity_verifier::LiquidityVerifyError;
use degenbot_bot::bot_core::registration_lifecycle::RegistrationLifecycleError;
use degenbot_bot::bot_core::snapshot_verify::VerifyError;
use degenbot_pools::state_history::JournalError;

use crate::bot::engine::{
    DynamicFeePoolRejectedError, HighFeePoolRejectedError, HookedPoolRejectedError,
    PathRegistryFullError, PoolAlreadyRegisteredError, SpecViolationError,
    VerificationMismatchError, VerificationRpcError,
};

// --- Driver lifecycle (`EngineDriver` seam) ---

/// Map a core [`DriverError`] to a Python exception.
pub(crate) fn map_driver_err(err: DriverError) -> PyErr {
    match err {
        // The typed receiver refusal carries no payload; Display is the single
        // source of the remediation text, so the Python `RuntimeError` string
        // stays byte-identical to the pre-typing refusal.
        DriverError::NoResultReceiver => PyRuntimeError::new_err(err.to_string()),
        // Every other non-lifecycle variant (phase/session/subscribe/resume/
        // registration) surfaces as the legacy `RuntimeError` with the driver's
        // message; the registration lifecycles route their verify errors
        // through the typed `map_driver_lifecycle_err` instead.
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

/// Map a lifecycle [`DriverError`] to the typed Python exception the verify
/// surface has always raised.
pub(crate) fn map_driver_lifecycle_err(err: DriverError) -> PyErr {
    match err {
        DriverError::Verify(e) => match e {
            RegistrationLifecycleError::Verify(v) => map_liquidity_verify_error(v),
            RegistrationLifecycleError::MissingProvider => VerificationRpcError::new_err(
                "registration verify requires an RPC provider for tracked pools — configure the bot's single provider"
                    .to_string(),
            ),
            RegistrationLifecycleError::MissingTickSpacing => PyRuntimeError::new_err(
                RegistrationLifecycleError::MissingTickSpacing.to_string(),
            ),
        },
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

/// Map a classified [`VerifyError`] from a retry-wrapped lifecycle to the typed
/// Python exception the verify surface has always raised.
pub(crate) fn map_verify_lifecycle_error(err: VerifyError) -> PyErr {
    match err {
        VerifyError::Snapshot(message) => VerificationMismatchError::new_err(message),
        VerifyError::Provider(message) | VerifyError::Rpc(message) => {
            VerificationRpcError::new_err(message)
        }
        VerifyError::Other(message) => PyRuntimeError::new_err(message),
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

/// Map a `LiquidityVerifyError` (from `liquidity_verifier::verify_v3/v4_pools`)
/// to a typed Python exception, mirroring `engine::verify::map_verify_err`.
///
/// - `Mismatch` → `VerificationMismatchError` (fatal — on-chain tick data
///   disagrees with the engine).
/// - `Rpc` → `VerificationRpcError` (per-call RPC transport failure — the
///   caller may retry/backoff; NOT evidence of a mismatch).
pub(crate) fn map_liquidity_verify_error(
    err: degenbot_bot::bot_core::liquidity_verifier::LiquidityVerifyError,
) -> PyErr {
    match err {
        LiquidityVerifyError::Mismatch(m) => VerificationMismatchError::new_err(m.to_string()),
        LiquidityVerifyError::Rpc { message } => VerificationRpcError::new_err(message),
    }
}

// --- Pool registration (admission) ---

/// Map a [`RegisterV2PoolError`] to a typed Python exception under the
/// `PoolRegistrationError` hierarchy.
///
/// - `AlreadyRegistered` → [`PoolAlreadyRegisteredError`]
/// - `SpecViolation` → [`SpecViolationError`] (the message names the
///   offending field, its value, and the bound it violates, mirroring
///   `spec_bounds::SpecViolation`'s `Display`)
///
/// These are subclasses of `PoolRegistrationError`, which is itself a
/// subclass of `ValueError`, so a broad `except ValueError:` (or
/// `except PoolRegistrationError:` to scope just admission refusals) keeps
/// working.
pub(crate) fn map_register_v2_err(err: degenbot_bot::bot_core::RegisterV2PoolError) -> pyo3::PyErr {
    match err {
        degenbot_bot::bot_core::RegisterV2PoolError::AlreadyRegistered { address } => {
            PoolAlreadyRegisteredError::new_err(format!(
                "V2 pool already registered: address={address}"
            ))
        }
        degenbot_bot::bot_core::RegisterV2PoolError::SpecViolation(v) => {
            SpecViolationError::new_err(format!("V2 pool registration failed: {v}"))
        }
    }
}

/// Map a [`RegisterV3PoolError`] to a typed Python exception under the
/// `PoolRegistrationError` hierarchy. Mirrors [`map_register_v2_err`].
pub(crate) fn map_register_v3_err(err: degenbot_bot::bot_core::RegisterV3PoolError) -> pyo3::PyErr {
    match err {
        degenbot_bot::bot_core::RegisterV3PoolError::AlreadyRegistered { address } => {
            PoolAlreadyRegisteredError::new_err(format!(
                "V3 pool already registered: address={address}"
            ))
        }
        degenbot_bot::bot_core::RegisterV3PoolError::SpecViolation(v) => {
            SpecViolationError::new_err(format!("V3 pool registration failed: {v}"))
        }
    }
}

/// Map a [`RegisterV4PoolError`] to a typed Python exception.
///
/// - `HookedPool` → [`HookedPoolRejectedError`] (V4 amount-modifying-hook
///   admission floor — the solver's CL math assumes no hook intervention).
/// - `DynamicFee` → [`DynamicFeePoolRejectedError`] (V4 dynamic-fee
///   admission floor — the solver assumes a fixed fee).
/// - `FeeExceedsEncoderLimit` → [`HighFeePoolRejectedError`] (V4 static-fee
///   exceeds the `cmd_executor`'s 2-byte encoding field; the
///   fee is protocol-valid but un-encodable and unprofitable).
/// - `AlreadyRegistered` → [`PoolAlreadyRegisteredError`] (duplicate
///   `(pool_manager, pool_id)` registration — a wiring/programming error
///   surfaced at admission time, unified with the V2/V3 twins under
///   `PoolRegistrationError`).
/// - `SpecViolation` → [`SpecViolationError`] (out-of-spec
///   sqrtPriceX96/tick/fee/tickSpacing, stop-gap upgraded to a typed
///   exception).
///
/// The message text for the V4-specific variants is byte-for-byte unchanged
/// from the legacy `Err(String)` formatting so `build_paths`'s classification
/// (now `isinstance`, was substring) matches the same diagnostics.
pub(crate) fn map_register_v4_err(err: degenbot_bot::bot_core::RegisterV4PoolError) -> pyo3::PyErr {
    match err {
        degenbot_bot::bot_core::RegisterV4PoolError::HookedPool { hook_flags } => {
            HookedPoolRejectedError::new_err(format!(
                "V4 pool has amount-modifying hooks (flags=0x{hook_flags:04X}, mask=0x{:04X}) — excluded from arbitrage",
                degenbot_bot::bot_core::AMOUNT_MODIFYING_HOOK_MASK
            ))
        }
        degenbot_bot::bot_core::RegisterV4PoolError::DynamicFee { fee } => {
            DynamicFeePoolRejectedError::new_err(format!(
                "V4 pool has dynamic fee (fee=0x{fee:06X}) — excluded from arbitrage"
            ))
        }
        degenbot_bot::bot_core::RegisterV4PoolError::FeeExceedsEncoderLimit { fee } => {
            HighFeePoolRejectedError::new_err(format!(
                "V4 pool fee (fee={fee}) exceeds the cmd_executor's 2-byte encoding limit (65535) — excluded from arbitrage"
            ))
        }
        degenbot_bot::bot_core::RegisterV4PoolError::AlreadyRegistered {
            pool_manager,
            pool_id,
        } => PoolAlreadyRegisteredError::new_err(format!(
            "V4 pool already registered: pool_manager={pool_manager}, pool_id=0x{}",
            alloy::hex::encode(pool_id),
        )),
        degenbot_bot::bot_core::RegisterV4PoolError::SpecViolation(v) => {
            SpecViolationError::new_err(format!("V4 pool registration failed: {v}"))
        }
    }
}

/// Map the typed path-registration refusal: a full registry is the benign
/// `PathRegistryFullError` stop signal; every `Invalid` refusal keeps the
/// legacy `ValueError` with its verbatim message.
pub(crate) fn map_path_registration_err(
    err: degenbot_bot::arb_engine::lifecycle::PathRegistrationError,
) -> pyo3::PyErr {
    match err {
        degenbot_bot::arb_engine::lifecycle::PathRegistrationError::Invalid(msg) => {
            PyValueError::new_err(msg)
        }
        degenbot_bot::arb_engine::lifecycle::PathRegistrationError::RegistryFull {
            cap,
            registered,
        } => PathRegistryFullError::new_err(format!(
            "registered-path cap reached ({registered}/{cap}) — the crawl must stop discovery"
        )),
    }
}

// --- Builder / build families ---

/// Map a Rust `PoolBuilder` error (the delegation adapter's
/// builder stage) to a Python `RuntimeError` carrying the RPC/CREATE2/spec/DB
/// failure cause. Registration-stage errors are mapped by the `map_register_v*`
/// fns above, so this covers only the pre-registration build stage.
pub(crate) fn map_builder_err(
    err: degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError,
) -> pyo3::PyErr {
    use degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError;
    match err {
        PoolBuilderError::Rpc(e) => PyRuntimeError::new_err(format!("pool build RPC error: {e}")),
        PoolBuilderError::UnknownVariant { factory } => PyRuntimeError::new_err(format!(
            "pool build unknown factory {factory} — no built-in DEX variant preset"
        )),
        PoolBuilderError::UnknownPoolIdentity { address } => PyValueError::new_err(format!(
            "pool build unknown identity at {address}: no identity selector answered"
        )),
        PoolBuilderError::Spec => PyRuntimeError::new_err("pool build out-of-spec V2 reserve"),
        PoolBuilderError::Create2 => {
            PyRuntimeError::new_err("pool build CREATE2 address verification failed")
        }
        PoolBuilderError::Db(e) => {
            PyRuntimeError::new_err(format!("pool build DB read failed: {e}"))
        }
        PoolBuilderError::Decoding { message } => {
            PyRuntimeError::new_err(format!("pool build decode failure: {message}"))
        }
        PoolBuilderError::MissingIdentity { message } => {
            PyValueError::new_err(format!("V4 identity incomplete: {message}"))
        }
        PoolBuilderError::TickAssembly(e) => {
            PyValueError::new_err(format!("Tracked tick map rejected at intake: {e}"))
        }
    }
}

/// Map the typed no-`ConstructionIo` refusal to the historical
/// method-prefixed `RuntimeError` (each pymethod owns its prefix).
pub(crate) fn map_no_construction_io(method: &'static str) -> pyo3::PyErr {
    PyRuntimeError::new_err(format!(
        "{method}: no ConstructionIo attached (requires an alloy provider)"
    ))
}

/// Map a core `BuildError` (the no-registration build families: Aerodrome /
/// Balancer weighted + stable / Curve / ERC-20 token) to the shell's two
/// historical surfaces: the method-prefixed no-io `RuntimeError` and the
/// builder error map.
pub(crate) fn map_build_err(
    method: &'static str,
    err: degenbot_bot::bot_core::build_register::BuildError,
) -> pyo3::PyErr {
    use degenbot_bot::bot_core::build_register::BuildError;
    match err {
        BuildError::NoConstructionIo => map_no_construction_io(method),
        BuildError::Builder(e) => map_builder_err(e),
    }
}

/// Map a core `V2BuildError` to the shell's three historical surfaces: the
/// method-prefixed no-io `RuntimeError`, the builder error map, and the V2
/// registration hierarchy map.
pub(crate) fn map_v2_build_err(
    err: degenbot_bot::bot_core::build_register::V2BuildError,
) -> pyo3::PyErr {
    use degenbot_bot::bot_core::build_register::V2BuildError;
    match err {
        V2BuildError::NoConstructionIo => map_no_construction_io("build_v2_pool"),
        V2BuildError::Builder(e) => map_builder_err(e),
        V2BuildError::Register(e) => map_register_v2_err(e),
    }
}

/// The V4 twin of [`map_v2_build_err`].
pub(crate) fn map_v4_build_err(
    err: degenbot_bot::bot_core::build_register::V4BuildError,
) -> pyo3::PyErr {
    use degenbot_bot::bot_core::build_register::V4BuildError;
    match err {
        V4BuildError::NoConstructionIo => map_no_construction_io("build_v4_pool"),
        V4BuildError::Builder(e) => map_builder_err(e),
        V4BuildError::Register(e) => map_register_v4_err(e),
    }
}

/// Map a core `V3BuildError` to the shell's historical surfaces: the
/// construction-route refusal map (the LOUD `UnsupportedPoolFamilyError`
/// among them) and the method-prefixed race-answer `RuntimeError`.
pub(crate) fn map_v3_build_err(
    err: degenbot_bot::bot_core::build_register::V3BuildError,
) -> pyo3::PyErr {
    use degenbot_bot::bot_core::build_register::V3BuildError;
    match err {
        V3BuildError::Refusal(e) => crate::bot::engine::map_construction_refusal(e),
        V3BuildError::NoReadableIdentity { address } => PyRuntimeError::new_err(format!(
            "build_v3_pool: registry GET answered {address} with no readable V3 identity"
        )),
    }
}

// --- Swap calculation ---

/// Map a core `CalcTokensOutError` to the shell's historical surfaces: the
/// `ValueError` for the overflow/on-chain-revert class, the
/// unknown-pool and unsupported-family `ValueError`s, and the legacy `0`
/// mapping for unrecovered sparse-map misses (done core-side).
pub(crate) fn map_calc_tokens_out_err(
    err: degenbot_bot::bot_core::build_register::CalcTokensOutError,
) -> pyo3::PyErr {
    use degenbot_bot::bot_core::build_register::CalcTokensOutError;
    match err {
        CalcTokensOutError::Overflow | CalcTokensOutError::NotComputable => {
            PyValueError::new_err(
                "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)",
            )
        }
        CalcTokensOutError::UnknownPool { pool_id } => {
            PyValueError::new_err(format!(
                "swap_simulation: pool {pool_id} is not registered"
            ))
        }
        CalcTokensOutError::UnsupportedFamily { pool_id, family } => {
            PyValueError::new_err(format!(
                "swap_simulation: pool {pool_id} family {family} is not supported for this operation"
            ))
        }
    }
}

/// Map a core `CalcTokensInError` to the shell's historical surfaces: the
/// overflow `ValueError` and the typed exact-output family gap.
pub(crate) fn map_calc_tokens_in_err(
    err: degenbot_bot::bot_core::build_register::CalcTokensInError,
) -> pyo3::PyErr {
    use degenbot_bot::bot_core::build_register::CalcTokensInError;
    match err {
        CalcTokensInError::Overflow => PyValueError::new_err(
            "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)",
        ),
        CalcTokensInError::UnsupportedFamily { pool_id, family } => {
            PyValueError::new_err(format!(
                "calculate_tokens_in: pool {pool_id} family {family} has no exact-output path"
            ))
        }
    }
}

// --- Registration wrappers (CREATE2 verify → admission) ---

/// Map a core
/// [`V2RegistrationError`](degenbot_bot::bot_core::registration::V2RegistrationError)
/// to the shell's two historical surfaces: the bare CREATE2-mismatch
/// `ValueError` and the `PoolRegistrationError` hierarchy map
/// ([`map_register_v2_err`]).
pub(crate) fn map_v2_registration_err(
    err: degenbot_bot::bot_core::registration::V2RegistrationError,
) -> pyo3::PyErr {
    use degenbot_bot::bot_core::registration::V2RegistrationError;
    match err {
        V2RegistrationError::Create2(m) => PyValueError::new_err(m.to_string()),
        V2RegistrationError::Register(e) => map_register_v2_err(e),
    }
}

/// The V3 twin of [`map_v2_registration_err`].
pub(crate) fn map_v3_registration_err(
    err: degenbot_bot::bot_core::registration::V3RegistrationError,
) -> pyo3::PyErr {
    use degenbot_bot::bot_core::registration::V3RegistrationError;
    match err {
        V3RegistrationError::Create2(m) => PyValueError::new_err(m.to_string()),
        V3RegistrationError::Register(e) => map_register_v3_err(e),
    }
}

/// Map an Aerodrome registration refusal (only the EIP-1167 verify can
/// refuse) to the bare-mismatch `ValueError` the shell has always raised.
pub(crate) fn map_aerodrome_registration_err(
    err: degenbot_bot::bot_core::registration::AerodromeRegistrationError,
) -> pyo3::PyErr {
    match err {
        degenbot_bot::bot_core::registration::AerodromeRegistrationError::Create2(m) => {
            PyValueError::new_err(m.to_string())
        }
    }
}

/// Map a core
/// [`ResolveV4IdentityError`](degenbot_bot::bot_core::registration::ResolveV4IdentityError)
/// to the shell's historical surfaces: the method-prefixed "no
/// ConstructionIo attached" `RuntimeError` and the builder error map.
pub(crate) fn map_resolve_v4_identity_err(
    err: degenbot_bot::bot_core::registration::ResolveV4IdentityError,
) -> pyo3::PyErr {
    use degenbot_bot::bot_core::registration::ResolveV4IdentityError;
    match err {
        ResolveV4IdentityError::NoConstructionIo => PyRuntimeError::new_err(
            "resolve_v4_identity: no ConstructionIo attached (requires an alloy provider)",
        ),
        ResolveV4IdentityError::Builder(e) => map_builder_err(e),
    }
}

// --- Journal ---

/// Map a [`JournalError`] to a Python `ValueError` with the `NoPoolStateAvailable`
///-shaped message the Python pool companion expects (and re-raises as
/// `NoPoolStateAvailable`). ADR-005 slice 4 decision 2: reorg errors that used
/// to panic must surface as `ValueError`. Shared by `PyBot` and `PyLiquidityPool`.
pub(crate) fn journal_err_to_py(e: JournalError) -> PyErr {
    match e {
        JournalError::NoStatePriorToBlock { block } => {
            PyValueError::new_err(format!("No pool state known prior to block {block}"))
        }
        JournalError::NoStateAtOrAfterBlock { block } => {
            PyValueError::new_err(format!("No pool state known at or after block {block}"))
        }
    }
}

#[cfg(test)]
mod tests {
    //! Pin the historical message vocabulary per mapper. The Python seam and
    //! its fixtures classify on the exact text (or the typed exception) these
    //! constructors produce, so the strings below are a behavioral contract.
    //! Addresses used in pins are all-digit hex so their rendering is
    //! identical under any case convention.

    #![expect(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use alloy::primitives::{Address, B256, U256};
    use degenbot_bot::bot_core::liquidity_verifier::VerificationMismatch;
    use degenbot_bot::bot_core::registration::{
        AerodromeRegistrationError, ResolveV4IdentityError, V2RegistrationError,
    };
    use degenbot_bot::bot_core::{RegisterV2PoolError, RegisterV3PoolError, RegisterV4PoolError};
    use degenbot_pools::spec_bounds::{SpecValue, SpecViolation};

    fn addr(s: &str) -> Address {
        s.parse().unwrap()
    }

    /// `PyErr`'s `Display` renders `<ExceptionClass>: <message>`; the pins
    /// assert on the message the Python caller sees.
    fn strip_class(rendered: String) -> String {
        match rendered.split_once(": ") {
            Some((_class, rest)) => rest.to_string(),
            None => rendered,
        }
    }

    fn message(err: PyErr) -> String {
        strip_class(err.to_string())
    }

    fn value_error(err: PyErr) -> String {
        let rendered = Python::attach(|py| {
            assert!(
                err.is_instance_of::<PyValueError>(py),
                "expected a ValueError, got: {err}"
            );
            err.to_string()
        });
        strip_class(rendered)
    }

    fn runtime_error(err: PyErr) -> String {
        let rendered = Python::attach(|py| {
            assert!(
                err.is_instance_of::<PyRuntimeError>(py),
                "expected a RuntimeError, got: {err}"
            );
            err.to_string()
        });
        strip_class(rendered)
    }

    #[test]
    fn driver_err_passthrough_uses_display_text() {
        let err = map_driver_err(DriverError::NoResultReceiver);
        assert_eq!(
            runtime_error(err),
            DriverError::NoResultReceiver.to_string(),
            "the receiver refusal keeps Display as the single source of its text"
        );
        let err = map_driver_err(DriverError::SessionState("already subscribed".to_string()));
        assert_eq!(runtime_error(err), "already subscribed");
    }

    #[test]
    fn driver_lifecycle_err_pins_the_verify_vocabulary() {
        let err = map_driver_lifecycle_err(DriverError::Verify(
            RegistrationLifecycleError::MissingProvider,
        ));
        Python::attach(|py| {
            assert!(err.is_instance_of::<VerificationRpcError>(py));
        });
        assert_eq!(
            message(err),
            "registration verify requires an RPC provider for tracked pools — configure the bot's single provider"
        );

        let err = map_driver_lifecycle_err(DriverError::Verify(
            RegistrationLifecycleError::MissingTickSpacing,
        ));
        assert_eq!(
            runtime_error(err),
            RegistrationLifecycleError::MissingTickSpacing.to_string()
        );
    }

    #[test]
    fn verify_lifecycle_err_pins_the_typed_vocabulary() {
        let err = map_verify_lifecycle_error(VerifyError::Snapshot("tick drift".to_string()));
        Python::attach(|py| {
            assert!(err.is_instance_of::<VerificationMismatchError>(py));
        });
        assert_eq!(message(err), "tick drift");

        let err = map_verify_lifecycle_error(VerifyError::Provider("http down".to_string()));
        Python::attach(|py| {
            assert!(err.is_instance_of::<VerificationRpcError>(py));
        });
        assert_eq!(message(err), "http down");

        let err = map_verify_lifecycle_error(VerifyError::Rpc("timeout".to_string()));
        Python::attach(|py| {
            assert!(err.is_instance_of::<VerificationRpcError>(py));
        });
        assert_eq!(message(err), "timeout");

        let err = map_verify_lifecycle_error(VerifyError::Other("boom".to_string()));
        assert_eq!(runtime_error(err), "boom");
    }

    #[test]
    fn liquidity_verify_err_pins_the_typed_vocabulary() {
        let err =
            map_liquidity_verify_error(LiquidityVerifyError::Mismatch(VerificationMismatch {
                message: "V3 pool 0x.. block=1: tick 5 liquidityGross mismatch".to_string(),
            }));
        Python::attach(|py| {
            assert!(
                err.is_instance_of::<VerificationMismatchError>(py),
                "LiquidityVerifyError::Mismatch must surface as VerificationMismatchError (fatal), not PyRuntimeError"
            );
            assert!(
                !err.is_instance_of::<VerificationRpcError>(py),
                "genuine mismatch is NOT an Rpc error (distinct types)"
            );
        });
        assert_eq!(
            message(err),
            "V3 pool 0x.. block=1: tick 5 liquidityGross mismatch"
        );

        let err = map_liquidity_verify_error(LiquidityVerifyError::Rpc {
            message: "V3 pool 0x..: tickBitmap(0) RPC call failed: timeout".to_string(),
        });
        Python::attach(|py| {
            assert!(
                err.is_instance_of::<VerificationRpcError>(py),
                "LiquidityVerifyError::Rpc must surface as VerificationRpcError (retryable), not PyRuntimeError"
            );
            assert!(
                !err.is_instance_of::<VerificationMismatchError>(py),
                "RPC transport failure is NOT a mismatch (distinct types)"
            );
        });
        assert_eq!(
            message(err),
            "V3 pool 0x..: tickBitmap(0) RPC call failed: timeout"
        );
    }

    #[test]
    fn register_v2_err_pins_the_vocabulary() {
        let err = map_register_v2_err(RegisterV2PoolError::AlreadyRegistered {
            address: addr("0x0101010101010101010101010101010101010101"),
        });
        assert_eq!(
            message(err),
            "V2 pool already registered: address=0x0101010101010101010101010101010101010101"
        );

        let err = map_register_v2_err(RegisterV2PoolError::SpecViolation(SpecViolation {
            field: "reserve0",
            value: SpecValue::U256(U256::from(7u8)),
            bound: "uint112 (≤ 2^112 − 1)",
        }));
        assert_eq!(
            message(err),
            "V2 pool registration failed: field `reserve0` value 7 is out of bounds: uint112 (≤ 2^112 − 1)"
        );
    }

    #[test]
    fn register_v3_err_pins_the_vocabulary() {
        let err = map_register_v3_err(RegisterV3PoolError::AlreadyRegistered {
            address: addr("0x0202020202020202020202020202020202020202"),
        });
        assert_eq!(
            message(err),
            "V3 pool already registered: address=0x0202020202020202020202020202020202020202"
        );

        let err = map_register_v3_err(RegisterV3PoolError::SpecViolation(SpecViolation {
            field: "tick",
            value: SpecValue::I32(887_273),
            bound: "|tick| ≤ 887272",
        }));
        assert_eq!(
            message(err),
            "V3 pool registration failed: field `tick` value 887273 is out of bounds: |tick| ≤ 887272"
        );
    }

    #[test]
    fn register_v4_err_pins_the_vocabulary() {
        let err = map_register_v4_err(RegisterV4PoolError::HookedPool { hook_flags: 1 });
        assert_eq!(
            message(err),
            format!(
                "V4 pool has amount-modifying hooks (flags=0x0001, mask=0x{:04X}) — excluded from arbitrage",
                degenbot_bot::bot_core::AMOUNT_MODIFYING_HOOK_MASK
            )
        );

        let err = map_register_v4_err(RegisterV4PoolError::DynamicFee { fee: 0x80_0000 });
        assert_eq!(
            message(err),
            "V4 pool has dynamic fee (fee=0x800000) — excluded from arbitrage"
        );

        let err = map_register_v4_err(RegisterV4PoolError::FeeExceedsEncoderLimit { fee: 70_000 });
        assert_eq!(
            message(err),
            "V4 pool fee (fee=70000) exceeds the cmd_executor's 2-byte encoding limit (65535) — excluded from arbitrage"
        );

        let err = map_register_v4_err(RegisterV4PoolError::AlreadyRegistered {
            pool_manager: addr("0x0303030303030303030303030303030303030303"),
            pool_id: [0u8; 32],
        });
        assert_eq!(
            message(err),
            format!(
                "V4 pool already registered: pool_manager={}, pool_id=0x{}",
                addr("0x0303030303030303030303030303030303030303"),
                "0".repeat(64),
            )
        );

        let err = map_register_v4_err(RegisterV4PoolError::SpecViolation(SpecViolation {
            field: "tick_spacing",
            value: SpecValue::I32(0),
            bound: "nonzero",
        }));
        assert_eq!(
            message(err),
            "V4 pool registration failed: field `tick_spacing` value 0 is out of bounds: nonzero"
        );
    }

    #[test]
    fn path_registration_err_pins_the_vocabulary() {
        let err = map_path_registration_err(
            degenbot_bot::arb_engine::lifecycle::PathRegistrationError::Invalid(
                "path crosses itself".to_string(),
            ),
        );
        assert_eq!(value_error(err), "path crosses itself");

        let err = map_path_registration_err(
            degenbot_bot::arb_engine::lifecycle::PathRegistrationError::RegistryFull {
                cap: 100,
                registered: 128,
            },
        );
        assert_eq!(
            message(err),
            "registered-path cap reached (128/100) — the crawl must stop discovery"
        );
    }

    #[test]
    fn builder_err_pins_the_vocabulary() {
        use degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError;

        assert_eq!(
            message(map_builder_err(PoolBuilderError::Spec)),
            "pool build out-of-spec V2 reserve"
        );
        assert_eq!(
            message(map_builder_err(PoolBuilderError::Create2)),
            "pool build CREATE2 address verification failed"
        );
        assert_eq!(
            message(map_builder_err(PoolBuilderError::Decoding {
                message: "bad data".to_string(),
            })),
            "pool build decode failure: bad data"
        );
        assert_eq!(
            message(map_builder_err(PoolBuilderError::MissingIdentity {
                message: "no ticks".to_string(),
            })),
            "V4 identity incomplete: no ticks"
        );
        let factory = addr("0x0404040404040404040404040404040404040404");
        assert_eq!(
            message(map_builder_err(PoolBuilderError::UnknownVariant {
                factory
            })),
            format!("pool build unknown factory {factory} — no built-in DEX variant preset")
        );
        let address = addr("0x0505050505050505050505050505050505050505");
        assert_eq!(
            message(map_builder_err(PoolBuilderError::UnknownPoolIdentity {
                address
            })),
            format!("pool build unknown identity at {address}: no identity selector answered")
        );
    }

    #[test]
    fn no_construction_io_pins_the_method_prefix() {
        assert_eq!(
            message(map_no_construction_io("build_v2_pool")),
            "build_v2_pool: no ConstructionIo attached (requires an alloy provider)"
        );
    }

    #[test]
    fn build_err_pins_the_no_io_prefix_and_builder_routing() {
        assert_eq!(
            message(map_build_err(
                "build_curve_pool",
                degenbot_bot::bot_core::build_register::BuildError::NoConstructionIo,
            )),
            "build_curve_pool: no ConstructionIo attached (requires an alloy provider)"
        );
        assert_eq!(
            message(map_build_err(
                "build_curve_pool",
                degenbot_bot::bot_core::build_register::BuildError::Builder(
                    degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError::Spec,
                ),
            )),
            "pool build out-of-spec V2 reserve"
        );
    }

    #[test]
    fn v2_build_err_pins_the_no_io_prefix_and_routing() {
        assert_eq!(
            message(map_v2_build_err(
                degenbot_bot::bot_core::build_register::V2BuildError::NoConstructionIo,
            )),
            "build_v2_pool: no ConstructionIo attached (requires an alloy provider)"
        );
        assert_eq!(
            message(map_v2_build_err(
                degenbot_bot::bot_core::build_register::V2BuildError::Register(
                    RegisterV2PoolError::AlreadyRegistered {
                        address: addr("0x0101010101010101010101010101010101010101"),
                    },
                ),
            )),
            "V2 pool already registered: address=0x0101010101010101010101010101010101010101"
        );
    }

    #[test]
    fn v3_build_err_pins_the_refusal_and_race_answer() {
        assert_eq!(
            message(map_v3_build_err(
                degenbot_bot::bot_core::build_register::V3BuildError::NoReadableIdentity {
                    address: addr("0x0606060606060606060606060606060606060606"),
                },
            )),
            "build_v3_pool: registry GET answered 0x0606060606060606060606060606060606060606 with no readable V3 identity"
        );
    }

    #[test]
    fn calc_tokens_out_err_pins_the_vocabulary() {
        use degenbot_bot::bot_core::build_register::CalcTokensOutError;
        let overflow = "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)";
        assert_eq!(
            value_error(map_calc_tokens_out_err(CalcTokensOutError::Overflow)),
            overflow
        );
        assert_eq!(
            value_error(map_calc_tokens_out_err(CalcTokensOutError::NotComputable)),
            overflow
        );
        assert_eq!(
            value_error(map_calc_tokens_out_err(CalcTokensOutError::UnknownPool {
                pool_id: 5
            })),
            "swap_simulation: pool 5 is not registered"
        );
        assert_eq!(
            value_error(map_calc_tokens_out_err(
                CalcTokensOutError::UnsupportedFamily {
                    pool_id: 5,
                    family: "v9",
                }
            )),
            "swap_simulation: pool 5 family v9 is not supported for this operation"
        );
    }

    #[test]
    fn calc_tokens_in_err_pins_the_vocabulary() {
        use degenbot_bot::bot_core::build_register::CalcTokensInError;
        assert_eq!(
            value_error(map_calc_tokens_in_err(CalcTokensInError::Overflow)),
            "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)"
        );
        assert_eq!(
            value_error(map_calc_tokens_in_err(
                CalcTokensInError::UnsupportedFamily {
                    pool_id: 5,
                    family: "v9",
                }
            )),
            "calculate_tokens_in: pool 5 family v9 has no exact-output path"
        );
    }

    #[test]
    fn v2_registration_err_pins_the_create2_and_admission_vocabulary() {
        let mismatch = degenbot_uniswap::deployments::AddressMismatch {
            chain_id: 1,
            factory: addr("0x0101010101010101010101010101010101010101"),
            deployer: addr("0x0202020202020202020202020202020202020202"),
            init_hash: B256::ZERO,
            expected: addr("0x0303030303030303030303030303030303030303"),
            computed: addr("0x0404040404040404040404040404040404040404"),
        };
        let expected = mismatch.to_string();
        assert_eq!(
            value_error(map_v2_registration_err(V2RegistrationError::Create2(
                mismatch
            ))),
            expected
        );
        assert!(
            expected.starts_with("pool address does not match CREATE2 for chain 1"),
            "the mismatch vocabulary itself is pinned core-side: {expected}"
        );

        let err = map_v2_registration_err(V2RegistrationError::Register(
            RegisterV2PoolError::AlreadyRegistered {
                address: addr("0x0101010101010101010101010101010101010101"),
            },
        ));
        assert_eq!(
            message(err),
            "V2 pool already registered: address=0x0101010101010101010101010101010101010101"
        );
    }

    #[test]
    fn aerodrome_registration_err_pins_the_bare_mismatch() {
        let mismatch = degenbot_uniswap::deployments::AddressMismatch {
            chain_id: 8453,
            factory: addr("0x0101010101010101010101010101010101010101"),
            deployer: addr("0x0202020202020202020202020202020202020202"),
            init_hash: B256::ZERO,
            expected: addr("0x0303030303030303030303030303030303030303"),
            computed: addr("0x0404040404040404040404040404040404040404"),
        };
        let expected = mismatch.to_string();
        assert_eq!(
            value_error(map_aerodrome_registration_err(
                AerodromeRegistrationError::Create2(mismatch),
            )),
            expected
        );
    }

    #[test]
    fn resolve_v4_identity_err_pins_the_no_io_refusal() {
        assert_eq!(
            message(map_resolve_v4_identity_err(
                ResolveV4IdentityError::NoConstructionIo,
            )),
            "resolve_v4_identity: no ConstructionIo attached (requires an alloy provider)"
        );
    }

    #[test]
    fn journal_err_pins_the_no_state_vocabulary() {
        assert_eq!(
            value_error(journal_err_to_py(JournalError::NoStatePriorToBlock {
                block: 9
            })),
            "No pool state known prior to block 9"
        );
        assert_eq!(
            value_error(journal_err_to_py(JournalError::NoStateAtOrAfterBlock {
                block: 11
            })),
            "No pool state known at or after block 11"
        );
    }
}
