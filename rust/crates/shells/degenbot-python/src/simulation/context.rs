//! `PySimulateContext` — the session-static config bag the cockpit constructs
//! once per `BotRunner` and reuses across every block's sim fan-out.
//!
//! Mirrors `PyDispatcher::for_block`'s "construct once, drive many" shape, but
//! longer-lived: the executor / owner / `WETH` / `PoolManager` addresses + the
//! inject flag + the runtime bytecode do not vary per block. The per-block parts
//! (`base_fee_next` / `current_block` / `block_priority_fees`) are call-args, not
//! fields here — they are stitched into a borrowed `SimulateContext<'_>` at call
//! time.
//!
//! The runtime bytecode's provenance is the engine's
//! (`degenbot_arbitrage::executor_runtime`): this seam carries the two operator
//! values that can move the artifact's path and nothing else — no resolution,
//! no file read, no hex validation in the driver shell.
//!
//! Holds an `Arc<AlloyProvider>` — cloned at construction via
//! [`PyAsyncAlloyProvider::provider_arc`] — so a caller can move an owned
//! handle into an async block + construct the `SimulateContext` borrowing it
//! (the `'static` future owns the arc; the `<'a>` borrow is block-local).
//!
//! # GIL discipline
//!
//! `#[new]` runs under the GIL (it must reach into the Python
//! `PyAsyncAlloyProvider` + the bytearray + the address strings). The fields
//! are plain owned data (arc + addresses + bytes); the sim fan-out releases
//! the GIL across the RPC `.await`s. No business logic here — pure config
//! storage (the ADR-005 PyO3-layer discipline).

use crate::address_utils::parse_address;
use crate::prelude::*;
use crate::provider::AlloyProvider;
use crate::rpc::async_provider::PyAsyncAlloyProvider;
use alloy::primitives::Bytes;
use degenbot_arbitrage::{CmdExecutorBinding, SimulationDeployment};
use degenbot_core::op_info;
use degenbot_executor::composers::EncodeOptions;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use std::path::Path;
use std::sync::Arc;

/// The session-static simulation config bag.
///
/// Construct once per session from a `&PyAsyncAlloyProvider` + the addresses +
/// the inject flag + the executor-runtime artifact's location. Read each block
/// alongside the per-block args.
#[pyclass(name = "SimulateContext", module = "degenbot._ffi.simulation")]
// Every field is stitched into the borrowed `SimulateContext<'_>` the sim
// fan-out consumes.
pub struct PySimulateContext {
    /// The typed RPC provider handle (cloned from the `PyAsyncAlloyProvider`
    /// the cockpit holds). Moved into A4's async block; the borrowed
    /// `SimulateContext<'_>.provider` borrows `&*this`.
    pub(crate) provider: Arc<AlloyProvider>,
    /// The built-in settlement deployment binding: the executor / WETH /
    /// `PoolManager` identities plus the session's encode policy.
    /// The warmup math and the runtime-artifact selection are the binding's, so
    /// this seam carries no second copy of either.
    pub(crate) binding: CmdExecutorBinding,
    pub(crate) executor_address: alloy::primitives::Address,
    pub(crate) inject_code: bool,
    pub(crate) injected_address: Option<alloy::primitives::Address>,
    /// The loaded executor runtime (the artifact the Declared override installs).
    pub(crate) runtime_bytecode: Bytes,
}

impl PySimulateContext {
    /// The injected execution deployment for a consumer's own encode policy.
    ///
    /// `inject_code` selects the declared-code override (the artifact installed
    /// at `injected_address`); otherwise the deployment simulates the code the
    /// chain holds at the executor address.
    #[expect(clippy::expect_used)] // the constructor holds injected == executor
    pub(crate) fn deployment(&self, opts: EncodeOptions) -> Arc<SimulationDeployment> {
        let binding = self.binding.with_opts(opts);
        Arc::new(match (self.inject_code, self.injected_address) {
            // [`PySimulateContext::new`] refused a declared address that is not
            // the executor, so this override installs at the very address the
            // built-in adapter encodes its transaction to.
            (true, Some(address)) => binding
                .injected(address, self.runtime_bytecode.clone())
                .expect("the constructor validated injected_address == executor_address"),
            _ => binding.deployment(),
        })
    }
}

#[pymethods]
impl PySimulateContext {
    /// Build a session-long simulation context.
    ///
    /// Args:
    ///     `provider`: the `AsyncAlloyProvider` the cockpit dispatches over.
    ///         Its inner `Arc<AlloyProvider>` is cloned here; A4 moves it into
    ///         the async block.
    ///     `executor_owner`: the operator key's address (hex string) — the
    ///         `from` of `execute()` + the owner funded with ETH in
    ///         `stateOverrides`.
    ///     `executor_address`: the `cmd_executor` contract address (hex) — the
    ///         `execute()` target, i.e. the receipt's subject. No balances are
    ///         read: the contract reports its own profit.
    ///     `weth_address`: WETH9 contract address (hex).
    ///     `pool_manager_address`: Uniswap V4 `PoolManager` address (hex).
    ///     `inject_code`: whether to inject the executor runtime bytecode
    ///         (the `simulation.inject_executor_code` stance).
    ///     `injected_address`: the fresh address to inject the executor at
    ///         (hex); required when `inject_code` is `True`.
    ///     `executor_runtime_path`: the operator's explicit artifact path (the
    ///         declared `driver.executor_runtime` key). `None` hands resolution
    ///         to the engine.
    ///     `contracts_dir`: the declared `dispatch.contracts_dir` key — the
    ///         directory holding `cmd_executor_runtime_bytecode.txt`. `None`
    ///         (or an empty path) falls through to the engine's source-layout
    ///         candidate.
    ///
    /// The artifact itself — resolution precedence, the read, the `0x`-hex
    /// validation — is the engine's (`degenbot_arbitrage::executor_runtime`);
    /// this seam only carries the two operator values that move its path.
    ///
    /// # Errors
    /// `ValueError`: if any address string is not a valid EIP-55 address, or
    ///         `inject_code` is `True` and `injected_address` is `None`.
    /// `RuntimeError`: if the engine cannot resolve, read, or decode the
    ///         executor-runtime artifact (the message names the file and the
    ///         knob that would fix it).
    #[new]
    #[pyo3(signature = (
        provider, executor_owner, executor_address, weth_address, pool_manager_address,
        inject_code, injected_address=None, executor_runtime_path=None, contracts_dir=None,
    ))]
    #[expect(clippy::too_many_arguments)]
    fn new(
        provider: &PyAsyncAlloyProvider,
        executor_owner: &str,
        executor_address: &str,
        weth_address: &str,
        pool_manager_address: &str,
        inject_code: bool,
        injected_address: Option<&str>,
        executor_runtime_path: Option<&str>,
        contracts_dir: Option<&str>,
    ) -> PyResult<Self> {
        let provider = provider.provider_arc();
        let owner = parse_address(executor_owner)
            .map_err(|e| PyValueError::new_err(format!("Invalid executor_owner: {e}")))?;
        let exec = parse_address(executor_address)
            .map_err(|e| PyValueError::new_err(format!("Invalid executor_address: {e}")))?;
        let weth = parse_address(weth_address)
            .map_err(|e| PyValueError::new_err(format!("Invalid weth_address: {e}")))?;
        let pm = parse_address(pool_manager_address)
            .map_err(|e| PyValueError::new_err(format!("Invalid pool_manager_address: {e}")))?;

        let injected = match injected_address {
            Some(s) => Some(
                parse_address(s)
                    .map_err(|e| PyValueError::new_err(format!("Invalid injected_address: {e}")))?,
            ),
            None => None,
        };
        if inject_code && injected.is_none() {
            return Err(PyValueError::new_err(
                "inject_code is True but injected_address is None — supply the injection address",
            ));
        }
        // The built-in adapter encodes every transaction to the executor, so a
        // declared override anywhere else would simulate code the transaction
        // never runs. Fail closed at the boundary instead.
        if let (true, Some(injected_address)) = (inject_code, injected) {
            if injected_address != exec {
                return Err(PyValueError::new_err(format!(
                    "injected_address {injected_address} is not executor_address {exec} — the                      executor's code is overridden at the executor address, so the injection                      must name it"
                )));
            }
        }

        // The built-in binding resolves + loads the executor runtime artifact
        // (one precedence chain and one validated read for every consumer), and
        // it owns the settlement warmup math the deployment declares.
        let binding = CmdExecutorBinding::new(owner, exec, weth, pm, EncodeOptions::default());
        let runtime_bytecode = CmdExecutorBinding::load_runtime(
            executor_runtime_path.map(Path::new),
            contracts_dir.map(Path::new),
        )
        .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;

        Ok(Self {
            provider,
            binding,
            executor_address: exec,
            inject_code,
            injected_address: injected,
            runtime_bytecode,
        })
    }

    /// The RPC URL the held provider points at (for the `[sim]` log line).
    #[getter]
    fn rpc_url(&self) -> &str {
        self.provider.rpc_url()
    }

    /// The executor contract address (checksummed hex) — SIMPIPE2 T3: the
    /// Python payload path constructs `SubmitCandidate`s directly for
    /// inline-simulated entries, and the submit leaf needs the same
    /// session-static `executor_address` the FFI join stamps onto each one.
    #[getter]
    fn executor_address(&self) -> String {
        crate::address_utils::address_to_checksum_string(&self.executor_address)
    }
}

#[pymethods]
impl crate::bot::engine::PyArbEngine {
    /// SIMPIPE2 T4: install the production inline-sim hook — the
    /// `degenbot_bot`-defined `InlineSimulator` seam's concrete closure over
    /// THIS session's sim config + the engine's shared state (theADR-019 D7
    /// dependency inversion, plumbed exactly like `set_result_channel`).
    /// Construction-time wiring: call once after the `SimulateContext` is
    /// built (a later call simply replaces the hook).
    ///
    /// The sim itself runs per-path in the SOLVE WORKER (the T2 clamp seam)
    /// through a dedicated multi-thread runtime — see
    /// `simulation::inline_hook` for the thread/runtime matrix.
    fn install_inline_simulator(
        &self,
        py: Python<'_>,
        context: &PySimulateContext,
        erc6909_profit: bool,
    ) {
        let bot_state = self.bot_state_arc();
        let warm_cache = self.warm_code_cache_arc();
        // The hook's own encode axis set (the payload path's `erc6909_profit`).
        let deployment = context.deployment(EncodeOptions {
            erc6909_profit,
            ..EncodeOptions::default()
        });
        let hook = Arc::new(crate::simulation::inline_hook::InlineSimHook::new(
            Arc::new(context.provider.sim_bounded()),
            deployment,
            bot_state,
            warm_cache,
        ));
        // LW-T3 (Seam C): the inline-sim runtime registers itself as the
        // DEFAULT escalation port at hook install — escalated lane work
        // drives on THIS runtime, never on CPU seats; the cold-miss budget
        // ceiling is visible at the port's gauge surface.
        let escalation_port = hook.escalation_port();
        // ADR-090 H3: the SAME installed object carries the finalist
        // re-quote tier — the black-box V4 leg quote shares the hook's
        // provider, warm code cache (and its learned per-hook slot sets), and
        // dedicated runtime, so a re-quote prices the anchor on exactly the
        // machinery the verification frame uses.
        let hooked_leg_requote: Arc<dyn degenbot_bot::arb_engine::HookedLegReQuote> =
            Arc::clone(&hook) as Arc<dyn degenbot_bot::arb_engine::HookedLegReQuote>;
        self.with_stages(py, move |e| {
            e.set_inline_simulator(hook);
            e.set_hooked_leg_requote(hooked_leg_requote);
        });
        degenbot_workers::lane::install_default_escalation_port(escalation_port);
        // The soak's hook-wiring tell — one line at install, matching the
        // `[solve-phase] cycle complete` inline.* fields it pairs with.
        op_info!(domain = sim, erc6909_profit, "hook installed");
    }
}
