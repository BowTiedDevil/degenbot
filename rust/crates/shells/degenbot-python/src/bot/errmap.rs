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
//! `degenbot_pools` register errors keep a thin entry (`map_register_v2_err`)
//! that delegates to it; the V4 family's refusals cross only as the one
//! [`RefusalKind`] vocabulary (no typed-error mapper).
//!
//! Address parsing shares the same contract:
//! [`degenbot_bot::bot_core::parse_address_str`] owns the
//! `Invalid address '<input>': <source>` vocabulary; the shell only wraps
//! its error in the historical `ValueError`.
//!
//! Other domains keep their own mappers: `db::db_err_to_py`, `abi/*`,
//! `fork/*`, and `engine/strategy.rs` are out of scope here.

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use degenbot_bot::arb_engine::ConnectRefusal;
use degenbot_bot::bot_core::{
    BuildFailure, IdentityMismatchShape, LifecyclePrecondition, RefusalKind, RegisterRefusal,
    SnapshotLoadReason, UnsupportedSubject, VerifyOrigin,
};
use degenbot_pools::RegisterV2PoolError;
use degenbot_pools::RegisteredPoolFamily;

use crate::bot::engine::{
    DynamicFeePoolRejectedError, HighFeePoolRejectedError, HookedPoolRejectedError,
    PathRegistryFullError, PoolAlreadyRegisteredError, PoolBuildDecodeError, PoolBuildRpcError,
    SpecViolationError, SwapMathOverflowError, SwapNotComputableError,
    TickMapIntakeInconsistentError, UnsupportedPoolFamilyError, VerificationMismatchError,
    VerificationRpcError,
};

/// Map a `connect` refusal to a Python exception.
///
/// A pose refusal and a transport refusal are different answers ("you called
/// this at the wrong time" versus "the endpoint refused us"), so the text names
/// which one it is while the exception type stays the shell's historical
/// `RuntimeError`.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the mapper consumes the connect refusal, the shell's one error channel"
)]
pub(crate) fn map_connect_err(err: ConnectRefusal) -> PyErr {
    PyRuntimeError::new_err(err.to_string())
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
/// rendering, plus the construction route's taxonomy arms). The two I/O arms
/// are TYPED, not text: `Rpc` (the read failed — retry-worthy) and `Decoding`
/// (the read succeeded with a value no pool can hold — permanent for that
/// state) ride their own `PoolRegistrationError` subclasses, so a caller can
/// skip-by-type without parsing the message. Their message text is the
/// historical rendering, unchanged.
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
        // A stable fee refusal is a permanent refusal: the pool is out of
        // spec, so it surfaces as the spec class a caller skips by type.
        BuildFailure::Fee(detail) => {
            SpecViolationError::new_err(format!("pool build refused: out of spec ({detail})"))
        }
        BuildFailure::Rpc(detail) => {
            PoolBuildRpcError::new_err(format!("pool build RPC error: {detail}"))
        }
        BuildFailure::Spec(reason) => {
            SpecViolationError::new_err(format!("pool build refused: out of spec ({reason})"))
        }
        BuildFailure::Decoding(message) => {
            PoolBuildDecodeError::new_err(format!("pool build decode failure: {message}"))
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
        BuildFailure::TickMapInconsistent(detail) => TickMapIntakeInconsistentError::new_err(
            format!("Tracked tick map rejected at intake: {detail}"),
        ),
        BuildFailure::Transient(detail) => PyRuntimeError::new_err(detail),
    }
}

/// The registry-insert refusal's Python surface (the historical
/// `map_register_v*_err` rendering).
fn map_register_refusal(family: Option<RegisteredPoolFamily>, reason: RegisterRefusal) -> PyErr {
    match reason {
        RegisterRefusal::AlreadyRegistered {
            address, pool_key, ..
        } => match (address, pool_key) {
            (Some(address), _) => PoolAlreadyRegisteredError::new_err(format!(
                "{} already registered: address={address}",
                family_subject(family)
            )),
            (None, Some((pool_manager, pool_id))) => PoolAlreadyRegisteredError::new_err(format!(
                "v4 pool already registered: pool_manager={pool_manager}, pool_id=0x{}",
                alloy::hex::encode(pool_id)
            )),
            (None, None) => PoolAlreadyRegisteredError::new_err("pool already registered"),
        },
        RegisterRefusal::SpecViolation { detail } => SpecViolationError::new_err(format!(
            "{} registration failed: {detail}",
            family_subject(family)
        )),
        RegisterRefusal::Admission {
            failure,
            hook_flags,
            fee,
        } => map_admission(failure, hook_flags, fee),
    }
}

/// The V4 admission refusals' Python surface (the historical register
/// refusal's rendering, now the shared `map_refusal` path).
fn map_admission(failure: BuildFailure, hook_flags: Option<u16>, fee: Option<u32>) -> PyErr {
    match failure {
        BuildFailure::HookedPool => HookedPoolRejectedError::new_err(format!(
            "V4 pool has amount-modifying hooks (flags=0x{:04X}, mask=0x{:04X}) — excluded from arbitrage",
            hook_flags.unwrap_or_default(),
            degenbot_pools::AMOUNT_MODIFYING_HOOK_MASK
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

/// The registration messages' family subject: `<registry-tag> pool` when the
/// refusal names a family, the bare `pool` when it does not.
///
/// Every family renders its registry tag ([`RegisteredPoolFamily::tag`]) — the
/// one lowercase kebab vocabulary the core spells a family with, the same label
/// `degenbot_strategy::frame_pipeline::family_label` puts in an extract row — so
/// no family borrows another's spelling and a Balancer or Curve refusal names
/// itself. A family-less refusal names no family: it renders the bare noun,
/// never a default family's spelling standing in for one. The label is display
/// text; the operator wire's `family` field keeps its own `V2`/`V3`/`V4`
/// spelling, which this function never produces.
fn family_subject(family: Option<RegisteredPoolFamily>) -> String {
    match family {
        Some(family) => format!("{} pool", family.tag()),
        None => String::from("pool"),
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

/// Map the typed path-registration refusal: a full registry is the benign
/// `PathRegistryFullError` stop signal; every `Invalid` refusal keeps the
/// legacy `ValueError` with its verbatim message.
pub(crate) fn map_path_registration_err(
    err: degenbot_bot::arb_engine::PathRegistrationError,
) -> pyo3::PyErr {
    match err {
        degenbot_bot::arb_engine::PathRegistrationError::Invalid(msg) => PyValueError::new_err(msg),
        degenbot_bot::arb_engine::PathRegistrationError::RegistryFull { cap, registered } => {
            PathRegistryFullError::new_err(format!(
                "registered-path cap reached ({registered}/{cap}) — the crawl must stop discovery"
            ))
        }
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

/// Map a core [`SwapOpError`] from an exact-input calc op to its historical
/// Python surface (`op` is the pymethod name the fetch/mislabel surfaces
/// name). The overflow/on-chain-revert class is the typed
/// [`SwapMathOverflowError`]; the unknown-pool, unsupported-family, and
/// miss-recovery refusals keep their `ValueError` vocabulary. Every arm is
/// typed — no non-computed read folds to a zero quote.
pub(crate) fn map_calc_tokens_out_err(
    op: &'static str,
    err: &degenbot_bot::bot_core::SwapOpError,
) -> pyo3::PyErr {
    use degenbot_bot::bot_core::SwapOpError;
    match err {
        // The exact-input not-computable surface IS the overflow (one text, two
        // typed causes) — the on-chain `getAmountOut` revert class.
        SwapOpError::AmountOverflow | SwapOpError::NotComputable => swap_math_overflow(),
        SwapOpError::UnknownPool { pool_id } => swap_unknown_pool(*pool_id),
        SwapOpError::UnsupportedFamily { pool_id, family } => swap_family_gap(*pool_id, family),
        SwapOpError::FetchFailed { word } => swap_fetch_failed(op, *word),
        SwapOpError::FetchExhausted { word } => swap_fetch_exhausted(op, *word),
    }
}

/// Map a core [`SwapOpError`] from an exact-output calc op to its historical
/// Python surface (`op` is the pymethod name the surfaces name). The
/// request-amount overflow keeps the overflow text while the not-computable
/// overdraw is named its own class ([`SwapNotComputableError`]); the
/// unknown-pool, exact-output family gap, and miss-recovery refusals keep
/// their `ValueError` vocabulary. Every arm is typed.
pub(crate) fn map_calc_tokens_in_err(
    op: &'static str,
    err: &degenbot_bot::bot_core::SwapOpError,
) -> pyo3::PyErr {
    use degenbot_bot::bot_core::SwapOpError;
    match err {
        SwapOpError::AmountOverflow => swap_math_overflow(),
        SwapOpError::NotComputable => swap_not_computable(op),
        SwapOpError::UnknownPool { pool_id } => swap_unknown_pool(*pool_id),
        SwapOpError::UnsupportedFamily { pool_id, family } => {
            exact_output_family_gap(op, *pool_id, family)
        }
        SwapOpError::FetchFailed { word } => swap_fetch_failed(op, *word),
        SwapOpError::FetchExhausted { word } => swap_fetch_exhausted(op, *word),
    }
}

// --- Registration wrappers (CREATE2 verify → admission) ---

// --- Pool swap-sim surfaces (`bot_core::pool_ops`) ---

/// The on-chain `getAmountOut` SafeMath-parity overflow `ValueError` — the
/// shared surface for a request amount that did not fit the signed
/// user-perspective delta and a constant-product `uint256` intermediate
/// overflow (byte-identical historical text).
pub(crate) fn swap_math_overflow() -> PyErr {
    SwapMathOverflowError::new_err(
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

/// `ValueError` for a swap read the sim ran but could not compute (`op` is
/// the pymethod name the surface names) — the overdraw / exhausted-range /
/// non-computable-invariant class, distinct from the request-amount overflow
/// and from the miss-recovery refusals.
pub(crate) fn swap_not_computable(op: &str) -> PyErr {
    SwapNotComputableError::new_err(format!(
        "{op}: swap is not computable (overdraw, exhausted range, or a non-computable invariant)"
    ))
}

/// `ValueError` for a sparse-map miss whose word fetch itself failed (`op` is
/// the pymethod name the surface names).
pub(crate) fn swap_fetch_failed(op: &str, word: i32) -> PyErr {
    PyValueError::new_err(format!("{op}: sparse tick word {word} fetch failed"))
}

/// `ValueError` for a sparse-map miss whose word recovery was impossible or
/// exhausted (`op` is the pymethod name the surface names).
pub(crate) fn swap_fetch_exhausted(op: &str, word: i32) -> PyErr {
    PyValueError::new_err(format!(
        "{op}: sparse tick word {word} recovery exhausted (no fetcher, or a repeated miss)"
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
    use degenbot_bot::bot_core::IdentityDivergence;
    use degenbot_bot::bot_core::PoolBuilderError;
    use degenbot_bot::bot_core::RegistrationPhase;
    use degenbot_core::errors::ProviderError;
    use degenbot_core::fee::V2FeeRefusal;
    use degenbot_pools::RegisterAerodromePoolError;
    use degenbot_pools::RegisterV2PoolError;
    use degenbot_pools::RegisterV3PoolError;
    use degenbot_pools::RegisterV4PoolError;
    use degenbot_pools::SpecRefusal;
    use degenbot_pools::{SpecValue, SpecViolation};

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
            "v3 pool 0x.. block=1: tick 5 liquidityGross mismatch",
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
            "v3 pool 0x.. block=1: tick 5 liquidityGross mismatch"
        );

        let err = map_refusal(RefusalKind::verify_failed(
            RegistrationPhase::Verify,
            false,
            VerifyOrigin::Transport,
            "v3 pool 0x..: tickBitmap(0) RPC call failed: timeout".to_string(),
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
            "v3 pool 0x..: tickBitmap(0) RPC call failed: timeout"
        );
    }

    #[test]
    fn register_v2_err_pins_the_vocabulary() {
        let err = map_register_v2_err(&RegisterV2PoolError::AlreadyRegistered {
            address: addr("0x0101010101010101010101010101010101010101"),
        });
        assert_eq!(
            message(&err),
            "v2 pool already registered: address=0x0101010101010101010101010101010101010101"
        );

        let err = map_register_v2_err(&RegisterV2PoolError::SpecViolation(SpecViolation {
            field: "reserve0",
            value: SpecValue::U256(U256::from(7u8)),
            bound: "uint112 (≤ 2^112 − 1)",
        }));
        assert_eq!(
            message(&err),
            "v2 pool registration failed: field `reserve0` value 7 is out of bounds: uint112 (≤ 2^112 − 1)"
        );
    }

    /// The registration messages' family subject IS the registry tag: one
    /// vocabulary over the whole registry, no family borrowing another's
    /// spelling, and a family-less refusal rendering the bare noun — the
    /// renderer never invents a family. The closed-set census is
    /// [`RegisteredPoolFamily::tag`]'s own pin.
    #[test]
    fn family_subject_is_the_registry_tag_over_every_family() {
        for family in RegisteredPoolFamily::ALL {
            assert_eq!(
                family_subject(Some(family)),
                format!("{} pool", family.tag()),
                "{family:?} must not borrow another family's spelling"
            );
        }
        assert_eq!(
            family_subject(None),
            "pool",
            "a family-less refusal must render family-neutral"
        );
    }

    /// The same registration refusal renders the family it names and stays
    /// family-neutral when it names none: `v2 pool …` with a family, the bare
    /// `pool …` without one — a family-less refusal never borrows the default
    /// family's spelling.
    #[test]
    fn register_refusals_render_family_present_vs_family_absent() {
        let render = |family, reason| {
            message(&map_refusal(RefusalKind::RegisterRefused {
                family,
                phase: RegistrationPhase::Register,
                reason,
            }))
        };

        let already = RegisterRefusal::AlreadyRegistered {
            address: Some(addr("0x0101010101010101010101010101010101010101")),
            pool_key: None,
            detail: String::new(),
        };
        assert_eq!(
            render(Some(RegisteredPoolFamily::V2), already.clone()),
            "v2 pool already registered: address=0x0101010101010101010101010101010101010101"
        );
        assert_eq!(
            render(None, already),
            "pool already registered: address=0x0101010101010101010101010101010101010101"
        );

        let spec = RegisterRefusal::SpecViolation {
            detail: String::from(
                "field `reserve0` value 7 is out of bounds: uint112 (≤ 2^112 − 1)",
            ),
        };
        assert_eq!(
            render(Some(RegisteredPoolFamily::V2), spec.clone()),
            "v2 pool registration failed: field `reserve0` value 7 is out of bounds: uint112 (≤ 2^112 − 1)"
        );
        assert_eq!(
            render(None, spec),
            "pool registration failed: field `reserve0` value 7 is out of bounds: uint112 (≤ 2^112 − 1)"
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
            "v3 pool already registered: address=0x0202020202020202020202020202020202020202"
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
            "v3 pool registration failed: field `tick` value 887273 is out of bounds: |tick| ≤ 887272"
        );
    }

    #[test]
    fn register_v4_err_pins_the_vocabulary() {
        // The V4 refusals cross only as the one `RefusalKind` vocabulary —
        // the admission arms ride `BuildFailure` per the ledger's fold — so
        // the pins below construct the ledger arms directly and assert the
        // shared `map_refusal` rendering.
        let kind = |reason| RefusalKind::RegisterRefused {
            family: Some(RegisteredPoolFamily::V4),
            phase: RegistrationPhase::Register,
            reason,
        };

        let err = map_refusal(kind(RegisterRefusal::Admission {
            failure: BuildFailure::HookedPool,
            hook_flags: Some(1),
            fee: None,
        }));
        assert_eq!(
            message(&err),
            format!(
                "V4 pool has amount-modifying hooks (flags=0x0001, mask=0x{:04X}) — excluded from arbitrage",
                degenbot_pools::AMOUNT_MODIFYING_HOOK_MASK
            )
        );

        let err = map_refusal(kind(RegisterRefusal::Admission {
            failure: BuildFailure::DynamicFee,
            hook_flags: None,
            fee: Some(0x80_0000),
        }));
        assert_eq!(
            message(&err),
            "V4 pool has dynamic fee (fee=0x800000) — excluded from arbitrage"
        );

        let err = map_refusal(kind(RegisterRefusal::Admission {
            failure: BuildFailure::HighFee,
            hook_flags: None,
            fee: Some(70_000),
        }));
        assert_eq!(
            message(&err),
            "V4 pool fee (fee=70000) exceeds the cmd_executor's 2-byte encoding limit (65535) — excluded from arbitrage"
        );

        let already = RegisterV4PoolError::AlreadyRegistered {
            pool_manager: addr("0x0303030303030303030303030303030303030303"),
            pool_id: [0u8; 32],
        };
        let err = map_refusal(kind(RegisterRefusal::AlreadyRegistered {
            address: None,
            pool_key: Some((
                addr("0x0303030303030303030303030303030303030303"),
                [0u8; 32],
            )),
            detail: format!("{already:?}"),
        }));
        assert_eq!(
            message(&err),
            format!(
                "v4 pool already registered: pool_manager={}, pool_id=0x{}",
                addr("0x0303030303030303030303030303030303030303"),
                "0".repeat(64),
            )
        );

        let err = map_refusal(kind(RegisterRefusal::SpecViolation {
            detail: SpecViolation {
                field: "tick_spacing",
                value: SpecValue::I32(0),
                bound: "nonzero",
            }
            .to_string(),
        }));
        assert_eq!(
            message(&err),
            "v4 pool registration failed: field `tick_spacing` value 0 is out of bounds: nonzero"
        );
    }

    #[test]
    fn path_registration_err_pins_the_vocabulary() {
        let err =
            map_path_registration_err(degenbot_bot::arb_engine::PathRegistrationError::Invalid(
                "path crosses itself".to_string(),
            ));
        assert_eq!(value_error(&err), "path crosses itself");

        let err = map_path_registration_err(
            degenbot_bot::arb_engine::PathRegistrationError::RegistryFull {
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

        Python::attach(|py| {
            assert!(
                mapped(&PoolBuilderError::Spec(SpecRefusal::CurveBrokenPool))
                    .is_instance_of::<SpecViolationError>(py),
                "expected a SpecViolationError"
            );
        });
        // The reason rides the MESSAGE: two refusals that used to render one
        // undifferentiated string now name their own mechanism.
        assert_eq!(
            message(&mapped(&PoolBuilderError::Spec(
                SpecRefusal::CurveBrokenPool
            ))),
            "pool build refused: out of spec (curve broken pool)"
        );
        assert_eq!(
            message(&mapped(&PoolBuilderError::Spec(
                SpecRefusal::BalancerBrokenPool
            ))),
            "pool build refused: out of spec (balancer broken pool)"
        );
        // A stable fee refusal is permanent, so it surfaces as the spec class
        // a caller skips by type — never the retryable I/O classes.
        let fee_address = addr("0x0606060606060606060606060606060606060606");
        let fee_refusal = PoolBuilderError::AerodromeFee {
            address: fee_address,
            refusal: V2FeeRefusal::FeeExceedsDenominator,
        };
        Python::attach(|py| {
            assert!(
                mapped(&fee_refusal).is_instance_of::<SpecViolationError>(py),
                "expected a SpecViolationError"
            );
        });
        assert_eq!(
            message(&mapped(&fee_refusal)),
            format!(
                "pool build refused: out of spec (Aerodrome pool {fee_address} answered an unusable getFee: V2 fee exceeds its denominator)"
            )
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
    fn unmodeled_balancer_refusal_pins_the_reason() {
        // The unmodeled-family verdict never reaches a builder: the driver
        // resolved the identity (a Balancer Vault pool — `getPoolId()`
        // answered) and no family arm models its shape, so the construction
        // switchboard answers the taxonomy's reason directly. It must name the
        // Balancer family — the measured defect was this identity falling
        // through to the Curve arm and refusing as "curve required read
        // reverted".
        let err = map_refusal(RefusalKind::unbuilt_spec_refusal(
            SpecRefusal::BalancerUnmodeledFamily,
        ));
        Python::attach(|py| {
            assert!(
                err.is_instance_of::<SpecViolationError>(py),
                "expected a SpecViolationError"
            );
        });
        assert_eq!(
            message(&err),
            "pool build refused: out of spec (balancer unmodeled family)"
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
        Python::attach(|py| {
            assert!(
                map_build_refusal(
                    "build_curve_pool",
                    RefusalKind::from_builder_error(
                        &PoolBuilderError::Spec(SpecRefusal::V2Reserve),
                        None,
                        RegistrationPhase::Build,
                    ),
                )
                .is_instance_of::<SpecViolationError>(py),
                "expected a SpecViolationError"
            );
        });
    }

    #[test]
    fn v2_build_refusals_pin_the_no_io_prefix_and_routing() {
        assert_eq!(
            message(&map_build_refusal(
                "build_v2_pool",
                RefusalKind::missing_construction_io(
                    Some(RegisteredPoolFamily::V2),
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
            "v2 pool already registered: address=0x0101010101010101010101010101010101010101"
        );
    }

    #[test]
    fn v3_build_refusals_pin_the_race_answer() {
        assert_eq!(
            message(&map_refusal(RefusalKind::unreadable_identity(
                Some(RegisteredPoolFamily::V3),
                RegistrationPhase::Register,
                addr("0x0606060606060606060606060606060606060606"),
                "registry GET answered 0x0606060606060606060606060606060606060606 with no readable V3 identity".to_string(),
            ))),
            "build_v3_pool: registry GET answered 0x0606060606060606060606060606060606060606 with no readable V3 identity"
        );
    }

    #[test]
    fn calc_tokens_out_err_pins_the_vocabulary() {
        use degenbot_bot::bot_core::SwapOpError;
        let overflow = "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)";
        assert_eq!(
            value_error(&map_calc_tokens_out_err(
                "calculate_tokens_out",
                &SwapOpError::AmountOverflow
            )),
            overflow
        );
        assert_eq!(
            value_error(&map_calc_tokens_out_err(
                "calculate_tokens_out",
                &SwapOpError::NotComputable
            )),
            overflow
        );
        assert_eq!(
            value_error(&map_calc_tokens_out_err(
                "calculate_tokens_out",
                &SwapOpError::UnknownPool { pool_id: 5 }
            )),
            "swap_simulation: pool 5 is not registered"
        );
        assert_eq!(
            value_error(&map_calc_tokens_out_err(
                "calculate_tokens_out",
                &SwapOpError::UnsupportedFamily {
                    pool_id: 5,
                    family: "v9",
                }
            )),
            "swap_simulation: pool 5 family v9 is not supported for this operation"
        );
        assert_eq!(
            value_error(&map_calc_tokens_out_err(
                "calculate_tokens_out_with_fetch",
                &SwapOpError::FetchFailed { word: -7 }
            )),
            "calculate_tokens_out_with_fetch: sparse tick word -7 fetch failed"
        );
        assert_eq!(
            value_error(&map_calc_tokens_out_err(
                "calculate_tokens_out",
                &SwapOpError::FetchExhausted { word: 3 }
            )),
            "calculate_tokens_out: sparse tick word 3 recovery exhausted (no fetcher, or a repeated miss)"
        );
    }

    #[test]
    fn calc_tokens_in_err_pins_the_vocabulary() {
        use degenbot_bot::bot_core::SwapOpError;
        assert_eq!(
            value_error(&map_calc_tokens_in_err(
                "calculate_tokens_in",
                &SwapOpError::AmountOverflow
            )),
            "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)"
        );
        assert_eq!(
            value_error(&map_calc_tokens_in_err(
                "calculate_tokens_in",
                &SwapOpError::NotComputable
            )),
            "calculate_tokens_in: swap is not computable (overdraw, exhausted range, or a non-computable invariant)"
        );
        assert_eq!(
            value_error(&map_calc_tokens_in_err(
                "calculate_tokens_in",
                &SwapOpError::UnknownPool { pool_id: 5 }
            )),
            "swap_simulation: pool 5 is not registered"
        );
        assert_eq!(
            value_error(&map_calc_tokens_in_err(
                "calculate_tokens_in",
                &SwapOpError::UnsupportedFamily {
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
                Some(RegisteredPoolFamily::V2),
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
            "v2 pool already registered: address=0x0101010101010101010101010101010101010101"
        );
    }

    /// ADR-077 D3: the register path and the build path consume ONE identity
    /// verdict, so the same mismatch fact raises the SAME Python exception
    /// (type and message) whichever door reached it.
    #[test]
    fn identity_mismatch_raises_one_python_shape_for_both_paths() {
        let divergence = IdentityDivergence::Create2(address_mismatch(1));

        let register = map_refusal(RefusalKind::from_identity_divergence(
            Some(RegisteredPoolFamily::V2),
            &divergence,
        ));
        let build = map_refusal(RefusalKind::from_builder_error(
            &PoolBuilderError::Identity(Box::new(divergence)),
            Some(RegisteredPoolFamily::V2),
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
                Some(RegisteredPoolFamily::AerodromeV2),
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
            "aerodrome-v2 pool already registered: address=0x0505050505050505050505050505050505050505"
        );
    }

    #[test]
    fn construct_pool_refusals_pin_the_v4_no_io_refusal() {
        assert_eq!(
            message(&map_build_refusal(
                "construct_pool",
                RefusalKind::missing_construction_io(
                    Some(RegisteredPoolFamily::V4),
                    RegistrationPhase::Identity,
                ),
            )),
            "construct_pool: no ConstructionIo attached (requires an alloy provider)"
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
    fn pool_swap_refusal_surfaces_pin_their_vocabulary() {
        assert_eq!(
            value_error(&swap_not_computable("calculate_tokens_in")),
            "calculate_tokens_in: swap is not computable (overdraw, exhausted range, or a non-computable invariant)"
        );
        assert_eq!(
            value_error(&swap_fetch_failed("calculate_tokens_out_with_fetch", -7)),
            "calculate_tokens_out_with_fetch: sparse tick word -7 fetch failed"
        );
        assert_eq!(
            value_error(&swap_fetch_exhausted("calculate_tokens_out", 3)),
            "calculate_tokens_out: sparse tick word 3 recovery exhausted (no fetcher, or a repeated miss)"
        );
    }

    #[test]
    fn pool_override_and_handle_surfaces_pin_their_vocabulary() {
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

    /// The build taxonomy's I/O arms are typed: a transport hiccup (retry-worthy)
    /// and a permanent decode bug are distinguishable by exception type, and
    /// both stay under `PoolRegistrationError` so ONE `except` still covers the
    /// whole build/admission taxonomy. Messages are the historical text,
    /// unchanged.
    #[test]
    fn build_io_failures_raise_their_typed_class() {
        use crate::bot::engine::PoolRegistrationError;

        let refused = |failure: BuildFailure| {
            map_refusal(RefusalKind::BuildRefused {
                family: None,
                phase: RegistrationPhase::Build,
                failure,
            })
        };

        let rpc = refused(BuildFailure::Rpc("Request timeout: blip".to_string()));
        Python::attach(|py| {
            assert!(
                rpc.is_instance_of::<PoolBuildRpcError>(py),
                "a transport failure must surface as the retryable PoolBuildRpcError, got: {rpc}"
            );
            assert!(
                rpc.is_instance_of::<PoolRegistrationError>(py),
                "the typed build errors stay under the one admission parent"
            );
            assert!(
                !rpc.is_instance_of::<PoolBuildDecodeError>(py),
                "a transport failure is NOT a decode failure (distinct types)"
            );
        });
        assert_eq!(message(&rpc), "pool build RPC error: Request timeout: blip");

        let decode = refused(BuildFailure::Decoding("bad bytes".to_string()));
        Python::attach(|py| {
            assert!(
                decode.is_instance_of::<PoolBuildDecodeError>(py),
                "a decoded-value bug must surface as PoolBuildDecodeError, got: {decode}"
            );
            assert!(
                decode.is_instance_of::<PoolRegistrationError>(py),
                "the typed build errors stay under the one admission parent"
            );
            assert!(
                !decode.is_instance_of::<PoolBuildRpcError>(py),
                "a decode failure is NOT a transport failure (distinct types)"
            );
        });
        assert_eq!(message(&decode), "pool build decode failure: bad bytes");

        // The Spec arm is unchanged: a typed spec refusal keeps its own class.
        let spec = refused(BuildFailure::Spec(SpecRefusal::CurveBrokenPool));
        Python::attach(|py| {
            assert!(
                spec.is_instance_of::<SpecViolationError>(py),
                "the spec arm keeps its class (no regression), got: {spec}"
            );
            assert!(spec.is_instance_of::<PoolRegistrationError>(py));
        });
        assert_eq!(
            message(&spec),
            "pool build refused: out of spec (curve broken pool)"
        );
    }
}
