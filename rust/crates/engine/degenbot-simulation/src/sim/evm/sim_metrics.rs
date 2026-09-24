//! SIMPIPE T1 lab instrumentation — cold-fetch + handle-build counters.
//!
//! Counting seams:
//! - [`WarmCodeCache`](super::warm_code_cache::WarmCodeCache) forwards — every
//!   call reaching the underlying `Db` is BY DEFINITION a `CacheDB` miss
//!   (REVM consults the overlay only after its maps miss). `basic`/`code`
//!   may still be served from the cross-block warm cache (TTL hit = no
//!   underlying call = warm), while `storage`/`block_hash` always forward.
//! - Each forwarded call is split FAST (<100 µs — served by the in-process
//!   layer, state/anchor-adjacent) vs SLOW (>=100 µs — reached the RPC
//!   transport). The SLOW totals are the latency the M1 pooled-handle
//!   parallel-workers pipeline buys back; the FAST share sizes any prefetch
//!   headroom (JSXP3I direction 1).
//! - [`BlockSimHandle::build`](super::simulator::BlockSimHandle::build) —
//!   count + duration (the amortization target: today the build re-arms per
//!   1-candidate dispatch).
//!
//! Transport: plain atomics + a per-fan-out DELTA log line (emitted by
//! degenbot-arbitrage at fan-out exit) — one grep-able line per fan-out, no
//! Prometheus plumbing for what is a lab instrument. Was consumed by the
//! gitignored logs/ artifact `logs/simpipe_lab.md` (since removed).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

// Underlying forward counts (cold to the CacheDB overlay), latency-split.
static COLD_BASIC_FAST: AtomicU64 = AtomicU64::new(0);
static COLD_BASIC_SLOW: AtomicU64 = AtomicU64::new(0);
static COLD_BASIC_SLOW_NS: AtomicU64 = AtomicU64::new(0);
static BASIC_WARM_HITS: AtomicU64 = AtomicU64::new(0);
static COLD_CODE_SLOW: AtomicU64 = AtomicU64::new(0);
static COLD_CODE_SLOW_NS: AtomicU64 = AtomicU64::new(0);
static COLD_STORAGE_FAST: AtomicU64 = AtomicU64::new(0);
static COLD_STORAGE_SLOW: AtomicU64 = AtomicU64::new(0);
static COLD_STORAGE_SLOW_NS: AtomicU64 = AtomicU64::new(0);
static COLD_BLOCK_HASH: AtomicU64 = AtomicU64::new(0);

// Per-block handle build.
static HANDLE_BUILDS: AtomicU64 = AtomicU64::new(0);
static HANDLE_BUILD_NS: AtomicU64 = AtomicU64::new(0);

// Completed per-candidate sims.
static SIM_CALLS: AtomicU64 = AtomicU64::new(0);
static SIM_TOTAL_NS: AtomicU64 = AtomicU64::new(0);

/// FAST vs SLOW split: an anchor/BotStateDb serve lands ~µs, an RPC fetch
/// ~300 µs+; 100 µs separates them with ~two orders of margin.
const SLOW_FETCH_THRESHOLD: Duration = Duration::from_micros(100);

/// Record one `basic` forward (underlying call latency).
pub fn record_basic(dur: Duration) {
    if dur >= SLOW_FETCH_THRESHOLD {
        COLD_BASIC_SLOW.fetch_add(1, Ordering::Relaxed);
        COLD_BASIC_SLOW_NS.fetch_add(
            u64::try_from(dur.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    } else {
        COLD_BASIC_FAST.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one `basic` served by the cross-block warm cache (reuse signal).
pub fn record_basic_warm_hit() {
    BASIC_WARM_HITS.fetch_add(1, Ordering::Relaxed);
}

/// Record one `code_by_hash` forward.
pub fn record_code(dur: Duration) {
    if dur >= SLOW_FETCH_THRESHOLD {
        COLD_CODE_SLOW.fetch_add(1, Ordering::Relaxed);
        COLD_CODE_SLOW_NS.fetch_add(
            u64::try_from(dur.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

/// Record one `storage` forward (always reaches the underlying `Db`).
pub fn record_storage(dur: Duration) {
    if dur >= SLOW_FETCH_THRESHOLD {
        COLD_STORAGE_SLOW.fetch_add(1, Ordering::Relaxed);
        COLD_STORAGE_SLOW_NS.fetch_add(
            u64::try_from(dur.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    } else {
        COLD_STORAGE_FAST.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one `block_hash` forward.
pub fn record_block_hash() {
    COLD_BLOCK_HASH.fetch_add(1, Ordering::Relaxed);
}

/// Record one `BlockSimHandle` build.
pub fn record_handle_build(dur: Duration) {
    HANDLE_BUILDS.fetch_add(1, Ordering::Relaxed);
    HANDLE_BUILD_NS.fetch_add(
        u64::try_from(dur.as_nanos()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
}

/// Record one completed per-candidate sim.
pub fn record_sim(total: Duration) {
    SIM_CALLS.fetch_add(1, Ordering::Relaxed);
    SIM_TOTAL_NS.fetch_add(
        u64::try_from(total.as_nanos()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
}

/// Point-in-time copy of every lab counter.
#[derive(Debug, Clone, Copy, Default)]
pub struct SimLabSnapshot {
    pub cold_basic_fast: u64,
    pub cold_basic_slow: u64,
    pub cold_basic_slow_ns: u64,
    pub basic_warm_hits: u64,
    pub cold_code_slow: u64,
    pub cold_code_slow_ns: u64,
    pub cold_storage_fast: u64,
    pub cold_storage_slow: u64,
    pub cold_storage_slow_ns: u64,
    pub cold_block_hash: u64,
    pub handle_builds: u64,
    pub handle_build_ns: u64,
    pub sim_calls: u64,
    pub sim_total_ns: u64,
}

/// Read every counter.
#[must_use]
pub fn snapshot() -> SimLabSnapshot {
    SimLabSnapshot {
        cold_basic_fast: COLD_BASIC_FAST.load(Ordering::Relaxed),
        cold_basic_slow: COLD_BASIC_SLOW.load(Ordering::Relaxed),
        cold_basic_slow_ns: COLD_BASIC_SLOW_NS.load(Ordering::Relaxed),
        basic_warm_hits: BASIC_WARM_HITS.load(Ordering::Relaxed),
        cold_code_slow: COLD_CODE_SLOW.load(Ordering::Relaxed),
        cold_code_slow_ns: COLD_CODE_SLOW_NS.load(Ordering::Relaxed),
        cold_storage_fast: COLD_STORAGE_FAST.load(Ordering::Relaxed),
        cold_storage_slow: COLD_STORAGE_SLOW.load(Ordering::Relaxed),
        cold_storage_slow_ns: COLD_STORAGE_SLOW_NS.load(Ordering::Relaxed),
        cold_block_hash: COLD_BLOCK_HASH.load(Ordering::Relaxed),
        handle_builds: HANDLE_BUILDS.load(Ordering::Relaxed),
        handle_build_ns: HANDLE_BUILD_NS.load(Ordering::Relaxed),
        sim_calls: SIM_CALLS.load(Ordering::Relaxed),
        sim_total_ns: SIM_TOTAL_NS.load(Ordering::Relaxed),
    }
}

/// Per-field saturating delta between two snapshots.
#[must_use]
pub fn delta(after: SimLabSnapshot, before: SimLabSnapshot) -> SimLabSnapshot {
    macro_rules! d {
        ($f:ident) => {
            after.$f.saturating_sub(before.$f)
        };
    }
    SimLabSnapshot {
        cold_basic_fast: d!(cold_basic_fast),
        cold_basic_slow: d!(cold_basic_slow),
        cold_basic_slow_ns: d!(cold_basic_slow_ns),
        basic_warm_hits: d!(basic_warm_hits),
        cold_code_slow: d!(cold_code_slow),
        cold_code_slow_ns: d!(cold_code_slow_ns),
        cold_storage_fast: d!(cold_storage_fast),
        cold_storage_slow: d!(cold_storage_slow),
        cold_storage_slow_ns: d!(cold_storage_slow_ns),
        cold_block_hash: d!(cold_block_hash),
        handle_builds: d!(handle_builds),
        handle_build_ns: d!(handle_build_ns),
        sim_calls: d!(sim_calls),
        sim_total_ns: d!(sim_total_ns),
    }
}

/// Aggregate SLOW (>=100 µs, RPC-reached) forward nanoseconds across kinds.
#[must_use]
pub fn slow_total_ns(d: &SimLabSnapshot) -> u64 {
    d.cold_basic_slow_ns + d.cold_code_slow_ns + d.cold_storage_slow_ns
}

/// One grep-able lab line for a fan-out's delta — target `[sim-lab]`.
#[must_use]
pub fn format_delta(d: &SimLabSnapshot, candidates: usize) -> String {
    format!(
        "cands={candidates} sims={} sim_ms={} builds={} build_ms={} \
         cold: b_fast={} b_slow={} b_ms={} code={} c_ms={} s_fast={} s_slow={} s_ms={} \
         bh={} warm_basic_hits={} slow_ms={}",
        d.sim_calls,
        d.sim_total_ns / 1_000_000,
        d.handle_builds,
        d.handle_build_ns / 1_000_000,
        d.cold_basic_fast,
        d.cold_basic_slow,
        d.cold_basic_slow_ns / 1_000_000,
        d.cold_code_slow,
        d.cold_code_slow_ns / 1_000_000,
        d.cold_storage_fast,
        d.cold_storage_slow,
        d.cold_storage_slow_ns / 1_000_000,
        d.cold_block_hash,
        d.basic_warm_hits,
        slow_total_ns(d) / 1_000_000,
    )
}

/// Wall-clock helper for the instrumented call sites.
#[must_use]
pub fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    (f(), started.elapsed())
}
