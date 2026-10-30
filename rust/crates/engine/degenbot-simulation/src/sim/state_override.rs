//! The simulation state-override parameters — the per-block-invariant
//! overrides the engine's handle applies once at build: owner funding,
//! executor bytecode injection, and the warmup slots the executor's first
//! run touches (see `degenbot_executor::compute_simulation_warmup_slots`).

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

/// The override inputs (see the module doc).
#[derive(Debug, Clone)]
pub struct SimulationOverrideParams {
    /// The executor owner (funded with [`OWNER_FUND_ETH`] ETH for gas).
    pub owner: Address,
    /// Whether to inject the executor runtime bytecode.
    pub inject_code: bool,
    /// The injection address (required when `inject_code`).
    pub injected_address: Option<Address>,
    /// The executor runtime bytecode.
    pub runtime_bytecode: Bytes,
    /// The warmup slots (1-wei touches on WETH9 + the PoolManager).
    pub warmup: WarmupSlots,
    /// WETH9 contract address.
    pub weth_address: Address,
    /// Uniswap V4 PoolManager address.
    pub pool_manager_address: Address,
}
