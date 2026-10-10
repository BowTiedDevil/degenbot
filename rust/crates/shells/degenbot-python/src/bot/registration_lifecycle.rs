//! The registration-lifecycle value and its one `PyO3` entry pair.
//!
//! The choreography a concentrated-liquidity pool runs between admission and
//! solvability — quarantine, seed verification, drain + pin, post-drain
//! verification, `Live` — is core-owned (ADR-022 D1): `EngineDriver` in
//! `degenbot-bot` is its only home, and a Rust consumer reaches it through the
//! driver's own `run_v3_registration_lifecycle{,_sync,_with_retry*}` entries.
//!
//! What this module owns is the Python seam around those entries, once:
//! resolving the family's target (a pool address, or the
//! `(pool_manager, pool_id)` pair), selecting how the caller parks the core
//! future, converting the seam's `RetryPolicy` to the core type, and mapping a
//! core failure to its typed Python exception. [`PyRegistrationLifecycle`]
//! carries all of that, so "which driver entry point does this caller need" is
//! a value rather than a wrapper per (family x mode) cell. The bot handle and
//! the arbitrage engine each expose ONE `run_registration_lifecycle` pymethod
//! over the single implementation below.

use std::sync::Arc;

use alloy::primitives::Address;
use pyo3::exceptions::PyValueError;

use crate::bot::engine::{hex_string_to_pool_id, PyArbEngine};
use crate::bot::errmap::{map_driver_lifecycle_err, map_verify_lifecycle_error};
use crate::bot::PyBot;
use crate::config::RetryPolicy;
use crate::prelude::*;
use degenbot_bot::arb_engine::EngineDriver;

/// The concentrated-liquidity family a lifecycle targets.
#[derive(Clone, Copy)]
enum Family {
    /// Keyed by the pool's own contract address.
    V3,
    /// Keyed by the `(pool_manager, pool_id)` pair.
    V4,
}

impl Family {
    fn parse(value: &str) -> PyResult<Self> {
        match value {
            "v3" => Ok(Self::V3),
            "v4" => Ok(Self::V4),
            other => Err(PyValueError::new_err(format!(
                "registration lifecycle family must be 'v3' or 'v4', got {other:?}"
            ))),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::V3 => "v3",
            Self::V4 => "v4",
        }
    }
}

/// How the caller parks the core-owned choreography.
#[derive(Clone, Copy)]
enum Mode {
    /// An awaitable bridged onto the Python event loop.
    Async,
    /// A `block_on` inside `py.detach` — the seat-thread twin.
    Blocking,
    /// The same `block_on`, wrapped in the core's bounded retry dance.
    BlockingWithRetry,
}

impl Mode {
    fn parse(value: &str) -> PyResult<Self> {
        match value {
            "async" => Ok(Self::Async),
            "blocking" => Ok(Self::Blocking),
            "blocking_with_retry" => Ok(Self::BlockingWithRetry),
            other => Err(PyValueError::new_err(format!(
                "registration lifecycle mode must be 'async', 'blocking', or \
                 'blocking_with_retry', got {other:?}"
            ))),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Async => "async",
            Self::Blocking => "blocking",
            Self::BlockingWithRetry => "blocking_with_retry",
        }
    }
}

/// The typed target a lifecycle addresses, once its Python spelling has been
/// resolved.
#[derive(Debug)]
enum Target {
    V3(Address),
    V4(Address, [u8; 32]),
}

/// One pool's registration verify-lifecycle, as a Python caller states it.
///
/// The value says *what* to run and *how* the caller parks it; the
/// choreography, its at-most-once claim, and its failure classification stay
/// core-owned. `family` is `"v3"` (targeting `address`) or `"v4"` (targeting
/// the `(pool_manager, pool_id_hex)` pair); `mode` is `"async"`, `"blocking"`,
/// or `"blocking_with_retry"`. `snapshot_block` is the seed block the core
/// verifies against, and `policy` carries the retry dance's bounds — required
/// for `"blocking_with_retry"`, refused for the other modes.
#[pyclass(name = "RegistrationLifecycle", module = "degenbot._ffi", frozen)]
pub struct PyRegistrationLifecycle {
    family: Family,
    address: Option<String>,
    pool_manager: Option<String>,
    pool_id_hex: Option<String>,
    snapshot_block: Option<u64>,
    mode: Mode,
    policy: Option<Py<RetryPolicy>>,
}

impl PyRegistrationLifecycle {
    /// Resolve the family's Python-supplied target into typed core values.
    ///
    /// # Errors
    ///
    /// `ValueError` carrying the historical target-refusal vocabulary
    /// (`Invalid V3 address: ...` / `Invalid pool_manager: ...` /
    /// `Invalid pool_id: ...`).
    fn target(&self) -> PyResult<Target> {
        match self.family {
            Family::V3 => Ok(Target::V3(self.v3_address()?)),
            Family::V4 => {
                let pool_manager: Address = self
                    .pool_manager
                    .as_deref()
                    .unwrap_or_default()
                    .parse()
                    .map_err(|e| PyValueError::new_err(format!("Invalid pool_manager: {e}")))?;
                let pool_id =
                    hex_string_to_pool_id(self.pool_id_hex.as_deref().unwrap_or_default())
                        .map_err(|e| PyValueError::new_err(format!("Invalid pool_id: {e}")))?;
                Ok(Target::V4(pool_manager, pool_id))
            }
        }
    }

    fn v3_address(&self) -> PyResult<Address> {
        self.address
            .as_deref()
            .unwrap_or_default()
            .parse()
            .map_err(|e| PyValueError::new_err(format!("Invalid V3 address: {e}")))
    }

    /// The core retry policy the value carries, for the retry-wrapped mode.
    ///
    /// # Errors
    ///
    /// `ValueError` if the value carries no policy (the constructor refuses
    /// that combination, so this is a belt-and-braces arm).
    fn core_policy(&self, py: Python<'_>) -> PyResult<degenbot_core::retry::RetryPolicy> {
        self.policy
            .as_ref()
            .map(|policy| policy.borrow(py).to_core())
            .ok_or_else(|| {
                PyValueError::new_err(
                    "registration lifecycle mode 'blocking_with_retry' requires a policy",
                )
            })
    }
}

#[pymethods]
impl PyRegistrationLifecycle {
    /// State one pool's registration verify-lifecycle.
    ///
    /// The family picks the target's shape, the mode picks how the caller
    /// parks the core choreography, and the policy bounds the retry dance that
    /// `"blocking_with_retry"` selects.
    ///
    /// # Errors
    ///
    /// `ValueError` on an unknown family or mode, a target that does not match
    /// the family, or a `policy` supplied for a mode that does not retry.
    #[new]
    #[pyo3(signature = (family, *, mode, address=None, pool_manager=None, pool_id_hex=None, snapshot_block=None, policy=None))]
    fn new(
        family: &str,
        mode: &str,
        address: Option<String>,
        pool_manager: Option<String>,
        pool_id_hex: Option<String>,
        snapshot_block: Option<u64>,
        policy: Option<Py<RetryPolicy>>,
    ) -> PyResult<Self> {
        let family = Family::parse(family)?;
        let mode = Mode::parse(mode)?;
        match family {
            Family::V3 if address.is_none() => {
                return Err(PyValueError::new_err(
                    "a v3 registration lifecycle targets an `address`",
                ));
            }
            Family::V4 if pool_manager.is_none() || pool_id_hex.is_none() => {
                return Err(PyValueError::new_err(
                    "a v4 registration lifecycle targets a `(pool_manager, pool_id_hex)` pair",
                ));
            }
            Family::V3 | Family::V4 => {}
        }
        match mode {
            Mode::BlockingWithRetry => {
                if policy.is_none() {
                    return Err(PyValueError::new_err(
                        "registration lifecycle mode 'blocking_with_retry' requires a policy",
                    ));
                }
            }
            Mode::Async | Mode::Blocking => {
                if policy.is_some() {
                    return Err(PyValueError::new_err(format!(
                        "a policy is only meaningful for mode 'blocking_with_retry', not '{}'",
                        mode.name()
                    )));
                }
            }
        }
        Ok(Self {
            family,
            address,
            pool_manager,
            pool_id_hex,
            snapshot_block,
            mode,
            policy,
        })
    }

    /// The targeted family: `"v3"` or `"v4"`.
    #[getter]
    fn family(&self) -> &'static str {
        self.family.name()
    }

    /// The targeted V3 pool address, or `None` for a V4 lifecycle.
    #[getter]
    fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }

    /// The targeted V4 `PoolManager` address, or `None` for a V3 lifecycle.
    #[getter]
    fn pool_manager(&self) -> Option<&str> {
        self.pool_manager.as_deref()
    }

    /// The targeted V4 pool id (hex), or `None` for a V3 lifecycle.
    #[getter]
    fn pool_id_hex(&self) -> Option<&str> {
        self.pool_id_hex.as_deref()
    }

    /// The snapshot seed block the core verifies against.
    #[getter]
    fn snapshot_block(&self) -> Option<u64> {
        self.snapshot_block
    }

    /// How the caller parks the core choreography.
    #[getter]
    fn mode(&self) -> &'static str {
        self.mode.name()
    }

    /// The retry dance's bounds, or `None` outside `"blocking_with_retry"`.
    #[getter]
    fn policy(&self, py: Python<'_>) -> Option<Py<RetryPolicy>> {
        self.policy.as_ref().map(|policy| policy.clone_ref(py))
    }
}

/// Run one pool's registration verify-lifecycle through the driver seam.
///
/// The one home of the family target parse, the mode switch, the
/// `RetryPolicy` conversion, and the core-error mapping that the two
/// pymethods share. `async` answers the awaitable
/// bridged onto the Python event loop; the blocking modes run inline (parked
/// on the shared tokio runtime inside `py.detach`) and answer `None`.
///
/// # Errors
///
/// `ValueError` on a target the core cannot use; otherwise the typed exception
/// the core failure maps to (`VerificationMismatchError` for a fatal snapshot
/// mismatch, `VerificationRpcError` for a transient transport/provider
/// failure or a missing verify provider, `RuntimeError` for the rest).
pub(crate) fn run_registration_lifecycle(
    py: Python<'_>,
    driver: &Arc<EngineDriver>,
    lifecycle: &PyRegistrationLifecycle,
) -> PyResult<Py<PyAny>> {
    match lifecycle.target()? {
        Target::V3(address) => {
            let snapshot_block = lifecycle.snapshot_block;
            match lifecycle.mode {
                Mode::Async => {
                    let driver = Arc::clone(driver);
                    let future = crate::ambient_runtime::future_into_py(py, async move {
                        driver
                            .run_v3_registration_lifecycle(address, snapshot_block)
                            .await
                            .map_err(map_driver_lifecycle_err)
                    })?;
                    Ok(future.into())
                }
                Mode::Blocking => {
                    let driver = Arc::clone(driver);
                    py.detach(move || {
                        driver
                            .run_v3_registration_lifecycle_sync(address, snapshot_block)
                            .map_err(map_driver_lifecycle_err)
                    })?;
                    Ok(py.None())
                }
                Mode::BlockingWithRetry => {
                    let policy = lifecycle.core_policy(py)?;
                    let driver = Arc::clone(driver);
                    py.detach(move || {
                        driver
                            .run_v3_registration_lifecycle_with_retry_sync(
                                address,
                                snapshot_block,
                                &policy,
                            )
                            .map_err(map_verify_lifecycle_error)
                    })?;
                    Ok(py.None())
                }
            }
        }
        Target::V4(pool_manager, pool_id) => {
            let snapshot_block = lifecycle.snapshot_block;
            match lifecycle.mode {
                Mode::Async => {
                    let driver = Arc::clone(driver);
                    let future = crate::ambient_runtime::future_into_py(py, async move {
                        driver
                            .run_v4_registration_lifecycle(pool_manager, pool_id, snapshot_block)
                            .await
                            .map_err(map_driver_lifecycle_err)
                    })?;
                    Ok(future.into())
                }
                Mode::Blocking => {
                    let driver = Arc::clone(driver);
                    py.detach(move || {
                        driver
                            .run_v4_registration_lifecycle_sync(
                                pool_manager,
                                pool_id,
                                snapshot_block,
                            )
                            .map_err(map_driver_lifecycle_err)
                    })?;
                    Ok(py.None())
                }
                Mode::BlockingWithRetry => {
                    let policy = lifecycle.core_policy(py)?;
                    let driver = Arc::clone(driver);
                    py.detach(move || {
                        driver
                            .run_v4_registration_lifecycle_with_retry_sync(
                                pool_manager,
                                pool_id,
                                snapshot_block,
                                &policy,
                            )
                            .map_err(map_verify_lifecycle_error)
                    })?;
                    Ok(py.None())
                }
            }
        }
    }
}

#[pymethods]
impl PyArbEngine {
    /// Run a pool's core-owned registration verify-lifecycle.
    ///
    /// The [`RegistrationLifecycle`] value carries the family, target, mode,
    /// and retry policy; the choreography itself (quarantine, seed
    /// verification against the snapshot block, drain + pin, post-drain
    /// verification, `Live`) is the core's, entered at most once per live
    /// window regardless of how many callers race for the same identity.
    /// `mode="async"` answers a coroutine to await; the blocking modes run to
    /// completion and answer `None`.
    ///
    /// # Errors
    ///
    /// `ValueError` on a target the core cannot parse; `VerificationMismatchError`
    /// for a fatal on-chain snapshot mismatch; `VerificationRpcError` for a
    /// transient transport/provider failure (retryable) or a tracked pool with
    /// no verify provider configured.
    fn run_registration_lifecycle(
        &self,
        py: Python<'_>,
        lifecycle: &PyRegistrationLifecycle,
    ) -> PyResult<Py<PyAny>> {
        run_registration_lifecycle(py, &self.driver, lifecycle)
    }
}

#[pymethods]
impl PyBot {
    /// Run a pool's core-owned registration verify-lifecycle.
    ///
    /// The `Bot`-side twin of the engine's `run_registration_lifecycle`:
    /// same value, same choreography, reached through the pump session this
    /// bot owns.
    ///
    /// # Errors
    ///
    /// As the engine's twin.
    fn run_registration_lifecycle(
        &self,
        py: Python<'_>,
        lifecycle: &PyRegistrationLifecycle,
    ) -> PyResult<Py<PyAny>> {
        run_registration_lifecycle(py, &self.pump_state()?, lifecycle)
    }
}

#[cfg(test)]
mod tests {
    //! The value's own contract: a family/mode/target the core cannot use is
    //! refused with the historical target vocabulary, before any driver call.

    #![expect(clippy::expect_used, reason = "test assertions fail loudly")]

    use super::*;

    fn value(
        family: &str,
        address: Option<&str>,
        pool_id_hex: Option<&str>,
    ) -> PyRegistrationLifecycle {
        PyRegistrationLifecycle {
            family: Family::parse(family).expect("test family"),
            address: address.map(str::to_owned),
            pool_manager: Some("0x0000000000000000000000000000000000000001".to_owned()),
            pool_id_hex: pool_id_hex.map(str::to_owned),
            snapshot_block: None,
            mode: Mode::Blocking,
            policy: None,
        }
    }

    #[test]
    fn an_unparseable_v3_address_raises_the_value_error() {
        let lifecycle = value("v3", Some("not-an-address"), None);
        Python::attach(|py| {
            let err = lifecycle
                .target()
                .expect_err("an unparseable address is not a target");
            assert!(err.is_instance_of::<PyValueError>(py), "{err}");
            assert!(
                err.to_string()
                    .starts_with("ValueError: Invalid V3 address:"),
                "{err}"
            );
        });
    }

    #[test]
    fn a_bad_v4_pool_id_raises_the_value_error() {
        let lifecycle = value("v4", None, Some("zz"));
        Python::attach(|py| {
            let err = lifecycle
                .target()
                .expect_err("a malformed pool id is not a target");
            assert!(err.is_instance_of::<PyValueError>(py), "{err}");
            assert!(
                err.to_string().starts_with("ValueError: Invalid pool_id:"),
                "{err}"
            );
        });
    }

    #[test]
    fn an_unknown_family_or_mode_is_refused_at_construction() {
        Python::attach(|_py| {
            let err = PyRegistrationLifecycle::new(
                "v5",
                "blocking",
                Some("0x0000000000000000000000000000000000000001".to_owned()),
                None,
                None,
                None,
                None,
            )
            .err()
            .expect("v5 is not a family");
            assert!(
                err.to_string().contains("family must be 'v3' or 'v4'"),
                "{err}"
            );

            let err = PyRegistrationLifecycle::new(
                "v3",
                "blocking_with_retries",
                Some("0x0000000000000000000000000000000000000001".to_owned()),
                None,
                None,
                None,
                None,
            )
            .err()
            .expect("only the three declared modes exist");
            assert!(err.to_string().contains("mode must be"), "{err}");
        });
    }
}
