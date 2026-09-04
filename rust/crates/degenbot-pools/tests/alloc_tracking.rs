//! Per-op heap tracking harness (epic HTPKLX, task KKNKVS).
//!
//! Cloudflare-method baseline: wrap the system allocator with an opt-in
//! counting shim and measure heap bytes + allocation count for the core
//! pool-lifecycle workloads. Later epilogue tasks (step-outcome merge,
//! `PoolEntry` boxing, `TickInfo` shrink) read their before/after numbers from
//! this harness and record them under logs/.
//!
//! Gate: measurements only run when `DEGENBOT_ALLOC_TRACK=1` (the repo
//! opt-in runtime-gate convention); without the gate the test passes
//! instantly and the shim stays inactive.
//!
//! The binary is fully single-threaded (one #[test] drives every phase) so
//! cross-thread allocation noise cannot skew counts.

use degenbot_pools::registry::ConcentratedLiquidityPoolMut;
use degenbot_pools::v2_state::RegisterV2PoolParams;
use degenbot_pools::v2_state::V2PoolState;
use degenbot_pools::v3_state::{RegisterV3PoolParams, V3PoolState};
use degenbot_pools::TickInfo;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use alloy::primitives::{I256, U128, U256};
use hashbrown::HashMap as HB;

struct Tracking;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static BYTES: AtomicU64 = AtomicU64::new(0);
static ALLOCS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() && ACTIVE.load(Ordering::Relaxed) {
            BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
    }
}

#[global_allocator]
static A: Tracking = Tracking;

#[expect(
    clippy::print_stdout,
    reason = "JSON lines are the harness report consumed by the HTPKLX baseline log"
)]
fn measure<T>(label: &str, f: impl FnOnce() -> T) -> T {
    BYTES.store(0, Ordering::Relaxed);
    ALLOCS.store(0, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
    let out = f();
    ACTIVE.store(false, Ordering::Relaxed);
    println!(
        "{{\"op\":\"{label}\",\"bytes\":{},\"allocs\":{}}}",
        BYTES.load(Ordering::Relaxed),
        ALLOCS.load(Ordering::Relaxed)
    );
    out
}

fn seeded_tick_map(initialized_ticks: usize) -> HB<i32, TickInfo> {
    (0..initialized_ticks)
        .map(|i| {
            // i32::MAX/60 > 128 — no truncation for our tick counts.
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_possible_wrap,
                reason = "tick indices bounded by TICKS constant"
            )]
            let tick = (i as i32) * 60; // spacing-60-like positions
            (
                tick,
                TickInfo {
                    liquidity_gross: U128::from(1_000_000u64 + i as u64),
                    liquidity_net: I256::try_from(1_000i64).unwrap_or(I256::ONE), // 1,000 < I256::MAX — always Some
                    block: 0,
                },
            )
        })
        .collect()
}

fn v3_params_with(tick_data: HB<i32, TickInfo>) -> RegisterV3PoolParams {
    RegisterV3PoolParams {
        tick_data,
        ..RegisterV3PoolParams::default()
    }
}

const TICKS: usize = 128;
const JOURNAL_DEPTH: usize = 8;

#[test]
#[expect(
    clippy::print_stderr,
    reason = "gate hint for the human running the harness"
)]
fn pool_lifecycle_alloc_report() {
    if std::env::var("DEGENBOT_ALLOC_TRACK").as_deref() != Ok("1") {
        eprintln!("DEGENBOT_ALLOC_TRACK!=1; tracking skipped (set gate + --nocapture)");
        return;
    }

    // V3 registration (tick-heavy family): tick-map heap + journal genesis.
    let v3 = measure("v3_register_128_ticks", || {
        V3PoolState::from_params(v3_params_with(seeded_tick_map(TICKS)), JOURNAL_DEPTH)
    });
    let mut v3_state = v3.1;

    // V3 state clone (snapshot path; no pinned seeds).
    measure("v3_state_clone_128_ticks", || v3_state.clone());

    // V3 apply_swap (journal delta + reverse-apply priors; empty priors).
    let tick_priors: Vec<(i32, TickInfo)> = Vec::new();
    measure("v3_apply_swap_empty_priors", || {
        v3_state.apply_swap(U256::from(2u128) << 96, 2_000_000, 1, 12, &tick_priors);
    });

    // Bare tick map build (per-tick amortized cost).
    measure("tick_map_build_128", || seeded_tick_map(128));

    // V2 registration (journal-twinned scalars, no tick map).
    measure("v2_register_journal8", || {
        V2PoolState::from_params(&RegisterV2PoolParams::default(), JOURNAL_DEPTH)
    });

    // V2 state clone.
    let (_, v2_state) = V2PoolState::from_params(&RegisterV2PoolParams::default(), JOURNAL_DEPTH);
    measure("v2_state_clone", || v2_state.clone());
}
