//! The built-in `cmd_executor` simulation binding: the settlement deployment's
//! Encode, Probe, and Assess parts over the generic engine leaf (ADR-098 D3).
//!
//! Everything the production settlement path knows about ITS execution contract
//! lives here, on the built-in side of the seam:
//!
//! - **Encode** — the command stream + packed `execute(bytes,uint256)` call, from
//!   the session's [`degenbot_executor::composers::EncodeOptions`] (the cmd
//!   config axis) and the deployment's executor / PoolManager / WETH identities.
//! - **Probe** — nothing. The contract measures its own profit (ADR-098 D7), so
//!   the built-in declares no pre/post balance reads: the [`ExecutionReceipt`]
//!   in the `execute` call's return data is the evidence.
//! - **Assess** — decode that receipt and price the RETAINED profit
//!   (`gross_profit − bribe_paid`, before gas). The operator's gas floor and
//!   market-aware priority-fee sizing are applied ONCE above this seam by the
//!   settlement strategy ([`crate::simulator`]), which is where ADR-098 D7 puts
//!   gas profitability and operator margins.
//!
//! # The accounting domain is the contract's, the pricing policy is ours
//!
//! `profit_mode` selects what the receipt's `gross_profit` measures: physical
//! WETH + native ETH (1) or PoolManager ERC6909 WETH claims (2). In mode 2 the
//! bribe is still native ETH leaving the executor's physical balance, so
//! `gross_profit − bribe_paid` intentionally spans the two ledgers — it is the
//! value the operator retained, which is what the public `gross_profit` has
//! always meant here.
//!
//! The warmup-slot math, the runtime-artifact selection, and the packed cmd
//! config are this binding's too: a deployment built here carries its own
//! declaration, so a foreign deployment never inherits any of it (ADR-098 D6).

use std::path::Path;
use std::sync::Arc;

use alloy::primitives::{Address, Bytes, U256};

use degenbot_core::op_info;
use degenbot_execution::{
    AssessError, ComposeError, ComposerInputs, DeploymentId, ExecutionAdapter,
    ExecutionObservation, ExecutionResult, FeePolicy, PathInfo, ProbeSpecs, TransactionRequest,
};
use degenbot_executor::composers::{EncodeContext, EncodeOptions, EncodeRequest};
use degenbot_executor::compute_simulation_warmup_slots;
use degenbot_executor::release_registry::{CmdStreamV1Encoding, Release, ReleaseEncoding};
use degenbot_simulation::{DeclaredCode, SettlementWarmup};

use crate::deployment::SimulationDeployment;
use crate::executor_runtime::{load_executor_runtime, ExecutorRuntimeError};

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
        opts: EncodeOptions,
    ) -> Self {
        Self {
            owner,
            executor,
            weth,
            pool_manager,
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

    /// The built-in adapter over this build's explicit current release
    /// (`cmd_executor/1`), with no release-capability planning.
    ///
    /// This is the offline/current consumer's mapping: the session already
    /// names the release by construction, so nothing is discovered and no
    /// release is chosen. A deployment whose release must be *validated* from
    /// declared metadata binds through
    /// [`crate::release_binding::BoundCmdExecutor`], whose adapter carries the
    /// selected [`Release`] and declines a path the release cannot run.
    #[must_use]
    pub fn adapter(&self) -> CmdExecutorSimAdapter {
        CmdExecutorSimAdapter::new(self, Arc::new(CmdStreamV1Encoding), None)
    }

    /// The deployment of the ACTUAL deployed contract: production sims the code
    /// the chain holds at [`Self::executor`], and the state overrides declare no
    /// code.
    ///
    /// `deployment_id` is the operator-assigned identity the configuration
    /// boundary names; the built-in adapter keeps no identity of its own
    /// (ADR-098 D4).
    #[must_use]
    pub fn deployment(&self, deployment_id: DeploymentId) -> SimulationDeployment {
        self.build_deployment(deployment_id, None, None)
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
        deployment_id: DeploymentId,
        address: Address,
        runtime: Bytes,
    ) -> Result<SimulationDeployment, InjectionAddressMismatch> {
        if address != self.executor {
            return Err(InjectionAddressMismatch {
                address,
                executor: self.executor,
            });
        }
        Ok(self.build_deployment(deployment_id, Some(address), Some(runtime)))
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
        deployment_id: DeploymentId,
        injected: Option<Address>,
        runtime: Option<Bytes>,
    ) -> SimulationDeployment {
        self.deployment_with_adapter(deployment_id, injected, runtime, self.adapter())
    }

    /// Compose a deployment around a caller-supplied adapter.
    ///
    /// The release-bound binding
    /// ([`crate::release_binding::BoundCmdExecutor`]) supplies the adapter that
    /// carries its resolved release; everything else — the warmup it declares,
    /// the gas limit, the target, and the market-aware fee sizing — is the
    /// built-in deployment's, exactly as for the explicit current-release
    /// mapping.
    pub(crate) fn deployment_with_adapter(
        &self,
        deployment_id: DeploymentId,
        injected: Option<Address>,
        runtime: Option<Bytes>,
        adapter: CmdExecutorSimAdapter,
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
            deployment_id,
            Arc::new(adapter),
            self.owner,
            // The built-in adapter encodes every transaction to the executor,
            // so the deployment's execution target IS that address (a declared
            // override must name it too — see `injected`).
            self.executor,
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

/// The built-in contract's own profit measurement (ADR-098 D7), owned beside
/// the release model that declares its shape and re-exported here for the
/// adapter's consumers.
pub use degenbot_executor::receipt::{ExecutionReceipt, EXECUTION_RECEIPT_LEN};

/// The built-in `cmd_executor` [`ExecutionAdapter`]: one complete release
/// transaction for the deployment, no declared probes, and its receipt-based
/// assessment.
///
/// The adapter holds the release's own [`ReleaseEncoding`] — so the command
/// stream, the packed config, the `execute` ABI, and the receipt shape all move
/// together with the release — plus, for a bound deployment, the [`Release`]
/// itself, which is what makes capability planning possible: a call that needs
/// a capability the release does not declare is a
/// [`ComposeError::Unsupported`] decline for THIS candidate, never an encoding
/// error (ADR-098 D4/D5).
#[derive(Clone, Debug)]
pub struct CmdExecutorSimAdapter {
    binding: CmdExecutorBinding,
    encoding: Arc<dyn ReleaseEncoding>,
    release: Option<Arc<Release>>,
}

impl CmdExecutorSimAdapter {
    /// Compose an adapter from the deployment facts, the release's encoding,
    /// and (for a metadata-bound deployment) the release to plan against.
    #[must_use]
    pub fn new(
        binding: &CmdExecutorBinding,
        encoding: Arc<dyn ReleaseEncoding>,
        release: Option<Arc<Release>>,
    ) -> Self {
        Self {
            binding: *binding,
            encoding,
            release,
        }
    }

    /// The deployment facts this adapter encodes against.
    #[must_use]
    pub const fn binding(&self) -> &CmdExecutorBinding {
        &self.binding
    }

    /// The release encoding this adapter produces bytes with.
    #[must_use]
    pub fn encoding(&self) -> &dyn ReleaseEncoding {
        self.encoding.as_ref()
    }

    /// The validated release this adapter plans against, if it is bound.
    #[must_use]
    pub fn release(&self) -> Option<&Release> {
        self.release.as_deref()
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

        // One release encodes stream, config, and execute ABI together, so the
        // call is the release's answer as a unit.
        let call = self.encoding.encode(&context, &request)?;

        // Capability planning: a bound deployment checks the call's declared
        // requirements against the capabilities its release declares. A missing
        // one is a routine decline for this candidate (the fan-out offers the
        // path to the next target); it is not an encoding error, because
        // another release would run the same path.
        if let Some(release) = &self.release {
            let missing = release.missing_capabilities(&call.requires);
            if !missing.is_empty() {
                return Err(ComposeError::unsupported(format!(
                    "release {} does not declare {}; this path needs it",
                    release.release_id(),
                    release.describe_capabilities(&missing)
                )));
            }
        }

        Ok(TransactionRequest::new(
            self.binding.executor,
            call.data,
            call.value,
        ))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        // The receipt is the evidence (ADR-098 D7): the built-in assesses the
        // `execute` return data, so it declares no pre/post reads and spends no
        // sequence steps on balances nobody interprets.
        static NO_PROBES: ProbeSpecs = Vec::new();
        &NO_PROBES
    }

    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        let policy = Self::pricing_floor();
        if !observation.succeeded() {
            // A revert moved no value, and its payload is a revert reason rather
            // than a receipt: the shared pricing helper declines it with zero
            // gross instead of decoding bytes the contract never measured.
            return Ok(ExecutionResult::price(
                U256::ZERO,
                Vec::new(),
                observation,
                U256::ZERO,
                &policy,
            ));
        }

        // The contract measured its own profit: the receipt's retained figure
        // (`gross_profit − bribe_paid`, before gas) is what the public
        // `gross_profit` means, so gas and the fee policy apply exactly once
        // here and once more only if the deployment opts into market-aware
        // sizing above the seam.
        let retained = self
            .encoding
            .decode_receipt(observation.return_data.as_ref())?
            .retained()?;
        Ok(ExecutionResult::price(
            retained,
            Vec::new(),
            observation,
            U256::ZERO,
            &policy,
        ))
    }
}
