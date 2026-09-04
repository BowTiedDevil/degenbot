//! Solve-result allocation baseline (epic HTPKLX, task KKNKVS; consumed by
//! task 4JLQNS). Measures heap bytes + allocs for building/cloning the
//! per-hop parallel-Vec solve results BEFORE the step-outcome merge.
//! Same method as degenbot-pools/tests/alloc_tracking.rs.

use alloy::primitives::U256;
use degenbot_math::v2::hop_state::SimulationResult;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

#[test]
#[expect(
    clippy::print_stderr,
    reason = "gate hint for the human running the harness"
)]
fn solve_result_alloc_report() {
    if std::env::var("DEGENBOT_ALLOC_TRACK").as_deref() != Ok("1") {
        eprintln!("DEGENBOT_ALLOC_TRACK!=1; tracking skipped");
        return;
    }

    for hops in [2usize, 3, 5] {
        measure(&format!("simulation_result_build_hops{hops}"), || {
            SimulationResult {
                final_output: U256::from(1_000u64),
                hop_outputs: (1..=hops).map(|i| U256::from(i as u64 * 100)).collect(),
                consumed_inputs: (1..=hops).map(|i| U256::from(i as u64 * 90)).collect(),
            }
        });
    }

    let three = SimulationResult {
        final_output: U256::from(1_000u64),
        hop_outputs: vec![U256::from(300u64), U256::from(600u64), U256::from(1_000u64)],
        consumed_inputs: vec![U256::from(280u64), U256::from(550u64), U256::from(900u64)],
    };
    measure("simulation_result_clone_hops3", || three.clone());
}
