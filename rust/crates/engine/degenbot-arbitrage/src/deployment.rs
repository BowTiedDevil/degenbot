//! The simulation composition boundary: block-wide facts beside the injected
//! execution deployment (ADR-098 D3).
//!
//! A production simulation crosses two different kinds of fact, and this module
//! keeps them apart:
//!
//! - [`SimulationBlock`] — the facts ONE simulated block contributes: the
//!   provider the cold-miss fallback reads through, the block environment, and
//!   the fee samples. Every candidate in a fan-out shares these.
//! - [`SimulationDeployment`] — the facts ONE execution deployment contributes:
//!   its operator-assigned [`degenbot_execution::DeploymentId`], its
//!   [`degenbot_execution::ExecutionAdapter`] (Encode / Probe / Assess), the
//!   account whose key sends the opportunity's transaction, the gas limit that
//!   transaction may burn, any declared code override, and the settlement
//!   warmup it declares.
//!
//! Nothing here names a `cmd_executor`, a settlement token, a PoolManager, or a
//! command stream: a foreign contract's deployment declares no settlement
//! identity at all ([`SimulationDeployment::settlement`] is `None`) and crosses
//! the exact same simulation path as the built-in one (ADR-098 D6). The
//! built-in binding lives in [`crate::cmd_executor`] — on the built-in side of
//! the seam, never underneath it.
//!
//! # Actual deployed code is the production authority
//!
//! [`SimulationDeployment::code`] is an EXPLICIT development override: declared
//! runtime installed at a declared address in the simulated block. `None` means
//! the sim executes the code the chain holds at the deployment's address. The
//! override never substitutes silently for a different deployed implementation,
//! and the submission side refuses live submission under it
//! (`ExecutorPolicy::inject_code_guard`).
//!
//! # Fee sizing is a deployment choice, not an adapter-type guess
//!
//! [`SimulationDeployment::fee_sizing`] names how the adapter's assessed gross
//! becomes the returned net. [`FeeSizing::AdapterOwned`] (the default) returns
//! the adapter's [`degenbot_execution::ExecutionResult`] unchanged — its priority fee, net,
//! threshold, and verdict are the deployment's answer, so a foreign adapter's
//! Fee decision is never overwritten. Only a deployment that explicitly opts
//! into [`FeeSizing::MarketAware`] is re-priced with the settlement strategy's
//! market-aware sizing; the built-in `cmd_executor` binding is the one caller
//! that does (ADR-098 D7 keeps operator fee policy above the seam, and the
//! built-in path is where that policy applies).

use std::sync::Arc;

use alloy::primitives::Address;
use degenbot_execution::{DeploymentId, ExecutionAdapter};
use degenbot_rpc::fees::BlockPriorityFees;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::{DeclaredCode, SettlementWarmup, SimulationOverrideParams};

/// The facts ONE simulated block contributes to every candidate simulated at it.
#[derive(Clone)]
pub struct SimulationBlock<'a> {
    /// The typed RPC provider (the in-process engine's cold-miss fallback DB).
    pub provider: &'a AlloyProvider,
    /// The base fee of the next block (wei/gas) — the fee floor the simulated
    /// transactions run under and the pricing half of the built-in fee sizing.
    pub base_fee_next: u128,
    /// The block the batch is simulated at.
    pub current_block: u64,
    /// The timestamp of `current_block` (the EVM's `block.timestamp`, so a V2
    /// pair's `_update()` computes a correct `timeElapsed`).
    pub block_timestamp: u64,
    /// The latest block priority-fee percentiles (the fee-sizing bounds).
    pub block_priority_fees: Option<BlockPriorityFees>,
}

impl std::fmt::Debug for SimulationBlock<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The provider has no `Debug`; its `rpc_url` is logged separately by the
        // driver.
        f.debug_struct("SimulationBlock")
            .field("rpc_url", &self.provider.rpc_url())
            .field("base_fee_next", &self.base_fee_next)
            .field("current_block", &self.current_block)
            .field("block_timestamp", &self.block_timestamp)
            .field("block_priority_fees", &self.block_priority_fees)
            .finish()
    }
}

/// One execution deployment: the injected adapter plus the deployment facts the
/// engine's state overrides and the opportunity's transaction need.
pub struct SimulationDeployment {
    deployment_id: DeploymentId,
    adapter: Arc<dyn ExecutionAdapter + Send + Sync>,
    owner: Address,
    gas_limit: u64,
    code: Option<DeclaredCode>,
    settlement: Option<SettlementWarmup>,
    fee_sizing: FeeSizing,
}

/// How a deployment turns its adapter's assessed gross figure into the net
/// profit the strategy reports.
///
/// The adapter's Assess carries Fee as its pricing half (ADR-025 D2), so a
/// deployment returns that answer verbatim unless it says otherwise. Nothing
/// here inspects the adapter's type: a foreign adapter and the built-in one are
/// indistinguishable to the seam, and only an explicit deployment choice
/// re-prices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FeeSizing {
    /// The adapter owns Fee as well as Encode / Probe / Assess: its
    /// [`degenbot_execution::ExecutionResult`] reaches the caller unchanged — priority fee, net,
    /// threshold verdict and all. The default for every deployment.
    #[default]
    AdapterOwned,
    /// The deployment re-prices the adapter's gross figure with the settlement
    /// strategy's market-aware priority-fee sizing and the dispatch gas floor
    /// (`crate::simulator::price_gross`). Opted into by the built-in
    /// `cmd_executor` binding; a foreign deployment never inherits it.
    MarketAware,
}

impl std::fmt::Debug for SimulationDeployment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The adapter is user code with no `Debug` bound, so the derived fields
        // are named explicitly and the adapter is summarized by name.
        f.debug_struct("SimulationDeployment")
            .field("deployment_id", &self.deployment_id)
            .field("adapter", &"<ExecutionAdapter>")
            .field("owner", &self.owner)
            .field("gas_limit", &self.gas_limit)
            .field("code", &self.code)
            .field("settlement", &self.settlement)
            .field("fee_sizing", &self.fee_sizing)
            .finish()
    }
}

impl SimulationDeployment {
    /// Compose a deployment from its operator-assigned identity, its adapter,
    /// and its declared facts.
    ///
    /// `deployment_id` is required: a deployment has no unnamed form, because
    /// every candidate it executes is keyed by that identity (ADR-098 D4). The
    /// caller that composes the session names it — degenbot never invents one.
    ///
    /// `code` is the deployment's declared development override (see the module
    /// doc); `settlement` is what the deployment declares for WETH/PoolManager
    /// warmup — `None` for a contract with neither identity.
    ///
    /// The fee sizing starts at [`FeeSizing::AdapterOwned`]: the adapter's own
    /// `ExecutionResult` is the result until the deployment opts into
    /// market-aware re-pricing with [`Self::with_fee_sizing`].
    #[must_use]
    pub fn new(
        deployment_id: DeploymentId,
        adapter: Arc<dyn ExecutionAdapter + Send + Sync>,
        owner: Address,
        gas_limit: u64,
        code: Option<DeclaredCode>,
        settlement: Option<SettlementWarmup>,
    ) -> Self {
        Self {
            deployment_id,
            adapter,
            owner,
            gas_limit,
            code,
            settlement,
            fee_sizing: FeeSizing::default(),
        }
    }

    /// Re-bind the deployment's fee sizing (an explicit deployment policy).
    #[must_use]
    pub const fn with_fee_sizing(mut self, fee_sizing: FeeSizing) -> Self {
        self.fee_sizing = fee_sizing;
        self
    }

    /// This deployment's operator-assigned identity — the half of every
    /// [`degenbot_execution::CandidateId`] it executes.
    #[must_use]
    pub const fn deployment_id(&self) -> DeploymentId {
        self.deployment_id
    }

    /// How this deployment turns the assessed gross figure into net profit.
    #[must_use]
    pub const fn fee_sizing(&self) -> FeeSizing {
        self.fee_sizing
    }

    /// The injected Encode / Probe / Assess adapter.
    #[must_use]
    pub fn adapter(&self) -> &dyn ExecutionAdapter {
        self.adapter.as_ref()
    }

    /// The account whose key sends the opportunity's transaction (the `from`,
    /// and the account the engine funds in the simulated block).
    #[must_use]
    pub const fn owner(&self) -> Address {
        self.owner
    }

    /// The top-level gas limit the opportunity's transaction may burn.
    #[must_use]
    pub const fn gas_limit(&self) -> u64 {
        self.gas_limit
    }

    /// The declared development code override, if any.
    #[must_use]
    pub const fn code(&self) -> Option<&DeclaredCode> {
        self.code.as_ref()
    }

    /// The settlement warmup this deployment declares, if any.
    ///
    /// `None` is a complete answer: a foreign contract with no WETH or
    /// PoolManager identity declares none, and the fetch/prewarm lanes derive
    /// only the words its paths name.
    #[must_use]
    pub const fn settlement(&self) -> Option<SettlementWarmup> {
        self.settlement
    }

    /// Project the engine's state-override parameters for this deployment.
    #[must_use]
    pub fn override_params(&self) -> SimulationOverrideParams {
        SimulationOverrideParams {
            owner: self.owner,
            code: self.code.clone(),
            settlement: self.settlement,
        }
    }
}

/// The simulation context the canonical production entry drives: one simulated
/// block plus the deployment executing its opportunities.
#[derive(Debug)]
pub struct SimulateContext<'a> {
    /// The block-wide facts.
    pub block: SimulationBlock<'a>,
    /// The injected execution deployment.
    pub deployment: &'a SimulationDeployment,
}
