//! Size-fidelity pins for the hot pool-state structs.
//!
//! Every later layout change must land as a deliberate diff to this file: the
//! asserts turn an unintended `repr(Rust)` growth (new field, padding shift,
//! enum widening) into a red test instead of silent fleet-wide cost.
//! Cloudflare's 1.1.1.1 DNS-cache writeup is the discipline source: per-entry
//! bytes are the currency; a single byte pinched per tick is fleet-wide.
//!
//! The pinned numbers are accepted-with-rationale: dragging a boxed collection
//! back inline, widening a numeric field, or adding a variant each raises the
//! per-entry footprint of a struct the fleet keeps millions of. A change that
//! intentionally alters a size updates the assert and the comment beside the
//! new number.

use degenbot_pools::sim::registry::PoolEntry;
use degenbot_pools::state::balancer_stable_state::{
    BalancerStablePoolIdentity, BalancerStablePoolState,
};
use degenbot_pools::state::balancer_weighted_state::{
    BalancerWeightedPoolIdentity, BalancerWeightedPoolState,
};
use degenbot_pools::state::curve_state::{CurvePoolIdentity, CurvePoolState};
use degenbot_pools::state::v2_state::V2PoolState;
use degenbot_pools::state::v3_state::V3PoolState;
use degenbot_pools::state::v4_state::V4PoolState;
use degenbot_pools::TickInfo;
use std::mem::size_of;

/// Report the unpinned diagnostics (runs under `--nocapture`).
#[test]
#[expect(
    clippy::print_stdout,
    reason = "size report feeds the allocation baseline log"
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

// --- TickInfo pin --------------------------------------------------------
// The tick entry is the fleet's highest-population struct: one per
// initialized tick per live V3/V4 pool, stored in `HashMap<i32, TickInfo>`.
// 48 = 16 (U128 gross) + 16 (i128 net, the on-chain int128 width)
// + 8 (u64 block), padded to the 16-byte alignment the U128 forces.
// --- PoolEntry pin -------------------------------------------------------
// Every variant carries the (identity, state) pair behind a `Box`, so the
// registry slot is the pointer+tag minimum: 8 + 8. Held inline, the slot is
// the widest variant's width — V4PoolState (384 B) plus its identity fields,
// 512 B — which inflates the whole HashMap's cache footprint 32x per entry.
#[test]
fn pool_entry_size_pinned() {
    assert_eq!(
        size_of::<PoolEntry>(),
        16,
        "PoolEntry drift: the slot must stay at the pointer+tag minimum. An \
         inline family re-inflates the registry cache footprint to the widest \
         variant's 512 B — re-measure that cost before accepting it"
    );
}

#[test]
fn tick_info_size_pinned() {
    assert_eq!(
        size_of::<TickInfo>(),
        48,
        "TickInfo drift: the layout is 48 B (16 gross + 16 i128 net + 8 block, \
         pad-to-16). Update this pin if the change is intentional; otherwise \
         the struct grew silently"
    );
}
