//! The in-process simulation primitives + entry point.
//!
//! Owns the SHARED sim types + pure functions BOTH consumers of the Rust core
//! reach: `simulate_one` (the `eth_simulateV1` path in `degenbot-simulation`)
//! and `simulate_in_process` (the in-process revm path, this crate). The two
//! are dual drivers over one engine — they share `SimResult`, `FailBuckets`,
//! `compute_priority_fee`, `fits_int128`, `SimulateContext`, `SimulatePath`,
//! `BlockPriorityFees` + the priority-fee constants so the dispatch leaf can
//! swap `dispatch::simulate_v1` for `simulate_in_process` behind a single
//! call-site change with no type translation.
//!
//! `degenbot-simulation` re-exports these (`pub use degenbot_evm::{...}`) so
//! existing `use degenbot_simulation::SimResult` call sites + the PyO3
//! wrappers stay unchanged.
//!
//! # `simulate_in_process` (task `JHGLF4`)
//!
//! Executes the 7-call vector (pre-balances → `execute()` → post-balances) via
//! revm `transact`, returning the same `SimResult` shape `simulate_one` yields.
//! See `docs/spikes/revm-composition-api-and-cold-miss-latency.md` §3 for the
//! measured latency profile (cold 8374 µs / 9 RPCs; warm 442 µs / 0 RPC; 18.9×
//! speedup vs cold) + §5 for the `ResultAndState.state` access-list emission API.

// Solidity/EVM identifiers (execute(bytes,uint256), int128, V4 BalanceDelta,
// WETH9, PoolManager, Multicall3, balanceOf, getEthBalance, ERC6909, etc.) are
// ubiquitous here — match the degenbot-simulation convention.
#![allow(clippy::doc_markdown)]

use std::collections::BTreeMap;

use alloy::eips::BlockId;
use alloy::network::Ethereum;
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, RootProvider};
use alloy::rpc::types::AccessList;
use degenbot_core::errors::{ProviderError, ProviderResult};
use degenbot_executor::composers::{encode_cmd_stream, EncodeOptions, HopInfo, PathInfo};
use degenbot_executor::WarmupSlots;
use degenbot_rpc::provider::AlloyProvider;
use revm::database_interface::WrapDatabaseAsync;
use std::sync::Arc;
// `ExecutionResult` lives in `revm::context_interface` (re-exported by the revm
// umbrella); `TxEnv` + `TxKind` in `revm::context` / `revm::primitives`.
use revm::context::TxEnv;
use revm::context_interface::result::ExecutionResult;
use revm::database::CacheDB;
use revm::primitives::TxKind;
// `MainBuilder` (`.build_mainnet()`) + `MainContext` (`.mainnet()`) — the revm
// EVM builder traits re-exported from the handler crate.
use revm::{ExecuteEvm, MainBuilder, MainContext};

use crate::calldata::{
    encode_balance_of_calldata, encode_erc6909_balance_of_calldata,
    encode_get_eth_balance_calldata, wrap_execute_calldata,
};
use crate::state_override::SimulationOverrideParams;

// ─────────────────────────────────────────────────────────────────────────
// Constants (ports the Python oracle's module-level literals)
// ─────────────────────────────────────────────────────────────────────────

/// The target profit ratio `TARGET_PROFIT_RATIO = 1.25` (L147) — the priority
/// fee is sized so the gross profit / (gas_fee + priority_fee*gas) ≈ 1.25.
pub const TARGET_PROFIT_RATIO: f64 = 1.25;

/// The age-decay constant `AGE_DECAY_CONSTANT = 0.25` (L150) — older results
/// are worth exponentially less: `priority_fee *= 1/(1 + 0.25*age)`.
pub const AGE_DECAY_CONSTANT: f64 = 0.25;

/// The min-priority-fee percentile index (`MIN_PRIORITY_FEE_PERCENTILE = 10`,
/// L151) — the floor is `p10 + 1`.
pub const MIN_PRIORITY_FEE_PERCENTILE: u64 = 10;

/// The max-priority-fee percentile index (`MAX_PRIORITY_FEE_PERCENTILE = 50`,
/// L152) — the ceiling is `p50 + 1`.
pub const MAX_PRIORITY_FEE_PERCENTILE: u64 = 50;

/// The 1.5× gas safety margin (`gas_used * 1.5`, L2421) — the simulate's
/// `gasUsed` for the `execute()` call is inflated before being assigned to
/// `tx_params["gas"]`.
pub const GAS_SAFETY_MARGIN: f64 = 1.5;

/// The initial gas cap the Python oracle grants the execute call for the
/// access-list computation (the `gas=5_000_000` literal at L1993). After the
/// simulate returns the real `gasUsed`, `tx_params["gas"]` is overwritten with
/// `gasUsed * 1.5`.
pub const INITIAL_EXECUTE_GAS: u64 = 5_000_000;

/// The `config=0` (check_mode=0, no bribe) the oracle uses — the operator
/// verifies profitability off-chain via the pre/post balance reads rather than
/// an on-chain profit check (L2017–L2020).
pub const EXECUTE_CONFIG: U256 = U256::ZERO;

/// The int128 range bounds (ports `INT128_MIN`/`INT128_MAX` from
/// `degenbot.arbitrage.encoding`, L19–L20).
pub const INT128_MIN: i128 = i128::MIN;
pub const INT128_MAX: i128 = i128::MAX;

// ─────────────────────────────────────────────────────────────────────────
// The int128 guard (C3 — ports with the WWC4DL reference)
// ─────────────────────────────────────────────────────────────────────────

/// Return `true` if `value` fits in a signed 128-bit integer.
///
/// Ports `fits_int128` (`INT128_MIN <= value <= INT128_MAX`). V4 `BalanceDelta`
/// uses int128 per component, so `amountSpecified` and the swap output delta
/// must both fit — the encoder rejects int128 overflow to avoid wasted
/// encoding (L1887–L1900).
#[must_use]
#[inline]
pub fn fits_int128(value: u128) -> bool {
    // `value` is non-negative by construction (a `u128` solver output).
    // `fits_int128(value)` ⟺ `value <= INT128_MAX` (the lower bound is
    // trivially satisfied since `u128` has no sign bit's payload).
    i128::try_from(value).is_ok() && value <= INT128_MAX as u128
}

// ─────────────────────────────────────────────────────────────────────────
// The result + failure types
// ─────────────────────────────────────────────────────────────────────────

/// The profitable simulation result — `(path_id, gross, net, gas, tx_params, path_info)`.
///
/// Mirrors the Python `simulate_one` return tuple (L2436). All gross-profitable
/// results are returned — the caller separates gas-profitable from
/// gas-unprofitable but onchain-valid (the comment at L2432–L2434).
#[derive(Debug, Clone)]
pub struct SimResult {
    /// The path id (`path_id`).
    pub path_id: u64,
    /// Gross profit = `(weth_after + eth_after + erc6909_after) -
    /// (weth_before + eth_before + erc6909_before)` (C2).
    pub gross_profit: U256,
    /// Net profit = `gross_profit - gas_used * (base_fee_next + priority_fee)`.
    pub net_profit: U256,
    /// The simulate's `gasUsed` for the `execute()` call (call `[3]`),
    /// UN-inflated. The 1.5× safety-margin `tx_params.gas` is exposed via
    /// [`SimResult::inflated_gas`].
    pub gas_used: u64,
    /// The market-aware priority fee (C4, the `_compute_priority_fee` output).
    pub priority_fee: u128,
    /// The base fee of the next block (`base_fee_next`).
    pub base_fee_next: u128,
    /// The `execute()` calldata (selector + ABI-wrapped `(bytes, uint256)`).
    pub execute_calldata: alloy::primitives::Bytes,
    /// The EIP-2930 access list computed for the `execute()` call, if any.
    pub access_list: Option<AccessList>,
    /// The number of hops in the path (for the caller's `path_info` reshape).
    pub hop_count: usize,
}

impl SimResult {
    /// The 1.5× safety-margin gas assigned to `tx_params["gas"]` (L2421).
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub fn inflated_gas(&self) -> u64 {
        // `int(gas_used * 1.5)` — the truncate semantics:
        // `gas_used` is `u64`; `* 1.5` is f64; `int(...)` truncates toward zero.
        // For any `u64`, `gas_used as f64 * 1.5` is exact for `gas_used < 2^53`,
        // and `u64` gas values are far below that ceiling.
        let g = (self.gas_used as f64) * GAS_SAFETY_MARGIN;
        g as u64
    }
}

/// A per-path simulation failure record captured alongside the [`FailBuckets`]
/// tally when a path fails simulation, surfaced as `DispatchOutcome::failures`
/// so the Python driver can render a per-candidate `[sim-fail]` line
/// (path_id + bucket label + the failing call's index in the 7-call vector +
/// the raw revert data bytes).
///
/// `fail_index` is `Some(idx)` for failures attributable to a specific
/// simulated call — the revert branch (`result.first_failure`) AND the
/// balance-decode branch (the malformed word's call index). It is `None` for
/// orchestration-only buckets (`int128-overflow`, `encode-failed`,
/// `rpc-failed`, `no-profit`) where no single call failed.
#[derive(Debug, Clone)]
pub struct SimFailure {
    /// The path id (mirror of `SimulatePath::path_id`).
    pub path_id: u64,
    /// The bucket label — the `classify_revert` output for reverts, else the
    /// orchestration-only bucket string.
    pub bucket: String,
    /// The index of the failing call in the 7-call vector, if any.
    pub fail_index: Option<usize>,
    /// The raw revert data bytes (the 4-byte selector + ABI-encoded args).
    /// Empty for orchestration-only buckets + the balance-decode branch
    /// (where the call succeeded but its returnData wasn't a uint256).
    pub revert_data: alloy::primitives::Bytes,
}

/// A revert-bucket tally accumulator (ports `_tally_fail`, L1769–L1771).
///
/// `_tally_fail(bucket)` does `_fail_buckets[bucket] = _fail_buckets.get(bucket, 0) + 1`.
/// The bucket strings are the `classify_revert` labels (SYI3PG) + a handful of
/// orchestration-only buckets (`int128-overflow`, `rpc-failed`,
/// `balance-decode`, `no-profit`, `encode-failed`, `blocked-path`).
///
/// In addition to the per-bucket count, this struct carries the per-path
/// [`SimFailure`] records (one per `tally`/`record` site) so the dispatch
/// outcome can surface per-candidate failure detail across the FFI boundary —
/// the count alone is insufficient for the operator to identify WHICH path
/// reverted against WHICH pools.
///
/// The rendering of these maps (the `[sim] summary` + the `[sim-fail]` lines)
/// stays in the Python driver — sibling D4 `stays-python`. This leaf only
/// ACCUMULATES.
#[derive(Debug, Default, Clone)]
pub struct FailBuckets {
    buckets: BTreeMap<String, u64>,
    failures: Vec<SimFailure>,
}

impl FailBuckets {
    /// Construct an empty tally.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment the bucket count (ports `_tally_fail`).
    pub fn tally(&mut self, bucket: &str) {
        *self.buckets.entry(bucket.to_string()).or_insert(0) += 1;
    }

    /// Record a per-path failure: increment the bucket count AND push the
    /// [`SimFailure`] detail so the dispatch outcome can surface per-candidate
    /// attribution (`path_id` + `fail_index` + `revert_data`).
    ///
    /// `tally` sites that should also be surfaced as a `[sim-fail]` detail call
    /// this instead of `tally` directly (most do — the exception is the
    /// hop-output length mismatch guard, which is a pre-sim skip, not a
    /// sim-failed bucket, and stays `tally`-less).
    pub fn record(
        &mut self,
        path_id: u64,
        bucket: &str,
        fail_index: Option<usize>,
        revert_data: alloy::primitives::Bytes,
    ) {
        self.tally(bucket);
        self.failures.push(SimFailure {
            path_id,
            bucket: bucket.to_string(),
            fail_index,
            revert_data,
        });
    }

    /// Read a bucket's count (0 if absent).
    #[must_use]
    pub fn get(&self, bucket: &str) -> u64 {
        self.buckets.get(bucket).copied().unwrap_or(0)
    }

    /// The underlying bucket→count map (for the Python driver's `[sim] summary`).
    #[must_use]
    pub fn buckets(&self) -> &BTreeMap<String, u64> {
        &self.buckets
    }

    /// The underlying bucket→count map, mutable (for the dispatch fan-out to
    /// merge per-path buckets into the outcome tally).
    pub fn buckets_mut(&mut self) -> &mut BTreeMap<String, u64> {
        &mut self.buckets
    }

    /// The per-path [`SimFailure`] records accumulated via [`record`] —
    /// surfaced as `DispatchOutcome::failures` for the Python driver's
    /// `[sim-fail]` line.
    #[must_use]
    pub fn failures(&self) -> &[SimFailure] {
        &self.failures
    }

    /// Take ownership of the per-path failures (used by the dispatch fan-out
    /// to move the per-path record set into the outcome tally without cloning).
    #[must_use]
    pub fn into_failures(self) -> Vec<SimFailure> {
        self.failures
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The per-block priority-fee percentiles (moved from degenbot-simulation::dispatch)
// ─────────────────────────────────────────────────────────────────────────

/// A per-block percentile fee summary the `_compute_priority_fee` consumer reads.
///
/// Mirrors the Python `dispatcher.block_priority_fees[block]` dict
/// (`dict(zip(FEE_PERCENTILES, reward[-1]))` — L2851): p10 and p50 priority-fee
/// samples for a single block, keyed by percentile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockPriorityFees {
    /// The block number these fees describe.
    pub block: u64,
    /// The p10 priority-fee sample (wei).
    pub p10: U256,
    /// The p50 priority-fee sample (wei).
    pub p50: U256,
}

// ─────────────────────────────────────────────────────────────────────────
// The market-aware age-decay priority fee (C4)
// ─────────────────────────────────────────────────────────────────────────

/// Compute the market-aware priority fee with age decay (ports
/// `_compute_priority_fee`, L1570–L1615).
///
/// The fee is:
/// 1. **Target fee** — the priority fee that achieves `TARGET_PROFIT_RATIO`
///    (1.25): `int((gross_profit / 1.25 - gas_used * base_fee_next) / gas_used)`,
///    floored at 1.
/// 2. **Age decay** — older results are worth exponentially less:
///    `priority_fee = int(target_priority_fee * (1 / (1 + 0.25 * age)))`,
///    where `age = max(0, current_block - solve_block)`.
/// 3. **Market bounds** — clamped to `[min_priority_fee, max_priority_fee]`,
///    where `min = max(p10 + 1, 1)` and `max = max(p50 + 1, min)`. If no
///    feeHistory percentiles are available, `min = 1` and `max = target`.
///
/// # Float→int truncate semantics (§4.2)
///
/// Both `target_priority_fee` and `priority_fee` use Python's `int(...)`,
/// which truncates toward zero. The Rust port uses `as u128` casts on `f64`,
/// which also truncate toward zero for non-negative values — the only values
/// possible here (the inputs are non-negative ints + the floor at 1 ensures
/// non-negativity).
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]
pub fn compute_priority_fee(
    gross_profit: U256,
    gas_used: u64,
    base_fee_next: u128,
    solve_block: u64,
    current_block: u64,
    block_priority_fees: Option<&BlockPriorityFees>,
) -> u128 {
    // Target fee from profit ratio.
    if gas_used == 0 {
        return 1;
    }
    let gross_f64 = u256_to_f64_lossy(gross_profit);
    let gas_f64 = gas_used as f64;
    let base_f64 = base_fee_next as f64;
    // `target_priority_fee = max(1, int((gross / 1.25 - gas * base) / gas))`.
    let target_raw = (gross_f64 / TARGET_PROFIT_RATIO - gas_f64 * base_f64) / gas_f64;
    let target_priority_fee = target_raw.max(1.0) as u128;

    // Age decay: factor = 1 / (1 + 0.25 * age). `age = max(0, current - solve)`.
    let age = current_block.saturating_sub(solve_block);
    let age_factor = 1.0 / (1.0 + AGE_DECAY_CONSTANT * age as f64);
    let priority_fee = (target_priority_fee as f64 * age_factor) as u128;

    // Market bounds from feeHistory.
    let mut min_priority_fee: u128 = 1;
    let mut max_priority_fee: u128 = target_priority_fee;
    if let Some(fees) = block_priority_fees {
        let p10 = fees.p10.to::<u128>();
        let p50 = fees.p50.to::<u128>();
        min_priority_fee = p10.saturating_add(1).max(1);
        max_priority_fee = p50.saturating_add(1).max(min_priority_fee);
    }

    // Clamp: `max(min, min(priority_fee, max))`.
    priority_fee.clamp(min_priority_fee, max_priority_fee)
}

/// Lossy `U256` → `f64` (mirrors Python's `int / float` promotion). The
/// priority-fee path is inherently lossy (the Python oracle mixes `int` gross
/// with `float` ratio); the §4.2 parity is on the `int(...)` truncation of the
/// final float, not on the f64 precision of the gross (which is exact for any
/// realistic Wei-denominated gross < 2^53).
#[allow(clippy::cast_precision_loss)]
fn u256_to_f64_lossy(v: U256) -> f64 {
    // `U256` → `f64` via the little-endian u64 limbs (high limb first).
    //
    // The §4.2 parity is on the `int(...)` truncate of the FINAL float (the
    // priority fee), not the f64 precision of the gross. Gross profits are
    // Wei-denominated and realistically < 2^53, where the f64 is exact.
    let limbs: &[u64; 4] = v.as_limbs();
    let mut acc: f64 = 0.0;
    for &limb in limbs.iter().rev() {
        acc = acc * ((1u128 << 64) as f64) + limb as f64;
    }
    acc
}

// ─────────────────────────────────────────────────────────────────────────
// The orchestration inputs (moved from degenbot-simulation::simulate_one)
// ─────────────────────────────────────────────────────────────────────────

/// Inputs to [`simulate_in_process`] / `simulate_one` that don't vary per-path.///
/// Ports the closure-captured state in the Python `simulate_one`: the
/// executor/weth/pm/multicall addresses, the funding flags, the warmup slots,
/// the dispatcher's `block_priority_fees` + the block context.
///
/// `provider` is omitted from `Debug` (the `AlloyProvider` doesn't impl it;
/// its `rpc_url` is logged separately by the driver).
#[derive(Clone)]
pub struct SimulateContext<'a> {
    /// The typed RPC provider (the §ZUZANP leaf, wrapped). The in-process path
    /// uses it for the cold-miss `AlloyDB` fallback + (interim, until
    /// [`crate::access_list::emit_access_list_from_state`] lands in task
    /// `ED3Q7R`) the `eth_createAccessList` / `eth_feeHistory` paths.
    pub provider: &'a AlloyProvider,
    /// The operator key's address — the `from` of the `execute()` call + the
    /// owner funded with ETH in `stateOverrides` (TCTUAW).
    pub executor_owner: Address,
    /// The cmd_executor contract address — the `execute()` target + the
    /// address whose balances are diffed.
    pub executor_address: Address,
    /// WETH9 contract address.
    pub weth_address: Address,
    /// Uniswap V4 PoolManager address.
    pub pool_manager_address: Address,
    /// Multicall3 contract address (the folded ETH `getEthBalance` target).
    pub multicall3_address: Address,
    /// Whether to inject the executor runtime bytecode (the `INJECT_EXECUTOR_CODE`
    /// flag) + the address to inject at (`INJECTED_EXECUTOR_ADDRESS`).
    pub inject_code: bool,
    /// The injected executor address. Used only when `inject_code` is `true`.
    pub injected_address: Option<Address>,
    /// The executor runtime bytecode (injected when `inject_code` is `true`).
    pub runtime_bytecode: alloy::primitives::Bytes,
    /// The warmup slots (the §62H23D leaf).
    pub warmup: WarmupSlots,
    /// The base fee of the NEXT block (`base_fee_next`).
    pub base_fee_next: u128,
    /// The block the solver produced the result on (`solve_block` is per-path;
    /// this is `current_block`).
    pub current_block: u64,
    /// The latest block priority-fee percentiles (p10/p50) — the
    /// `dispatcher.block_priority_fees[max(...)]` the Python oracle reads.
    pub block_priority_fees: Option<BlockPriorityFees>,
}

impl SimulateContext<'_> {
    /// Build the [`SimulationOverrideParams`] the state-override adaptor reads,
    /// projected from this context (owner, inject flags, warmup, addresses).
    #[must_use]
    pub fn override_params(&self) -> SimulationOverrideParams {
        SimulationOverrideParams {
            owner: self.executor_owner,
            inject_code: self.inject_code,
            injected_address: self.injected_address,
            runtime_bytecode: self.runtime_bytecode.clone(),
            warmup: self.warmup,
            weth_address: self.weth_address,
            pool_manager_address: self.pool_manager_address,
        }
    }
}

/// Per-path inputs to [`simulate_in_process`] / `simulate_one`.
#[derive(Debug, Clone)]
pub struct SimulatePath {
    /// The path id.
    pub path_id: u64,
    /// The arb's optimal input (`optimal_input`).
    pub optimal_input: u128,
    /// The per-hop solver outputs (`hop_outputs`).
    pub hop_outputs: Vec<u128>,
    /// The path info (the ordered hops — consumed by `encode_cmd_stream`).
    pub path_info: PathInfo,
    /// The block the solver produced the result on.
    pub solve_block: u64,
    /// The encode options (the `erc6909_profit` / `use_v4_batch` knobs the
    /// Python seam exposes).
    pub opts: EncodeOptions,
}

impl SimulatePath {
    /// The number of hops in the path (convenience for `path_info.hops.len()`).
    #[must_use]
    pub fn hop_count(&self) -> usize {
        self.path_info.hops.len()
    }

    /// Whether any hop is a V4 hop (the int128 guard keys on this).
    #[must_use]
    pub fn has_v4_hop(&self) -> bool {
        self.path_info
            .hops
            .iter()
            .any(|h| matches!(h, HopInfo::V4(_)))
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The in-process entry point (task JHGLF4)
// ─────────────────────────────────────────────────────────────────────────

/// The gas limit granted to each balance-of read call in the 7-call vector
/// (revm path). Mirrors the implicit gas the `eth_simulateV1` node grants a
/// read-only `eth_call`-shaped entry; a balanceOf SLOAD + RETURN fits well
/// under 100k gas.
pub const BALANCE_CALL_GAS_LIMIT: u64 = 100_000;

/// A `Provider<Ethereum>` newtype over `Arc<dyn Provider<Ethereum>>`, so the
/// type-erased provider from [`AlloyProvider::provider_arc`] satisfies
/// `P: Provider<Ethereum>` for [`revm::database::AlloyDB::new`].
///
/// Alloy's `Provider` trait carries `#[auto_impl::auto_impl(&, &mut, Rc, Arc,
/// Box)]`, but the generated impl is `impl<T: Provider + Sized> Provider for
/// Arc<T>` — it does **not** cover `?Sized` trait objects, so
/// `Arc<dyn Provider<Ethereum>>` itself does not satisfy `P: Provider` (the
/// task-body fork is real, not a non-issue). This newtype bridges the gap: it
/// stores the `Arc<dyn Provider>` (always `Sized`) and impls `Provider` by
/// delegating `root()` — the one non-default `Provider` method. Every other
/// `Provider` method has a default impl routed through `root()`.
#[derive(Clone)]
pub(crate) struct ArcDynProviderEthereum(Arc<dyn Provider<Ethereum>>);

impl Provider<Ethereum> for ArcDynProviderEthereum {
    fn root(&self) -> &RootProvider<Ethereum> {
        self.0.root()
    }
}

/// Execute the 7-call simulate vector in-process via revm, returning the same
/// [`SimResult`] shape `degenbot-simulation::simulate_one` yields.
///
/// Orchestration: int128 guard → encode (`encode_cmd_stream`,
/// `degenbot-executor`) → `execute()` calldata wrap
/// ([`wrap_execute_calldata`]) → apply state overrides
/// ([`apply_simulation_overrides`]) → revm `transact_one` per call (the
/// journaled state accumulates across the 7 calls, so post-balance reads see
/// the execute() changes) → parse balance diffs → compute gross/net profit +
/// the market-aware priority fee ([`compute_priority_fee`]) → return
/// [`SimResult`].
///
/// Block-env parity: revm gives explicit `BlockEnv { basefee, number, … }` —
/// pinned to `ctx.base_fee_next` + `ctx.current_block` (the same env the
/// Python oracle's `block_identifier="pending"` resolved to on the node).
///
/// The `bot_state` borrow backs [`crate::bot_state_db::BotStateDb`] (option B):
/// tracked pool slots served from engine state (0 RPC); untracked contracts +
/// pool internal slots fall through to the `WrapDatabaseAsync<AlloyDB>`
/// cold-miss fallback (RPC). The `provider` is the AlloyDB backing's RPC handle.
///
/// # Errors
///
/// Returns `Ok(None)` for any non-profitable / reverted / failed outcome — the
/// bucket is tallied before returning. Returns `Err` only on an unrecoverable
/// RPC failure (`rpc-failed`) or a BlockEnv/DB construction failure.
pub fn simulate_in_process(
    ctx: &SimulateContext<'_>,
    bot_state: &degenbot_bot::bot_core::BotState,
    path: SimulatePath,
    fail_buckets: &mut FailBuckets,
) -> ProviderResult<Option<SimResult>> {
    // Build the layered DB (option B — the engine state IS the EVM's Database,
    // AlloyDB cold-miss fallback for untracked contracts):
    //   CacheDB<BotStateDb<WrapDatabaseAsync<AlloyDB<Ethereum, ArcDynProviderEthereum>>>>
    //
    // `AlloyDB::new` requires `P: Provider<Ethereum>` by value. Alloy's
    // `Provider` trait carries `#[auto_impl::auto_impl(&, &mut, Rc, Arc, Box)]`,
    // but the generated impl is `impl<T: Provider + Sized> Provider for Arc<T>`
    // — it does NOT cover `?Sized` trait objects, so `Arc<dyn Provider>`
    // itself does not satisfy `P: Provider` (the fork in the original task
    // body is real). [`ArcDynProviderEthereum`] bridges the gap: it stores the
    // `Arc<dyn Provider>` and impls `Provider` by delegating `root()` (the one
    // non-default method; every other `Provider` method has a default impl
    // routed through `root()`).
    let alloy_db = revm::database::AlloyDB::new(
        ArcDynProviderEthereum(ctx.provider.provider_arc()),
        BlockId::Number(ctx.current_block.into()),
    );
    // `WrapDatabaseAsync::new` blocks on the ambient tokio runtime; it returns
    // `None` if there is no runtime or the runtime is `CurrentThread`. The pump
    // drives `simulate_in_process` through `pyo3_async_runtimes`, whose default
    // is `Builder::new_multi_thread()` — so this `None` path is unreachable in
    // production. A `None` here means the dispatch host lost its runtime; tally
    // `rpc-failed` identically to `simulate_one`'s RPC-failure path.
    let Some(wrap_db) = WrapDatabaseAsync::new(alloy_db) else {
        fail_buckets.record(
            path.path_id,
            "rpc-failed",
            None,
            alloy::primitives::Bytes::new(),
        );
        return Ok(None);
    };
    let bot_state_db = crate::BotStateDb::new(bot_state, wrap_db);
    let mut cache_db = CacheDB::new(bot_state_db);
    // Apply the state-override adaptor (owner funding, executor code injection,
    // warmup slots) over the layered DB — the same `apply_simulation_overrides`
    // the `simulate_one` path's `eth_simulateV1` `stateOverrides` carries.
    if let Err(err) =
        crate::state_override::apply_simulation_overrides(&mut cache_db, &ctx.override_params())
    {
        // The override adaptor only fails on an override-application error
        // (e.g. a warmup-slot write to an account the DB refused). Tally as
        // `rpc-failed` to keep the bucket shape identical to `simulate_one`'s
        // construction-failure path; the override-params are operator config,
        // so a failure here is a wiring error, not a per-path revert.
        fail_buckets.record(
            path.path_id,
            "rpc-failed",
            None,
            alloy::primitives::Bytes::new(),
        );
        log::warn!(
            "simulate_in_process: state-override application failed for path {}: {}",
            path.path_id,
            err
        );
        return Ok(None);
    }
    simulate_in_process_with_db(ctx, cache_db, path, fail_buckets)
}

/// The testable in-process orchestration core: runs the 7-call vector over a
/// PRE-BUILT `CacheDB` (overrides already applied by the caller) via revm
/// `transact_one` (the journaled state accumulates across the 7 calls), then
/// parses the balance diffs + computes the [`SimResult`].
///
/// Sync — no RPC, no tokio runtime needed (the `CacheDB`'s backing `Database`
/// is whatever the caller supplied; `EmptyDB` for the smoke test,
/// `BotStateDb<WrapDatabaseAsync<AlloyDB>>` for production via
/// [`simulate_in_process`]). This split lets the revm orchestration be
/// smoke-tested against `CacheDB<EmptyDB>` without a live RPC.
///
/// The caller owns the `cache_db` (by value); it is moved into the revm
/// `Context::with_db` (revm owns the DB for the EVM's lifetime). Overrides
/// must be applied BEFORE this call.
///
/// # Errors
///
/// Returns `Ok(None)` for non-profitable / reverted outcomes (bucket tallied).
/// Returns `Err` only on an unrecoverable revm `transact` error (a DB
/// cold-miss RPC failure — `rpc-failed`).
/// # Panics
///
/// Panics if a `TxEnv::builder().build()` fails (cannot happen with the
/// well-formed balance/execute calldata + addresses this fn constructs).
#[allow(clippy::needless_pass_by_value)]
#[allow(clippy::too_many_lines)]
pub fn simulate_in_process_with_db<Db>(
    ctx: &SimulateContext<'_>,
    cache_db: CacheDB<Db>,
    path: SimulatePath,
    fail_buckets: &mut FailBuckets,
) -> ProviderResult<Option<SimResult>>
where
    Db: revm::database_interface::DatabaseRef,
    <Db as revm::database_interface::DatabaseRef>::Error: std::fmt::Display,
{
    // C3 — int128 check (mirrors simulate_one's guard).
    if path.hop_outputs.len() != path.path_info.hops.len() {
        return Ok(None);
    }
    for (i, hop) in path.path_info.hops.iter().enumerate() {
        if let HopInfo::V4(_) = hop {
            let amount_specified = if i == 0 {
                path.optimal_input
            } else {
                path.hop_outputs[i - 1]
            };
            let output_amount = path.hop_outputs[i];
            if !fits_int128(amount_specified) || !fits_int128(output_amount) {
                fail_buckets.record(
                    path.path_id,
                    "int128-overflow",
                    None,
                    alloy::primitives::Bytes::new(),
                );
                return Ok(None);
            }
        }
    }

    // Encode the cmd_executor command stream (YQORTM).
    let cmd_bytes = encode_cmd_stream(
        &path.path_info,
        path.optimal_input,
        &path.hop_outputs,
        ctx.executor_address,
        ctx.pool_manager_address,
        ctx.weth_address,
        path.opts,
    );
    let Some(cmd_bytes) = cmd_bytes else {
        fail_buckets.record(
            path.path_id,
            "encode-failed",
            None,
            alloy::primitives::Bytes::new(),
        );
        return Ok(None);
    };

    // execute(bytes, uint256) ABI wrap (config=0 — no on-chain profit check).
    let execute_calldata = wrap_execute_calldata(ctx.executor_address, &cmd_bytes, EXECUTE_CONFIG)
        .map_err(|e| ProviderError::RpcError {
            code: -32603,
            message: format!("execute() ABI encode failed: {e}"),
        })?;

    // Build the 7 calldata blobs (the balance reads + the execute call).
    let weth_call =
        encode_balance_of_calldata(ctx.executor_address).expect("valid address encodes");
    let eth_call =
        encode_get_eth_balance_calldata(ctx.executor_address).expect("valid address encodes");
    let erc6909_call = encode_erc6909_balance_of_calldata(ctx.executor_address, ctx.weth_address)
        .expect("valid address + weth encode");

    // Build the EVM: CacheDB moved into the revm Context (revm owns it for the
    // EVM's lifetime). Block env pinned to ctx.base_fee_next + current_block
    // (the same env the Python oracle's `block_identifier="pending"` resolved to).
    //
    // `disable_nonce_check`: the 7 calls share ONE owner; `eth_simulateV1` does
    // NOT bump the caller's nonce per call (each entry is an `eth_call`-shaped
    // read, not a real tx), so revm's per-tx nonce floor would reject calls
    // [1..6] (nonce 0 < account nonce 1 after call [0] commits). Disable the
    // check — parity with the node's lenient simulate.
    let mut revm_ctx = revm::context::Context::mainnet();
    revm_ctx.cfg.disable_nonce_check = true;
    let mut evm = revm_ctx.with_db(cache_db).build_mainnet();
    evm.ctx.modify_block(|block| {
        block.basefee = u64::try_from(ctx.base_fee_next).unwrap_or(u64::MAX);
        block.number = U256::from(ctx.current_block);
    });

    // The 7 calls: 3 pre-balance reads, execute(), 3 post-balance reads. The
    // journaled state ACCUMULATES across transact_one calls (revm 41 API:
    // transact_one stores changes in the journal; finalize clears it), so the
    // post reads see the execute() changes. A reverted execute rolls back ONLY
    // its own changes (revm invariants) — the pre reads persist.
    let txs = [
        build_balance_tx(ctx, ctx.weth_address, &weth_call),
        build_balance_tx(ctx, ctx.multicall3_address, &eth_call),
        build_balance_tx(ctx, ctx.pool_manager_address, &erc6909_call),
        build_execute_tx(ctx, &execute_calldata),
        build_balance_tx(ctx, ctx.weth_address, &weth_call),
        build_balance_tx(ctx, ctx.multicall3_address, &eth_call),
        build_balance_tx(ctx, ctx.pool_manager_address, &erc6909_call),
    ];

    let mut results: Vec<ExecutionResult> = Vec::with_capacity(7);
    let mut first_failure: Option<usize> = None;
    for (idx, tx) in txs.into_iter().enumerate() {
        let Ok(res) = evm.transact_one(tx) else {
            // A revm transact error (DB cold-miss RPC failure, or an
            // invalid tx). Treat as rpc-failed (the Python oracle swallows
            // these into rpc-failed).
            fail_buckets.record(
                path.path_id,
                "rpc-failed",
                Some(idx),
                alloy::primitives::Bytes::new(),
            );
            return Ok(None);
        };
        if !res.is_success() && first_failure.is_none() {
            first_failure = Some(idx);
        }
        results.push(res);
    }

    // Finalize the journaled state (clears the journal). The state is available
    // for access-list emission (task ED3Q7R — currently a no-op stub, so the
    // access_list field stays None).
    let _state = evm.finalize();

    // Classify + tally the first revert if any call failed.
    if let Some(fail_idx) = first_failure {
        let revert_data = results[fail_idx]
            .output()
            .cloned()
            .filter(|b| !b.is_empty())
            .unwrap_or_default();
        let bucket = degenbot_decoders::revert::classify_revert(&revert_data);
        fail_buckets.record(path.path_id, &bucket, Some(fail_idx), revert_data);
        return Ok(None);
    }

    // C2 — gross profit: decode the 7 return values (3 pre + 3 post balance
    // diffs). execute() call [3] output is unused (its gas_used is read below).
    let decode = |idx: usize| -> Option<U256> { Some(decode_balance(results[idx].output()?)) };
    let Some(weth_before) = decode(0) else {
        fail_buckets.record(
            path.path_id,
            "balance-decode",
            Some(0),
            alloy::primitives::Bytes::new(),
        );
        return Ok(None);
    };
    let Some(eth_before) = decode(1) else {
        fail_buckets.record(
            path.path_id,
            "balance-decode",
            Some(1),
            alloy::primitives::Bytes::new(),
        );
        return Ok(None);
    };
    let Some(erc6909_before) = decode(2) else {
        fail_buckets.record(
            path.path_id,
            "balance-decode",
            Some(2),
            alloy::primitives::Bytes::new(),
        );
        return Ok(None);
    };
    let Some(weth_after) = decode(4) else {
        fail_buckets.record(
            path.path_id,
            "balance-decode",
            Some(4),
            alloy::primitives::Bytes::new(),
        );
        return Ok(None);
    };
    let Some(eth_after) = decode(5) else {
        fail_buckets.record(
            path.path_id,
            "balance-decode",
            Some(5),
            alloy::primitives::Bytes::new(),
        );
        return Ok(None);
    };
    let Some(erc6909_after) = decode(6) else {
        fail_buckets.record(
            path.path_id,
            "balance-decode",
            Some(6),
            alloy::primitives::Bytes::new(),
        );
        return Ok(None);
    };

    let combined_before = weth_before + eth_before + erc6909_before;
    let combined_after = weth_after + eth_after + erc6909_after;
    let gross_profit = combined_after.saturating_sub(combined_before);
    if gross_profit.is_zero() {
        fail_buckets.record(
            path.path_id,
            "no-profit",
            None,
            alloy::primitives::Bytes::new(),
        );
        return Ok(None);
    }

    // The execute() call's gas_used (revm per-call, no eth_simulateV1
    // block-aggregation edge case).
    let gas_used = results[3].gas().total_gas_spent();

    // C4 — the market-aware age-decay priority fee.
    let priority_fee = compute_priority_fee(
        gross_profit,
        gas_used,
        ctx.base_fee_next,
        path.solve_block,
        ctx.current_block,
        ctx.block_priority_fees.as_ref(),
    );

    // Net profit = gross - gas_used * (base_fee_next + priority_fee).
    let gas_fee = U256::from(gas_used)
        .saturating_mul(U256::from(ctx.base_fee_next.saturating_add(priority_fee)));
    let net_profit = gross_profit.saturating_sub(gas_fee);

    // ED3Q7R — emit the EIP-2930 access list the execute() call warmed. The
    // 7-call profit-detection above used `transact_one` (journal accumulates
    // across all 7 calls), so its `finalize()` state carries the cumulative
    // touches (balance reads + execute). For the SUBMITTED execute() tx's
    // access list we need execute()-only touched slots — so a separate
    // `transact(execute_tx)` (fresh journal for this one tx) captures its
    // `ResultAndState.state` + `emit_access_list_from_state` emits the list.
    //
    // The re-run executes on the post-7-call committed `CacheDB` state
    // (execute's first run already committed). The touched SLOT SET is
    // invariant to balance values (execute() warms the same pool slots
    // regardless of the balance state — a revert still warms the slots it
    // accessed before reverting), so the re-run's access list equals the
    // first run's. No `eth_createAccessList` RPC.
    let access_list = match evm.transact(build_execute_tx(ctx, &execute_calldata)) {
        Ok(result_and_state) => Some(crate::access_list::emit_access_list_from_state(
            &result_and_state.state,
        )),
        Err(_) => {
            // The re-run failed (a DB cold-miss RPC failure on the committed
            // state). Fall back to no access list — the submitted tx carries
            // none (same as a simulate-one path whose eth_createAccessList
            // swallowed + re-simulated without one).
            None
        }
    };

    Ok(Some(SimResult {
        path_id: path.path_id,
        gross_profit,
        net_profit,
        gas_used,
        priority_fee,
        base_fee_next: ctx.base_fee_next,
        execute_calldata,
        access_list,
        hop_count: path.hop_count(),
    }))
}

/// Build a read-only balance-call `TxEnv` (caller = owner, target = `to`, gas =
/// [`BALANCE_CALL_GAS_LIMIT`]). `gas_price` is set to `ctx.base_fee_next` so the
/// top-level tx clears revm's `max_fee_per_gas >= basefee` check (the simulate
/// oracle's `max_fee_per_gas=0` was accepted by the node's lenient
/// `eth_simulateV1`; revm enforces the fee floor).
fn build_balance_tx(
    ctx: &SimulateContext<'_>,
    to: Address,
    data: &alloy::primitives::Bytes,
) -> TxEnv {
    TxEnv::builder()
        .caller(ctx.executor_owner)
        .kind(TxKind::Call(to))
        .data(alloy::primitives::Bytes::copy_from_slice(data))
        .value(U256::ZERO)
        .gas_limit(BALANCE_CALL_GAS_LIMIT)
        .gas_price(ctx.base_fee_next.max(1))
        .build()
        .expect("valid balance-call TxEnv")
}

/// Build the `execute(bytes, uint256)` `TxEnv` (caller = owner, target =
/// executor, gas = [`INITIAL_EXECUTE_GAS`]).
fn build_execute_tx(ctx: &SimulateContext<'_>, data: &alloy::primitives::Bytes) -> TxEnv {
    TxEnv::builder()
        .caller(ctx.executor_owner)
        .kind(TxKind::Call(ctx.executor_address))
        .data(alloy::primitives::Bytes::copy_from_slice(data))
        .value(U256::ZERO)
        .gas_limit(INITIAL_EXECUTE_GAS)
        .gas_price(ctx.base_fee_next.max(1))
        .build()
        .expect("valid execute TxEnv")
}

/// Decode a 32-byte big-endian uint256 from a revm call's return output.
/// Mirrors `simulate_one`'s `decode_balance`: empty → `0`;
/// 32 bytes → big-endian uint256; >32 → last 32 bytes (uint256 ABI right-align);
/// 1..31 → left-padded.
fn decode_balance(data: &alloy::primitives::Bytes) -> U256 {
    match data.len() {
        0 => U256::ZERO,
        32 => U256::from_be_slice(data),
        n if n > 32 => {
            let tail = &data[n - 32..];
            U256::from_be_slice(tail)
        }
        n => {
            let mut buf = [0u8; 32];
            buf[32 - n..].copy_from_slice(data);
            U256::from_be_slice(&buf)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;
    use alloy::primitives::U256;

    // ── C3: fits_int128 ──────────────────────────────────────────────────

    #[test]
    fn fits_int128_matches_python_bounds() {
        assert!(fits_int128(0));
        assert!(fits_int128(1));
        // INT128_MAX = (1<<127) - 1 fits; (1<<127) doesn't.
        assert!(fits_int128(INT128_MAX as u128));
        assert!(!fits_int128((INT128_MAX as u128) + 1));
        assert!(!fits_int128(u128::MAX));
    }

    // ── C4: compute_priority_fee — §4.2 parity vs _compute_priority_fee ──

    #[test]
    fn priority_fee_zero_gas_returns_one() {
        let fees = BlockPriorityFees {
            block: 100,
            p10: U256::ZERO,
            p50: U256::ZERO,
        };
        assert_eq!(
            compute_priority_fee(U256::from(1_000u128), 0, 1, 100, 100, Some(&fees)),
            1
        );
    }

    #[test]
    fn priority_fee_target_truncates_toward_zero() {
        // gross=1000 wei, gas=100, base=0, age=0.
        // target = max(1, int((1000/1.25 - 0)/100)) = int(800/100) = int(8.0) = 8.
        // age_factor = 1/(1+0) = 1.0. priority_fee = int(8*1.0) = 8.
        // bounds: p10=p50=0 → min=max(1,1)=1, max=max(1,1)=1.
        // clamp(8, 1, 1) = 1.
        let fees = BlockPriorityFees {
            block: 100,
            p10: U256::ZERO,
            p50: U256::ZERO,
        };
        assert_eq!(
            compute_priority_fee(U256::from(1_000u128), 100, 0, 100, 100, Some(&fees)),
            1
        );
    }

    #[test]
    fn priority_fee_uses_p10_plus_one_floor() {
        // gross=1e6, gas=100, base=0, age=0.
        // target = int(1e6/1.25/100) = int(8000.0) = 8000.
        // priority_fee = 8000 (age 0). bounds: p10=99, p50=199.
        //   min = max(99+1, 1) = 100. max = max(199+1, 100) = 200.
        // clamp(8000, 100, 200) = 200.
        let fees = BlockPriorityFees {
            block: 100,
            p10: U256::from(99u128),
            p50: U256::from(199u128),
        };
        assert_eq!(
            compute_priority_fee(U256::from(1_000_000u128), 100, 0, 100, 100, Some(&fees)),
            200
        );
    }

    #[test]
    fn priority_fee_applies_age_decay() {
        // gross=1e9, gas=1000, base=0, solve=100, current=104 → age=4.
        // target = int(1e9/1.25/1000) = int(800_000.0) = 800_000.
        // age_factor = 1/(1+0.25*4) = 1/2 = 0.5. priority_fee = int(400_000.0) = 400_000.
        // bounds: p10=0, p50=0 → min=1, max=max(1,1)=1. clamp(400_000, 1, 1) = 1.
        let fees = BlockPriorityFees {
            block: 100,
            p10: U256::ZERO,
            p50: U256::ZERO,
        };
        assert_eq!(
            compute_priority_fee(
                U256::from(1_000_000_000u128),
                1000,
                0,
                100,
                104,
                Some(&fees)
            ),
            1
        );
    }

    #[test]
    fn priority_fee_age_decay_without_market_clamp() {
        // Same as above but with a generous p50 so the age-decayed fee survives.
        // target=800_000, age_factor=0.5 → priority=400_000.
        // p10=0, p50=1_000_000 → min=1, max=1_000_001 ≥ 400_000.
        let fees = BlockPriorityFees {
            block: 100,
            p10: U256::ZERO,
            p50: U256::from(1_000_000u128),
        };
        assert_eq!(
            compute_priority_fee(
                U256::from(1_000_000_000u128),
                1000,
                0,
                100,
                104,
                Some(&fees)
            ),
            400_000
        );
    }

    #[test]
    fn priority_fee_no_market_uses_target_as_ceiling() {
        // No feeHistory → min=1, max=target. age=0 → priority=target.
        // gross=1e6, gas=100, base=0 → target=8000, priority=8000.
        // clamp(8000, 1, 8000) = 8000.
        assert_eq!(
            compute_priority_fee(U256::from(1_000_000u128), 100, 0, 100, 100, None),
            8000
        );
    }

    // ── simulate_in_process_with_db: the in-process smoke test ───────────

    use alloy::primitives::{address, Bytes};
    use alloy::providers::{Provider, ProviderBuilder};
    use alloy::rpc::client::ClientBuilder;
    use alloy::transports::mock::{Asserter, MockTransport};
    use degenbot_executor::composers::{EncodeOptions, HopInfo, PathInfo, V2HopInfo};
    use degenbot_executor::{compute_simulation_warmup_slots, WarmupSlots};
    use degenbot_rpc::provider::AlloyProvider;
    use revm::database::CacheDB;
    use revm::database_interface::EmptyDB;
    use std::sync::Arc;

    const SMOKE_OWNER: Address = address!("9c56a29c7231974c269e24f9fb3c29203039089e");
    const SMOKE_EXECUTOR: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const SMOKE_WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const SMOKE_PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
    const SMOKE_MULTICALL3: Address = address!("c411372f0b8ae58585e33b78aea9e0596da9a6f1");

    fn smoke_provider(asserter: &Asserter) -> AlloyProvider {
        let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
        )
    }

    fn smoke_warmup() -> WarmupSlots {
        compute_simulation_warmup_slots(SMOKE_EXECUTOR, SMOKE_WETH, SMOKE_PM)
    }

    fn smoke_ctx(provider: &AlloyProvider) -> SimulateContext<'_> {
        SimulateContext {
            provider,
            executor_owner: SMOKE_OWNER,
            executor_address: SMOKE_EXECUTOR,
            weth_address: SMOKE_WETH,
            pool_manager_address: SMOKE_PM,
            multicall3_address: SMOKE_MULTICALL3,
            inject_code: true,
            injected_address: Some(SMOKE_EXECUTOR),
            runtime_bytecode: Bytes::from_static(&[0xfe]), // INVALID — execute() reverts
            warmup: smoke_warmup(),
            base_fee_next: 1_000_000_000u128,
            current_block: 100,
            block_priority_fees: Some(BlockPriorityFees {
                block: 100,
                p10: U256::from(500_000_000u64),
                p50: U256::from(2_000_000_000u64),
            }),
        }
    }

    fn smoke_v2_path(path_id: u64) -> SimulatePath {
        SimulatePath {
            path_id,
            optimal_input: 1_000_000_000_000_000_000u128,
            hop_outputs: vec![1_100_000_000_000_000_000u128, 1_210_000_000_000_000_000u128],
            path_info: PathInfo::new(vec![
                HopInfo::V2(V2HopInfo {
                    pool_address: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                    token0_address: SMOKE_WETH,
                    token1_address: address!("1111111111111111111111111111111111111111"),
                    fee: 30,
                    zfo: true,
                }),
                HopInfo::V2(V2HopInfo {
                    pool_address: address!("cccccccccccccccccccccccccccccccccccccccc"),
                    token0_address: address!("1111111111111111111111111111111111111111"),
                    token1_address: SMOKE_WETH,
                    fee: 30,
                    zfo: true,
                }),
            ]),
            solve_block: 100,
            opts: EncodeOptions {
                erc6909_profit: false,
                use_v4_batch: false,
            },
        }
    }

    /// The smoke test: `simulate_in_process_with_db` over `CacheDB<EmptyDB>`
    /// (no RPC, no `BotState` tracked pools) with the executor injected as a
    /// `0xfe` INVALID contract → the execute() call [3] reverts → the
    /// `first_failure` is [3] + the revert bucket tallies. Proves the full
    /// revm 7-call orchestration runs end-to-end (CacheDB built, overrides
    /// applied, EVM built, block env set, 7 transact_one calls executed,
    /// journal accumulated + finalized, revert classified + tallied).
    #[test]
    fn simulate_in_process_with_db_revert_path_smoke() {
        let asserter = Asserter::new();
        let provider = smoke_provider(&asserter);
        let ctx = smoke_ctx(&provider);
        let mut cache_db: CacheDB<EmptyDB> = CacheDB::new(EmptyDB::default());
        crate::state_override::apply_simulation_overrides(&mut cache_db, &ctx.override_params())
            .expect("overrides apply over EmptyDB");
        let mut buckets = FailBuckets::new();

        let result =
            simulate_in_process_with_db(&ctx, cache_db, smoke_v2_path(7), &mut buckets).unwrap();
        assert!(result.is_none(), "reverting execute returns None");
        // The execute() call [3] reverted (0xfe INVALID Halt). A Halt has no
        // output → classify_revert on empty bytes → the "empty" bucket.
        assert_eq!(buckets.get("empty"), 1, "revert bucket tallied");
        let failures = buckets.failures();
        assert_eq!(failures.len(), 1, "one per-path failure recorded");
        assert_eq!(failures[0].path_id, 7);
        assert_eq!(
            failures[0].fail_index,
            Some(3),
            "the execute() call [3] failed"
        );
    }

    /// The no-profit smoke: executor injected as EMPTY bytecode (no-op) → all 7
    /// calls succeed (balance calls to no-code accounts return empty → 0) →
    /// pre==post → gross 0 → `no-profit` bucket. Proves the success-path
    /// orchestration (7 transact_one, journal finalize, balance decode,
    /// profit arithmetic, tally) runs end-to-end.
    #[test]
    fn simulate_in_process_with_db_no_profit_path_smoke() {
        let asserter = Asserter::new();
        let provider = smoke_provider(&asserter);
        let mut ctx = smoke_ctx(&provider);
        ctx.runtime_bytecode = Bytes::new(); // empty bytecode — execute() is a no-op
        let mut cache_db: CacheDB<EmptyDB> = CacheDB::new(EmptyDB::default());
        crate::state_override::apply_simulation_overrides(&mut cache_db, &ctx.override_params())
            .expect("overrides apply over EmptyDB");
        let mut buckets = FailBuckets::new();

        let result =
            simulate_in_process_with_db(&ctx, cache_db, smoke_v2_path(11), &mut buckets).unwrap();
        assert!(result.is_none(), "no-profit returns None");
        assert_eq!(buckets.get("no-profit"), 1, "no-profit bucket tallied");
        let failures = buckets.failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].path_id, 11);
        assert_eq!(failures[0].bucket, "no-profit");
        assert!(failures[0].fail_index.is_none(), "no call reverted");
    }
}
