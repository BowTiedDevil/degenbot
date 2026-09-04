//! Size-fidelity pins for the hot pool-state structs (epic HTPKLX, task KKNKVS).
//!
//! Every later layout change in the memory-optimization epilogue must land as a
//! deliberate diff to this file: the asserts turn an unintended `repr(Rust)`
//! growth (new field, padding shift, enum widening) into a red test instead of
//! silent fleet-wide cost. Cloudflare's 1.1.1.1 DNS-cache writeup is the
//! discipline source: per-entry bytes are the currency; a single byte pinched
//! per tick is fleet-wide.
//!
//! The pinned numbers record the BASELINE at the start of the epic (commit
//! 9ec8d5398 landed the Box<[T]> conversions for state + identity config
//! Vecs). When a task intentionally changes a size, update the assert AND cite
//! the epic task in a comment beside the new number.

use degenbot_pools::balancer_stable_state::{BalancerStablePoolIdentity, BalancerStablePoolState};
use degenbot_pools::balancer_weighted_state::{
    BalancerWeightedPoolIdentity, BalancerWeightedPoolState,
};
use degenbot_pools::curve_state::{CurvePoolIdentity, CurvePoolState};
use degenbot_pools::registry::PoolEntry;
use degenbot_pools::v2_state::V2PoolState;
use degenbot_pools::v3_state::V3PoolState;
use degenbot_pools::v4_state::V4PoolState;
use degenbot_pools::TickInfo;
use std::mem::size_of;

/// Report the unpinned diagnostics (runs under `--nocapture`).
#[test]
#[expect(
    clippy::print_stdout,
    reason = "size report feeds the HTPKLX baseline log"
)]
fn report_pool_state_sizes() {
    println!(
        "PoolEntry={} V2PoolState={} V3PoolState={} V4PoolState={} \
         CurvePoolState={} BalancerWeightedPoolState={} \
         BalancerStablePoolState={} CurvePoolIdentity={} \
         BalancerWeightedPoolIdentity={} BalancerStablePoolIdentity={}",
        size_of::<PoolEntry>(),
        size_of::<V2PoolState>(),
        size_of::<V3PoolState>(),
        size_of::<V4PoolState>(),
        size_of::<CurvePoolState>(),
        size_of::<BalancerWeightedPoolState>(),
        size_of::<BalancerStablePoolState>(),
        size_of::<CurvePoolIdentity>(),
        size_of::<BalancerWeightedPoolIdentity>(),
        size_of::<BalancerStablePoolIdentity>(),
    );
}

// --- baseline pins (2026-09-04, post-9ec8d5398) --------------------------
// The tick entry is the fleet's highest-population struct: one per
// initialized tick per live V3/V4 pool, stored in `HashMap<i32, TickInfo>`.
// 56 = 16 (U128 gross) + 32 (I256 net — over-wide vs the on-chain int128)
// + 8 (u64 block).
// --- PoolEntry pin (2026-09-04, post-KO3SBO boxing) -----------------------
// Every variant now carries the (identity, state) pair behind a `Box`, so
// the registry slot is the pointer+tag minimum: 8 + 8. The pre-boxing slot
// was 512 B (pinned by the largest variant, V4PoolState at 384 B + identity
// fields), inflating the whole HashMap's cache footprint 32x per entry.
#[test]
fn pool_entry_size_pinned() {
    assert_eq!(
        size_of::<PoolEntry>(),
        16,
        "PoolEntry drift: the slot must stay at the pointer+tag minimum. If a \
         family was added inline, re-measure the registry cache footprint and \
         cite the task; the KO3SBO boxing shrank this from 512 B"
    );
}

#[test]
fn tick_info_size_pinned() {
    assert_eq!(
        size_of::<TickInfo>(),
        56,
        "TickInfo drift: if this change is an intentional optimization from \
         the HTPKLX epilogue (e.g. the I256->i128 net task), update the pin \
         and cite the task; otherwise the struct grew silently"
    );
}
