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
//!
//! # Capital targeting is selected independently of both
//!
//! The capital overrides (native ETH, WETH9, USDC, WBTC, and PoolManager
//! ERC6909 claims) are ONE value — [`SimulationOverrideParams::capital`], a
//! [`crate::CapitalOverridePlan`] — which names the account it funds or clears
//! itself and derives the account-keyed slots from it. The plan is produced once
//! where the deployment is composed
//! ([`crate::ProbeCapital::override_plan`]), from the deployment's execution
//! target: neither arm of the declared-code / deployed-code split inherits a
//! target from the other, and no capital slot is keyed on the warmup's own
//! derivation — a deployment whose warmup slots name one account and whose probe
//! names another still clears the probe's account. The owner's gas funding and
//! every account the overrides do not name are left alone.

use alloy::primitives::{address, Address, Bytes, U256};

use degenbot_executor::WarmupSlots;

use crate::CapitalOverridePlan;

/// The owner funding, in whole ETH.
pub const OWNER_FUND_ETH: u64 = 100;

/// The injected executor's funding, in whole ETH (the deployed contract
/// wraps this much at construction; code injection skips that, so the
/// balance is set explicitly).
pub const EXECUTOR_FUND_ETH: u64 = 10;

/// One wei — the warmup touch amount.
pub const ONE_WEI: U256 = U256::from_limbs([1, 0, 0, 0]);

/// Circle FiatToken V2's `balances` mapping base on Ethereum mainnet,
/// independently verified against the USDC/WETH 0.05% pool's live balance.
pub const USDC_MAINNET_BALANCE_SLOT: u64 = 9;
/// The mainnet USDC token named by the executor's deployment configuration.
pub const USDC_MAINNET: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
/// Wrapped BTC's `balances` mapping base on Ethereum mainnet, independently
/// verified against the WBTC/WETH 0.3% pool's live balance.
pub const WBTC_MAINNET_BALANCE_SLOT: u64 = 0;
pub const WBTC_MAINNET: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");

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
/// The built-in binding keys them on the deployment's EXECUTOR — the account the
/// deployment's transaction targets, whose capital the contract spends — in both
/// deployment arms, whether the deployment declares its code or executes the
/// code the chain holds. That subject is the binding's declaration, not an
/// invariant of this type. The slots are a warmup fact, not a capital target:
/// the override application derives its own capital slots from the account each
/// capital parameter names, so a warmup keyed on some other account can never
/// redirect a probe's funding or clearing.
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
#[derive(Debug, Clone, Default)]
pub struct SimulationOverrideParams {
    /// The simulated sender, funded with [`OWNER_FUND_ETH`] ETH for gas.
    pub owner: Address,
    /// The deployment's declared code override; `None` = the deployment's
    /// contract is deployed and the sim executes its actual code.
    pub code: Option<DeclaredCode>,
    /// The settlement warmup this deployment declares; `None` = the deployment
    /// has no WETH/PoolManager identity to warm.
    pub settlement: Option<SettlementWarmup>,
    /// The capital posture this simulation assumes, as one plan: nothing
    /// (production), the known asset set installed at the executor
    /// (self-funded), or that set stripped from the executor (zero-balance).
    ///
    /// The plan's [`crate::CapitalOverridePlan::holder`] IS the capital target:
    /// every capital slot is derived from it, in both deployment arms. A
    /// declared-code override keeps the runtime it installed; a deployed
    /// executor keeps the code the chain holds — clearing touches the balance
    /// and the account's token/claim slots, never the code.
    pub capital: CapitalOverridePlan,
}
