//! The backrun strategy's sim value types + the 7-call orchestration.
//!
//! Owns the strategy-value surface BOTH consumers of the Rust core reach:
//! the type `SimResult`, `FailBuckets`, `compute_priority_fee`, `fits_int128`,
//! `SimulateContext`, `SimulatePath`, `BlockPriorityFees` (re-exported from
//! `degenbot_rpc::fees` — the fee struct is market data, owned by the RPC
//! crate per ADR-019 D5) + the priority-fee constants, + the in-process revm
//! 7-call vector `simulate_path_on_evm` driven per-block over the engine's
//! `BlockSimHandle::evm_mut`.
//!
//! ADR-019 D4/D7 (decision R — Rust-canonical): this is the backrun bundle +
//! gross/net + priority-fee sizing — one strategy over the
//! `degenbot-simulation` engine. The engine owns the revm EVM handle
//! (`BlockSimHandle`), the DB stack, overrides, + the AL inspector; this crate
//! drives the borrowed `&mut evm` the engine exposes. The Python driver is a
//! thin cockpit over a PyO3 wrapper around `dispatch_profitable_results` —
//! it does NOT re-derive the 7-call bundle (AGENTS.md: "driver shell, not a
//! co-implementation").
//!
//! # In-process revm sim (task `JHGLF4`, Tier 1 `V5HCR5`)
//!
//! Executes the 7-call vector (pre-balances → `execute()` → post-balances) via
//! revm `transact_one`, returning the `SimResult` shape the dispatch leaf
//! consumes. See `docs/spikes/revm-composition-api-and-cold-miss-latency.md`
//! §3 for the measured latency profile (cold 8374 µs / 9 RPCs; warm 442 µs / 0
//! RPC; 18.9× speedup vs cold) + §5 for the `ResultAndState.state`
//! access-list emission API.

// Solidity/EVM identifiers (execute(bytes,uint256), int128, V4 BalanceDelta,
// WETH9, PoolManager, Multicall3, balanceOf, getEthBalance, ERC6909, etc.) are
// ubiquitous here — match the degenbot-simulation convention.
#![allow(clippy::doc_markdown)]

use std::collections::BTreeMap;

use alloy::primitives::{Address, U256};
use alloy::rpc::types::AccessList;
use degenbot_core::errors::{ProviderError, ProviderResult};
use degenbot_executor::composers::{
    encode_cmd_stream, EncodeOptions, HopInfo, PathInfo, V2HopInfo, V4HopInfo,
};
use degenbot_executor::WarmupSlots;
use degenbot_rpc::fees::BlockPriorityFees;
use degenbot_rpc::provider::AlloyProvider;
// `ExecutionResult` lives in `revm::context_interface` (re-exported by the revm
// umbrella); `TxEnv` + `TxKind` in `revm::context` / `revm::primitives`.
use revm::context::TxEnv;
use revm::context_interface::result::ExecutionResult;
use revm::database::CacheDB;
use revm::primitives::TxKind;
// `MainBuilder` (`.build_mainnet()`) + `MainContext` (`.mainnet()`) — the revm
// EVM builder traits re-exported from the handler crate. The `AccessListCollector`
// inspector + `SimulationOverrideParams` are engine types the strategy drives.
use degenbot_simulation::{
    AccessListCollector, CallTraceInspector, CapturedSwap, SimInspector, SimulationOverrideParams,
    SwapEventCaptureInspector,
};
use revm::{ExecuteEvm, InspectEvm, MainBuilder, MainContext};

use crate::calldata::{
    encode_balance_of_calldata, encode_erc6909_balance_of_calldata,
    encode_get_eth_balance_calldata, wrap_execute_calldata,
};

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
/// Mirrors the Python oracle's per-path simulate return tuple (L2436). All gross-profitable
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
    /// The swap events (`Sync`/`Swap`) the `SwapEventCaptureInspector`
    /// captured during `execute()` call [3]'s `inspect_one` — the V2/V3/V4
    /// pools' own emitted swap events, decoded. The ground-truth "what each
    /// hop actually produced, as simulated" — replaces the onchain-recompute
    /// pipeline (ergo epic 63I7WJ). Empty if `execute()` reverted before any
    /// swap emitted. V4 amount correctness is gated on task `5RI47E`.
    pub captured_swaps: Vec<CapturedSwap>,
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
/// the raw revert data bytes + the inspector-captured reverting-frame
/// attribution when available).
///
/// `fail_index` is `Some(idx)` for failures attributable to a specific
/// simulated call — the revert branch (`result.first_failure`) AND the
/// balance-decode branch (the malformed word's call index). It is `None` for
/// orchestration-only buckets (`int128-overflow`, `encode-failed`,
/// `rpc-failed`, `no-profit`) where no single call failed.
///
/// `reverting_frame` is `Some` only for `execute()` reverts where the
/// `CallTraceInspector` (attached at call [3]) captured the failing frame —
/// it carries the DEEP attribution (depth, target, selector, revert data,
/// `classify_revert` label) of the frame that actually reverted, rather than
/// the top-level bubble. `None` for orchestration-only buckets + the
/// balance-decode branch (no `inspect_one` ran on the failing call).
#[derive(Debug, Clone)]
pub struct SimFailure {
    /// The path id (mirror of `SimulatePath::path_id`).
    pub path_id: u64,
    /// The bucket label — the `classify_revert` output for reverts, else the
    /// orchestration-only bucket string.
    pub bucket: String,
    /// The index of the failing call in the 7-call vector, if any.
    pub fail_index: Option<usize>,
    /// The raw revert data bytes (the 4-byte selector + ABI-encoded args) —
    /// the TOP-LEVEL revert bubble's data. Empty for orchestration-only
    /// buckets + the balance-decode branch. For `execute()` reverts this is
    /// the same bytes as [`RevertingFrame::revert_data`] (the reverting
    /// frame's depth + target are the new attribution; this field is kept for
    /// the Python `[sim-fail]` column-stability contract).
    pub revert_data: alloy::primitives::Bytes,
    /// The inspector-captured reverting-frame attribution — `Some` only for
    /// `execute()` reverts where call [3]'s `inspect_one` ran the
    /// `CallTraceInspector`. `None` for orchestration-only buckets + the
    /// balance-decode branch. Ergo epic 63I7WJ task 3AJ4I4.
    pub reverting_frame: Option<RevertingFrame>,
    /// The swap events captured BEFORE the revert (the swaps `execute()` did
    /// before reverting) — diagnostic for WHY it reverted (e.g. a partial fill
    /// that blew the price limit). Empty for orchestration-only buckets + the
    /// balance-decode branch (no `inspect_one` ran). Ergo epic 63I7WJ.
    pub captured_swaps: Vec<CapturedSwap>,
    /// The solver's optimal input for this path (`path.optimal_input`). Carried
    /// so the `[sim-diag]` classifier (ergo epic 63I7WJ task AM5AJW) can emit
    /// the expected-vs-actual comparison without re-deriving it — the
    /// *actual* amount comes from `captured_swaps`.
    pub optimal_input: u128,
    /// The solver's per-hop outputs (`path.hop_outputs`). The EXPECTED amount
    /// the solver said each hop would produce; the ACTUAL amount lives on each
    /// `captured_swaps` entry's `amount0`/`amount1` — the gap between them is
    /// the new `SolverCalc` classification basis (replaces the deleted
    /// `recompute.matches_solver`). Empty for orchestration-only buckets where
    /// no hop ran (the int128-overflow guard, the encode-failed guard).
    pub hop_outputs: Vec<u128>,
}

/// The inspector-captured attribution of a failing `execute()` frame — the
/// depth, target, selector, revert data, + `classify_revert` label of the
/// deepest non-`Success` frame the `CallTraceInspector` saw during call [3]'s
/// `inspect_one` run. Replaces the top-level-only `fail_index` + revert-bubble
/// data with the frame that actually reverted (or halted).
///
/// For a `Halt` (e.g. `0xfe` INVALID, OOG), `revert_data` is empty + `label`
/// is `classify_revert` on empty bytes (the `"empty"` bucket) — parity with
/// the pre-inspector behavior, now attributed to the halting frame's target.
#[derive(Debug, Clone)]
pub struct RevertingFrame {
    /// The call-stack depth (1 = top-level `execute()`, 2 = first sub-call, …).
    pub depth: usize,
    /// The reverting/halting contract's address.
    pub target: alloy::primitives::Address,
    /// The first 4 bytes of the call's calldata (the Solidity selector).
    pub selector: [u8; 4],
    /// The reverting frame's revert data (`0x` for a `Halt`).
    pub revert_data: alloy::primitives::Bytes,
    /// The `classify_revert` label run on `revert_data`.
    pub label: String,
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
        optimal_input: u128,
        hop_outputs: Vec<u128>,
    ) {
        self.tally(bucket);
        self.failures.push(SimFailure {
            path_id,
            bucket: bucket.to_string(),
            fail_index,
            revert_data,
            reverting_frame: None,
            captured_swaps: Vec::new(),
            optimal_input,
            hop_outputs,
        });
    }

    /// Record a per-path `execute()` revert WITH the inspector-captured
    /// reverting-frame attribution. Like [`record`](Self::record) but populates
    /// [`SimFailure::reverting_frame`] — the deep (depth/target/selector/
    /// revert-data/label) attribution of the frame that actually reverted.
    #[allow(clippy::too_many_arguments)]
    pub fn record_revert(
        &mut self,
        path_id: u64,
        bucket: &str,
        fail_index: Option<usize>,
        reverting_frame: RevertingFrame,
        captured_swaps: Vec<CapturedSwap>,
        optimal_input: u128,
        hop_outputs: Vec<u128>,
    ) {
        self.tally(bucket);
        self.failures.push(SimFailure {
            path_id,
            bucket: bucket.to_string(),
            fail_index,
            revert_data: reverting_frame.revert_data.clone(),
            reverting_frame: Some(reverting_frame),
            captured_swaps,
            optimal_input,
            hop_outputs,
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

/// The V2 hop's input token address (`zfo=true` → token0).
fn v2_input_token(hop: &V2HopInfo) -> Address {
    if hop.zfo {
        hop.token0_address
    } else {
        hop.token1_address
    }
}

/// The V2 hop's output token address (`zfo=true` → token1).
fn v2_output_token(hop: &V2HopInfo) -> Address {
    if hop.zfo {
        hop.token1_address
    } else {
        hop.token0_address
    }
}

/// The V4 hop's input currency (`zfo=true` → currency0).
fn v4_input_currency(hop: &V4HopInfo) -> Address {
    if hop.zfo {
        hop.currency0_address
    } else {
        hop.currency1_address
    }
}

/// The V4 hop's output currency (`zfo=true` → currency1).
fn v4_output_currency(hop: &V4HopInfo) -> Address {
    if hop.zfo {
        hop.currency1_address
    } else {
        hop.currency0_address
    }
}

/// Scan `hops` for a V4↔V2 adjacency that needs a representation bridge
/// (native on the V4 side, WETH on the V2 side) — the case the 3-hop
/// composers do NOT encode (ergo TGXBCE). Returns a human-readable
/// description of the gap when one exists; `None` otherwise.
///
/// Native-ETH and WETH are economically the same token but V4 tracks them as
/// distinct delta currencies (NATIVE_ADDRESS vs the WETH ERC20), while V2
/// pools hold the WETH ERC20 token directly. A path whose adjacency is
/// V4(native) → V2(WETH) or V2(WETH) → V4(native) needs an explicit
/// `WETH_DEPOSIT` / `WETH_WITHDRAW` opcode that the 3-hop composers #[allow]
/// (they only handle native/WETH identical to the boundary token).
///
/// The 2-hop `encode_cmd_v4_v2` bridges this gap; the 3-hop twins don't.
#[must_use]
fn scan_for_v4_v2_boundary_bridge(hops: &[HopInfo], weth_address: Address) -> Option<String> {
    const NATIVE: Address = Address::ZERO;
    for i in 0..hops.len().saturating_sub(1) {
        let (a, b) = (&hops[i], &hops[i + 1]);
        // V4 → V2: V4's output is native, V2's input is WETH.
        if let (HopInfo::V4(va), HopInfo::V2(vb)) = (a, b) {
            if v4_output_currency(va) == NATIVE && v2_input_token(vb) == weth_address {
                return Some(format!(
                    "boundary {}→{}: V4 native-out → V2 WETH-in (needs Wrap)",
                    i,
                    i + 1
                ));
            }
        }
        // V2 → V4: V2's output is WETH, V4's input is native.
        if let (HopInfo::V2(vb), HopInfo::V4(va)) = (a, b) {
            if v2_output_token(vb) == weth_address && v4_input_currency(va) == NATIVE {
                return Some(format!(
                    "boundary {}→{}: V2 WETH-out → V4 native-in (needs Unwrap)",
                    i,
                    i + 1
                ));
            }
        }
    }
    None
}

// ─────────────────────────────────────────────────────────────────────────
// The orchestration inputs
// ─────────────────────────────────────────────────────────────────────────

/// Inputs to [`BlockSimHandle`] / [`simulate_path_on_evm`] that don't vary per-path.
///
/// Ports the closure-captured state in the Python oracle's per-path simulate: the
/// executor/weth/pm/multicall addresses, the funding flags, the warmup slots,
/// the dispatcher's `block_priority_fees` + the block context.
///
/// `provider` is omitted from `Debug` (the `AlloyProvider` doesn't impl it;
/// its `rpc_url` is logged separately by the driver).
#[derive(Clone)]
pub struct SimulateContext<'a> {
    /// The typed RPC provider (the §ZUZANP leaf, wrapped). The in-process path
    /// uses it for the cold-miss `AlloyDB` fallback under the engine's
    /// `WrapDatabaseAsync` (a sim miss for an untracked account/block routes
    /// through `block_in_place` to a single `eth_call`).
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
    /// The timestamp of `current_block` (the block header's `timestamp`). Used
    /// as the EVM's `block.timestamp` so the V2 pair's `_update()` computes a
    /// correct `timeElapsed` (a stale/default timestamp overflows
    /// `UQ112x112.mul(timeElapsed)` — task XPPMQG). Threaded from the pump's
    /// block header to avoid a per-path `eth_getBlockByNumber` RPC.
    pub block_timestamp: u64,
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

/// Per-path inputs to [`BlockSimHandle`] / [`simulate_path_on_evm`].
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

// The engine `BlockSimHandle` (the per-block shared EVM handle) + the
// `BlockEvm`/`ProductionBlockDb` type aliases + the `ArcDynProviderEthereum`
// provider newtype live in `degenbot-simulation` (the engine). This strategy
// builds the handle via `BlockSimHandle::build` + drives the borrowed `&mut evm`
// the engine exposes via `BlockSimHandle::evm_mut`.

/// The testable in-process orchestration core: runs the 7-call vector over a
/// PRE-BUILT `CacheDB` (overrides already applied by the caller) via revm
/// `transact_one` (the journaled state accumulates across the 7 calls), then
/// parses the balance diffs + computes the [`SimResult`].
///
/// Sync — no RPC, no tokio runtime needed (the `CacheDB`'s backing `Database`
/// is whatever the caller supplied; `EmptyDB` for the smoke test,
/// `BotStateDb<WrapDatabaseAsync<AlloyDB>>` for production via
/// [`BlockSimHandle`]). Today `BotStateDb` forwards every read to the
/// fallback (typed-state serving not wired — see `bot_state_db`'s doc). This split lets the revm orchestration be
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
pub fn simulate_in_process_with_db<Db>(
    ctx: &SimulateContext<'_>,
    cache_db: CacheDB<Db>,
    path: &SimulatePath,
    fail_buckets: &mut FailBuckets,
) -> ProviderResult<Option<SimResult>>
where
    Db: revm::database_interface::DatabaseRef,
    <Db as revm::database_interface::DatabaseRef>::Error: std::fmt::Display,
{
    // Build the EVM (`CacheDB` → revm `Context`) + pin the block env to the
    // per-block `ctx` values (shared by every path in the fan-out). The 7-call
    // orchestration + profit/access-list logic lives in
    // [`simulate_path_on_evm`] so a shared per-block EVM (Tier 1, `V5HCR5`)
    // can call it with `&mut evm` without rebuilding the DB stack per path.
    //
    // `disable_nonce_check`: the 7 calls share ONE owner; `eth_simulateV1` does
    // NOT bump the caller's nonce per call (each entry is an `eth_call`-shaped
    // read, not a real tx), so revm's per-tx nonce floor would reject calls
    // [1..6]. Disable the check — parity with the node's lenient simulate.
    let mut revm_ctx = revm::context::Context::mainnet();
    revm_ctx.cfg.disable_nonce_check = true;
    let mut evm = revm_ctx
        .with_db(cache_db)
        .build_mainnet_with_inspector(SimInspector::default());
    evm.ctx.modify_block(|block| {
        block.basefee = u64::try_from(ctx.base_fee_next).unwrap_or(u64::MAX);
        block.number = U256::from(ctx.current_block);
        // The block timestamp, threaded from the pump's block header via
        // `SimulateContext::block_timestamp`. The default `timestamp = 1`
        // causes V2 pair `_update` to overflow `price0CumulativeLast` in
        // Solidity 0.8+ forks (Camelot/Aerodrome), reverting every swap — the
        // root cause of the in-process-evm parity gap (XPPMQG).
        block.timestamp = U256::from(ctx.block_timestamp);
    });
    simulate_path_on_evm(&mut evm, ctx, path, fail_buckets)
}

/// Run one path's 7-call vector on a PRE-BUILT `&mut EVM` (block env already
/// set by the caller; `CacheDB` + overrides applied upstream). The journaled
/// state accumulates across the 7 `transact_one` calls (pre reads → execute →
/// post reads see execute's changes), then `finalize()` clears the journal so
/// the next path on the same shared EVM starts from clean committed state —
/// the per-path isolation Tier 1 (`V5HCR5`) needs for a shared per-block EVM.
///
/// Generic over `E` so the smoke test (an `EmptyDB`-backed EVM built by the
/// caller) and production (a shared `BotStateDb<WrapDatabaseAsync<AlloyDB>>`
/// -backed EVM) both reach the same 7-call orchestration.
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
#[allow(clippy::too_many_lines)]
pub fn simulate_path_on_evm<E>(
    evm: &mut E,
    ctx: &SimulateContext<'_>,
    path: &SimulatePath,
    fail_buckets: &mut FailBuckets,
) -> ProviderResult<Option<SimResult>>
where
    E: ExecuteEvm<
            Tx = TxEnv,
            ExecutionResult = revm::context_interface::result::ExecutionResult,
            State = revm::state::EvmState,
        > + InspectEvm<Inspector = SimInspector>,
    <E as ExecuteEvm>::Error: std::fmt::Display,
{
    // C3 — int128 check (mirrors the oracle's guard).
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
                    path.optimal_input,
                    path.hop_outputs.clone(),
                );
                return Ok(None);
            }
        }
    }

    // TGXBCE investigation: scan for a V4↔V2 boundary-bridge signature
    // (V4 side native, V2 side WETH) — a path shape the 3-hop composers do
    // NOT encode (the 2-hop `encode_cmd_v4_v2` handles it via
    // `V4_TAKE(native,self) + WETH_DEPOSIT + V2_SWAP_COMPACT`, but the bridge
    // does not trivially port to 3-hop). This probe logs any such path so we
    // can tell whether a mainnet boundary-bridge materializes; gated by the
    // `DEGENBOT_BRIDGE_PROBE` env var so it is zero-cost in production.
    if std::env::var_os("DEGENBOT_BRIDGE_PROBE").is_some() {
        if let Some(desc) = scan_for_v4_v2_boundary_bridge(&path.path_info.hops, ctx.weth_address) {
            log::info!(
                "[bridge-probe] path_id={} {} — V4 native ↔ V2 WETH boundary; 3-hop composer does not encode this",
                path.path_id,
                desc,
            );
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
            path.optimal_input,
            path.hop_outputs.clone(),
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
    // `execute()` (call [3]) runs with the composed [`SimInspector`] tuple
    // `(AccessListCollector, (CallTraceInspector, SwapEventCaptureInspector))`
    // attached via `inspect_one` (ADR-019 D3 + ergo epic 63I7WJ) so the EIP-2930
    // warmed-slot access list + the call trace + the swap events are byproducts
    // of the FIRST execute() run — no post-re-`transact`. The balance reads
    // [0..3] + [4..7] use `transact_one`, which does NOT invoke the inspector,
    // so the inspectors see execute()-only opcodes. The tuple is moved into the
    // EVM by `inspect_one`; each member's paired handle retains a shared
    // `Rc<RefCell<…>>` to drain after the run.
    let (al, access_list_handle) = AccessListCollector::new();
    let (ct, call_trace_handle) = CallTraceInspector::new();
    let (se, swap_events_handle) = SwapEventCaptureInspector::new();
    let mut inspector_opt = Some((al, (ct, se)));
    for (idx, tx) in txs.into_iter().enumerate() {
        let result = if idx == 3 {
            let inspector = inspector_opt.take().expect("inspector taken only at idx 3");
            evm.inspect_one(tx, inspector)
        } else {
            evm.transact_one(tx)
        };
        let Ok(res) = result else {
            // A revm transact error (DB cold-miss RPC failure, or an
            // invalid tx). Treat as rpc-failed (the Python oracle swallows
            // these into rpc-failed).
            //
            // Finalize the partial journal before returning: calls [0..idx-1]
            // may have succeeded (accumulating read-caches in the journal —
            // `transact_one` stores to the journal, NOT the `CacheDB`). On the
            // SHARED per-block `&mut evm` (Tier 1), an un-finalized journal
            // would leak those partial read-caches into the next path's first
            // `transact_one`. `finalize` returns + discards the per-path
            // `State` (NOT committed — `simulate_path_on_evm` never calls
            // `commit`), so execute() SSTOREs stay out of the shared `CacheDB`.
            let _ = evm.finalize();
            fail_buckets.record(
                path.path_id,
                "rpc-failed",
                Some(idx),
                alloy::primitives::Bytes::new(),
                path.optimal_input,
                path.hop_outputs.clone(),
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

    // Drain the inspector buffers (call trace + captured swaps) ONCE here so
    // both the revert branch (below) + the success branch (profit path) have
    // the captured data, AND the buffers are reset for the next path on the
    // shared per-block `&mut evm`. The `AccessListCollector` is drained only
    // on the success path (its AL is meaningful for profitable paths only).
    let captured_call_trace = call_trace_handle.take_trace();
    let captured_swaps = swap_events_handle.take_swaps();

    // Classify + tally the first revert if any call failed.
    if let Some(fail_idx) = first_failure {
        let revert_data = results[fail_idx]
            .output()
            .cloned()
            .filter(|b| !b.is_empty())
            .unwrap_or_default();
        let bucket = degenbot_decoders::revert::classify_revert(&revert_data);
        // Ergo epic 63I7WJ task 3AJ4I4 — attribute the revert to the DEEPEST
        // failing frame the `CallTraceInspector` captured during execute()
        // call [3]'s `inspect_one`, rather than the top-level bubble. The
        // inspector ran only at call [3], so this is `Some` only when
        // `fail_idx == 3` (the execute() call) AND a failing frame was captured.
        // Otherwise (balance-decode at a non-[3] call, or no frame captured)
        // fall back to the plain `record` (top-level revert data, no deep
        // attribution).
        if let Some(frame) = captured_call_trace.failing_frame() {
            let frame_revert_data = match &frame.outcome {
                Some(degenbot_simulation::FrameOutcome::Revert { data, .. }) => data.clone(),
                _ => alloy::primitives::Bytes::new(),
            };
            let frame_label = degenbot_decoders::revert::classify_revert(&frame_revert_data);
            // `frame_label` moves into `RevertingFrame`; clone for the bucket
            // arg (record_revert borrows `bucket: &str` + moves the frame).
            let bucket_label = frame_label.clone();
            fail_buckets.record_revert(
                path.path_id,
                &bucket_label,
                Some(fail_idx),
                RevertingFrame {
                    depth: frame.depth,
                    target: frame.target,
                    selector: frame.selector,
                    revert_data: frame_revert_data,
                    label: frame_label,
                },
                captured_swaps.clone(),
                path.optimal_input,
                path.hop_outputs.clone(),
            );
        } else {
            fail_buckets.record(
                path.path_id,
                &bucket,
                Some(fail_idx),
                revert_data,
                path.optimal_input,
                path.hop_outputs.clone(),
            );
        }
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
            path.optimal_input,
            path.hop_outputs.clone(),
        );
        return Ok(None);
    };
    let Some(eth_before) = decode(1) else {
        fail_buckets.record(
            path.path_id,
            "balance-decode",
            Some(1),
            alloy::primitives::Bytes::new(),
            path.optimal_input,
            path.hop_outputs.clone(),
        );
        return Ok(None);
    };
    let Some(erc6909_before) = decode(2) else {
        fail_buckets.record(
            path.path_id,
            "balance-decode",
            Some(2),
            alloy::primitives::Bytes::new(),
            path.optimal_input,
            path.hop_outputs.clone(),
        );
        return Ok(None);
    };
    let Some(weth_after) = decode(4) else {
        fail_buckets.record(
            path.path_id,
            "balance-decode",
            Some(4),
            alloy::primitives::Bytes::new(),
            path.optimal_input,
            path.hop_outputs.clone(),
        );
        return Ok(None);
    };
    let Some(eth_after) = decode(5) else {
        fail_buckets.record(
            path.path_id,
            "balance-decode",
            Some(5),
            alloy::primitives::Bytes::new(),
            path.optimal_input,
            path.hop_outputs.clone(),
        );
        return Ok(None);
    };
    let Some(erc6909_after) = decode(6) else {
        fail_buckets.record(
            path.path_id,
            "balance-decode",
            Some(6),
            alloy::primitives::Bytes::new(),
            path.optimal_input,
            path.hop_outputs.clone(),
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
            path.optimal_input,
            path.hop_outputs.clone(),
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

    // ADR-019 D3 + ergo epic 63I7WJ — the EIP-2930 access list execute() warmed
    // was collected as a byproduct of the FIRST execute() `inspect_one` run
    // (call [3] above) by the `AccessListCollector` member of the composed
    // `SimInspector` tuple. Drain it via the paired handle — no post-re-
    // `transact` (execute() ran once). The `CallTraceInspector` +
    // `SwapEventCaptureInspector` members are drained here too (their captured
    // data is surfaced in the revert-attribution + classifier tasks, steps
    // 2 + 5 of epic 63I7WJ; drained now to prove the composition end-to-end
    // + to free the buffers for the next path on the shared per-block EVM).
    // `emit_access_list_from_state` stays as an engine-generic primitive
    // (emitting from a `State` journal); it is just no longer the production
    // AL path.
    let access_list = access_list_handle.take_access_list();
    // The call trace + captured swaps were drained right after `finalize`
    // (before the revert branch) so both branches have them.

    Ok(Some(SimResult {
        path_id: path.path_id,
        gross_profit,
        net_profit,
        gas_used,
        priority_fee,
        base_fee_next: ctx.base_fee_next,
        execute_calldata,
        access_list: Some(access_list),
        captured_swaps,
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
/// Mirrors the oracle's `decode_balance`: empty → `0`;
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
            block_timestamp: 0,
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
        degenbot_simulation::apply_simulation_overrides(&mut cache_db, &ctx.override_params())
            .expect("overrides apply over EmptyDB");
        let mut buckets = FailBuckets::new();

        let path = smoke_v2_path(7);
        let result = simulate_in_process_with_db(&ctx, cache_db, &path, &mut buckets).unwrap();
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
        // Ergo epic 63I7WJ task 3AJ4I4 — the `CallTraceInspector` captured the
        // halting frame's attribution. The 0xfe INVALID is a Halt (not a
        // Revert), so the reverting frame's revert_data is empty + label is
        // `classify_revert` on empty bytes (the "empty" bucket label). The
        // target is the executor + depth 1 (the top-level execute() call).
        let rf = failures[0]
            .reverting_frame
            .as_ref()
            .expect("the Halt frame is attributed via the call trace");
        assert_eq!(rf.depth, 1, "the halting frame is the top-level execute()");
        assert_eq!(rf.target, SMOKE_EXECUTOR, "the executor reverted");
        assert_eq!(
            rf.label, "empty",
            "Halt => empty revert_data => empty label"
        );
        assert!(rf.revert_data.is_empty(), "a Halt carries no revert data");
    }

    /// The exerciser: executor injected as bytecode that REVERTs with a 4-byte
    /// selector `0xcafebabe` (`PUSH4 0xcafebabe; MSTORE; REVERT(28,4)` roots
    /// the revert data at mem[28..32]). `classify_revert("cafebabe")` → the
    /// `unknown:0xcafebabe` bucket. The `CallTraceInspector` (attached at
    /// execute() call [3]) captures the reverting frame — ergo epic 63I7WJ
    /// task 3AJ4I4. Proves the deep attribution surfaces the reverting
    /// CONTRACT + the revert DATA + the label, not just the top-level bubble.
    #[test]
    fn simulate_in_process_with_db_revert_with_data_attributes_reverting_frame() {
        let asserter = Asserter::new();
        let provider = smoke_provider(&asserter);
        let mut ctx = smoke_ctx(&provider);
        // REVERT with 0xcafebabe (4 bytes) — classify_revert → "unknown:0xcafebabe".
        ctx.runtime_bytecode = Bytes::from_static(&[
            0x63, 0xca, 0xfe, 0xba, 0xbe, // PUSH4 0xcafebabe
            0x60, 0x00, // PUSH1 0x00
            0x52, // MSTORE — mem[0..32] = 0x00..00cafebabe (bytes 28..31)
            0x60, 0x04, // PUSH1 0x04 (len)
            0x60, 0x1c, // PUSH1 0x1c (offset=28)
            0xfd, // REVERT — returns mem[28..32] = 0xcafebabe
        ]);
        let mut cache_db: CacheDB<EmptyDB> = CacheDB::new(EmptyDB::default());
        degenbot_simulation::apply_simulation_overrides(&mut cache_db, &ctx.override_params())
            .expect("overrides apply over EmptyDB");
        let mut buckets = FailBuckets::new();

        let path = smoke_v2_path(42);
        let result = simulate_in_process_with_db(&ctx, cache_db, &path, &mut buckets).unwrap();
        assert!(result.is_none(), "reverting execute returns None");
        assert_eq!(
            buckets.get("unknown:0xcafebabe"),
            1,
            "the custom-selector revert bucket tallies"
        );
        let failures = buckets.failures();
        assert_eq!(failures.len(), 1, "one per-path failure recorded");
        assert_eq!(failures[0].path_id, 42);
        assert_eq!(
            failures[0].fail_index,
            Some(3),
            "execute() call [3] reverted"
        );
        // The reverting-frame attribution — the DEEP capture.
        let rf = failures[0]
            .reverting_frame
            .as_ref()
            .expect("the reverting frame is captured by the CallTraceInspector");
        assert_eq!(
            rf.depth, 1,
            "the revert is at the top-level execute() frame"
        );
        assert_eq!(rf.target, SMOKE_EXECUTOR, "the executor reverted");
        assert_eq!(
            rf.label, "unknown:0xcafebabe",
            "classify_revert on the frame's data"
        );
        assert_eq!(
            rf.revert_data.as_ref(),
            &[0xca, 0xfe, 0xba, 0xbe],
            "the frame's revert data is the 4-byte selector"
        );
        assert_eq!(
            failures[0].revert_data.as_ref(),
            &[0xca, 0xfe, 0xba, 0xbe],
            "the top-level revert_data matches the frame's (same bytes)"
        );
        // No swap events were emitted before the (immediate) revert — the
        // captured_swaps list is empty for this stub executor.
        assert!(
            failures[0].captured_swaps.is_empty(),
            "the cafebabe stub emits no swap events before reverting"
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
        degenbot_simulation::apply_simulation_overrides(&mut cache_db, &ctx.override_params())
            .expect("overrides apply over EmptyDB");
        let mut buckets = FailBuckets::new();

        let path = smoke_v2_path(11);
        let result = simulate_in_process_with_db(&ctx, cache_db, &path, &mut buckets).unwrap();
        assert!(result.is_none(), "no-profit returns None");
        assert_eq!(buckets.get("no-profit"), 1, "no-profit bucket tallied");
        let failures = buckets.failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].path_id, 11);
        assert_eq!(failures[0].bucket, "no-profit");
        assert!(failures[0].fail_index.is_none(), "no call reverted");
    }

    // ── TGXBCE: scan_for_v4_v2_boundary_bridge ─────────────────────────
    #[test]
    fn scan_detects_v4_native_to_v2_weth_boundary() {
        use degenbot_executor::composers::{HopInfo, V2HopInfo, V4HopInfo};
        let native = Address::ZERO;
        let weth = Address::from([0xc0u8; 20]);
        let usdc = Address::from([0xa0u8; 20]);
        // V4a: USDC→native (zfo=false → in=c1=USDC, out=c0=native)
        let a = V4HopInfo {
            pool_manager_address: Address::ZERO,
            pool_id_hex: String::new(),
            currency0_address: native,
            currency1_address: usdc,
            fee: 0,
            tick_spacing: 0,
            hook_address: Address::ZERO,
            zfo: false,
        };
        // V2b: WETH→USDC (zfo=true → in=token0=WETH)
        let b = V2HopInfo {
            pool_address: Address::ZERO,
            token0_address: weth,
            token1_address: usdc,
            fee: 0,
            zfo: true,
        };
        let hops = vec![HopInfo::V4(a), HopInfo::V2(b)];
        let got = scan_for_v4_v2_boundary_bridge(&hops, weth);
        assert!(
            got.is_some(),
            "V4 native-out → V2 WETH-in must scan as a boundary bridge"
        );
        assert!(got.unwrap().contains("Wrap"), "Wrap direction signalled");
    }

    #[test]
    fn scan_detects_v2_weth_to_v4_native_boundary() {
        use degenbot_executor::composers::{HopInfo, V2HopInfo, V4HopInfo};
        let native = Address::ZERO;
        let weth = Address::from([0xc0u8; 20]);
        let usdc = Address::from([0xa0u8; 20]);
        // V2a: USDC→WETH (zfo=false → in=token1=USDC, out=token0=WETH)
        let a = V2HopInfo {
            pool_address: Address::ZERO,
            token0_address: weth,
            token1_address: usdc,
            fee: 0,
            zfo: false,
        };
        // V4b: native→USDC (zfo=true → in=token0=native)
        let b = V4HopInfo {
            pool_manager_address: Address::ZERO,
            pool_id_hex: String::new(),
            currency0_address: native,
            currency1_address: usdc,
            fee: 0,
            tick_spacing: 0,
            hook_address: Address::ZERO,
            zfo: true,
        };
        let hops = vec![HopInfo::V2(a), HopInfo::V4(b)];
        let got = scan_for_v4_v2_boundary_bridge(&hops, weth);
        assert!(
            got.is_some(),
            "V2 WETH-out → V4 native-in must scan as a boundary bridge"
        );
        assert!(
            got.unwrap().contains("Unwrap"),
            "Unwrap direction signalled"
        );
    }

    #[test]
    fn scan_returns_none_for_native_path_ends_v4_v2_v4() {
        // V4a native-in, V4c native-out — native at PATH ENDS, boundary token
        // is WETH (ERC20) on the V2 ↔ V4 sides. No bridge needed.
        use degenbot_executor::composers::{HopInfo, V2HopInfo, V4HopInfo};
        let native = Address::ZERO;
        let weth = Address::from([0xc0u8; 20]);
        let usdc = Address::from([0xa0u8; 20]);
        let wbtc = Address::from([0xbbu8; 20]);
        // V4a: native→USDC (zfo=true → in=c0=native, out=c1=USDC)
        let a = V4HopInfo {
            pool_manager_address: Address::ZERO,
            pool_id_hex: String::new(),
            currency0_address: native,
            currency1_address: usdc,
            fee: 0,
            tick_spacing: 0,
            hook_address: Address::ZERO,
            zfo: true,
        };
        // V2b: USDC→WBTC (zfo=true, boundary token USDC is ERC20)
        let b = V2HopInfo {
            pool_address: Address::ZERO,
            token0_address: usdc,
            token1_address: wbtc,
            fee: 0,
            zfo: true,
        };
        // V4c: WBTC→native (zfo=true → in=c0=WBTC, out=c1=native)
        let c = V4HopInfo {
            pool_manager_address: Address::ZERO,
            pool_id_hex: String::new(),
            currency0_address: wbtc,
            currency1_address: native,
            fee: 0,
            tick_spacing: 0,
            hook_address: Address::ZERO,
            zfo: true,
        };
        let hops = vec![HopInfo::V4(a), HopInfo::V2(b), HopInfo::V4(c)];
        assert!(
            scan_for_v4_v2_boundary_bridge(&hops, weth).is_none(),
            "native-path-ends with ERC20 boundary must NOT scan as boundary bridge"
        );
    }
}
