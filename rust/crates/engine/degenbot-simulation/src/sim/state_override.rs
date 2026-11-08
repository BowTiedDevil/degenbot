//! The simulation state-override parameters — the per-block-invariant
//! overrides the engine's handle applies once at build: owner funding, a
//! deployment's declared code override, and the settlement warmup slots its
//! execution contract touches (see
//! `degenbot_executor::compute_simulation_warmup_slots`).
//!
//! The three facts are separate declarations on purpose. Funding the simulated
//! sender is generic. Declared code is an explicit development override — the
//! production authority is the deployed code the EVM reads from chain. The
//! settlement warmup belongs to a deployment whose contract has WETH /
//! PoolManager identities; a foreign contract declares none, and
//! [`SimulationOverrideParams::settlement`] is then `None` rather than a
//! placeholder address.

use alloy::primitives::{Address, Bytes, U256};

use degenbot_executor::WarmupSlots;

/// The owner funding, in whole ETH.
pub const OWNER_FUND_ETH: u64 = 100;

/// The injected executor's funding, in whole ETH (the deployed contract
/// wraps this much at construction; code injection skips that, so the
/// balance is set explicitly).
pub const EXECUTOR_FUND_ETH: u64 = 10;

/// One wei — the warmup touch amount.
pub const ONE_WEI: U256 = U256::from_limbs([1, 0, 0, 0]);

/// A deployment's declared development code override: `runtime` is installed at
/// `address` in the simulated block.
///
/// This is explicit deployment data, never a silent substitution: a production
/// deployment leaves it `None` so the sim executes the code the chain holds at
/// the deployment's address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredCode {
    /// The address the declared runtime is installed at.
    pub address: Address,
    /// The declared runtime bytecode.
    pub runtime: Bytes,
}

/// The settlement warmup a deployment declares: the WETH and PoolManager
/// identities its execution contract accounts against, plus the slots that
/// contract touches on its first run.
///
/// A deployment with neither identity (a foreign contract) declares no
/// settlement warmup at all — see [`SimulationOverrideParams::settlement`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlementWarmup {
    /// WETH9 contract address.
    pub weth_address: Address,
    /// Uniswap V4 PoolManager address, when the deployment names one (a lab
    /// fixture without a PoolManager declares none, and its two ERC6909 words are
    /// then simply not derived or warmed).
    pub pool_manager_address: Option<Address>,
    /// The warmup slots (1-wei touches on WETH9 + the PoolManager).
    pub slots: WarmupSlots,
}

/// The override inputs (see the module doc).
#[derive(Debug, Clone)]
pub struct SimulationOverrideParams {
    /// The simulated sender, funded with [`OWNER_FUND_ETH`] ETH for gas.
    pub owner: Address,
    /// The deployment's declared code override; `None` = the deployment's
    /// contract is deployed and the sim executes its actual code.
    pub code: Option<DeclaredCode>,
    /// The settlement warmup this deployment declares; `None` = the deployment
    /// has no WETH/PoolManager identity to warm.
    pub settlement: Option<SettlementWarmup>,
}
