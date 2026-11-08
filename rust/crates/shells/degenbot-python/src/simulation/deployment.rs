//! `SimulationDeployment` — the Python-facing configuration projection over the
//! core execution deployment (ADR-098 D3/D4).
//!
//! One core value carries an execution deployment: the operator-assigned
//! [`degenbot_execution::DeploymentId`], the injected
//! [`degenbot_execution::ExecutionAdapter`] (Encode / Probe / Assess), the
//! account whose key sends the opportunity's transaction, the EXECUTION TARGET
//! (the `to` of the transaction the adapter encodes), the gas limit that
//! transaction may burn, and the optional declared-code override. This pyclass
//! is that value's Python spelling and nothing more: it validates the declared
//! arguments, lifts the existing Python execution adapter
//! ([`crate::execution::PyPayloadAdapter`]) into the core seam, and hands the
//! resulting frozen `Arc<SimulationDeployment>` to the batch executor's
//! composition boundary ([`crate::simulation::batch::build_batch_executor_py`]).
//!
//! # What a Python deployment does NOT declare
//!
//! - **No settlement warmup.** A foreign contract has no WETH or `PoolManager`
//!   identity, so the deployment declares none (`None`) and the engine derives
//!   only the words its paths name. The built-in settlement deployment's warmup
//!   math stays the built-in binding's ([`degenbot_arbitrage::CmdExecutorBinding`]).
//! - **No fee sizing policy.** The deployment keeps `FeeSizing::AdapterOwned` (the
//!   core constructor's default):
//!   the adapter's own `ExecutionResult` — priority fee, net, threshold,
//!   verdict — reaches the caller unchanged. Market-aware re-pricing is the
//!   built-in binding's explicit choice, never a second Python strategy.
//!
//! # Composition invariants are the core's
//!
//! The registry rules that decide whether a session may run — a non-empty
//! registry, distinct deployment identities, and every deployment's owner
//! signed by the session's one signer — are
//! [`degenbot_arbitrage::DeploymentRegistry`]'s, evaluated once at
//! [`crate::simulation::batch::build_batch_executor_py`] with the
//! [`degenbot_batch_executor::ExecutorConfig`] composition boundary. This seam
//! refuses only what it can see about ONE declaration (an unparseable address, an
//! empty declared runtime, a zero gas limit) and maps the core registry errors
//! to Python exceptions; it computes no identity, authority, or capacity policy
//! of its own.
//!
//! The declared structure of a deployment survives verbatim into the core value
//! (the deployment id, target, owner, gas limit, and code override are copies);
//! what the fan-out does with it — which deployments a solved path is offered to
//! ([`degenbot_arbitrage::TargetSelection`]), the candidate each examined target
//! produces, partition by `CandidateId`, per-target submit rows — is
//! [`degenbot_arbitrage::dispatch_across_deployments`]'s, not Python's.

use std::sync::Arc;

use degenbot_arbitrage::SimulationDeployment;
use degenbot_batch_executor::ExecutorConfig;
use degenbot_execution::DeploymentId;
use degenbot_simulation::DeclaredCode;
use pyo3::exceptions::PyValueError;

use crate::address_utils::{address_to_checksum_string, parse_address};
use crate::execution::PyPayloadAdapter;
use crate::prelude::*;

/// One execution deployment a session fans its solved paths out to.
///
/// Construct once per foreign contract, then hand the list to
/// [`crate::simulation::batch::build_batch_executor_py`]'s `deployments`
/// argument, in registration order, after the built-in settlement deployment
/// (whose identity is [`builtin_deployment_id`]). Which of them each solved path
/// is offered to is that function's `target_selection` policy: the default
/// ordered walk stops at the first deployment whose composition accepts the
/// path, and the explicit all-target fan-out offers every path to every entry.
///
/// The adapter is an existing [`crate::execution::PyPayloadAdapter`] — the
/// Python-authored Encode callable bound to the transaction it executes. This
/// class adds the deployment facts calldata cannot carry (identity, owner,
/// target, gas limit, optional declared runtime); it is not a second adapter
/// framework and it invents no economics.
#[pyclass(
    name = "SimulationDeployment",
    module = "degenbot._ffi.simulation",
    frozen
)]
#[derive(Debug)]
pub struct PySimulationDeployment {
    inner: Arc<SimulationDeployment>,
}

impl PySimulationDeployment {
    /// The frozen core deployment this value projects (the batch executor's
    /// `ExecutorRuntime::additional_deployments` entry).
    pub(crate) fn deployment_arc(&self) -> Arc<SimulationDeployment> {
        Arc::clone(&self.inner)
    }
}

#[pymethods]
impl PySimulationDeployment {
    /// Compose one execution deployment from its adapter and declared facts.
    ///
    /// Args:
    ///     `deployment_id`: the operator-assigned identity (a plain count). It
    ///         must differ from every other deployment in the session — including
    ///         the built-in settlement deployment's
    ///         [`builtin_deployment_id`] — because every post-target row keys on
    ///         `(path, deployment)`. Duplicates are refused at executor
    ///         construction, not here.
    ///     `adapter`: the Python execution adapter
    ///         ([`crate::execution::PyPayloadAdapter`]) that encodes this
    ///         contract's transaction and declares its probe reads and minimum
    ///         net profit.
    ///     `owner`: the account whose key sends the opportunity's transaction
    ///         (the `from` of the simulated and submitted transaction). It must
    ///         be the session signer's account; a foreign owner is refused at
    ///         executor construction.
    ///     `target`: the deployment's EXECUTION TARGET — the `to` of the
    ///         transaction its adapter encodes. The submit join stamps this
    ///         address, so a heterogeneous session routes each candidate to its
    ///         own contract.
    ///     `gas_limit`: the top-level gas limit the opportunity's transaction
    ///         may burn.
    ///     `code`: an optional declared runtime (bytecode) installed at THIS
    ///         deployment's `target` in the simulated block — explicit
    ///         development data for an undeployed build. The override installs
    ///         only at the one address the adapter's transaction targets, so it
    ///         can never simulate code the transaction does not run. `None`
    ///         executes the code the chain holds at `target`.
    ///
    /// The deployment declares no settlement warmup (a foreign contract has no
    /// WETH or `PoolManager` identity) and keeps adapter-owned fee sizing (the
    /// adapter's own verdict, never the built-in market-aware re-pricing).
    ///
    /// # Errors
    ///
    /// `ValueError` when `owner`/`target` is not a valid EIP-55 address, when
    /// `code` is an empty runtime, or when `gas_limit` is zero (a deployment
    /// that may burn no gas has no opportunity to execute).
    #[new]
    #[pyo3(signature = (deployment_id, adapter, owner, target, gas_limit, *, code = None))]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "PyO3 extracts a borrowed pyclass argument by value"
    )]
    fn new(
        py: Python<'_>,
        deployment_id: u64,
        adapter: PyRef<'_, PyPayloadAdapter>,
        owner: &str,
        target: &str,
        gas_limit: u64,
        code: Option<Vec<u8>>,
    ) -> PyResult<Self> {
        let owner_address = parse_address(owner)
            .map_err(|error| PyValueError::new_err(format!("Invalid deployment owner: {error}")))?;
        let target_address = parse_address(target).map_err(|error| {
            PyValueError::new_err(format!("Invalid deployment target: {error}"))
        })?;
        if gas_limit == 0 {
            return Err(PyValueError::new_err(
                "gas_limit must be non-zero: an opportunity's transaction needs gas",
            ));
        }
        let declared_code = match code {
            Some(runtime) => {
                if runtime.is_empty() {
                    return Err(PyValueError::new_err(
                        "code must be non-empty runtime bytecode (omit it to simulate the \
                         deployed contract)",
                    ));
                }
                Some(DeclaredCode {
                    address: target_address,
                    runtime: runtime.into(),
                })
            }
            None => None,
        };
        Ok(Self {
            inner: Arc::new(SimulationDeployment::new(
                DeploymentId::new(deployment_id),
                adapter.adapter_arc(py),
                owner_address,
                target_address,
                gas_limit,
                declared_code,
                // A foreign contract declares no WETH/PoolManager warmup.
                None,
            )),
        })
    }

    /// The operator-assigned deployment identity (the deployment half of every
    /// [`crate::simulation::identity::PyCandidateId`] this deployment
    /// executes).
    #[getter]
    fn deployment_id(&self) -> u64 {
        self.inner.deployment_id().value()
    }

    /// The account whose key sends this deployment's transactions (the `from`;
    /// EIP-55 checksummed).
    #[getter]
    fn owner(&self) -> String {
        address_to_checksum_string(&self.inner.owner())
    }

    /// The deployment's execution target — the `to` its adapter encodes
    /// (EIP-55 checksummed).
    #[getter]
    fn target(&self) -> String {
        address_to_checksum_string(&self.inner.target())
    }

    /// The top-level gas limit the opportunity's transaction may burn.
    #[getter]
    fn gas_limit(&self) -> u64 {
        self.inner.gas_limit()
    }

    /// The declared runtime installed at [`Self::target`] in the simulated
    /// block, or `None` when the deployment simulates the deployed contract.
    #[getter]
    fn declared_code<'py>(&self, py: Python<'py>) -> Option<Bound<'py, pyo3::types::PyBytes>> {
        self.inner
            .code()
            .map(|code| pyo3::types::PyBytes::new(py, &code.runtime))
    }

    fn __repr__(&self) -> String {
        format!(
            "SimulationDeployment(deployment_id={}, target={}, owner={}, gas_limit={})",
            self.inner.deployment_id().value(),
            address_to_checksum_string(&self.inner.target()),
            address_to_checksum_string(&self.inner.owner()),
            self.inner.gas_limit()
        )
    }
}

/// The identity the built-in settlement deployment is registered under.
///
/// [`ExecutorConfig::from_verdict`] registers it as the FIRST entry of every
/// session's deployment registry, so a session that names no additional
/// deployment is that one entry. A Python deployment that reused this number
/// would make every post-target row ambiguous, so the executor constructor
/// refuses it — the number itself is the core's to name, which is why it is
/// read here rather than restated in Python.
#[pyfunction]
#[must_use]
pub const fn builtin_deployment_id() -> u64 {
    ExecutorConfig::BUILTIN_DEPLOYMENT_ID.value()
}

/// Map a core registry refusal onto the Python exception the driver sees.
///
/// A composition error is a configuration mistake, so it surfaces as
/// `ValueError` with the core's own message (which names the deployment and the
/// authority it disagrees with) — never as a `PanicException` or a Rust abort.
pub(crate) fn registry_error_to_py(error: &degenbot_arbitrage::DeploymentRegistryError) -> PyErr {
    PyValueError::new_err(error.to_string())
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::primitives::Address;
    use degenbot_arbitrage::DeploymentRegistryError;
    use degenbot_execution::{ComposerInputs, HopInfo, PathInfo, V2HopInfo};

    const OWNER: Address = alloy::primitives::address!("19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A");
    const TARGET: Address = alloy::primitives::address!("00000000000000000000000000000000000B00B5");

    fn adapter<'py>(py: Python<'py>, target: &str) -> PyRef<'py, PyPayloadAdapter> {
        let callback = py
            .eval(c"lambda result: bytes([0xAB, 0xCD])", None, None)
            .unwrap()
            .unbind();
        let bound = Py::new(py, PyPayloadAdapter::test_binding(callback, target)).unwrap();
        bound.into_any().extract(py).unwrap()
    }

    fn sample_path() -> PathInfo {
        PathInfo::new(vec![HopInfo::V2(V2HopInfo {
            pool_address: alloy::primitives::address!("1100000000000000000000000000000000000000"),
            token0_address: alloy::primitives::address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            token1_address: alloy::primitives::address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            fee: 30,
            zfo: true,
        })])
    }

    fn sample_inputs<'a>(outputs: &'a [u128], consumed: &'a [u128]) -> ComposerInputs<'a> {
        ComposerInputs {
            optimal_input: 1_000,
            hop_outputs: outputs,
            consumed_inputs: consumed,
            opts: degenbot_execution::ComposeOptions,
        }
    }

    /// The projection carries the declared facts verbatim into the core value,
    /// and the lifted adapter encodes the deployment's OWN transaction.
    #[test]
    fn a_python_deployment_projects_onto_the_core_deployment() {
        Python::attach(|py| {
            let deployment = PySimulationDeployment::new(
                py,
                2,
                adapter(py, "0x00000000000000000000000000000000000B00B5"),
                "0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A",
                "0x00000000000000000000000000000000000B00B5",
                1_000_000,
                Some(vec![0x60, 0x00, 0x60, 0x00, 0xf3]),
            )
            .unwrap();
            let projected = deployment.deployment_arc();
            assert_eq!(projected.deployment_id(), DeploymentId::new(2));
            assert_eq!(projected.owner(), OWNER);
            assert_eq!(projected.target(), TARGET);
            assert_eq!(projected.gas_limit(), 1_000_000);
            assert_eq!(
                projected.settlement(),
                None,
                "a foreign contract declares no settlement warmup"
            );
            assert_eq!(
                projected.fee_sizing(),
                degenbot_arbitrage::FeeSizing::AdapterOwned,
                "the adapter's own verdict is the deployment's answer"
            );
            let runtime = projected
                .code()
                .expect("the declared runtime rides the deployment");
            assert_eq!(
                runtime.address, TARGET,
                "the declared runtime installs at the deployment's own target"
            );
            assert_eq!(runtime.runtime.to_vec(), vec![0x60, 0x00, 0x60, 0x00, 0xf3]);

            let outputs = [1_000u128];
            let consumed = [1_000u128];
            let transaction = projected
                .adapter()
                .encode(&sample_path(), &sample_inputs(&outputs, &consumed))
                .expect("the lifted Python adapter encodes");
            assert_eq!(transaction.to, TARGET);
            assert_eq!(transaction.value, alloy::primitives::U256::ZERO);
            assert_eq!(transaction.data.to_vec(), vec![0xAB, 0xCD]);
        });
    }

    /// A deployment's declared code installs at ITS target, so two deployments
    /// keep two independent code stances.
    #[test]
    fn two_deployments_keep_their_own_targets_and_codes() {
        Python::attach(|py| {
            let a = PySimulationDeployment::new(
                py,
                2,
                adapter(py, "0x00000000000000000000000000000000000000A1"),
                "0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A",
                "0x00000000000000000000000000000000000000A1",
                500_000,
                Some(vec![0x00]),
            )
            .unwrap();
            let b = PySimulationDeployment::new(
                py,
                3,
                adapter(py, "0x00000000000000000000000000000000000000B2"),
                "0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A",
                "0x00000000000000000000000000000000000000B2",
                500_000,
                None,
            )
            .unwrap();
            assert_eq!(
                a.deployment_arc().target(),
                alloy::primitives::address!("00000000000000000000000000000000000000A1")
            );
            assert_eq!(
                b.deployment_arc().target(),
                alloy::primitives::address!("00000000000000000000000000000000000000B2")
            );
            assert!(a.deployment_arc().code().is_some());
            assert!(b.deployment_arc().code().is_none());
        });
    }

    /// Declaration-level malformations are refused at the boundary.
    #[test]
    fn malformed_declarations_are_refused() {
        Python::attach(|py| {
            let err = PySimulationDeployment::new(
                py,
                2,
                adapter(py, "0x00000000000000000000000000000000000B00B5"),
                "not-an-address",
                "0x00000000000000000000000000000000000B00B5",
                1_000_000,
                None,
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("Invalid deployment owner"),
                "{err}"
            );

            let err = PySimulationDeployment::new(
                py,
                2,
                adapter(py, "0x00000000000000000000000000000000000B00B5"),
                "0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A",
                "not-an-address",
                1_000_000,
                None,
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("Invalid deployment target"),
                "{err}"
            );

            let err = PySimulationDeployment::new(
                py,
                2,
                adapter(py, "0x00000000000000000000000000000000000B00B5"),
                "0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A",
                "0x00000000000000000000000000000000000B00B5",
                0,
                None,
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("gas_limit must be non-zero"),
                "{err}"
            );

            let err = PySimulationDeployment::new(
                py,
                2,
                adapter(py, "0x00000000000000000000000000000000000B00B5"),
                "0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A",
                "0x00000000000000000000000000000000000B00B5",
                1_000_000,
                Some(Vec::new()),
            )
            .unwrap_err();
            assert!(err.to_string().contains("non-empty runtime"), "{err}");
        });
    }

    /// Every core registry refusal reaches Python as a `ValueError` carrying the
    /// core's own message — including the empty registry, which the Python
    /// boundary cannot reach because the built-in deployment is always entry one.
    #[test]
    fn registry_refusals_map_to_value_errors() {
        Python::attach(|py| {
            let deployments = [
                DeploymentRegistryError::Empty,
                DeploymentRegistryError::DuplicateId {
                    deployment_id: DeploymentId::new(1),
                },
                DeploymentRegistryError::OwnerNotSigner {
                    deployment_id: DeploymentId::new(2),
                    owner: OWNER,
                    authority: Address::repeat_byte(0x11),
                },
            ];
            for error in deployments {
                let py_error = registry_error_to_py(&error);
                assert!(py_error.is_instance_of::<PyValueError>(py), "{py_error}");
                assert!(
                    py_error.to_string().contains(&error.to_string()),
                    "the core's own message reaches the driver: {py_error}"
                );
            }
        });
    }
}
