//! Sim-scoped override application on a `CacheDB`.
//!
//! Ports `degenbot-simulation::build_simulation_state_overrides` (owner funded
//! 100 ETH, injected executor + runtime bytecode, warmup slots, WETH9
//! `balanceOf` override) into `CacheDB::insert_account_storage` /
//! `insert_account_info` calls, preserving the **explicit-balance-wins** merge
//! documented on the source leaf (the executor's 10-ETH `balance` override
//! must NOT be clobbered by the warmup's residual-`0x0` balance; the WETH9
//! `balanceOf` slot IS overwritten by the warmup-to-10-ETH raise).
//!
//! This adaptor is **backing-agnostic** — it only calls `CacheDB::*` methods
//! that work identically whether the backing `Database` is `WrapDatabaseAsync<
//! AlloyDB>` (option A) or `BotStateDb<WrapDatabaseAsync<AlloyDB>>` (option B).
//!
//! # Filled by task `RBCQTQ`.
//!
//! See `docs/spikes/revm-composition-api-and-cold-miss-latency.md` §2.2 for the
//! verified `CacheDB` insertion API.

use revm::database::CacheDB;
use revm::database_interface::DatabaseRef;
use revm::primitives::{Address, Bytes, U256};
use revm::state::AccountInfo;

/// The owner's ETH-funding amount in the `stateOverrides` (100 ETH — mirrors
/// `build_simulation_state_overrides`'s owner funding for gas). Ports the Python
/// oracle's literal.
pub const OWNER_FUND_ETH: u128 = 100;

/// The injected executor's ETH-funding amount (10 ETH — for V4 settlement +
/// V3 callback WETH payments). Ports the Python oracle's literal.
pub const EXECUTOR_FUND_ETH: u128 = 10;

/// Parameters mirroring `degenbot-simulation::build_simulation_state_overrides`
/// inputs, supplied by the dispatch leaf. Carries the addresses + the runtime
/// bytecode + the warmup slots computed by `degenbot-executor`.
#[derive(Debug, Clone)]
pub struct SimulationOverrideParams {
    /// The operator key's address — the owner funded with ETH.
    pub owner: Address,
    /// Whether to inject the executor runtime bytecode at `injected_address`.
    pub inject_code: bool,
    /// The address to inject the executor bytecode at (used iff `inject_code`).
    pub injected_address: Option<Address>,
    /// The executor runtime bytecode (injected when `inject_code` is `true`).
    pub runtime_bytecode: Bytes,
    /// The warmup slots (WETH9 `balanceOf`, PoolManager ERC6909 `balanceOf`,
    /// the WETH9 `balanceOf`-slot-raise) from `degenbot_executor::WarmupSlots`.
    pub warmup: WarmupSlotsView,
    /// WETH9 contract address.
    pub weth_address: Address,
    /// Uniswap V4 PoolManager address.
    pub pool_manager_address: Address,
}

/// A crate-local view of `degenbot_executor::WarmupSlots` carrying the three
/// computed warmup **slot addresses** as `U256`. The executor leaf owns the
/// computation; this struct crosses the crate boundary as the raw slot keys.
///
/// Filled in by task `RBCQTQ` (constructed from
/// `degenbot_executor::compute_simulation_warmup_slots`).
#[derive(Debug, Clone, Copy, Default)]
pub struct WarmupSlotsView {
    /// WETH9 `balanceOf(executor)` mapping slot.
    pub weth_balance: U256,
    /// PoolManager ERC6909 `balanceOf(executor, weth)` mapping slot.
    pub pm_erc6909_balance: U256,
    /// The WETH9 `balanceOf` slot raised to the operational 10-ETH amount.
    pub weth_balance_raised: U256,
}

/// Apply the simulation state-overrides onto a `CacheDB`, mirroring
/// `degenbot-simulation::build_simulation_state_overrides` field-for-field.
///
/// The merge is **explicit-balance-wins**: an existing `balance` is preserved;
/// only absent balances are filled from the warmup. The WETH9 `balanceOf` slot
/// IS overwritten to the operational 10-ETH amount. See the source leaf's
/// doc-comment for the merge subtlety.
///
/// # Errors
///
/// Returns `OverrideError` if a `CacheDB` insertion fails (address-encoding
/// overflow — unreachable for valid `U256` slot keys).
///
/// # Filled by task `RBCQTQ`.
#[allow(clippy::missing_errors_doc)]
pub fn apply_simulation_overrides<ExtDb>(
    _cache_db: &mut CacheDB<ExtDb>,
    _params: &SimulationOverrideParams,
) -> Result<(), OverrideError>
where
    ExtDb: DatabaseRef,
{
    // TODO(RBCQTQ): port build_simulation_state_overrides — insert_account_info
    // for the owner (100 ETH) + injected executor (10 ETH + runtime bytecode);
    // insert_account_storage for the warmup slots + the WETH9 balanceOf raise.
    // Preserve the explicit-balance-wins merge order.
    let _ = OWNER_FUND_ETH;
    let _ = EXECUTOR_FUND_ETH;
    let _ = AccountInfo::default();
    todo!("RBCQTQ: port build_simulation_state_overrides into CacheDB insert_*")
}

/// Errors raised by `apply_simulation_overrides`.
#[derive(Debug, thiserror::Error)]
pub enum OverrideError {
    /// A `CacheDB` insertion failed (slot-key encoding overflow — unreachable
    /// for valid `U256` keys).
    #[error("CacheDB insertion failed: {0}")]
    Insertion(String),
}
