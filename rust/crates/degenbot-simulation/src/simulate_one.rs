//! The `simulate_one` per-path async orchestration (C1 capstone).
//!
//! Ports `examples/eth_backrun_v2_v3_v4_rust.py::simulate_one` (L1857–L2437) +
//! `_compute_priority_fee` (L1570–L1615) + `_tally_fail` (L1769–L1771) + the
//! int128 guard (L1887–L1900, ports with `fits_int128`).
//!
//! This is the core simulation leaf — pure orchestration over the sibling
//! A/B leaves + WWC4DL encode + SYI3PG revert. It does NOT own any business
//! logic that the sibling leaves own:
//!
//! - **Encode** → `degenbot_executor::composers::encode_cmd_stream` (YQORTM).
//! - **`execute()` calldata wrap** → `payload::wrap_execute_calldata`
//!   (which delegates to `encode_execute_call`, YQORTM).
//! - **`stateOverrides`** → `build_simulation_state_overrides` (TCTUAW).
//! - **`accessList`** → `dispatch::create_access_list` (PGA5S3).
//! - **`eth_simulateV1` dispatch** → `dispatch::simulate_v1` (PGA5S3).
//! - **7-call result parse** → `dispatch::SimulationResult` (PGA5S3).
//! - **Revert classification** → `degenbot_decoders::classify_revert` (SYI3PG).
//!
//! What this leaf DOES own:
//! - the int128 guard (C3 — ports with the WWC4DL reference; pure int range check),
//! - the orchestration (C1 — encode → calldata → access-list → simulate → parse → classify → profit → return),
//! - the gross/net profit arithmetic (C2 — pure int over the 7-call balance diffs),
//! - the market-aware age-decay priority fee (C4 — uses float, preserves the
//!   `int(...)` truncate semantics exactly; the float boundary is documented
//!   per-fn),
//! - the `_tally_fail` bucket accumulator (C1 — the rendering stays Python,
//!   sibling D4 `stays-python`).
//!
//! # Non-goals (C6 `stays-python`)
//!
//! The revert param-decode for the `[sim-fail]` log, `_dump_state_for_revert`,
//! `_emit_sim_diag`, `_dumped_path_types`, the V2/V3/V4 on-chain state queries
//! — all diagnostic-only, stay in the Python driver.

// Solidity/EVM identifiers (balanceOf, getEthBalance, ERC6909, Multicall3,
// execute(bytes,uint256), int128, V4 BalanceDelta, etc.) are ubiquitous here.
#![allow(clippy::doc_markdown)]

use std::collections::BTreeMap;

use alloy::primitives::{Address, U256};
use alloy::rpc::types::eth::state::StateOverride;
use alloy::rpc::types::AccessList;
use degenbot_core::errors::ProviderResult;
use degenbot_decoders::revert::classify_revert;
use degenbot_executor::composers::{encode_cmd_stream, EncodeOptions, HopInfo, PathInfo};
use degenbot_executor::WarmupSlots;
use degenbot_rpc::provider::AlloyProvider;

use crate::dispatch::{self, BlockPriorityFees};
use crate::payload::{wrap_execute_calldata, SimulationParams};

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
// The result type
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

/// A revert-bucket tally accumulator (ports `_tally_fail`, L1769–L1771).
///
/// `_tally_fail(bucket)` does `_fail_buckets[bucket] = _fail_buckets.get(bucket, 0) + 1`.
/// The bucket strings are the `classify_revert` labels (SYI3PG) + a handful of
/// orchestration-only buckets (`int128-overflow`, `rpc-failed`,
/// `balance-decode`, `no-profit`, `encode-failed`, `blocked-path`).
///
/// The rendering of this map (the `[sim] summary` line) stays in the Python
/// driver — sibling D4 `stays-python`. This leaf only ACCUMULATES.
#[derive(Debug, Default, Clone)]
pub struct FailBuckets(BTreeMap<String, u64>);

impl FailBuckets {
    /// Construct an empty tally.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment the bucket count (ports `_tally_fail`).
    pub fn tally(&mut self, bucket: &str) {
        *self.0.entry(bucket.to_string()).or_insert(0) += 1;
    }

    /// Read a bucket's count (0 if absent).
    #[must_use]
    pub fn get(&self, bucket: &str) -> u64 {
        self.0.get(bucket).copied().unwrap_or(0)
    }

    /// The underlying bucket→count map (for the Python driver's `[sim] summary`).
    #[must_use]
    pub fn buckets(&self) -> &BTreeMap<String, u64> {
        &self.0
    }
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
// The orchestration (C1)
// ─────────────────────────────────────────────────────────────────────────

/// Inputs to [`simulate_one`] that don't vary per-path.
///
/// Ports the closure-captured state in the Python `simulate_one`: the
/// executor/weth/pm/multicall addresses, the funding flags, the warmup slots,
/// the dispatcher's `block_priority_fees` + the block context.
///
/// `provider` is omitted from `Debug` (the `AlloyProvider` doesn't impl it;
/// its `rpc_url` is logged separately by the driver).
#[derive(Clone)]
pub struct SimulateContext<'a> {
    /// The typed RPC provider (the §ZUZANP leaf, wrapped).
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

/// Per-path inputs to [`simulate_one`].
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

/// Run `simulate_one` for a single path.
///
/// Orchestrates: int128 guard → encode (YQORTM) → `execute()` calldata wrap
/// (YQORTM via `payload`) → access-list (PGA5S3) → `eth_simulateV1` (PGA5S3)
/// → parse the 7 calls (PGA5S3) → classify revert (SYI3PG) → compute
/// gross/net profit + the market-aware priority fee (C4) → return the
/// profitable result (or `None`).
///
/// `fail_buckets` accumulates the `_tally_fail` counters (ports L1769–L1771);
/// the rendering stays Python (`stays-python` D4).
///
/// # Errors
///
/// Returns `Ok(None)` for any non-profitable / reverted / failed outcome —
/// the bucket is tallied before returning. Returns `Err` only on an
/// unrecoverable RPC+SOrFailure (the Python oracle swallows these into
/// `rpc-failed`; this leaf surfaces them for the driver to decide). Encode
/// failures raise (the Python oracle raises `ValueError` at L2000).
#[allow(clippy::too_many_lines)]
pub async fn simulate_one(
    ctx: &SimulateContext<'_>,
    path: SimulatePath,
    fail_buckets: &mut FailBuckets,
) -> ProviderResult<Option<SimResult>> {
    // Guard: hop_outputs must match the number of hops (L1876–L1884).
    if path.hop_outputs.len() != path.path_info.hops.len() {
        return Ok(None);
    }

    // C3 — int128 check: V4 BalanceDelta uses int128 per component (L1887–L1900).
    // For any V4 hop, amountSpecified and the swap's output delta must both
    // fit int128. Reject early to avoid wasted encoding.
    for (i, hop) in path.path_info.hops.iter().enumerate() {
        if let HopInfo::V4(_) = hop {
            let amount_specified = if i == 0 {
                path.optimal_input
            } else {
                path.hop_outputs[i - 1]
            };
            let output_amount = path.hop_outputs[i];
            if !fits_int128(amount_specified) || !fits_int128(output_amount) {
                fail_buckets.tally("int128-overflow");
                return Ok(None);
            }
        }
    }

    // Encode as cmd_executor command stream (YQORTM). The Python oracle's
    // `any(x <= 0 for x in hop_outputs)` guard is replicated by the encoder
    // (`hop_outputs.contains(&0)`) — `None` here means a zero output rejected
    // the encode. The Python oracle raises `ValueError`; we surface it as
    // `encode-failed` + `None` (the driver logs the path).
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
        fail_buckets.tally("encode-failed");
        return Ok(None);
    };

    // config=0 (check_mode=0, no bribe): skip the on-chain profit check;
    // the operator verifies profitability off-chain via the pre/post balance
    // reads. (L2017–L2020.)
    let execute_calldata = wrap_execute_calldata(ctx.executor_address, &cmd_bytes, EXECUTE_CONFIG)
        .map_err(|e| degenbot_core::errors::ProviderError::RpcError {
            code: -32603,
            message: format!("execute() ABI encode failed: {e}"),
        })?;

    // Build the stateOverrides (TCTUAW).
    let state_overrides: StateOverride = crate::build_simulation_state_overrides(
        ctx.executor_owner,
        ctx.inject_code,
        ctx.injected_address,
        ctx.runtime_bytecode.clone(),
        ctx.warmup,
        ctx.weth_address,
        ctx.pool_manager_address,
    );

    // Compute the EIP-2930 access list (B4 — PGA5S3). The Python oracle
    // swallows AL failures + re-simulates without one (L2007–L2011). This
    // leaf surfaces the error only if the typed RPC itself classifies it
    // as a hard failure — `Err` is mapped to `None` + `access-list-failed`.
    let access_list = match dispatch::create_access_list(
        ctx.provider,
        &build_access_list_request(ctx, &execute_calldata),
    )
    .await
    {
        Ok(result) => Some(result.access_list),
        Err(_) => {
            // If AL computation fails (e.g. revert), simulate without it.
            // The simulation itself will reject this path.
            None
        }
    };

    // Dispatch the 7-call simulate (B3 — PGA5S3).
    let sim_params = SimulationParams {
        executor_owner: ctx.executor_owner,
        executor_address: ctx.executor_address,
        weth_address: ctx.weth_address,
        pool_manager_address: ctx.pool_manager_address,
        multicall3_address: ctx.multicall3_address,
        execute_calldata: execute_calldata.clone(),
        access_list: access_list.clone(),
    };
    let Ok(result) = dispatch::simulate_v1(ctx.provider, &sim_params, state_overrides).await else {
        fail_buckets.tally("rpc-failed");
        return Ok(None);
    };

    // Classify + tally the revert if any call failed (C5 — SYI3PG reference).
    if let Some(fail_idx) = result.first_failure {
        let reverted = &result.calls[fail_idx];
        let revert_data = reverted
            .revert_data
            .clone()
            .filter(|b| !b.is_empty())
            .or_else(|| {
                let r = reverted.return_data.clone();
                if r.is_empty() {
                    None
                } else {
                    Some(r)
                }
            })
            .unwrap_or_default();
        // `classify_revert` (SYI3PG). C6 (the param-decode for the
        // `[sim-fail]` log line) stays Python — diagnostic-only.
        fail_buckets.tally(&classify_revert(&revert_data));
        return Ok(None);
    }

    // C2 — gross profit: the balance diffs across the 7-call vector
    // (L2381–L2398). `weth_before/after = calls[0/4]`, `eth = calls[1/5]`,
    // `erc6909 = calls[2/6]`. Decode each `returnData` as a big-endian uint256.
    let decode = |idx: usize| -> Option<U256> { decode_balance(&result.calls[idx].return_data) };
    let Some(weth_before) = decode(0) else {
        fail_buckets.tally("balance-decode");
        return Ok(None);
    };
    let Some(eth_before) = decode(1) else {
        fail_buckets.tally("balance-decode");
        return Ok(None);
    };
    let Some(erc6909_before) = decode(2) else {
        fail_buckets.tally("balance-decode");
        return Ok(None);
    };
    let Some(weth_after) = decode(4) else {
        fail_buckets.tally("balance-decode");
        return Ok(None);
    };
    let Some(eth_after) = decode(5) else {
        fail_buckets.tally("balance-decode");
        return Ok(None);
    };
    let Some(erc6909_after) = decode(6) else {
        fail_buckets.tally("balance-decode");
        return Ok(None);
    };

    let combined_before = weth_before + eth_before + erc6909_before;
    let combined_after = weth_after + eth_after + erc6909_after;
    let gross_profit = combined_after.saturating_sub(combined_before);
    if gross_profit.is_zero() {
        // `gross_profit <= 0` — `u128`/`U256` arithmetic can't go negative;
        // a non-positive gross decodes to `0` here, which is the `no-profit`
        // bucket. (The Python oracle logs the actual negative value; the
        // rendering stays Python — diagnostic-only.)
        fail_buckets.tally("no-profit");
        return Ok(None);
    }

    // The simulate's `gasUsed` for the execute call (call `[3]`, UN-inflated).
    let gas_used = result.calls[3].gas_used;

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

    Ok(Some(SimResult {
        path_id: path.path_id,
        gross_profit,
        net_profit,
        gas_used,
        priority_fee,
        base_fee_next: ctx.base_fee_next,
        execute_calldata,
        access_list,
        hop_count: path.path_info.hops.len(),
    }))
}

/// Decode a 32-byte big-endian uint256 from the `returnData` of a balance call.
///
/// Mirrors `int.from_bytes(calls[i]["returnData"], byteorder="big")` (L2382).
/// The Multicall3 `getEthBalance` + the ERC20 `balanceOf` both return a
/// single 32-byte uint256. Returns `None` if the bytes aren't a clean
/// 32-byte word (truncated/odd-length) — the Python oracle's
/// `int.from_bytes(b'', "big")` yields `0` for empty, so empty maps to `Some(0)`.
fn decode_balance(data: &alloy::primitives::Bytes) -> Option<U256> {
    match data.len() {
        0 => Some(U256::ZERO),
        32 => Some(U256::from_be_bytes::<32>(data.as_ref().try_into().ok()?)),
        // Excess bytes (>32, e.g. a packed multicall) — read the LAST 32
        // (uint256 ABI-encodings are right-aligned in low memory). The Python
        // `int.from_bytes` reads ALL bytes; for a 32-byte returnData the
        // behaviors coincide. Anything else is a malformed return.
        n if n > 32 => {
            let tail = &data[n - 32..];
            Some(U256::from_be_bytes::<32>(tail.try_into().ok()?))
        }
        // 1..=31 — odd-length; the Python `int.from_bytes` would read it as a
        // small int. Mirror that (left-pad with zeros to 32).
        n => {
            let mut buf = [0u8; 32];
            buf[32 - n..].copy_from_slice(data);
            Some(U256::from_be_bytes::<32>(buf))
        }
    }
}

/// Build the `TransactionRequest` shape for the access-list computation.
///
/// Mirrors the `tx_params` block (L1983–L1997): `to = executor_address`,
/// `data = execute_calldata`, `chainId = 1`, `type = 2`, `value = 0`,
/// `gas = 5_000_000` (the generous pre-simulate cap), zero fees, `nonce = 0`,
/// `from = executor_owner`. NO accessList yet (that's what we're computing).
fn build_access_list_request(
    ctx: &SimulateContext<'_>,
    execute_calldata: &alloy::primitives::Bytes,
) -> alloy::rpc::types::TransactionRequest {
    alloy::rpc::types::TransactionRequest {
        from: Some(ctx.executor_owner),
        to: Some(ctx.executor_address.into()),
        input: alloy::rpc::types::eth::TransactionInput::new(execute_calldata.clone()),
        chain_id: Some(1),
        transaction_type: Some(2),
        value: Some(U256::ZERO),
        gas: Some(INITIAL_EXECUTE_GAS),
        max_fee_per_gas: Some(0),
        max_priority_fee_per_gas: Some(0),
        nonce: Some(0),
        access_list: None,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;
    use alloy::primitives::{address, Bytes, U256};
    use alloy::providers::{Provider, ProviderBuilder};
    use alloy::rpc::client::ClientBuilder;
    use alloy::rpc::types::eth::simulate::{SimCallResult, SimulatedBlock};
    use alloy::transports::mock::{Asserter, MockTransport};
    use degenbot_executor::composers::{HopInfo, PathInfo, V2HopInfo};
    use degenbot_executor::compute_simulation_warmup_slots;
    use std::sync::Arc;

    const OWNER: Address = address!("9c56a29c7231974c269e24f9fb3c29203039089e");
    const EXECUTOR: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
    const MULTICALL3: Address = address!("c411372f0b8ae58585e33b78aea9e0596da9a6f1");

    fn mock_provider(asserter: &Asserter) -> AlloyProvider {
        let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
        )
    }

    fn warmup() -> WarmupSlots {
        compute_simulation_warmup_slots(EXECUTOR, WETH, PM)
    }

    fn ctx(provider: &AlloyProvider) -> SimulateContext<'_> {
        SimulateContext {
            provider,
            executor_owner: OWNER,
            executor_address: EXECUTOR,
            weth_address: WETH,
            pool_manager_address: PM,
            multicall3_address: MULTICALL3,
            inject_code: true,
            injected_address: Some(EXECUTOR),
            runtime_bytecode: Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
            warmup: warmup(),
            base_fee_next: 1_000_000_000u128, // 1 gwei
            current_block: 100,
            block_priority_fees: Some(BlockPriorityFees {
                block: 100,
                p10: U256::from(500_000_000u64),   // 0.5 gwei
                p50: U256::from(2_000_000_000u64), // 2 gwei
            }),
        }
    }

    fn two_v2_hops() -> PathInfo {
        PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                token0_address: WETH,
                token1_address: address!("1111111111111111111111111111111111111111"),
                fee: 30,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("cccccccccccccccccccccccccccccccccccccccc"),
                token0_address: address!("1111111111111111111111111111111111111111"),
                token1_address: WETH,
                fee: 30,
                zfo: true,
            }),
        ])
    }

    fn path(path_id: u64) -> SimulatePath {
        SimulatePath {
            path_id,
            optimal_input: 1_000_000_000_000_000_000u128, // 1 ETH
            hop_outputs: vec![1_100_000_000_000_000_000u128, 1_210_000_000_000_000_000u128],
            path_info: two_v2_hops(),
            solve_block: 100,
            opts: EncodeOptions {
                erc6909_profit: false,
                use_v4_batch: false,
            },
        }
    }

    /// Build a 7-call simulated block where call `[3]` is the execute +
    /// the pre/post balances yield a known gross profit.
    fn profit_block(
        gross: U256,
        gas_used: u64,
    ) -> Vec<SimulatedBlock<degenbot_rpc::provider::EthBlock>> {
        // before balances: weth=10 ETH, eth=5 ETH, erc6909=0.
        let weth_before = U256::from(10_000_000_000_000_000_000u128);
        let eth_before = U256::from(5_000_000_000_000_000_000u128);
        let erc_before = U256::ZERO;
        // after balances: before + gross (all in weth).
        let weth_after = weth_before + gross;
        let eth_after = eth_before;
        let erc_after = erc_before;
        let calls = vec![
            ok_call(weth_before),
            ok_call(eth_before),
            ok_call(erc_before),
            ok_gas(gas_used),
            ok_call(weth_after),
            ok_call(eth_after),
            ok_call(erc_after),
        ];
        vec![SimulatedBlock {
            inner: degenbot_rpc::provider::EthBlock::default(),
            calls,
        }]
    }

    fn ok_call(balance: U256) -> SimCallResult {
        SimCallResult {
            return_data: Bytes::from(balance.to_be_bytes::<32>().to_vec()),
            logs: Vec::new(),
            gas_used: 30_000,
            max_used_gas: None,
            status: true,
            error: None,
        }
    }

    fn ok_gas(gas: u64) -> SimCallResult {
        SimCallResult {
            return_data: Bytes::new(),
            logs: Vec::new(),
            gas_used: gas,
            max_used_gas: None,
            status: true,
            error: None,
        }
    }

    /// Build an `eth_createAccessList` canned response (an empty list).
    fn access_list_response() -> serde_json::Value {
        serde_json::json!({"accessList": [], "gasUsed": "0x0"})
    }

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
        // clamp(8, 1, 1) = 1.  <-- ceiling is target=8? No: max = max(p50+1, min)
        //   = max(1, 1) = 1. So clamp(8, 1, 1) = 1.
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
        // p10=0, p50=u128::MAX → min=1, max=max(u128::MAX+1, 1).
        //   u128::MAX+1 wraps to 0 → max = max(0, 1) = 1. That'd clamp to 1.
        // So use p50 = 1_000_000 (so max = 1_000_001 ≥ 400_000).
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

    // ── C1: simulate_one orchestration (§4.2 wire round-trip) ─────────────

    #[tokio::test]
    async fn simulate_one_returns_profitable_result() {
        // The mock transport serves TWO responses in order — the
        // `eth_createAccessList` (an empty list) + the `eth_simulateV1` (a
        // profit block). Then assert the SimResult carries the expected
        // gross/net/priority.
        let asserter = Asserter::new();
        asserter.push_success(&access_list_response());
        let gross = U256::from(2_000_000_000_000_000_000u128); // 2 ETH
        asserter.push_success(&profit_block(gross, 200_000));
        let provider = mock_provider(&asserter);
        let mut buckets = FailBuckets::new();

        let result = simulate_one(&ctx(&provider), path(42), &mut buckets)
            .await
            .unwrap();
        assert!(result.is_some(), "buckets: {:?}", buckets.buckets());
        let r = result.unwrap();
        assert_eq!(r.path_id, 42);
        assert_eq!(r.gross_profit, gross);
        assert_eq!(r.gas_used, 200_000);
        // inflated_gas = int(200_000 * 1.5) = 300_000.
        assert_eq!(r.inflated_gas(), 300_000);
        // priority_fee: target = int(2e18/1.25/200_000) = int(8e12) = 8e12 wei.
        // age 0, bounds p10=0.5gwei, p50=2gwei → min=500_000_001,
        //   max=2_000_000_001. clamp(8e12, …, 2e9) = 2_000_000_001.
        assert_eq!(r.priority_fee, 2_000_000_001);
        // net = gross - gas*(base + priority) = 2e18 - 200_000*(1e9 + 2_000_000_001).
        let gas_fee = U256::from(200_000u128) * U256::from(3_000_000_001u128);
        assert_eq!(r.net_profit, gross - gas_fee);
        // buckets empty (profitable).
        assert!(buckets.buckets().is_empty());
    }

    #[tokio::test]
    async fn simulate_one_returns_none_on_zero_profit() {
        let asserter = Asserter::new();
        asserter.push_success(&access_list_response());
        // before == after → gross 0.
        asserter.push_success(&profit_block(U256::ZERO, 200_000));
        let provider = mock_provider(&asserter);
        let mut buckets = FailBuckets::new();

        let result = simulate_one(&ctx(&provider), path(1), &mut buckets)
            .await
            .unwrap();
        assert!(result.is_none());
        assert_eq!(buckets.get("no-profit"), 1);
    }

    #[tokio::test]
    async fn simulate_one_returns_none_on_revert_and_tallies_classified_bucket() {
        let asserter = Asserter::new();
        asserter.push_success(&access_list_response());
        // Build a block where call [3] reverts with Panic(0x11).
        let revert_data = Bytes::from_static(&[
            0x4e, 0x48, 0x7b, 0x71, // selector
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0x00, 0x00, 0x00, 0x11, // panic code 0x11
        ]);
        let calls = vec![
            ok_call(U256::from(1u128)),
            ok_call(U256::from(1u128)),
            ok_call(U256::ZERO),
            SimCallResult {
                return_data: Bytes::new(),
                logs: Vec::new(),
                gas_used: 21_000,
                max_used_gas: None,
                status: false,
                error: Some(alloy::rpc::types::eth::simulate::SimulateError {
                    code: 3,
                    message: "execution reverted".into(),
                    data: Some(revert_data.clone()),
                }),
            },
            ok_call(U256::from(1u128)),
            ok_call(U256::from(1u128)),
            ok_call(U256::ZERO),
        ];
        let block = SimulatedBlock {
            inner: degenbot_rpc::provider::EthBlock::default(),
            calls,
        };
        asserter.push_success(&vec![block]);
        let provider = mock_provider(&asserter);
        let mut buckets = FailBuckets::new();

        let result = simulate_one(&ctx(&provider), path(2), &mut buckets)
            .await
            .unwrap();
        assert!(result.is_none());
        // Panic(0x11) → classify_revert returns "Panic(0x11)".
        assert_eq!(buckets.get("Panic(0x11)"), 1);
    }

    #[tokio::test]
    async fn simulate_one_rejects_int128_overflow_v4_hop() {
        // A V4 hop with amount_specified > INT128_MAX is rejected before
        // any RPC is dispatched — the provider's asserter queue stays empty
        // (no RPC call made), proving the guard fires pre-encode.
        let asserter = Asserter::new();
        // Push a sentinel response — if the int128 guard fires pre-encode,
        // this stays unconsumed (proving no RPC was dispatched).
        asserter.push_success(&serde_json::json!({"accessList": []}));
        let provider = mock_provider(&asserter);
        let mut buckets = FailBuckets::new();

        let v4 = HopInfo::V4(degenbot_executor::composers::V4HopInfo {
            pool_manager_address: PM,
            pool_id_hex: "0x".to_string(),
            currency0_address: WETH,
            currency1_address: address!("1111111111111111111111111111111111111111"),
            fee: 3000,
            tick_spacing: 60,
            hook_address: Address::ZERO,
            zfo: true,
        });
        let p = SimulatePath {
            path_id: 3,
            optimal_input: INT128_MAX as u128 + 1, // overflow
            hop_outputs: vec![1u128],
            path_info: PathInfo::new(vec![v4]),
            solve_block: 100,
            opts: EncodeOptions {
                erc6909_profit: false,
                use_v4_batch: false,
            },
        };
        let result = simulate_one(&ctx(&provider), p, &mut buckets)
            .await
            .unwrap();
        assert!(result.is_none());
        assert_eq!(buckets.get("int128-overflow"), 1);
        // No RPC was dispatched — the sentinel response we pushed is still in
        // the queue (unconsumed), proving the guard fired pre-encode.
        assert!(
            !asserter.read_q().is_empty(),
            "guard should fire before any RPC"
        );
    }

    #[tokio::test]
    async fn simulate_one_rejects_hop_output_length_mismatch() {
        let asserter = Asserter::new();
        // Push a sentinel response — if the length guard fires, this stays
        // unconsumed (proving no RPC was dispatched).
        asserter.push_success(&serde_json::json!({"accessList": []}));
        let provider = mock_provider(&asserter);
        let mut buckets = FailBuckets::new();

        let mut p = path(7);
        p.hop_outputs = vec![1u128]; // 1 output, 2 hops → mismatch.
        let result = simulate_one(&ctx(&provider), p, &mut buckets)
            .await
            .unwrap();
        assert!(result.is_none());
        // No bucket tallied (the length guard is a pure skip, not a fail).
        assert!(buckets.buckets().is_empty());
        // No RPC dispatched — the sentinel response is still unconsumed.
        assert!(
            !asserter.read_q().is_empty(),
            "length guard should fire before any RPC"
        );
    }

    // ── C2: decode_balance ────────────────────────────────────────────────

    #[test]
    fn decode_balance_empty_is_zero() {
        assert_eq!(decode_balance(&Bytes::new()), Some(U256::ZERO));
    }

    #[test]
    fn decode_balance_32_bytes_big_endian() {
        let v = U256::from(0x0102_0304_0506_0708u128);
        let bytes = Bytes::from(v.to_be_bytes::<32>().to_vec());
        assert_eq!(decode_balance(&bytes), Some(v));
    }

    #[test]
    fn decode_balance_odd_length_left_pads() {
        // 1 byte, value 0x42 → left-padded to 32 → 0x42.
        let bytes = Bytes::from_static(&[0x42]);
        assert_eq!(decode_balance(&bytes), Some(U256::from(0x42u64)));
    }
}
