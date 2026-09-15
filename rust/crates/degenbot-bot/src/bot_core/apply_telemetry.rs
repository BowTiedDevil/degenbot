//! Per-family apply-cost telemetry : the hotpath
//! `measure_block!` labels do not aggregate reliably under `impl_type`
//! measurement, so the apply arms record into global atomics instead and
//! the block-end event surfaces the family split.

use alloy::primitives::{Address, U256};
use degenbot_core::diag;
use std::sync::atomic::{AtomicU64, Ordering};

/// Family index for the apply arms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyFamily {
    V2Sync,
    V3Swap,
    V3Liquidity,
    V4Swap,
    V4Liquidity,
}

pub const FAMILY_NAMES: [&str; 5] = [
    "v2_sync",
    "v3_swap",
    "v3_liquidity",
    "v4_swap",
    "v4_liquidity",
];

macro_rules! cells {
    ($name:ident) => {
        pub static $name: [AtomicU64; 5] = [
            const { AtomicU64::new(0) },
            const { AtomicU64::new(0) },
            const { AtomicU64::new(0) },
            const { AtomicU64::new(0) },
            const { AtomicU64::new(0) },
        ];
    };
}

cells!(APPLY_CALLS);
cells!(APPLY_TOTAL_NS);

pub fn record(family: ApplyFamily, elapsed_ns: u64) {
    let i = family as usize;
    APPLY_CALLS[i].fetch_add(1, Ordering::Relaxed);
    APPLY_TOTAL_NS[i].fetch_add(elapsed_ns, Ordering::Relaxed);
}

/// Snapshot + reset, returned as parallel arrays over `FAMILY_NAMES`.
pub fn snapshot_reset() -> ([u64; 5], [u128; 5]) {
    let mut calls = [0u64; 5];
    let mut totals = [0u128; 5];
    for i in 0..5 {
        calls[i] = APPLY_CALLS[i].swap(0, Ordering::Relaxed);
        totals[i] = u128::from(APPLY_TOTAL_NS[i].swap(0, Ordering::Relaxed));
    }
    (calls, totals)
}

pub(super) fn drain_dbg_log_buf(
    address: Address,
    tag: char,
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: i128,
    block_number: u64,
) {
    diag!(domain = pump, %tag,
        pool_addr = %format!("{address:x}"),
        tick_lower,
        tick_upper,
        liquidity_delta,
        block_number,
        "INSERT"
    );
}

/// Per-pool WS-pump trace: log every relevant-topic log the live pump
/// dispatches for the traced pool. Emits block, log-index, tx-index,
/// topic0, removed flag, and the ADR-008 clock decision — so the
/// delivery order of same-block Mint/Burn/ModifyLiquidity logs is visible
/// (a Burn arriving after the registration drain+pin is the rolling-start
/// race this probe exists to catch). Always-on DEBUG on `ingest`; high
/// volume by design, filtered at the sink.
pub(crate) fn trace_ws_log_dispatch(
    address: Address,
    topics: &[alloy::primitives::B256],
    block_number: u64,
    log_index: Option<u64>,
    tx_index: Option<u64>,
    removed: bool,
    decision: &str,
) {
    let first_topic = topics
        .first()
        .copied()
        .unwrap_or(alloy::primitives::B256::ZERO);
    diag!(domain = ingest, pool_addr = %format!("{address:x}"),
        block = block_number,
        log_index = ?log_index,
        tx_index = ?tx_index,
        topic0 = %first_topic, // full topic — greppable by short prefix
        topic1 = ?topics.get(1), // 42FL35: V4 PoolId lives here - greppable
        removed,
        decision = %decision,
        "ws-log"
    );
}

/// V3 `Swap`-arrival trace: log every swap the live pump dispatches, with
/// its on-chain `sqrt_price_x96`, `liquidity`, `tick`, and `block`. Answers
/// whether a within-tick swap that should have advanced the pool's sqrtPrice
/// actually ARRIVED (and with what value) — the discriminator between a swap
/// that was never delivered and one that was delivered but not applied.
/// Always-on DEBUG on `state`.
pub(crate) fn trace_apply_swap_v3(
    pool_address: Address,
    sqrt_price_x96: U256,
    liquidity: u128,
    tick: i32,
    block_number: u64,
) {
    diag!(domain = state, pool_addr = %format!("{pool_address:x}"),
        family = "V3",
        sqrt_price_x96 = %sqrt_price_x96,
        liquidity,
        tick,
        block = block_number,
        "swap-apply"
    );
}

/// V4 twin of [`trace_apply_swap_v3`] — logs a V4 `Swap` dispatch keyed by
/// `pool_id_hex` (the V4 analog of the pool address). Always-on DEBUG on
/// `state`.
pub(crate) fn trace_apply_swap_v4(
    pool_manager: Address,
    pool_id_hex: &str,
    sqrt_price_x96: U256,
    liquidity: u128,
    tick: i32,
    block_number: u64,
) {
    diag!(domain = state, pool_manager = %format!("{pool_manager:x}"),
        pool_id = %pool_id_hex,
        family = "V4",
        sqrt_price_x96 = %sqrt_price_x96,
        liquidity,
        tick,
        block = block_number,
        "swap-apply"
    );
}

/// V3 apply-route trace: log how a V3 liquidity update was routed —
/// `(lifecycle, routed_to)` where `routed_to` ∈ {"buffer-pump",
/// "buffer-pump-quarantined", "direct-live", "no-pool"}. Always-on DEBUG on
/// `state`.
pub(crate) fn trace_apply_route_v3(
    address: Address,
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: i128,
    block_number: u64,
    lifecycle: &str,
    routed_to: &str,
) {
    diag!(domain = state, pool_addr = %format!("{address:x}"),
        family = "V3",
        tick_lower,
        tick_upper,
        liquidity_delta,
        block = block_number,
        lifecycle = %lifecycle,
        routed_to = %routed_to,
        "apply-route"
    );
}

/// V4 twin of [`trace_apply_route_v3`] — logs how a V4 `ModifyLiquidity`
/// update was routed (`buffer-pump` / `buffer-pump-quarantined` /
/// `direct-live` / `no-pool`). Keyed by `(pool_manager, pool_id_hex)` so the
/// failing V4 pool's add/remove split is visible across the registration
/// lifecycle transition. Always-on DEBUG on `state`.
#[expect(clippy::too_many_arguments)]
pub(crate) fn trace_apply_route_v4(
    pool_manager: Address,
    pool_id_hex: &str,
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: alloy::primitives::I256,
    block_number: u64,
    lifecycle: &str,
    routed_to: &str,
) {
    diag!(domain = state, pool_manager = %format!("{pool_manager:x}"),
        pool_id = %pool_id_hex,
        family = "V4",
        tick_lower,
        tick_upper,
        liquidity_delta = %liquidity_delta,
        block = block_number,
        lifecycle = %lifecycle,
        routed_to = %routed_to,
        "apply-route"
    );
}
