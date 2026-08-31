//! Per-family apply-cost telemetry (loop-5 / task 2SDIQW): the hotpath
//! `measure_block!` labels do not aggregate reliably under `impl_type`
//! measurement, so the apply arms record into global atomics instead and
//! the block-end event surfaces the family split.

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
