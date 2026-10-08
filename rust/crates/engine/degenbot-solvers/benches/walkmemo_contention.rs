#![expect(clippy::unwrap_used, clippy::expect_used)]

//! WalkMemo contention baseline — TODAY's single-mutex shape, documented.
//!
//! Rayon workers drive the CURRENT single-`parking_lot::Mutex` `WalkMemo`
//! through the public all-CL solve entry ([`solve_cl_piecewise`]) — the
//! production probe/store path: an unlocked `walk_path_fingerprint`, one
//! locked probe (census insert + cache consult, result clone on a `Hit`),
//! and on a `Miss` the inner walk plus the locked `note_cost`/`store`
//! commit. Whatever a future memo-sharding change builds, its throughput
//! here per worker count is the number it must beat.
//!
//! Arms (each benches its own `Arc<WalkMemo>` shared by every worker):
//!
//! - `walkmemo_hits` — every composition primed to a profitable solve, so
//!   every op is a cached `Hit` replay (probe + clone, no walk).
//! - `walkmemo_negatives` — every composition primed to `None` (the
//!   same-price 1:1 pair the memo tests pin as unprofitable), so every op
//!   is a cached `Negative` replay.
//! - `walkmemo_misses` — never-primed compositions rotate through windows
//!   sized so census-scoped eviction (previous epoch only) has dropped each
//!   fingerprint again by the time rotation re-probes it: every op is a
//!   genuine fresh `Miss` (walk + commit runs every time).
//! - `walkmemo_mixed` — one sweep mixes the three: 50% cached `Hit`, 25%
//!   cached `Negative`, 25% fresh `Miss` (the composition-repeat profile
//!   the memo exists for).
//!
//! Every sweep advances the memo epoch (`begin_block`), mirroring the
//! engine's per-block lifecycle; that is also what makes the fresh-Miss
//! rotation sound. The memo runs the fully-instrumented arm (memo_on +
//! stats_on), matching the memo tests.
//!
//! Criterion `elem/s` is the reported metric: one element = one probe/store
//! op, swept over worker counts 1/2/4/8 (1 = the uncontended baseline) and
//! set sizes 64 (hot set) / 4096 (the old wholesale-clear cap). Fixtures
//! mirror `src/cl/tests/memo.rs`; a `+i` liquidity delta per bank entry
//! makes every composition a distinct fingerprint without moving the
//! economics, and priming asserts each entry landed in its intended arm.

#![expect(clippy::doc_markdown)]

use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use alloy::primitives::U256;
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::cl::{solve_cl_piecewise, ClSolveTables, WalkMemo, WalkOutcome};
use degenbot_solvers::runtime::SolveRuntimeConfig;
use rayon::iter::{IntoParallelIterator, ParallelIterator};

/// Worker counts swept for every arm; 1 is the uncontended baseline.
const WORKER_COUNTS: [usize; 4] = [1, 2, 4, 8];

/// Composition-set sizes: 64 (cache-resident hot set) and 4096 (the old
/// wholesale-clear cap this memo's census-scoped eviction replaced).
const SET_SIZES: [usize; 2] = [64, 4096];

/// Fresh-miss window depth: a sweep probes one window of the miss bank, and
/// `begin_block` retention covers only the current + previous epoch, so a
/// window probed in sweep r is evicted by sweep r+2 and genuinely missing
/// again when rotation reuses it in sweep r+4.
const MISS_WINDOW_ROTATION: usize = 4;

// ---------------------------------------------------------------------------
// Fixtures — mirror the memo-test fixtures (src/cl/tests/memo.rs).
// ---------------------------------------------------------------------------

/// Sqrt price at `tick` (the memo tests' `sp_at` helper).
fn sp_at(tick: i32) -> U256 {
    U256::from(get_sqrt_ratio_at_tick_internal(tick).unwrap())
}

/// One 1:1 single-range hop at tick 0 with a ±60-tick band — the memo
/// tests' `make_v3_hop_at_1to1` shape.
fn hop_at_1to1(liquidity: u128, zfo: bool) -> IntV3TickRangeHop {
    IntV3TickRangeHop {
        liquidity,
        sqrt_price_x96: sp_at(0),
        sqrt_price_lower_x96: sp_at(-60),
        sqrt_price_upper_x96: sp_at(60),
        gamma_numer: 997_000, // 0.3% fee → gamma = 997_000 / 1_000_000
        fee_denom: 1_000_000,
        zero_for_one: zfo,
        word_boundary_prices: Vec::new(),
    }
}

fn one_range_seq(liquidity: u128, zfo: bool) -> IntV3TickRangeSequence {
    IntV3TickRangeSequence::new(vec![hop_at_1to1(liquidity, zfo)]).unwrap()
}

/// Multi-range CL sequence in swap order around `anchor_tick` — the memo
/// tests' `multi_range_sequence` helper shape.
fn multi_range_sequence(
    anchor_tick: i32,
    step: i32,
    zfo: bool,
    liquidities: &[u128],
) -> IntV3TickRangeSequence {
    let ranges: Vec<IntV3TickRangeHop> = liquidities
        .iter()
        .enumerate()
        .map(|(i, &liquidity)| {
            let i = i32::try_from(i).unwrap();
            let (tick_lo, tick_hi) = if zfo {
                (anchor_tick - (i + 1) * step, anchor_tick - i * step)
            } else {
                (anchor_tick + i * step, anchor_tick + (i + 1) * step)
            };
            let sqrt_price_x96 = if i == 0 {
                sp_at(anchor_tick)
            } else if zfo {
                sp_at(anchor_tick - i * step)
            } else {
                sp_at(anchor_tick + i * step)
            };
            IntV3TickRangeHop {
                liquidity,
                sqrt_price_x96,
                sqrt_price_lower_x96: sp_at(tick_lo),
                sqrt_price_upper_x96: sp_at(tick_hi),
                gamma_numer: 997_000,
                fee_denom: 1_000_000,
                zero_for_one: zfo,
                word_boundary_prices: Vec::new(),
            }
        })
        .collect();
    IntV3TickRangeSequence::new(ranges).unwrap()
}

/// A bank of two-sequence compositions with pre-derived `ClSolveTables`
/// (the projection-cache shape: tables exist per pool/direction before any
/// solve). One `solve_at` is one production memo op through the public
/// entry — fingerprint, locked probe, and only on a `Miss` the walk and the
/// locked commit.
struct PairBank {
    seq_a: Vec<IntV3TickRangeSequence>,
    seq_b: Vec<IntV3TickRangeSequence>,
    prepared: Vec<[ClSolveTables; 2]>,
}

impl PairBank {
    fn with_capacity(n: usize) -> Self {
        Self {
            seq_a: Vec::with_capacity(n),
            seq_b: Vec::with_capacity(n),
            prepared: Vec::with_capacity(n),
        }
    }

    fn push(&mut self, a: IntV3TickRangeSequence, b: IntV3TickRangeSequence) {
        self.prepared
            .push([ClSolveTables::derive(&a), ClSolveTables::derive(&b)]);
        self.seq_a.push(a);
        self.seq_b.push(b);
    }

    fn solve_at(&self, i: usize, memo: &WalkMemo, cfg: &SolveRuntimeConfig) -> WalkOutcome {
        let pair = [&self.seq_a[i], &self.seq_b[i]];
        solve_cl_piecewise(&pair, &self.prepared[i], Some(memo), cfg, None)
    }
}

/// Cached-`Hit` bank: the memo tests' pinned-profitable fixture (1-range
/// anchor-750 leg + 12-range late-liquidity leg). A `+i` liquidity delta on
/// the first leg makes every composition a distinct fingerprint without
/// moving the economics; priming asserts each stays profitable.
fn hit_bank(set_size: usize) -> PairBank {
    let mut liquidities = vec![1_000_000_000u128; 10];
    liquidities.push(10_000_000_000_000u128);
    liquidities.push(1_000_000_000u128);
    let mut bank = PairBank::with_capacity(set_size);
    for i in 0..set_size {
        let a = multi_range_sequence(750, 1300, true, &[1_000_000_000_000_000u128 + i as u128]);
        let b = multi_range_sequence(0, 60, false, &liquidities);
        bank.push(a, b);
    }
    bank
}

/// Cached-`Negative` bank: two same-price 1:1 pools in opposite directions —
/// the memo tests' pinned-unprofitable shape (fees dominate, always `None`).
fn negative_bank(set_size: usize) -> PairBank {
    let mut bank = PairBank::with_capacity(set_size);
    for i in 0..set_size {
        let liq = 10_000_000_000_000u128 + i as u128;
        bank.push(one_range_seq(liq, true), one_range_seq(liq, false));
    }
    bank
}

/// Fresh-`Miss` bank: `windows` × `per_window` never-primed compositions in
/// the same two-pool shape, plus one trailing sentinel composition used only
/// for the setup liveness check (never measured, so it cannot turn the
/// first measured op into a replay).
fn miss_bank(windows: usize, per_window: usize) -> PairBank {
    let total = windows * per_window + 1;
    let mut bank = PairBank::with_capacity(total);
    for i in 0..total {
        let liq = 8_000_000_000_000u128 + i as u128;
        bank.push(one_range_seq(liq, true), one_range_seq(liq, false));
    }
    bank
}

// ---------------------------------------------------------------------------
// Setup helpers.
// ---------------------------------------------------------------------------

/// The shared runtime config plus the active memo (memo_on + stats_on, the
/// memo tests' arm), advanced to epoch 1.
fn primed_memo() -> (SolveRuntimeConfig, Arc<WalkMemo>) {
    let cfg = SolveRuntimeConfig::default();
    let memo = Arc::new(WalkMemo::new(true, true));
    memo.begin_block(1);
    (cfg, memo)
}

/// Prime `count` compositions into `memo` (one walk each; the entry stores
/// the outcome), asserting every entry landed on the expected side so the
/// measured arms are what their names claim.
fn prime_bank(
    bank: &PairBank,
    count: usize,
    memo: &WalkMemo,
    cfg: &SolveRuntimeConfig,
    expect_profitable: bool,
) {
    for i in 0..count {
        let outcome = bank.solve_at(i, memo, cfg);
        if expect_profitable {
            assert!(
                outcome.result.is_some(),
                "hit fixture {i} must be profitable"
            );
        } else {
            assert!(
                outcome.result.is_none(),
                "negative fixture {i} must be unprofitable"
            );
        }
    }
}

fn sweep_pool(workers: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .expect("rayon pool builds")
}

// ---------------------------------------------------------------------------
// Benches.
// ---------------------------------------------------------------------------

/// Cached-`Hit` contention: every op probes a primed profitable composition.
fn bench_walkmemo_hits(c: &mut Criterion) {
    let mut group = c.benchmark_group("walkmemo_hits");
    for &set_size in &SET_SIZES {
        let (cfg, memo) = primed_memo();
        let bank = hit_bank(set_size);
        prime_bank(&bank, set_size, &memo, &cfg, true);
        // Arm sanity: a primed composition must replay byte-identical from
        // the cache, and priming + replay must all have consulted it.
        let primed = bank
            .solve_at(0, &memo, &cfg)
            .result
            .expect("primed hit has a result");
        let replay = bank.solve_at(0, &memo, &cfg);
        assert_eq!(
            replay.result.as_ref(),
            Some(&primed),
            "primed hit must replay from the cache"
        );
        assert_eq!(
            memo.take_stats().cache_plays,
            set_size as u64 + 2,
            "priming + primed-extraction + replay must all consult the cache"
        );

        group.throughput(Throughput::Elements(set_size as u64));
        let epoch = AtomicU64::new(2);
        for &workers in &WORKER_COUNTS {
            let pool = sweep_pool(workers);
            group.bench_function(format!("set{set_size}/w{workers}"), |bencher| {
                bencher.iter(|| {
                    // Block lifecycle per sweep: advance the epoch and prune
                    // the census; every primed fingerprint is re-probed each
                    // sweep, so the eviction retains the whole set.
                    memo.begin_block(epoch.fetch_add(1, Ordering::Relaxed));
                    pool.install(|| {
                        (0..set_size).into_par_iter().for_each(|i| {
                            black_box(bank.solve_at(i, &memo, &cfg));
                        });
                    });
                });
            });
        }
    }
    group.finish();
}

/// Cached-`Negative` contention: every op probes a primed `None` entry.
fn bench_walkmemo_negatives(c: &mut Criterion) {
    let mut group = c.benchmark_group("walkmemo_negatives");
    for &set_size in &SET_SIZES {
        let (cfg, memo) = primed_memo();
        let bank = negative_bank(set_size);
        prime_bank(&bank, set_size, &memo, &cfg, false);
        // Arm sanity: the gauge mirrors the cached Nones, the replay played
        // a negative, and priming + replay all consulted the cache.
        let replay = bank.solve_at(0, &memo, &cfg);
        assert!(replay.result.is_none(), "primed negative must replay None");
        let stats = memo.take_stats();
        assert_eq!(
            stats.negative_entries, set_size as u64,
            "gauge must mirror the cached Nones"
        );
        assert_eq!(
            stats.negatives_played, 1,
            "the replay must play the cached negative"
        );
        assert_eq!(
            stats.cache_plays,
            set_size as u64 + 1,
            "priming + replay probes must all consult the cache"
        );

        group.throughput(Throughput::Elements(set_size as u64));
        let epoch = AtomicU64::new(2);
        for &workers in &WORKER_COUNTS {
            let pool = sweep_pool(workers);
            group.bench_function(format!("set{set_size}/w{workers}"), |bencher| {
                bencher.iter(|| {
                    memo.begin_block(epoch.fetch_add(1, Ordering::Relaxed));
                    pool.install(|| {
                        (0..set_size).into_par_iter().for_each(|i| {
                            black_box(bank.solve_at(i, &memo, &cfg));
                        });
                    });
                });
            });
        }
    }
    group.finish();
}

/// Fresh-`Miss` contention: never-primed compositions rotate through
/// eviction-safe windows, so every op walks and commits.
fn bench_walkmemo_misses(c: &mut Criterion) {
    let mut group = c.benchmark_group("walkmemo_misses");
    for &set_size in &SET_SIZES {
        let (cfg, memo) = primed_memo();
        let bank = miss_bank(MISS_WINDOW_ROTATION, set_size);
        // Liveness pre-flight on the sentinel: the cache starts empty, so
        // the probe is a Miss and the consult counter must say exactly one.
        let sentinel = MISS_WINDOW_ROTATION * set_size;
        bank.solve_at(sentinel, &memo, &cfg);
        assert_eq!(
            memo.take_stats().cache_plays,
            1,
            "the sentinel probe must consult the empty cache exactly once"
        );

        group.throughput(Throughput::Elements(set_size as u64));
        let epoch = AtomicU64::new(2);
        for &workers in &WORKER_COUNTS {
            let pool = sweep_pool(workers);
            group.bench_function(format!("set{set_size}/w{workers}"), |bencher| {
                bencher.iter(|| {
                    let e = epoch.fetch_add(1, Ordering::Relaxed);
                    memo.begin_block(e);
                    // Window w rotates once per sweep; `begin_block` keeps
                    // only the previous epoch's probes, so a window probed in
                    // sweep r is evicted by sweep r+2 and rotation reuses it
                    // in sweep r+4 — every probe here is a fresh Miss.
                    let window =
                        usize::try_from((e - 2) % MISS_WINDOW_ROTATION as u64).unwrap() * set_size;
                    pool.install(|| {
                        (0..set_size).into_par_iter().for_each(|i| {
                            black_box(bank.solve_at(window + i, &memo, &cfg));
                        });
                    });
                });
            });
        }
    }
    group.finish();
}

/// Mixed contention: one sweep is 50% cached `Hit`, 25% cached `Negative`,
/// 25% fresh `Miss` — the composition-repeat profile the memo exists for.
fn bench_walkmemo_mixed(c: &mut Criterion) {
    let mut group = c.benchmark_group("walkmemo_mixed");
    for &set_size in &SET_SIZES {
        let (cfg, memo) = primed_memo();
        let hits = hit_bank(set_size);
        let negatives = negative_bank(set_size);
        // Miss ops are set_size/4 per sweep, so the miss bank needs only
        // ROTATION windows of that size to keep every mixed miss fresh.
        let misses = miss_bank(MISS_WINDOW_ROTATION, set_size / 4);
        prime_bank(&hits, set_size, &memo, &cfg, true);
        prime_bank(&negatives, set_size, &memo, &cfg, false);
        // Liveness pre-flight on the miss sentinel, then pin the primed
        // arms: set_size cached Nones, and every priming probe + the
        // sentinel consulted the cache.
        misses.solve_at(MISS_WINDOW_ROTATION * (set_size / 4), &memo, &cfg);
        let stats = memo.take_stats();
        assert_eq!(
            stats.negative_entries,
            set_size as u64 + 1,
            "gauge must mirror the primed Nones + the sentinel's None"
        );
        assert_eq!(
            stats.cache_plays,
            2 * set_size as u64 + 1,
            "priming probes + the sentinel must all consult the cache"
        );

        group.throughput(Throughput::Elements(set_size as u64));
        let epoch = AtomicU64::new(2);
        for &workers in &WORKER_COUNTS {
            let pool = sweep_pool(workers);
            group.bench_function(format!("set{set_size}/w{workers}"), |bencher| {
                bencher.iter(|| {
                    let e = epoch.fetch_add(1, Ordering::Relaxed);
                    memo.begin_block(e);
                    let miss_window = usize::try_from((e - 2) % MISS_WINDOW_ROTATION as u64)
                        .unwrap()
                        * (set_size / 4);
                    pool.install(|| {
                        (0..set_size).into_par_iter().for_each(|i| {
                            // Role per 8-op block: 4 hits, 2 negatives,
                            // 2 fresh misses.
                            match i % 8 {
                                0..=3 => {
                                    black_box(hits.solve_at((i / 8) * 4 + (i % 8), &memo, &cfg));
                                }
                                4..=5 => {
                                    black_box(negatives.solve_at(
                                        (i / 8) * 2 + (i % 8 - 4),
                                        &memo,
                                        &cfg,
                                    ));
                                }
                                _ => {
                                    black_box(misses.solve_at(
                                        miss_window + (i / 8) * 2 + (i % 8 - 6),
                                        &memo,
                                        &cfg,
                                    ));
                                }
                            }
                        });
                    });
                });
            });
        }
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_walkmemo_hits,
    bench_walkmemo_negatives,
    bench_walkmemo_misses,
    bench_walkmemo_mixed,
);
criterion_main!(benches);
