//! The simulation state-override parameters — the per-block-invariant
//! overrides the engine's handle applies once at build: owner funding, a
//! deployment's declared code override, the settlement warmup slots its
//! execution contract touches (see
//! `degenbot_executor::compute_simulation_warmup_slots`), and the capital
//! override plan.
//!
//! The facts are separate declarations on purpose. Funding the simulated
//! sender is generic. Declared code is an explicit development override — the
//! production authority is the deployed code the EVM reads from chain. The
//! settlement warmup belongs to a deployment whose contract has WETH /
//! PoolManager identities; a foreign contract declares none, and
//! [`SimulationOverrideParams::settlement`] is then `None` rather than a
//! placeholder address.
//!
//! # The capital plan is applied last, and it is authoritative
//!
//! [`SimulationOverrideParams::capital`] is ONE value — a
//! [`crate::CapitalOverridePlan`] — which names the account it funds or clears
//! and every resolved write it applies (see [`crate::sim::capital`]). The plan
//! is produced once where a fan-out is composed
//! ([`crate::ProbeCapital::override_plan`]), from that fan-out's own paths and
//! its deployment's execution target: neither arm of the declared-code /
//! deployed-code split inherits a target from the other, and no capital slot is
//! keyed on the warmup's own derivation.
//!
//! The application order states one rule: **the capital plan is applied after
//! the settlement warmup**, so a warmup can never reintroduce capital a
//! zero-balance clearing removed, and a self-funded requirement — the amount the
//! built Plan declares — is never overwritten by the warmup's operational WETH
//! balance or read as one of its 1-wei touches. The owner's gas funding and every
//! account the plan does not name are left alone.

use alloy::primitives::{Address, Bytes};

use degenbot_executor::WarmupSlots;

use crate::CapitalOverridePlan;

/// The owner funding, in whole ETH.
pub const OWNER_FUND_ETH: u64 = 100;

/// The injected executor's funding, in whole ETH (the deployed contract
/// wraps this much at construction; code injection skips that, so the
/// balance is set explicitly).
pub const EXECUTOR_FUND_ETH: u64 = 10;

/// One wei — the warmup touch amount.
pub const ONE_WEI: alloy::primitives::U256 = alloy::primitives::U256::from_limbs([1, 0, 0, 0]);

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
/// the capital plan resolves its own targets from the account it names, so a
/// warmup keyed on some other account can never redirect a probe's funding or
/// clearing.
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
    /// (production), the paths' declared entry capital installed at the executor
    /// (self-funded), or every asset those paths touch stripped from the executor
    /// (zero-balance).
    ///
    /// The plan's [`crate::CapitalOverridePlan::holder`] IS the capital target:
    /// every resolved write names that account, in both deployment arms. A
    /// declared-code override keeps the runtime it installed; a deployed
    /// executor keeps the code the chain holds — clearing touches the balance
    /// and the account's token/claim words, never the code.
    pub capital: CapitalOverridePlan,
}
