//! The built-in `cmd_executor` simulation binding: the settlement deployment's
//! Encode, Probe, and Assess parts over the generic engine leaf (ADR-098 D3).
//!
//! Everything the production settlement path knows about ITS execution contract
//! lives here, on the built-in side of the seam:
//!
//! - **Encode** — the command stream + packed `execute(bytes,uint256)` call, from
//!   the session's [`degenbot_executor::composers::EncodeOptions`] (the cmd
//!   config axis) and the deployment's executor / PoolManager / WETH identities.
//! - **Probe** — the settlement balance reads: WETH `balanceOf(executor)`, the
//!   executor's native ETH balance through Multicall3 `getEthBalance(executor)`,
//!   and the PoolManager ERC6909 `balanceOf(executor, weth_id)`. Declared data,
//!   in this order, before and after the opportunity's transaction.
//! - **Assess** — the gross balance-domain increase
//!   `(weth + eth + erc6909)_after − (…)_before`. The operator's gas floor and
//!   market-aware priority-fee sizing are applied ONCE above this seam by the
//!   settlement strategy ([`crate::simulator`]), which is where ADR-098 D7 puts
//!   gas profitability and operator margins.
//!
//! The warmup-slot math, the runtime-artifact selection, and the packed cmd
//! config are this binding's too: a deployment built here carries its own
//! declaration, so a foreign deployment never inherits any of it (ADR-098 D6).

use std::path::Path;
use std::sync::Arc;

use alloy::primitives::{Address, Bytes, U256};

use degenbot_core::op_info;
use degenbot_execution::{
    AssessError, ComposeError, ComposerInputs, ExecutionAdapter, ExecutionObservation,
    ExecutionResult, FeePolicy, PathInfo, ProbeSpec, ProbeSpecs, TransactionRequest,
};
use degenbot_executor::composers::{
    config_for_options, encode_cmd_stream, encode_execute_call, EncodeContext, EncodeOptions,
    EncodeRequest,
};
use degenbot_executor::compute_simulation_warmup_slots;
use degenbot_simulation::{DeclaredCode, SettlementWarmup};

use crate::calldata::{
    encode_balance_of_calldata, encode_erc6909_balance_of_calldata, encode_get_eth_balance_calldata,
};
use crate::deployment::SimulationDeployment;
use crate::executor_runtime::{load_executor_runtime, ExecutorRuntimeError};

/// The declared label of the WETH `balanceOf(executor)` probe.
pub const PROBE_LABEL_WETH: &str = "WETH";
/// The declared label of the executor's native-ETH probe (Multicall3
/// `getEthBalance(executor)`).
pub const PROBE_LABEL_ETH: &str = "ETH";
/// The declared label of the PoolManager ERC6909 `balanceOf(executor, id)` probe.
pub const PROBE_LABEL_ERC6909: &str = "ERC6909_WETH";

/// The declared position of the WETH probe in [`CmdExecutorBinding::probes`].
pub const PROBE_INDEX_WETH: usize = 0;
/// The declared position of the native-ETH probe in [`CmdExecutorBinding::probes`].
pub const PROBE_INDEX_ETH: usize = 1;
/// The declared position of the ERC6909 probe in [`CmdExecutorBinding::probes`].
pub const PROBE_INDEX_ERC6909: usize = 2;
/// The number of declared settlement probes, i.e. the index of the
/// opportunity's own transaction inside the executed sequence.
pub const SETTLEMENT_PROBE_COUNT: usize = 3;

/// The session-scoped built-in `cmd_executor` facts: the deployment's identities
/// plus the session's encode policy.
///
/// Construct once per session and hand it to
/// [`CmdExecutorBinding::deployment`] (the already-deployed contract) or
/// [`CmdExecutorBinding::injected`] (an explicit declared-code override).
#[derive(Clone, Copy, Debug)]
pub struct CmdExecutorBinding {
    owner: Address,
    executor: Address,
    weth: Address,
    pool_manager: Address,
    multicall3: Address,
    opts: EncodeOptions,
}

impl CmdExecutorBinding {
    /// Capture the deployment identities and the session's encode policy.
    #[must_use]
    pub const fn new(
        owner: Address,
        executor: Address,
        weth: Address,
        pool_manager: Address,
        multicall3: Address,
        opts: EncodeOptions,
    ) -> Self {
        Self {
            owner,
            executor,
            weth,
            pool_manager,
            multicall3,
            opts,
        }
    }

    /// The operator key's address (the opportunity's `from`, and the account the
    /// engine funds in the simulated block).
    #[must_use]
    pub const fn owner(&self) -> Address {
        self.owner
    }

    /// The `cmd_executor` contract address.
    #[must_use]
    pub const fn executor(&self) -> Address {
        self.executor
    }

    /// The session's encode policy (the cmd config axes).
    #[must_use]
    pub const fn opts(&self) -> EncodeOptions {
        self.opts
    }

    /// Re-bind the encode policy (a consumer's own axis set, e.g. the inline
    /// sim's `erc6909_profit`).
    #[must_use]
    pub const fn with_opts(mut self, opts: EncodeOptions) -> Self {
        self.opts = opts;
        self
    }

    /// The declared settlement probes, in declared order.
    #[must_use]
    pub fn probes(&self) -> ProbeSpecs {
        settlement_probes(self.executor, self.weth, self.pool_manager, self.multicall3)
    }

    /// The built-in adapter: this deployment's Encode / Probe / Assess.
    #[must_use]
    pub fn adapter(&self) -> CmdExecutorSimAdapter {
        CmdExecutorSimAdapter {
            binding: *self,
            probes: self.probes(),
        }
    }

    /// The deployment of the ACTUAL deployed contract: production sims the code
    /// the chain holds at [`Self::executor`], and the state overrides declare no
    /// code.
    #[must_use]
    pub fn deployment(&self) -> SimulationDeployment {
        self.build_deployment(None, None)
    }

    /// The deployment of a DECLARED build: `runtime` is installed at `address`
    /// in the simulated block. Explicit development data — never a silent
    /// substitution for the deployed implementation.
    ///
    /// The built-in adapter encodes its transaction to [`Self::executor`], so
    /// `address` must BE the executor address: injecting the runtime anywhere
    /// else would simulate code the transaction never runs. A mismatch is
    /// refused rather than simulated.
    ///
    /// # Errors
    ///
    /// [`InjectionAddressMismatch`] when `address` is not [`Self::executor`].
    pub fn injected(
        &self,
        address: Address,
        runtime: Bytes,
    ) -> Result<SimulationDeployment, InjectionAddressMismatch> {
        if address != self.executor {
            return Err(InjectionAddressMismatch {
                address,
                executor: self.executor,
            });
        }
        Ok(self.build_deployment(Some(address), Some(runtime)))
    }

    /// Resolve + load the executor runtime artifact for a declared build.
    ///
    /// The artifact selection (one precedence chain, one validated read) is the
    /// binding's, so no driver shell resolves, stats, or decodes it.
    ///
    /// # Errors
    ///
    /// [`ExecutorRuntimeError`] when the artifact is absent, unreadable, or not
    /// `0x`-hex bytecode; the error names the file and the knob that would fix
    /// it.
    pub fn load_runtime(
        explicit: Option<&Path>,
        contracts_dir: Option<&Path>,
    ) -> Result<Bytes, ExecutorRuntimeError> {
        let runtime = load_executor_runtime(explicit, contracts_dir)?;
        op_info!(
            domain = sim,
            bytes = runtime.bytecode.len(),
            path = %runtime.path.display(),
            "executor runtime bytecode loaded"
        );
        Ok(runtime.bytecode)
    }

    fn build_deployment(
        &self,
        injected: Option<Address>,
        runtime: Option<Bytes>,
    ) -> SimulationDeployment {
        // The warmup slots key on the account whose ledger the contract touches:
        // the injected address for a declared build, else the owner (the
        // session's funding account).
        let warmup_subject = injected.unwrap_or(self.owner);
        let settlement = SettlementWarmup {
            weth_address: self.weth,
            pool_manager_address: Some(self.pool_manager),
            slots: compute_simulation_warmup_slots(warmup_subject, self.weth),
        };
        let code = match (injected, runtime) {
            (Some(address), Some(runtime)) => Some(DeclaredCode { address, runtime }),
            _ => None,
        };
        SimulationDeployment::new(
            Arc::new(self.adapter()),
            self.owner,
            crate::simulator::execute_gas_limit(),
            code,
            Some(settlement),
        )
        // The built-in settlement path is the one deployment that opts into the
        // strategy's market-aware fee sizing (ADR-098 D7): its Assess reports the
        // gross evidence and the operator's gas/priority policy lands once, above
        // the seam. Every other deployment keeps its adapter's own Fee decision.
        .with_fee_sizing(crate::deployment::FeeSizing::MarketAware)
    }
}

/// A declared-code override named an address other than the deployment's own
/// executor: the transaction the adapter encodes targets the executor, so the
/// injected runtime would never run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InjectionAddressMismatch {
    /// The address the caller declared for the injected runtime.
    pub address: Address,
    /// The executor the built-in adapter encodes its transaction to.
    pub executor: Address,
}

impl std::fmt::Display for InjectionAddressMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "declared code address {} is not the deployment's executor {}: the adapter              encodes its transaction to {}, so injecting elsewhere would simulate code              the transaction never runs",
            self.address, self.executor, self.executor
        )
    }
}

impl std::error::Error for InjectionAddressMismatch {}

/// The built-in settlement probes, in declared order.
fn settlement_probes(
    executor: Address,
    weth: Address,
    pool_manager: Address,
    multicall3: Address,
) -> ProbeSpecs {
    #[expect(clippy::expect_used)] // a valid `Address` always ABI-encodes
    let weth_read = encode_balance_of_calldata(executor).expect("a valid address encodes");
    #[expect(clippy::expect_used)] // a valid `Address` always ABI-encodes
    let eth_read = encode_get_eth_balance_calldata(executor).expect("a valid address encodes");
    #[expect(clippy::expect_used)] // a valid address pair always ABI-encodes
    let erc6909_read =
        encode_erc6909_balance_of_calldata(executor, weth).expect("a valid address pair encodes");
    vec![
        ProbeSpec::call(PROBE_LABEL_WETH, weth, weth_read),
        ProbeSpec::call(PROBE_LABEL_ETH, multicall3, eth_read),
        ProbeSpec::call(PROBE_LABEL_ERC6909, pool_manager, erc6909_read),
    ]
}

/// The built-in `cmd_executor` [`ExecutionAdapter`]: one complete
/// `execute(bytes,uint256)` transaction for the deployment, its declared
/// settlement probes, and its gross balance-domain assessment.
#[derive(Clone, Debug)]
pub struct CmdExecutorSimAdapter {
    binding: CmdExecutorBinding,
    probes: ProbeSpecs,
}

impl CmdExecutorSimAdapter {
    /// The deployment facts this adapter encodes against.
    #[must_use]
    pub const fn binding(&self) -> &CmdExecutorBinding {
        &self.binding
    }

    /// The built-in adapter's own pricing floor: base fee, no priority fee.
    ///
    /// The operator's market-aware priority fee is applied once by the strategy
    /// (`crate::simulator::price_gross`) — reached because this binding's
    /// deployment declares `FeeSizing::MarketAware` — never re-derived here.
    const fn pricing_floor() -> FeePolicy {
        FeePolicy {
            market_percentile: false,
            override_priority_fee: 0,
        }
    }
}

impl ExecutionAdapter for CmdExecutorSimAdapter {
    fn encode(
        &self,
        path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        let request = EncodeRequest::new(
            path.clone(),
            inputs.optimal_input,
            inputs.hop_outputs.to_vec(),
            inputs.consumed_inputs.to_vec(),
            self.binding.opts,
        );
        let context = EncodeContext::new(
            self.binding.executor,
            self.binding.pool_manager,
            self.binding.weth,
        );
        let Some(commands) = encode_cmd_stream(&context, &request) else {
            return Err(ComposeError::unsupported(
                "the command grammar has no producer for this path",
            ));
        };
        let config = config_for_options(self.binding.opts, U256::ZERO)
            .map_err(|error| ComposeError::encode(format!("config_for_options failed: {error}")))?;
        let call = encode_execute_call(self.binding.executor, &commands, config);
        Ok(TransactionRequest::new(
            self.binding.executor,
            Bytes::from(call.data),
            U256::ZERO,
        ))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        &self.probes
    }

    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        let policy = Self::pricing_floor();
        if !observation.succeeded() {
            // A revert moved no value: the shared pricing helper declines it with
            // zero gross, and the settlement reads are not evidence worth
            // demanding from a transaction that never completed.
            return Ok(ExecutionResult::price(
                U256::ZERO,
                Vec::new(),
                observation,
                U256::ZERO,
                &policy,
            ));
        }

        let mut before = U256::ZERO;
        let mut after = U256::ZERO;
        let mut deltas = Vec::with_capacity(observation.probe_reads.len());
        for reading in &observation.probe_reads {
            let label = reading.spec.label();
            let pre = reading.before.ok_or_else(|| {
                AssessError::evidence(format!(
                    "the settlement balance read `{label}` observed no pre-transaction figure"
                ))
            })?;
            let post = reading.after.ok_or_else(|| {
                AssessError::evidence(format!(
                    "the settlement balance read `{label}` observed no post-transaction figure"
                ))
            })?;
            before = before.saturating_add(pre);
            after = after.saturating_add(post);
            deltas.push(reading.delta().unwrap_or_default());
        }

        // The settlement gross: the combined WETH + native ETH + ERC6909 ledger
        // increase over the opportunity. A net decrease is zero gross (no
        // profit), never a negative figure.
        Ok(ExecutionResult::price(
            after.saturating_sub(before),
            deltas,
            observation,
            U256::ZERO,
            &policy,
        ))
    }
}
