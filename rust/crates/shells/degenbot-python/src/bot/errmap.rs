//! Core-error → historical-Python-exception translation for the bot shell.
//!
//! One home for every mapper the `PyBot` / `PyArbEngine` / `PyLiquidityPool`
//! pymethods raise through. The historical Python message vocabulary is a
//! behavioral contract: each mapper's output text is pinned by the tests in
//! this module, and `build_paths`' exception classification rides the typed
//! hierarchy these constructors produce.
//!
//! [`map_refusal`] is the ONE mapper over the one refusal vocabulary
//! (ADR-077 D2): every registration-phase refusal — the register, build,
//! identity, verify, lifecycle, and snapshot-load families — is a
//! [`RefusalKind`] variant, and this module renders it. The typed
//! `degenbot_pools` register errors keep a thin entry (`map_register_v*_err`)
//! that delegates to it.
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
use degenbot_bot::bot_core::registration_ledger::{
    BuildFailure, IdentityMismatchShape, LifecyclePrecondition, PoolFamilyTag, RefusalKind,
    RegisterRefusal, SnapshotLoadReason, UnsupportedSubject, VerifyOrigin,
};
use degenbot_bot::bot_core::{RegisterV2PoolError, RegisterV4PoolError};
use degenbot_pools::state_history::JournalError;

use crate::bot::engine::{
    DynamicFeePoolRejectedError, HighFeePoolRejectedError, HookedPoolRejectedError,
    PathRegistryFullError, PoolAlreadyRegisteredError, SpecViolationError,
    UnsupportedPoolFamilyError, VerificationMismatchError, VerificationRpcError,
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
        DriverError::Verify(kind) => map_refusal(kind),
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

/// The ONE mapper over the refusal vocabulary (ADR-077 D2).
///
/// Every registration-phase refusal — the register, build, identity, verify,
/// lifecycle, and snapshot-load families — is a [`RefusalKind`] variant, and
/// this function is the single home of the historical Python exception
/// vocabulary each one surfaces as. The pymethod-prefixed no-`ConstructionIo`
/// arm is supplied by [`map_build_refusal`] (the build entries own their
/// method name).
pub(crate) fn map_refusal(kind: RefusalKind) -> PyErr {
    match kind {
        RefusalKind::MalformedInput { detail, .. } => PyValueError::new_err(detail),
        RefusalKind::IdentityMismatch { shape, .. } => match shape {
            IdentityMismatchShape::Create2 { detail, .. }
            | IdentityMismatchShape::MintedVsRegistered { detail, .. } => {
                PyValueError::new_err(detail)
            }
        },
        RefusalKind::MissingConstructionIo { .. } => {
            PyRuntimeError::new_err("no ConstructionIo attached (requires an alloy provider)")
        }
        RefusalKind::UnsupportedFamily { subject, detail, .. } => match subject {
            UnsupportedSubject::Pool { address } => UnsupportedPoolFamilyError::new_err(format!(
                "construction route refused pool at {address}: {detail}"
            )),
            UnsupportedSubject::Factory { factory } => PyRuntimeError::new_err(format!(
                "pool build unknown factory {factory} — no built-in DEX variant preset"
            )),
            UnsupportedSubject::UnknownIdentity { address } => PyValueError::new_err(format!(
                "pool build unknown identity at {address}: no identity selector answered"
            )),
        },
        RefusalKind::BuildRefused { failure, .. } => map_build_failure(failure),
        RefusalKind::RegisterRefused { family, reason, .. } => {
            map_register_refusal(family, reason)
        }
        RefusalKind::UnreadableIdentity { address, .. } => PyRuntimeError::new_err(format!(
            "build_v3_pool: registry GET answered {address} with no readable V3 identity"
        )),
        RefusalKind::SnapshotLoadFailed { reason, .. } => PyRuntimeError::new_err(format!(
            "load_snapshot_from_db failed: {}",
            snapshot_reason_detail(&reason)
        )),
        RefusalKind::VerifyFailed { origin, detail, .. } => map_verify_failure(origin, detail),
        RefusalKind::VerifyNotConfigured { detail, .. } => {
            PyRuntimeError::new_err(format!("verify not configured: {detail}"))
        }
        RefusalKind::LifecyclePrecondition { reason, .. } => match reason {
            LifecyclePrecondition::MissingVerifyProvider => VerificationRpcError::new_err(
                "registration verify requires an RPC provider for tracked pools — configure the bot's single provider",
            ),
            LifecyclePrecondition::MissingTickSpacing => PyRuntimeError::new_err(
                "registration verify requires the registered pool's tick spacing",
            ),
        },
    }
}

/// The build-entry variant of [`map_refusal`]: the no-`ConstructionIo` arm's
/// historical message carries the pymethod prefix, which only the build call
/// sites own.
pub(crate) fn map_build_refusal(method: &'static str, kind: RefusalKind) -> PyErr {
    if matches!(kind, RefusalKind::MissingConstructionIo { .. }) {
        return map_no_construction_io(method);
    }
    map_refusal(kind)
}

/// The typed builder-failure vocabulary (the historical `map_builder_err`
/// rendering, plus the construction route's taxonomy arms).
fn map_build_failure(failure: BuildFailure) -> PyErr {
    match failure {
        BuildFailure::HookedPool => {
            HookedPoolRejectedError::new_err("pool refused: carries an amount-modifying hook")
        }
        BuildFailure::DynamicFee => {
            DynamicFeePoolRejectedError::new_err("pool refused: dynamic fee")
        }
        BuildFailure::HighFee => HighFeePoolRejectedError::new_err(
            "pool refused: static fee exceeds the cmd_executor's 2-byte encoding limit",
        ),
        BuildFailure::Rpc(detail) => {
            PyRuntimeError::new_err(format!("pool build RPC error: {detail}"))
        }
        BuildFailure::Spec => PyRuntimeError::new_err("pool build out-of-spec V2 reserve"),
        BuildFailure::Decoding(message) => {
            PyRuntimeError::new_err(format!("pool build decode failure: {message}"))
        }
        BuildFailure::Db(detail) => {
            PyRuntimeError::new_err(format!("pool build DB read failed: {detail}"))
        }
        BuildFailure::MissingIdentity(message) => {
            PyValueError::new_err(format!("V4 identity incomplete: {message}"))
        }
        BuildFailure::TickAssembly(detail) => {
            PyValueError::new_err(format!("Tracked tick map rejected at intake: {detail}"))
        }
        BuildFailure::Transient(detail) => PyRuntimeError::new_err(detail),
    }
}

/// The registry-insert refusal's Python surface (the historical
/// `map_register_v*_err` rendering).
fn map_register_refusal(family: Option<PoolFamilyTag>, reason: RegisterRefusal) -> PyErr {
    match reason {
        RegisterRefusal::AlreadyRegistered {
            address, pool_key, ..
        } => match (address, pool_key) {
            (Some(address), _) => PoolAlreadyRegisteredError::new_err(format!(
                "{} pool already registered: address={address}",
                family_label(family)
            )),
            (None, Some((pool_manager, pool_id))) => PoolAlreadyRegisteredError::new_err(format!(
                "V4 pool already registered: pool_manager={pool_manager}, pool_id=0x{}",
                alloy::hex::encode(pool_id)
            )),
            (None, None) => PoolAlreadyRegisteredError::new_err("pool already registered"),
        },
        RegisterRefusal::SpecViolation { detail } => SpecViolationError::new_err(format!(
            "{} pool registration failed: {detail}",
            family_label(family)
        )),
        RegisterRefusal::Admission {
            failure,
            hook_flags,
            fee,
        } => map_admission(failure, hook_flags, fee),
    }
}

/// The V4 admission refusals' Python surface (the historical
/// `map_register_v4_err` rendering).
fn map_admission(failure: BuildFailure, hook_flags: Option<u16>, fee: Option<u32>) -> PyErr {
    match failure {
        BuildFailure::HookedPool => HookedPoolRejectedError::new_err(format!(
            "V4 pool has amount-modifying hooks (flags=0x{:04X}, mask=0x{:04X}) — excluded from arbitrage",
            hook_flags.unwrap_or_default(),
            degenbot_bot::bot_core::AMOUNT_MODIFYING_HOOK_MASK
        )),
        BuildFailure::DynamicFee => DynamicFeePoolRejectedError::new_err(format!(
            "V4 pool has dynamic fee (fee=0x{:06X}) — excluded from arbitrage",
            fee.unwrap_or_default()
        )),
        BuildFailure::HighFee => HighFeePoolRejectedError::new_err(format!(
            "V4 pool fee (fee={}) exceeds the cmd_executor's 2-byte encoding limit (65535) — excluded from arbitrage",
            fee.unwrap_or_default()
        )),
        other => map_build_failure(other),
    }
}

/// The family label the registration messages spell.
fn family_label(family: Option<PoolFamilyTag>) -> &'static str {
    match family {
        Some(PoolFamilyTag::V3) => "V3",
        Some(PoolFamilyTag::AerodromeV2) => "Aerodrome",
        Some(PoolFamilyTag::V4) => "V4",
        // The address-keyed families share the V2 rendering; only V2 reaches
        // a register refusal today.
        _ => "V2",
    }
}

/// A verify failure's Python surface (the historical `map_verify_err` /
/// `map_liquidity_verify_error` rendering): a mismatch is the fatal
/// `VerificationMismatchError`; a transport or provider-construction refusal
/// is the retryable `VerificationRpcError`; everything else is a
/// `RuntimeError`.
fn map_verify_failure(origin: VerifyOrigin, detail: String) -> PyErr {
    match origin {
        VerifyOrigin::Mismatch => VerificationMismatchError::new_err(detail),
        VerifyOrigin::Transport | VerifyOrigin::ProviderConstruction => {
            VerificationRpcError::new_err(detail)
        }
        VerifyOrigin::ProtocolViolation | VerifyOrigin::Other => PyRuntimeError::new_err(detail),
    }
}

/// The snapshot-load reason's historical rendering.
fn snapshot_reason_detail(reason: &SnapshotLoadReason) -> &str {
    match reason {
        SnapshotLoadReason::Db { detail }
        | SnapshotLoadReason::Decode { detail }
        | SnapshotLoadReason::OutOfRange { detail } => detail,
    }
}

// --- Pool registration (admission) ---

/// Map a [`RegisterV2PoolError`] to its Python surface through the one
/// refusal mapper (the V2 address-keyed insert refusal).
pub(crate) fn map_register_v2_err(err: &RegisterV2PoolError) -> PyErr {
    map_refusal(RefusalKind::from_register_v2_error(err))
}

/// Map a [`RegisterV4PoolError`] to its Python surface through the one
/// refusal mapper.
///
/// - `HookedPool` → [`HookedPoolRejectedError`] (V4 amount-modifying-hook
///   admission floor — the solver's concentrated liquidity math assumes no hook intervention).
/// - `DynamicFee` → [`DynamicFeePoolRejectedError`] (V4 dynamic-fee
///   admission floor — the solver assumes a fixed fee).
/// - `FeeExceedsEncoderLimit` → [`HighFeePoolRejectedError`] (V4 static-fee
///   exceeds the `cmd_executor`'s 2-byte encoding field; the fee is
///   protocol-valid but un-encodable and unprofitable).
/// - `AlreadyRegistered` → [`PoolAlreadyRegisteredError`] (duplicate
///   `(pool_manager, pool_id)` registration — a wiring/programming error
///   surfaced at admission time, unified with the V2/V3 twins under
///   `PoolRegistrationError`).
/// - `SpecViolation` → [`SpecViolationError`] (out-of-spec
///   sqrtPriceX96/tick/fee/tickSpacing).
///
/// The message text is byte-for-byte the legacy `Err(String)` formatting, so
/// `build_paths`'s classification (now `isinstance`, was substring) matches
/// the same diagnostics.
pub(crate) fn map_register_v4_err(err: &RegisterV4PoolError) -> PyErr {
    map_refusal(RefusalKind::from_register_v4_error(err))
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

/// Map the typed no-`ConstructionIo` refusal to the historical
/// method-prefixed `RuntimeError` (each pymethod owns its prefix).
pub(crate) fn map_no_construction_io(method: &'static str) -> pyo3::PyErr {
    PyRuntimeError::new_err(format!(
        "{method}: no ConstructionIo attached (requires an alloy provider)"
    ))
}

// --- Swap calculation ---

/// Map a core `CalcTokensOutError` to the shell's historical surfaces: the
/// `ValueError` for the overflow/on-chain-revert class, the
/// unknown-pool and unsupported-family `ValueError`s, and the legacy `0`
/// mapping for unrecovered sparse-map misses (done core-side).
pub(crate) fn map_calc_tokens_out_err(
    err: &degenbot_bot::bot_core::build_register::CalcTokensOutError,
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
    err: &degenbot_bot::bot_core::build_register::CalcTokensInError,
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

// --- Pool swap-sim surfaces (`bot_core::pool_ops`) ---

/// The on-chain `getAmountOut` SafeMath-parity overflow `ValueError` — the
/// shared surface for a request amount that did not fit the signed
/// user-perspective delta and a constant-product `uint256` intermediate
/// overflow (byte-identical historical text).
pub(crate) fn swap_math_overflow() -> PyErr {
    PyValueError::new_err(
        "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)",
    )
}

/// The `getAmountIn` twin of [`swap_math_overflow`] — the exact-output pair
/// calculation's overflow surface.
pub(crate) fn swap_math_in_overflow() -> PyErr {
    PyValueError::new_err(
        "Pool swap math overflowed uint256 intermediate (on-chain getAmountIn SafeMath revert)",
    )
}

/// `ValueError` for a swap op against an unregistered pool id.
pub(crate) fn swap_unknown_pool(pool_id: u64) -> PyErr {
    PyValueError::new_err(format!("swap_simulation: pool {pool_id} is not registered"))
}

/// `ValueError` for a swap op the registered family does not implement.
pub(crate) fn swap_family_gap(pool_id: u64, family: &str) -> PyErr {
    PyValueError::new_err(format!(
        "swap_simulation: pool {pool_id} family {family} is not supported for this operation"
    ))
}

/// `ValueError` for an exact-output request against a family with no
/// exact-output path (`op` is the pymethod name the surface names).
pub(crate) fn exact_output_family_gap(op: &str, pool_id: u64, family: &str) -> PyErr {
    PyValueError::new_err(format!(
        "{op}: pool {pool_id} family {family} has no exact-output path"
    ))
}

/// `ValueError` for an override sim against a non-concentrated-liquidity family (the family tag
/// renders in its historical `Debug` form).
pub(crate) fn override_family_gap(pool_id: u64, family: &str) -> PyErr {
    PyValueError::new_err(format!(
        "simulate_override: pool {pool_id} family {family:?} has no concentrated-liquidity override state"
    ))
}

/// `ValueError` for a pool handle whose id is not registered.
pub(crate) fn pool_unregistered(pool_id: u64) -> PyErr {
    PyValueError::new_err(format!("pool {pool_id} is not registered"))
}

/// The archived V4 hooked-pool approximation exception — an amount-modifying
/// hook may have invalidated the standard math, so the approximate deltas
/// ride the exception text instead of a silently wrong number.
pub(crate) fn hooked_pool_approximation(
    consumed: alloy::primitives::I256,
    delivered: alloy::primitives::U256,
) -> PyErr {
    crate::bot::engine::PossibleInaccurateResult::new_err(format!(
        "pool has an amount-modifying V4 hook; approximation consumed={consumed} delivered={delivered}"
    ))
}

/// `ValueError` carrying the `encode_swap:`-prefixed encoder detail.
pub(crate) fn encode_swap_failed(detail: &str) -> PyErr {
    PyValueError::new_err(format!("encode_swap: {detail}"))
}

#[cfg(test)]
mod tests {
    //! Pin the historical message vocabulary per mapper. The Python seam and
    //! its fixtures classify on the exact text (or the typed exception) these
    //! constructors produce, so the strings below are a behavioral contract.
    //! Addresses used in pins are all-digit hex so their rendering is
    //! identical under any case convention.

    #![expect(clippy::unwrap_used)]

    use super::*;
    use alloy::primitives::{Address, B256, U256};
    use degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError;
    use degenbot_bot::bot_core::pool_builder::parity::IdentityDivergence;
    use degenbot_bot::bot_core::registration_ledger::RegistrationPhase;
    use degenbot_bot::bot_core::{
        RegisterAerodromePoolError, RegisterV2PoolError, RegisterV3PoolError, RegisterV4PoolError,
    };
    use degenbot_core::errors::ProviderError;
    use degenbot_pools::spec_bounds::{SpecValue, SpecViolation};

    fn addr(s: &str) -> Address {
        s.parse().unwrap()
    }

    /// A fatal verify mismatch at the verify phase.
    fn verify_mismatch(detail: &str) -> RefusalKind {
        RefusalKind::verify_failed(
            RegistrationPhase::Verify,
            true,
            VerifyOrigin::Mismatch,
            detail.to_owned(),
        )
    }

    /// A `CREATE2` recomputation mismatch — the ONE fact the register path and
    /// the build path both report.
    fn address_mismatch(chain_id: u64) -> degenbot_uniswap::deployments::AddressMismatch {
        degenbot_uniswap::deployments::AddressMismatch {
            chain_id,
            factory: addr("0x0101010101010101010101010101010101010101"),
            deployer: addr("0x0202020202020202020202020202020202020202"),
            init_hash: B256::ZERO,
            expected: addr("0x0303030303030303030303030303030303030303"),
            computed: addr("0x0404040404040404040404040404040404040404"),
        }
    }

    /// `PyErr`'s `Display` renders `<ExceptionClass>: <message>`; the pins
    /// assert on the message the Python caller sees.
    fn strip_class(rendered: String) -> String {
        match rendered.split_once(": ") {
            Some((_class, rest)) => rest.to_string(),
            None => rendered,
        }
    }

    fn message(err: &PyErr) -> String {
        strip_class(err.to_string())
    }

    fn value_error(err: &PyErr) -> String {
        let rendered = Python::attach(|py| {
            assert!(
                err.is_instance_of::<PyValueError>(py),
                "expected a ValueError, got: {err}"
            );
            err.to_string()
        });
        strip_class(rendered)
    }

    fn runtime_error(err: &PyErr) -> String {
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
            runtime_error(&err),
            DriverError::NoResultReceiver.to_string(),
            "the receiver refusal keeps Display as the single source of its text"
        );
        let err = map_driver_err(DriverError::SessionState("already subscribed".to_string()));
        assert_eq!(runtime_error(&err), "already subscribed");
    }

    #[test]
    fn driver_lifecycle_err_pins_the_verify_vocabulary() {
        let err = map_driver_lifecycle_err(DriverError::Verify(
            RefusalKind::lifecycle_precondition(LifecyclePrecondition::MissingVerifyProvider),
        ));
        Python::attach(|py| {
            assert!(err.is_instance_of::<VerificationRpcError>(py));
        });
        assert_eq!(
            message(&err),
            "registration verify requires an RPC provider for tracked pools — configure the bot's single provider"
        );

        let err = map_driver_lifecycle_err(DriverError::Verify(
            RefusalKind::lifecycle_precondition(LifecyclePrecondition::MissingTickSpacing),
        ));
        assert_eq!(
            runtime_error(&err),
            "registration verify requires the registered pool's tick spacing"
        );
    }

    #[test]
    fn verify_lifecycle_refusals_pin_the_typed_vocabulary() {
        let err = map_refusal(verify_mismatch("tick drift"));
        Python::attach(|py| {
            assert!(err.is_instance_of::<VerificationMismatchError>(py));
        });
        assert_eq!(message(&err), "tick drift");

        let err = map_refusal(RefusalKind::verify_failed(
            RegistrationPhase::Verify,
            false,
            VerifyOrigin::ProviderConstruction,
            "http down".to_string(),
        ));
        Python::attach(|py| {
            assert!(err.is_instance_of::<VerificationRpcError>(py));
        });
        assert_eq!(message(&err), "http down");

        let err = map_refusal(RefusalKind::verify_failed(
            RegistrationPhase::Verify,
            false,
            VerifyOrigin::Transport,
            "timeout".to_string(),
        ));
        Python::attach(|py| {
            assert!(err.is_instance_of::<VerificationRpcError>(py));
        });
        assert_eq!(message(&err), "timeout");

        let err = map_refusal(RefusalKind::verify_failed(
            RegistrationPhase::Verify,
            true,
            VerifyOrigin::Other,
            "boom".to_string(),
        ));
        assert_eq!(runtime_error(&err), "boom");

        let err = map_refusal(RefusalKind::verify_not_configured(
            RegistrationPhase::Verify,
            "set the rpc url".to_string(),
        ));
        assert_eq!(
            runtime_error(&err),
            "verify not configured: set the rpc url"
        );
    }

    #[test]
    fn liquidity_verify_refusals_pin_the_typed_vocabulary() {
        let err = map_refusal(verify_mismatch(
            "V3 pool 0x.. block=1: tick 5 liquidityGross mismatch",
        ));
        Python::attach(|py| {
            assert!(
                err.is_instance_of::<VerificationMismatchError>(py),
                "a map divergence must surface as VerificationMismatchError (fatal), not PyRuntimeError"
            );
            assert!(
                !err.is_instance_of::<VerificationRpcError>(py),
                "genuine mismatch is NOT an Rpc error (distinct types)"
            );
        });
        assert_eq!(
            message(&err),
            "V3 pool 0x.. block=1: tick 5 liquidityGross mismatch"
        );

        let err = map_refusal(RefusalKind::verify_failed(
            RegistrationPhase::Verify,
            false,
            VerifyOrigin::Transport,
            "V3 pool 0x..: tickBitmap(0) RPC call failed: timeout".to_string(),
        ));
        Python::attach(|py| {
            assert!(
                err.is_instance_of::<VerificationRpcError>(py),
                "a transport refusal must surface as VerificationRpcError (retryable), not PyRuntimeError"
            );
            assert!(
                !err.is_instance_of::<VerificationMismatchError>(py),
                "RPC transport failure is NOT a mismatch (distinct types)"
            );
        });
        assert_eq!(
            message(&err),
            "V3 pool 0x..: tickBitmap(0) RPC call failed: timeout"
        );
    }

    #[test]
    fn register_v2_err_pins_the_vocabulary() {
        let err = map_register_v2_err(&RegisterV2PoolError::AlreadyRegistered {
            address: addr("0x0101010101010101010101010101010101010101"),
        });
        assert_eq!(
            message(&err),
            "V2 pool already registered: address=0x0101010101010101010101010101010101010101"
        );

        let err = map_register_v2_err(&RegisterV2PoolError::SpecViolation(SpecViolation {
            field: "reserve0",
            value: SpecValue::U256(U256::from(7u8)),
            bound: "uint112 (≤ 2^112 − 1)",
        }));
        assert_eq!(
            message(&err),
            "V2 pool registration failed: field `reserve0` value 7 is out of bounds: uint112 (≤ 2^112 − 1)"
        );
    }

    #[test]
    fn register_v3_refusals_pin_the_vocabulary() {
        let err = map_refusal(RefusalKind::from_register_v3_error(
            &RegisterV3PoolError::AlreadyRegistered {
                address: addr("0x0202020202020202020202020202020202020202"),
            },
        ));
        assert_eq!(
            message(&err),
            "V3 pool already registered: address=0x0202020202020202020202020202020202020202"
        );

        let err = map_refusal(RefusalKind::from_register_v3_error(
            &RegisterV3PoolError::SpecViolation(SpecViolation {
                field: "tick",
                value: SpecValue::I32(887_273),
                bound: "|tick| ≤ 887272",
            }),
        ));
        assert_eq!(
            message(&err),
            "V3 pool registration failed: field `tick` value 887273 is out of bounds: |tick| ≤ 887272"
        );
    }

    #[test]
    fn register_v4_err_pins_the_vocabulary() {
        let err = map_register_v4_err(&RegisterV4PoolError::HookedPool { hook_flags: 1 });
        assert_eq!(
            message(&err),
            format!(
                "V4 pool has amount-modifying hooks (flags=0x0001, mask=0x{:04X}) — excluded from arbitrage",
                degenbot_bot::bot_core::AMOUNT_MODIFYING_HOOK_MASK
            )
        );

        let err = map_register_v4_err(&RegisterV4PoolError::DynamicFee { fee: 0x80_0000 });
        assert_eq!(
            message(&err),
            "V4 pool has dynamic fee (fee=0x800000) — excluded from arbitrage"
        );

        let err = map_register_v4_err(&RegisterV4PoolError::FeeExceedsEncoderLimit { fee: 70_000 });
        assert_eq!(
            message(&err),
            "V4 pool fee (fee=70000) exceeds the cmd_executor's 2-byte encoding limit (65535) — excluded from arbitrage"
        );

        let err = map_register_v4_err(&RegisterV4PoolError::AlreadyRegistered {
            pool_manager: addr("0x0303030303030303030303030303030303030303"),
            pool_id: [0u8; 32],
        });
        assert_eq!(
            message(&err),
            format!(
                "V4 pool already registered: pool_manager={}, pool_id=0x{}",
                addr("0x0303030303030303030303030303030303030303"),
                "0".repeat(64),
            )
        );

        let err = map_register_v4_err(&RegisterV4PoolError::SpecViolation(SpecViolation {
            field: "tick_spacing",
            value: SpecValue::I32(0),
            bound: "nonzero",
        }));
        assert_eq!(
            message(&err),
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
        assert_eq!(value_error(&err), "path crosses itself");

        let err = map_path_registration_err(
            degenbot_bot::arb_engine::lifecycle::PathRegistrationError::RegistryFull {
                cap: 100,
                registered: 128,
            },
        );
        assert_eq!(
            message(&err),
            "registered-path cap reached (128/100) — the crawl must stop discovery"
        );
    }

    #[test]
    fn builder_refusals_pin_the_vocabulary() {
        let mapped = |err: &PoolBuilderError| {
            map_refusal(RefusalKind::from_builder_error(
                err,
                None,
                RegistrationPhase::Build,
            ))
        };

        assert_eq!(
            message(&mapped(&PoolBuilderError::Spec)),
            "pool build out-of-spec V2 reserve"
        );
        // The identity verdict is the ONE exception the register path and the
        // build path both raise: the deployment table's own mismatch text.
        let mismatch = address_mismatch(1);
        assert_eq!(
            value_error(&mapped(&PoolBuilderError::Identity(Box::new(
                IdentityDivergence::Create2(mismatch),
            )))),
            mismatch.to_string()
        );
        assert_eq!(
            message(&mapped(&PoolBuilderError::Decoding {
                message: "bad data".to_string(),
            })),
            "pool build decode failure: bad data"
        );
        assert_eq!(
            message(&mapped(&PoolBuilderError::MissingIdentity {
                message: "no ticks".to_string(),
            })),
            "V4 identity incomplete: no ticks"
        );
        assert_eq!(
            message(&mapped(&PoolBuilderError::Rpc(ProviderError::Timeout {
                message: "blip".to_string(),
            }))),
            "pool build RPC error: Request timeout: blip"
        );
        let factory = addr("0x0404040404040404040404040404040404040404");
        assert_eq!(
            message(&mapped(&PoolBuilderError::UnknownVariant { factory })),
            format!("pool build unknown factory {factory} — no built-in DEX variant preset")
        );
        let address = addr("0x0505050505050505050505050505050505050505");
        assert_eq!(
            message(&mapped(&PoolBuilderError::UnknownPoolIdentity { address })),
            format!("pool build unknown identity at {address}: no identity selector answered")
        );
    }

    #[test]
    fn no_construction_io_pins_the_method_prefix() {
        assert_eq!(
            message(&map_no_construction_io("build_v2_pool")),
            "build_v2_pool: no ConstructionIo attached (requires an alloy provider)"
        );
    }

    #[test]
    fn build_refusals_pin_the_no_io_prefix_and_builder_routing() {
        assert_eq!(
            message(&map_build_refusal(
                "build_curve_pool",
                RefusalKind::missing_construction_io(None, RegistrationPhase::Build),
            )),
            "build_curve_pool: no ConstructionIo attached (requires an alloy provider)"
        );
        assert_eq!(
            message(&map_build_refusal(
                "build_curve_pool",
                RefusalKind::from_builder_error(
                    &PoolBuilderError::Spec,
                    None,
                    RegistrationPhase::Build,
                ),
            )),
            "pool build out-of-spec V2 reserve"
        );
    }

    #[test]
    fn v2_build_refusals_pin_the_no_io_prefix_and_routing() {
        assert_eq!(
            message(&map_build_refusal(
                "build_v2_pool",
                RefusalKind::missing_construction_io(
                    Some(PoolFamilyTag::V2),
                    RegistrationPhase::Build,
                ),
            )),
            "build_v2_pool: no ConstructionIo attached (requires an alloy provider)"
        );
        assert_eq!(
            message(&map_refusal(RefusalKind::from_register_v2_error(
                &RegisterV2PoolError::AlreadyRegistered {
                    address: addr("0x0101010101010101010101010101010101010101"),
                },
            ))),
            "V2 pool already registered: address=0x0101010101010101010101010101010101010101"
        );
    }

    #[test]
    fn v3_build_refusals_pin_the_race_answer() {
        assert_eq!(
            message(&map_refusal(RefusalKind::unreadable_identity(
                Some(PoolFamilyTag::V3),
                RegistrationPhase::Register,
                addr("0x0606060606060606060606060606060606060606"),
                "registry GET answered 0x0606060606060606060606060606060606060606 with no readable V3 identity".to_string(),
            ))),
            "build_v3_pool: registry GET answered 0x0606060606060606060606060606060606060606 with no readable V3 identity"
        );
    }

    #[test]
    fn calc_tokens_out_err_pins_the_vocabulary() {
        use degenbot_bot::bot_core::build_register::CalcTokensOutError;
        let overflow = "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)";
        assert_eq!(
            value_error(&map_calc_tokens_out_err(&CalcTokensOutError::Overflow)),
            overflow
        );
        assert_eq!(
            value_error(&map_calc_tokens_out_err(&CalcTokensOutError::NotComputable)),
            overflow
        );
        assert_eq!(
            value_error(&map_calc_tokens_out_err(&CalcTokensOutError::UnknownPool {
                pool_id: 5
            })),
            "swap_simulation: pool 5 is not registered"
        );
        assert_eq!(
            value_error(&map_calc_tokens_out_err(
                &CalcTokensOutError::UnsupportedFamily {
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
            value_error(&map_calc_tokens_in_err(&CalcTokensInError::Overflow)),
            "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)"
        );
        assert_eq!(
            value_error(&map_calc_tokens_in_err(
                &CalcTokensInError::UnsupportedFamily {
                    pool_id: 5,
                    family: "v9",
                }
            )),
            "calculate_tokens_in: pool 5 family v9 has no exact-output path"
        );
    }

    #[test]
    fn register_refusals_pin_the_create2_and_admission_vocabulary() {
        let mismatch = address_mismatch(1);
        let expected = mismatch.to_string();
        assert_eq!(
            value_error(&map_refusal(RefusalKind::from_identity_divergence(
                Some(PoolFamilyTag::V2),
                &IdentityDivergence::Create2(mismatch),
            ))),
            expected
        );
        assert!(
            expected.starts_with("pool address does not match CREATE2 for chain 1"),
            "the mismatch vocabulary itself is pinned core-side: {expected}"
        );

        let err = map_refusal(RefusalKind::from_register_v2_error(
            &RegisterV2PoolError::AlreadyRegistered {
                address: addr("0x0101010101010101010101010101010101010101"),
            },
        ));
        assert_eq!(
            message(&err),
            "V2 pool already registered: address=0x0101010101010101010101010101010101010101"
        );
    }

    /// ADR-077 D3: the register path and the build path consume ONE identity
    /// verdict, so the same mismatch fact raises the SAME Python exception
    /// (type and message) whichever door reached it.
    #[test]
    fn identity_mismatch_raises_one_python_shape_for_both_paths() {
        let divergence = IdentityDivergence::Create2(address_mismatch(1));

        let register = map_refusal(RefusalKind::from_identity_divergence(
            Some(PoolFamilyTag::V2),
            &divergence,
        ));
        let build = map_refusal(RefusalKind::from_builder_error(
            &PoolBuilderError::Identity(Box::new(divergence)),
            Some(PoolFamilyTag::V2),
            RegistrationPhase::Build,
        ));

        assert_eq!(
            value_error(&register),
            value_error(&build),
            "one identity fact, one Python exception shape"
        );
    }

    #[test]
    fn aerodrome_registration_refusals_pin_the_create2_and_admission_vocabulary() {
        let mismatch = address_mismatch(8453);
        let expected = mismatch.to_string();
        assert_eq!(
            value_error(&map_refusal(RefusalKind::from_identity_divergence(
                Some(PoolFamilyTag::AerodromeV2),
                &IdentityDivergence::Create2(mismatch),
            ))),
            expected
        );

        // The typed admission twin of the V2 arm: duplicate-address
        // refusals surface as PoolAlreadyRegisteredError (a
        // PoolRegistrationError subclass), not a bare ValueError.
        let err = map_refusal(RefusalKind::from_register_aerodrome_error(
            &RegisterAerodromePoolError::AlreadyRegistered {
                address: addr("0x0505050505050505050505050505050505050505"),
            },
        ));
        Python::attach(|py| {
            assert!(
                err.is_instance_of::<PoolAlreadyRegisteredError>(py),
                "Aerodrome duplicate-address refusal must surface as PoolAlreadyRegisteredError"
            );
        });
        assert_eq!(
            message(&err),
            "Aerodrome pool already registered: address=0x0505050505050505050505050505050505050505"
        );
    }

    #[test]
    fn resolve_v4_identity_refusals_pin_the_no_io_refusal() {
        assert_eq!(
            message(&map_build_refusal(
                "resolve_v4_identity",
                RefusalKind::missing_construction_io(
                    Some(PoolFamilyTag::V4),
                    RegistrationPhase::Identity,
                ),
            )),
            "resolve_v4_identity: no ConstructionIo attached (requires an alloy provider)"
        );
    }

    #[test]
    fn pool_swap_math_overflow_pins_the_on_chain_revert_text() {
        assert_eq!(
            value_error(&swap_math_overflow()),
            "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)"
        );
        assert_eq!(
            value_error(&swap_math_in_overflow()),
            "Pool swap math overflowed uint256 intermediate (on-chain getAmountIn SafeMath revert)"
        );
    }

    #[test]
    fn pool_swap_family_surfaces_pin_their_vocabulary() {
        assert_eq!(
            value_error(&swap_unknown_pool(7)),
            "swap_simulation: pool 7 is not registered"
        );
        assert_eq!(
            value_error(&swap_family_gap(7, "v2")),
            "swap_simulation: pool 7 family v2 is not supported for this operation"
        );
        assert_eq!(
            value_error(&exact_output_family_gap("calculate_tokens_in", 7, "curve")),
            "calculate_tokens_in: pool 7 family curve has no exact-output path"
        );
        assert_eq!(
            value_error(&exact_output_family_gap(
                "simulate_exact_output_swap_with_fetch",
                9,
                "balancer-stable"
            )),
            "simulate_exact_output_swap_with_fetch: pool 9 family balancer-stable has no exact-output path"
        );
    }

    #[test]
    fn pool_override_and_handle_surfaces_pin_their_vocabulary() {
        // The family tag renders in its historical Debug (quoted) form.
        assert_eq!(
            value_error(&override_family_gap(7, "v2")),
            "simulate_override: pool 7 family \"v2\" has no concentrated-liquidity override state"
        );
        assert_eq!(
            value_error(&pool_unregistered(12)),
            "pool 12 is not registered"
        );
        assert_eq!(
            value_error(&encode_swap_failed(
                "pool 7 family [\"v3\"] has no swap encoder"
            )),
            "encode_swap: pool 7 family [\"v3\"] has no swap encoder"
        );
    }

    #[test]
    fn pool_hooked_pool_approximation_pins_the_archived_text() {
        // The shell passes the negated (positive) consumed delta; the
        // mapper renders it verbatim.
        let consumed =
            alloy::primitives::I256::try_from(alloy::primitives::U256::from(102u64)).unwrap();
        let rendered = Python::attach(|_py| {
            let err = hooked_pool_approximation(consumed, alloy::primitives::U256::from(100u64));
            err.to_string()
        });
        assert!(
            rendered.ends_with(
                "pool has an amount-modifying V4 hook; approximation consumed=102 delivered=100"
            ),
            "rendered: {rendered}"
        );
    }

    #[test]
    fn journal_err_pins_the_no_state_vocabulary() {
        assert_eq!(
            value_error(&journal_err_to_py(JournalError::NoStatePriorToBlock {
                block: 9
            })),
            "No pool state known prior to block 9"
        );
        assert_eq!(
            value_error(&journal_err_to_py(JournalError::NoStateAtOrAfterBlock {
                block: 11
            })),
            "No pool state known at or after block 11"
        );
    }
}
