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

use alloy::primitives::{Address, U256};
use alloy::rpc::types::AccessList;
use degenbot_core::errors::ProviderResult;
use degenbot_executor::composers::{EncodeOptions, HopInfo, PathInfo};
use degenbot_executor::WarmupSlots;
use degenbot_rpc::provider::AlloyProvider;

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

/// Inputs to [`simulate_in_process`] / `simulate_one` that don't vary per-path.
///
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

/// Execute the 7-call simulate vector in-process via revm, returning the same
/// [`SimResult`] shape `degenbot-simulation::simulate_one` yields.
///
/// Orchestration: int128 guard → encode (`encode_cmd_stream`,
/// `degenbot-executor`) → `execute()` calldata wrap
/// (`degenbot_simulation::payload::wrap_execute_calldata`) → apply state
/// overrides ([`crate::state_override::apply_simulation_overrides`]) → revm
/// `transact` per call → parse balance diffs → compute gross/net profit +
/// the market-aware priority fee ([`compute_priority_fee`]) → return
/// [`SimResult`].
///
/// Block-env parity: revm gives explicit `BlockEnv { number, timestamp,
/// basefee, gas_limit, beneficiary }` — pinned to the same env the Python
/// oracle used (`base_fee_next`, the pump's block timestamp/number).
///
/// # Filled by task `JHGLF4`
///
/// The revm 7-call sequential execution (shared `CacheDB<BotStateDb<…>>`,
/// `transact` per call, the journaled-state persists across calls) + the
/// `ResultAndState` revert classification + the balance-diff decode land here.
#[allow(clippy::missing_errors_doc)]
pub async fn simulate_in_process(
    _ctx: &SimulateContext<'_>,
    _path: SimulatePath,
    _fail_buckets: &mut FailBuckets,
) -> ProviderResult<Option<SimResult>> {
    // TODO(JHGLF4): port simulate_one's 7-call orchestration into revm
    // transact. Return the same SimResult fields the dispatch leaf reads.
    todo!("JHGLF4: port simulate_one into in-process revm transact")
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
}
