//! ADR-090: the hooked-leg re-quote latency envelope, measured live.
//!
//! Three questions, one harness, all from a pinned archive block:
//!
//! 1. **AOT coverage.** Which of the contracts a re-quote executes are in the
//!    durable artifact store the production JIT holder opens
//!    (`<persistence.state_dir>/aot-artifacts`), and what does a boot pay for
//!    the ones that are not? The harness reads the PRODUCTION store's index
//!    (read-only) and prints, per contract, its code hash and whether an
//!    artifact for it exists.
//! 2. **The latency distribution.** 1,000 hooked re-quotes at a finalist-rate
//!    burst, rotating the H1 census's real hooked pools: p50/p95/p99 plus the
//!    per-quote cold-read count (the reads that reached the RPC leaf — the
//!    figure that must be ~0 for a warm tier).
//! 3. **Batch or not.** The cold tail the percentiles hide: the FIRST quote of
//!    each hook (code + slot set unlearned), reported per hook.
//!
//! # The three shapes measured
//!
//! - **shipped** — the production seam verbatim: one fresh anchor-pinned
//!   scratch per leg quote (`InlineSimHook::requote_leg` →
//!   `quote_leg_at_anchor`), driven through the hook's own
//!   `block_on(spawn_blocking(..))`, with a fresh quoter per call (so the
//!   quoter's bound-manager resolve frame runs every time). The shared
//!   `WarmCodeCacheInner` (accounts + bytecode, cross-quote) and the durable
//!   per-hook slot store are the production ones.
//! - **shipped + AOT** — the same seam with the process-shared runner built
//!   over a store that holds PoolManager, the V4 Quoter and every census hook
//!   (the "prepped at boot" state). Interpretation is what a cold production
//!   boot does on the first re-quote (worker-pool mode defers a miss to the
//!   interpreter, `jit::runtime_tuning`), so the pair isolates the AOT row's
//!   value.
//! - **per-anchor scratch** — one scratch + one quoter reused across the whole
//!   burst, which is the shape `v4_black_box`'s module docs describe ("the
//!   layered `CacheDB` beneath a scratch IS the per-anchor storage memo"). Its
//!   cold-read count is the floor this seam can reach at a fixed anchor.
//!
//! # Running it
//!
//! ```sh
//! cargo test -p degenbot-simulation --features evm2-jit \
//!   --test evm2_v4_hook_requote_bench requote_latency_envelope \
//!   -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `DEGENBOT_RPC_HTTP_CHAINID_1` (or `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`)
//! selects the node; the anchor needs archive state. Without the `evm2-jit`
//! feature the harness still runs and reports the INTERPRET rows only.

#![expect(
    clippy::print_stdout,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::doc_markdown
)]

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use alloy::primitives::{Address, Bytes, U256};
use parking_lot::RwLock;

use ::evm2::evm::{AccountInfo, CacheDB, Database, Db};

use degenbot_pools::{v4_pool_id, V4PoolKey};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::alloy_async_db::AlloyAsyncDb;
use degenbot_simulation::sim::evm2::blocking_async_db::BlockingAsyncDb;
use degenbot_simulation::sim::evm2::frame_replay::{
    Evm2CountingFrameDb, Evm2ScratchEvm, FrameInterpreterRunner,
};
use degenbot_simulation::sim::evm2::hook_slots::HookSlotStore;
use degenbot_simulation::sim::evm2::memoized_storage::MemoizedStorage;
use degenbot_simulation::sim::evm2::v4_black_box::{
    leg_state_slots, V4BlackBoxQuoter, V4Leg, V4_POOL_MANAGER, V4_QUOTER,
};
use degenbot_simulation::sim::evm2::warm_code_cache::{WarmCodeCache, WarmCodeCacheInner};
use degenbot_simulation::sim::frame_replay::{FrameRpcCounter, ScratchBlock};
use degenbot_simulation::sim::StorageMemo;

/// The archived block every read pins (the H1 census anchor).
const PIN: u64 = 26_320_000;

/// The H1 census: `hook  currency0  currency1  fee  tick_spacing  pool_id`.
const CENSUS: &str = include_str!("fixtures/v4_hook_census.tsv");

/// The burst length: 1,000 hooked re-quotes (a finalist-rate burst over the
/// census's hooked pools).
const BURST: usize = 1_000;

/// The burst length this run uses — `DEGENBOT_REQUOTE_BENCH_BURST` narrows it
/// for a smoke run; the report always states the count it measured.
fn burst_len() -> usize {
    std::env::var("DEGENBOT_REQUOTE_BENCH_BURST")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value: &usize| *value > 0)
        .unwrap_or(BURST)
}

/// The anchor a burst prices at — the solve's block plus its env.
#[derive(Clone, Copy)]
struct Anchor {
    block: u64,
    timestamp: u64,
    base_fee: u128,
}

/// One census pool, ready to re-quote.
#[derive(Clone)]
struct Pool {
    hook: Address,
    leg: V4Leg,
    amount_in: U256,
}

/// One measured re-quote.
#[derive(Clone, Copy)]
struct Sample {
    us: f64,
    reads: u64,
    ok: bool,
}

/// A distribution over one shape's burst.
struct Report {
    label: &'static str,
    count: usize,
    quoted: usize,
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
    reads_total: u64,
    reads_min: u64,
    reads_max: u64,
}

impl Report {
    fn of(label: &'static str, samples: &[Sample]) -> Self {
        let mut times: Vec<f64> = samples.iter().map(|s| s.us).collect();
        times.sort_by(f64::total_cmp);
        let mut reads: Vec<u64> = samples.iter().map(|s| s.reads).collect();
        reads.sort_unstable();
        Self {
            label,
            count: samples.len(),
            quoted: samples.iter().filter(|s| s.ok).count(),
            p50: percentile(&times, 50.0),
            p95: percentile(&times, 95.0),
            p99: percentile(&times, 99.0),
            max: times.last().copied().unwrap_or(0.0),
            reads_total: reads.iter().sum(),
            reads_min: reads.first().copied().unwrap_or(0),
            reads_max: reads.last().copied().unwrap_or(0),
        }
    }

    fn reads_per_quote(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        self.reads_total as f64 / self.count as f64
    }

    fn print(&self) {
        println!(
            "  {:<22} n={:<5} quoted={:<5} p50 {:<9} p95 {:<9} p99 {:<9} max {:<9} cold reads/quote {:<7} (min {} max {})",
            self.label,
            self.count,
            self.quoted,
            us(self.p50),
            us(self.p95),
            us(self.p99),
            us(self.max),
            format!("{:.2}", self.reads_per_quote()),
            self.reads_min,
            self.reads_max,
        );
    }
}

fn us(value: f64) -> String {
    format!("{value:.1}µs")
}

/// Nearest-rank percentile over an ascending slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn rpc_url() -> String {
    std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .or_else(|_| std::env::var("TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI"))
        .expect(
            "set DEGENBOT_RPC_HTTP_CHAINID_1 (or TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI) \
             to an archive mainnet endpoint",
        )
}

fn census() -> Vec<(Address, V4PoolKey)> {
    let mut rows = Vec::new();
    for line in CENSUS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 6, "census row shape: {line:?}");
        let hook = Address::from_str(fields[0]).expect("hook address");
        let key = V4PoolKey {
            currency0: Address::from_str(fields[1]).expect("currency0"),
            currency1: Address::from_str(fields[2]).expect("currency1"),
            fee: fields[3].parse().expect("fee"),
            tick_spacing: fields[4].parse().expect("tick spacing"),
            hooks: hook,
        };
        let pinned = alloy::primitives::B256::from_str(fields[5]).expect("pool id");
        assert_eq!(v4_pool_id(&key), pinned, "census row integrity: {line:?}");
        rows.push((hook, key));
    }
    rows
}

/// The layered ext the tier quotes over — the production stack
/// (`CacheDB<Db<WarmCodeCache<counting frame DB>>>`).
type TierExt<'a> = CacheDB<Db<WarmCodeCache<Evm2CountingFrameDb<BlockingAsyncDb<'a>>>>>;

/// The same ext with the anchor memo layer in front of the counting fallback.
type MemoExt<'a> =
    CacheDB<Db<WarmCodeCache<MemoizedStorage<Evm2CountingFrameDb<BlockingAsyncDb<'a>>>>>>;

fn scratch_block(anchor: Anchor) -> ScratchBlock {
    ScratchBlock {
        number: anchor.block,
        timestamp: anchor.timestamp,
        base_fee_next: anchor.base_fee,
    }
}

/// The production seam's body, verbatim in shape
/// (`InlineSimHook::quote_leg_at_anchor`): a fresh scratch + quoter per leg,
/// over the shared warm code cache and the durable hook slot store.
fn quote_leg_shipped(
    provider: &AlloyProvider,
    warm: &Arc<RwLock<WarmCodeCacheInner>>,
    store: &Arc<HookSlotStore>,
    anchor: Anchor,
    leg: &V4Leg,
    amount_in: U256,
    runner: Option<FrameInterpreterRunner>,
) -> (Option<U256>, u64) {
    let counter = Arc::new(FrameRpcCounter::default());
    let blocking = BlockingAsyncDb::new(AlloyAsyncDb::new(provider, anchor.block))
        .expect("a multi-threaded runtime is ambient on the blocking pool");
    let cold = Evm2CountingFrameDb::new(blocking, Arc::clone(&counter));
    let warm_db = WarmCodeCache::with_owner(Arc::clone(warm), anchor.block, cold);
    let ext: TierExt<'_> = CacheDB::new(Db::new(warm_db));
    let mut scratch =
        Evm2ScratchEvm::with_counter(ext, scratch_block(anchor), Arc::clone(&counter))
            .with_jit(runner);
    let mut quoter = V4BlackBoxQuoter::default().with_hook_slots(Arc::clone(store));
    let out = quoter
        .quote(&mut scratch, leg, amount_in)
        .ok()
        .map(|quote| quote.amount_out);
    (out, counter.reads())
}

/// The seam AFTER the anchor memo joined it: the same fresh scratch + quoter per
/// leg, but storage reads first hit the block's own memo — the anchor's inline
/// sims and earlier re-quotes already paid for those words.
fn quote_leg_shipped_memo(
    provider: &AlloyProvider,
    warm: &Arc<RwLock<WarmCodeCacheInner>>,
    store: &Arc<HookSlotStore>,
    memo: &Arc<StorageMemo>,
    anchor: Anchor,
    leg: &V4Leg,
    amount_in: U256,
    runner: Option<FrameInterpreterRunner>,
) -> (Option<U256>, u64) {
    let counter = Arc::new(FrameRpcCounter::default());
    let blocking = BlockingAsyncDb::new(AlloyAsyncDb::new(provider, anchor.block))
        .expect("a multi-threaded runtime is ambient on the blocking pool");
    let cold = Evm2CountingFrameDb::new(blocking, Arc::clone(&counter));
    let memoized = MemoizedStorage::new(cold, Arc::clone(memo));
    let warm_db = WarmCodeCache::with_owner(Arc::clone(warm), anchor.block, memoized);
    let mut ext: MemoExt<'_> = CacheDB::new(Db::new(warm_db));
    // The production seam seeds the quote frame's null caller (see
    // `quote_leg_at_anchor`): an immutable row that must not cost an RPC.
    ext.insert_account_info(&Address::ZERO, AccountInfo::default());
    let mut scratch =
        Evm2ScratchEvm::with_counter(ext, scratch_block(anchor), Arc::clone(&counter))
            .with_jit(runner);
    let mut quoter = V4BlackBoxQuoter::default().with_hook_slots(Arc::clone(store));
    let out = quoter
        .quote(&mut scratch, leg, amount_in)
        .ok()
        .map(|quote| quote.amount_out);
    (out, counter.reads())
}

/// The shipped seam's whole latency: the hook's own drive
/// (`block_on(spawn_blocking(..))`) around [`quote_leg_shipped`].
fn burst_shipped(
    rt: &tokio::runtime::Runtime,
    provider: &Arc<AlloyProvider>,
    warm: &Arc<RwLock<WarmCodeCacheInner>>,
    store: &Arc<HookSlotStore>,
    anchor: Anchor,
    pools: &[Pool],
    runner: Option<FrameInterpreterRunner>,
    burst: usize,
) -> Vec<Sample> {
    let mut samples = Vec::with_capacity(burst);
    for i in 0..burst {
        let pool = pools[i % pools.len()].clone();
        let provider = Arc::clone(provider);
        let warm = Arc::clone(warm);
        let store = Arc::clone(store);
        let runner = runner.clone();
        let started = Instant::now();
        let (out, reads) = rt.block_on(async move {
            tokio::task::spawn_blocking(move || {
                quote_leg_shipped(
                    &provider,
                    &warm,
                    &store,
                    anchor,
                    &pool.leg,
                    pool.amount_in,
                    runner,
                )
            })
            .await
            .expect("the quote task joins")
        });
        samples.push(Sample {
            us: started.elapsed().as_secs_f64() * 1e6,
            reads,
            ok: out.is_some(),
        });
    }
    samples
}

/// The census setup: each pool's liquidity word (the amount basis), and the
/// direction the Quoter can actually price. Runs on the blocking pool (the
/// layered DB needs an ambient multi-threaded runtime).
fn setup_pools(
    provider: &AlloyProvider,
    warm: &Arc<RwLock<WarmCodeCacheInner>>,
    anchor: Anchor,
) -> (Vec<Pool>, usize) {
    let counter = Arc::new(FrameRpcCounter::default());
    let blocking =
        BlockingAsyncDb::new(AlloyAsyncDb::new(provider, anchor.block)).expect("runtime");
    let cold = Evm2CountingFrameDb::new(blocking, Arc::clone(&counter));
    let warm_db = WarmCodeCache::with_owner(Arc::clone(warm), anchor.block, cold);
    let ext: TierExt<'_> = CacheDB::new(Db::new(warm_db));
    let mut setup = Evm2ScratchEvm::with_counter(ext, scratch_block(anchor), Arc::clone(&counter));

    let mut pools: Vec<Pool> = Vec::new();
    let mut skipped = 0usize;
    for (hook, key) in census() {
        let slots = leg_state_slots(&key);
        let slot0 = setup
            .ext_mut()
            .get_storage(&V4_POOL_MANAGER, &slots[0])
            .unwrap_or(U256::ZERO);
        let liquidity = setup
            .ext_mut()
            .get_storage(&V4_POOL_MANAGER, &slots[1])
            .unwrap_or(U256::ZERO);
        if liquidity.is_zero() || slot0.is_zero() {
            skipped += 1;
            continue;
        }
        let amount_in = (liquidity / U256::from(1_000_000u64)).max(U256::ONE);
        let mut selected = None;
        for zero_for_one in [true, false] {
            let leg = V4Leg::canonical(key.clone(), zero_for_one);
            if V4BlackBoxQuoter::default()
                .quote(&mut setup, &leg, amount_in)
                .is_ok()
            {
                selected = Some(leg);
                break;
            }
        }
        match selected {
            Some(leg) => pools.push(Pool {
                hook,
                leg,
                amount_in,
            }),
            None => skipped += 1,
        }
    }
    (pools, skipped)
}

/// The post-fix seam's burst: one fresh scratch per quote, the block memo
/// shared across the whole burst (exactly what the production tier does now).
fn burst_shipped_memo(
    rt: &tokio::runtime::Runtime,
    provider: &Arc<AlloyProvider>,
    warm: &Arc<RwLock<WarmCodeCacheInner>>,
    store: &Arc<HookSlotStore>,
    memo: &Arc<StorageMemo>,
    anchor: Anchor,
    pools: &[Pool],
    runner: Option<FrameInterpreterRunner>,
    burst: usize,
) -> Vec<Sample> {
    let mut samples = Vec::with_capacity(burst);
    for i in 0..burst {
        let pool = pools[i % pools.len()].clone();
        let provider = Arc::clone(provider);
        let warm = Arc::clone(warm);
        let store = Arc::clone(store);
        let memo = Arc::clone(memo);
        let runner = runner.clone();
        let started = Instant::now();
        let (out, reads) = rt.block_on(async move {
            tokio::task::spawn_blocking(move || {
                quote_leg_shipped_memo(
                    &provider,
                    &warm,
                    &store,
                    &memo,
                    anchor,
                    &pool.leg,
                    pool.amount_in,
                    runner,
                )
            })
            .await
            .expect("the quote task joins")
        });
        samples.push(Sample {
            us: started.elapsed().as_secs_f64() * 1e6,
            reads,
            ok: out.is_some(),
        });
    }
    samples
}

/// The per-anchor-scratch shape: one scratch + one quoter for the whole burst.
#[cfg(feature = "evm2-jit")]
fn burst_per_anchor_scratch(
    rt: &tokio::runtime::Runtime,
    provider: &Arc<AlloyProvider>,
    warm: &Arc<RwLock<WarmCodeCacheInner>>,
    store: &Arc<HookSlotStore>,
    anchor: Anchor,
    pools: &[Pool],
    runner: Option<FrameInterpreterRunner>,
    warm_passes: usize,
    burst: usize,
) -> Vec<Sample> {
    let provider = Arc::clone(provider);
    let warm = Arc::clone(warm);
    let store = Arc::clone(store);
    let pools = pools.to_vec();
    rt.block_on(async move {
        tokio::task::spawn_blocking(move || {
            let counter = Arc::new(FrameRpcCounter::default());
            let blocking =
                BlockingAsyncDb::new(AlloyAsyncDb::new(&provider, anchor.block)).expect("runtime");
            let cold = Evm2CountingFrameDb::new(blocking, Arc::clone(&counter));
            let warm_db = WarmCodeCache::with_owner(Arc::clone(&warm), anchor.block, cold);
            let ext: TierExt<'_> = CacheDB::new(Db::new(warm_db));
            let mut scratch =
                Evm2ScratchEvm::with_counter(ext, scratch_block(anchor), Arc::clone(&counter))
                    .with_jit(runner);
            let mut quoter = V4BlackBoxQuoter::default().with_hook_slots(Arc::clone(&store));
            // The untimed warm passes: the FIRST quote of each pool pays that
            // pool's whole cold read set into this scratch's memo.
            for i in 0..(warm_passes * burst) {
                let pool = &pools[i % pools.len()];
                let _ = quoter.quote(&mut scratch, &pool.leg, pool.amount_in);
            }
            let mut samples = Vec::with_capacity(burst);
            for i in 0..burst {
                let pool = &pools[i % pools.len()];
                let before = counter.reads();
                let started = Instant::now();
                let ok = quoter
                    .quote(&mut scratch, &pool.leg, pool.amount_in)
                    .is_ok();
                let elapsed = started.elapsed();
                samples.push(Sample {
                    us: elapsed.as_secs_f64() * 1e6,
                    reads: counter.reads() - before,
                    ok,
                });
            }
            samples
        })
        .await
        .expect("the burst task joins")
    })
}

/// The AOT prep lane (the `aot_prepare_executor` pattern) over a scratch store:
/// every address's bytecode compiled into `dir`, returning the wall cost and
/// the per-contract breakdown.
#[cfg(feature = "evm2-jit")]
fn prep_aot(dir: &std::path::Path, codes: &[(Address, Bytes)]) -> AotPrep {
    use ::evm2_jit::runtime::{AotRequest, JitBackend, ProgramKind, RuntimeConfig};
    use degenbot_simulation::sim::evm2::aot_store::DurableArtifactStore;
    use degenbot_simulation::sim::evm2::handle::EVM2_PRODUCTION_SPEC;

    // The production worker-pool mode (`blocking: false`), with promotion
    // disabled: the prepped store IS the resident set, so nothing compiles on
    // the hot path and the interpret-vs-AOT comparison is execution-only.
    let store = DurableArtifactStore::new(dir).expect("the scratch artifact store opens");
    let backend = JitBackend::new(RuntimeConfig {
        enabled: true,
        blocking: false,
        store: Some(Arc::new(store)),
        tuning: ::evm2_jit::runtime::RuntimeTuning {
            jit_hot_threshold: usize::MAX,
            ..::evm2_jit::runtime::RuntimeTuning::default()
        },
        ..RuntimeConfig::default()
    })
    .expect("the LLVM backend initializes");

    let started = Instant::now();
    let mut per_contract = Vec::with_capacity(codes.len());
    let mut prepared = 0usize;
    for (address, code) in codes {
        let t = Instant::now();
        let code_hash = alloy::primitives::keccak256(code);
        let result = backend.prepare_aot_sync(AotRequest {
            code_hash,
            code: code.clone(),
            spec_id: EVM2_PRODUCTION_SPEC,
        });
        let kind = backend.get_compiled(code_hash, EVM2_PRODUCTION_SPEC);
        let ok = result.is_ok() && kind.as_ref().is_some_and(|p| p.kind == ProgramKind::Aot);
        if ok {
            prepared += 1;
        }
        per_contract.push((*address, t.elapsed().as_secs_f64() * 1e3, ok));
    }
    AotPrep {
        backend,
        total_ms: started.elapsed().as_secs_f64() * 1e3,
        prepared,
        per_contract,
    }
}

/// Reads the PRODUCTION durable store's index (read-only) and reports, per
/// contract a re-quote executes, whether an artifact for its code hash exists.
#[cfg(feature = "evm2-jit")]
fn print_aot_coverage(
    codes: &[(Address, Bytes)],
    code_hashes: &HashMap<Address, alloy::primitives::B256>,
    pools: &[Pool],
) {
    use ::evm2_jit::runtime::ArtifactStore as _;
    use degenbot_simulation::sim::evm2::aot_store::DurableArtifactStore;

    let dir = degenbot_simulation::sim::evm2::aot_store::default_artifact_dir();
    let store = DurableArtifactStore::new(&dir).expect("the production store opens");
    let artifacts = store.load_all().expect("the production store lists");
    println!("  dir: {}", dir.display());
    println!("  artifacts: {}", artifacts.len());
    for (address, code) in codes {
        let hash = code_hashes[address];
        let present = artifacts
            .iter()
            .any(|(key, _)| key.runtime.code_hash == hash);
        let label = if *address == V4_POOL_MANAGER {
            "PoolManager (singleton)".to_string()
        } else if *address == V4_QUOTER {
            "V4 Quoter".to_string()
        } else {
            let count = pools.iter().filter(|p| p.hook == *address).count();
            format!("hook ({count} census pools)")
        };
        println!(
            "  0x{}  {:<26} code {:>6} B  hash 0x{}…  AOT {}",
            alloy::hex::encode(&address.as_slice()[..4]),
            label,
            code.len(),
            &format!("{hash:x}")[..16],
            if present { "PRESENT" } else { "absent" },
        );
    }
}

#[cfg(feature = "evm2-jit")]
struct AotPrep {
    backend: ::evm2_jit::runtime::JitBackend,
    total_ms: f64,
    prepared: usize,
    per_contract: Vec<(Address, f64, bool)>,
}

#[cfg(feature = "evm2-jit")]
fn runner_of(backend: &::evm2_jit::runtime::JitBackend) -> FrameInterpreterRunner {
    Arc::new(::evm2_jit::evm2_evm::JitInterpreterRunner::new(
        backend.clone(),
    ))
}

#[test]
#[ignore = "live-RPC benchmark over archive mainnet state; run with --ignored --nocapture"]
fn requote_latency_envelope() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("the bench runtime builds");

    let provider = Arc::new(
        rt.block_on(async { AlloyProvider::new(&rpc_url(), 3).await })
            .expect("the node answers"),
    );
    let chain_id = rt
        .block_on(async { provider.get_chain_id().await })
        .unwrap();
    assert_eq!(chain_id, 1, "the census pools are mainnet fixtures");
    let block = rt
        .block_on(async { provider.get_block(PIN).await })
        .unwrap()
        .expect("the anchor block is archived");
    let anchor = Anchor {
        block: PIN,
        timestamp: block.header.timestamp,
        base_fee: u128::from(block.header.base_fee_per_gas.unwrap_or(0)),
    };

    println!("=== ADR-090: hooked-leg re-quote latency envelope ===");
    println!(
        "anchor {PIN} (timestamp {}, base fee {} wei/gas)",
        anchor.timestamp, anchor.base_fee
    );
    let burst = burst_len();
    println!("burst: {burst} re-quotes, rotating the H1 census's hooked pools");
    println!(
        "production JIT mode: worker pool (blocking=false), hot threshold = \
         simulation.pipeline_concurrency (default 8) lookups"
    );

    // ── setup: the census pool set, its state, a quoting direction ───────
    let warm = WarmCodeCacheInner::shared_with_hook_slots(Arc::new(HookSlotStore::in_memory(
        degenbot_simulation::sim::evm2::handle::EVM2_PRODUCTION_SPEC,
    )));
    let store = warm
        .read()
        .hook_slots()
        .expect("the store is installed on the shared cache");

    let (pools, skipped) = {
        let provider = Arc::clone(&provider);
        let warm = Arc::clone(&warm);
        rt.block_on(async move {
            tokio::task::spawn_blocking(move || setup_pools(&provider, &warm, anchor))
                .await
                .expect("the setup task joins")
        })
    };
    let hooks: Vec<Address> = {
        let mut seen: Vec<Address> = pools.iter().map(|p| p.hook).collect();
        seen.sort();
        seen.dedup();
        seen
    };
    println!(
        "census: {} pools across {} hooks quoted ({} skipped: no liquidity / no quotable direction)",
        pools.len(),
        hooks.len(),
        skipped
    );

    // ── the contracts a re-quote executes, and their AOT coverage ────────
    let mut executed: Vec<Address> = vec![V4_POOL_MANAGER, V4_QUOTER];
    executed.extend(hooks.iter().copied());
    for pool in &pools {
        executed.push(pool.leg.pool_key.currency0);
        executed.push(pool.leg.pool_key.currency1);
    }
    executed.sort();
    executed.dedup();

    let mut codes: Vec<(Address, Bytes)> = Vec::with_capacity(executed.len());
    for address in executed {
        let code = rt.block_on(async { provider.get_code(&address, Some(PIN)).await.unwrap() });
        codes.push((address, code));
    }
    let mut code_hashes: HashMap<Address, alloy::primitives::B256> = HashMap::new();
    for (address, code) in &codes {
        code_hashes.insert(*address, alloy::primitives::keccak256(code));
    }

    // The memo layer must not change an answer. Every census leg is quoted
    // twice — through the anchor memo and without it — and the two figures must
    // agree to the wei. (The memo only ever holds the fallback's own answers.)
    {
        let provider = Arc::clone(&provider);
        let warm = Arc::clone(&warm);
        let store = Arc::clone(&store);
        let pools = pools.clone();
        let (checked, differences) = rt.block_on(async move {
            tokio::task::spawn_blocking(move || {
                let memo = Arc::new(StorageMemo::new());
                let mut checked = 0usize;
                let mut differences = Vec::new();
                for pool in &pools {
                    let plain = quote_leg_shipped(
                        &provider,
                        &warm,
                        &store,
                        anchor,
                        &pool.leg,
                        pool.amount_in,
                        None,
                    )
                    .0;
                    let memoized = quote_leg_shipped_memo(
                        &provider,
                        &warm,
                        &store,
                        &memo,
                        anchor,
                        &pool.leg,
                        pool.amount_in,
                        None,
                    )
                    .0;
                    if plain.is_some() {
                        checked += 1;
                    }
                    if plain != memoized {
                        differences.push((
                            pool.hook,
                            plain.map(|v| v.to_string()),
                            memoized.map(|v| v.to_string()),
                        ));
                    }
                }
                (checked, differences)
            })
            .await
            .expect("the parity task joins")
        });
        println!(
            "memo parity: {checked} leg(s) quoted both ways, {} difference(s)",
            differences.len()
        );
        assert!(
            differences.is_empty(),
            "the anchor memo changed an answer: {differences:?}"
        );
    }

    println!();
    println!("AOT coverage — the production store (<persistence.state_dir>/aot-artifacts):");
    #[cfg(feature = "evm2-jit")]
    print_aot_coverage(&codes, &code_hashes, &pools);
    #[cfg(not(feature = "evm2-jit"))]
    println!("  (evm2-jit off: no artifact store in this build)");

    // ── the burst: cold pass + measured pass, per shape ─────────────
    println!();
    println!("the {burst}-quote burst (each row: an untimed cold pass, then the measured pass):");

    // A cold boot's first re-quotes: no AOT, no learned slot sets, cold code.
    let cold_interp = burst_shipped(&rt, &provider, &warm, &store, anchor, &pools, None, burst);
    let shipped_interp = burst_shipped(&rt, &provider, &warm, &store, anchor, &pools, None, burst);
    Report::of("pre-memo, interpret", &shipped_interp).print();

    // The post-fix seam — a fresh scratch per quote, the block's memo shared.
    let interp_memo = Arc::new(StorageMemo::new());
    let cold_memo = burst_shipped_memo(
        &rt,
        &provider,
        &warm,
        &store,
        &interp_memo,
        anchor,
        &pools,
        None,
        burst,
    );
    let measured_memo = burst_shipped_memo(
        &rt,
        &provider,
        &warm,
        &store,
        &interp_memo,
        anchor,
        &pools,
        None,
        burst,
    );
    Report::of("memo, interpret", &measured_memo).print();
    report_cold_tail("memo, interpret (cold boot)", &cold_memo, &pools, &hooks);

    #[cfg(feature = "evm2-jit")]
    {
        let store_dir = std::env::temp_dir().join(format!("evm2-hook-requote-aot-{PIN}"));
        let _ = std::fs::remove_dir_all(&store_dir);
        let prepared = codes.len();
        let prep = prep_aot(&store_dir, &codes);
        println!(
            "  AOT prep: {}/{} contract(s) in {:.0} ms (singletons {} ms; slowest contract {:.0} ms)",
            prep.prepared,
            prepared,
            prep.total_ms,
            prep.per_contract
                .iter()
                .filter(|(address, _, _)| *address == V4_POOL_MANAGER || *address == V4_QUOTER)
                .map(|(_, ms, _)| format!("{ms:.0}"))
                .collect::<Vec<_>>()
                .join("+"),
            prep.per_contract
                .iter()
                .map(|(_, ms, _)| *ms)
                .fold(0.0_f64, f64::max),
        );
        if prep.prepared != prepared {
            println!(
                "  prep failures: {:?}",
                prep.per_contract
                    .iter()
                    .filter(|(_, _, ok)| !ok)
                    .map(|(address, _, _)| format!("0x{}", alloy::hex::encode(address)))
                    .collect::<Vec<_>>()
            );
        }

        let runner = Some(runner_of(&prep.backend));
        // The boot's first re-quotes with the store prepped, then steady state.
        let cold_aot = burst_shipped(
            &rt,
            &provider,
            &warm,
            &store,
            anchor,
            &pools,
            runner.clone(),
            burst,
        );
        let shipped_aot = burst_shipped(
            &rt,
            &provider,
            &warm,
            &store,
            anchor,
            &pools,
            runner.clone(),
            burst,
        );
        Report::of("pre-memo + AOT", &shipped_aot).print();

        // The production shape: AOT + the block's anchor memo.
        let aot_memo = Arc::new(StorageMemo::new());
        let cold_aot_memo = burst_shipped_memo(
            &rt,
            &provider,
            &warm,
            &store,
            &aot_memo,
            anchor,
            &pools,
            runner.clone(),
            burst,
        );
        let measured_aot_memo = burst_shipped_memo(
            &rt,
            &provider,
            &warm,
            &store,
            &aot_memo,
            anchor,
            &pools,
            runner.clone(),
            burst,
        );
        Report::of("memo + AOT (production)", &measured_aot_memo).print();

        // One untimed warm pass for the per-anchor shape (its first quote of
        // each pool pays that pool's cold set; the measured pass reads the
        // scratch memo). Two passes so every census pool is touched.
        let reused = burst_per_anchor_scratch(
            &rt, &provider, &warm, &store, anchor, &pools, runner, 2, burst,
        );
        Report::of("per-anchor scratch + AOT", &reused).print();

        let stats = prep.backend.stats();
        println!(
            "  AOT backend after the row: resident {} | lookup hits {} misses {} | \
             compilations dispatched {} succeeded {} failed {} (promotion disabled)",
            stats.resident_entries,
            stats.lookup_hits,
            stats.lookup_misses,
            stats.compilations_dispatched,
            stats.compilations_succeeded,
            stats.compilations_failed,
        );

        for (label, samples) in [
            ("pre-memo + AOT, cold boot", &cold_aot),
            ("memo + AOT, the boot's first re-quotes", &cold_aot_memo),
        ] {
            println!();
            report_cold_tail(label, samples, &pools, &hooks);
        }
    }
    #[cfg(not(feature = "evm2-jit"))]
    {
        println!("  (evm2-jit off: AOT rows unavailable - every frame interprets)");
        let _ = &codes;
        let _ = &code_hashes;
    }

    println!();
    report_cold_tail(
        "shipped, interpret (cold boot)",
        &cold_interp,
        &pools,
        &hooks,
    );
}

/// The cold tail the percentiles hide: each hook's FIRST re-quote in a pass
/// (cold code, unlearned slot set), outlined per hook.
fn report_cold_tail(label: &str, samples: &[Sample], pools: &[Pool], hooks: &[Address]) {
    // The bursts rotate the pool list, so a hook's first appearance is its
    // first quote.
    let mut firsts: Vec<(Address, f64, u64)> = Vec::new();
    for hook in hooks {
        if let Some(index) = pools.iter().position(|p| p.hook == *hook) {
            if let Some(sample) = samples.get(index) {
                firsts.push((*hook, sample.us, sample.reads));
            }
        }
    }
    let mut times: Vec<f64> = firsts.iter().map(|(_, value, _)| *value).collect();
    times.sort_by(f64::total_cmp);
    let reads: Vec<u64> = firsts.iter().map(|(_, _, r)| *r).collect();
    println!(
        "cold tail - {label}: p50 {} p95 {} max {} | cold reads min {} max {}",
        us(percentile(&times, 50.0)),
        us(percentile(&times, 95.0)),
        us(times.last().copied().unwrap_or(0.0)),
        reads.iter().min().copied().unwrap_or(0),
        reads.iter().max().copied().unwrap_or(0),
    );
    for (hook, first_us, read_count) in &firsts {
        println!(
            "  0x{}  first {:<11} cold reads {}",
            alloy::hex::encode(&hook.as_slice()[..4]),
            us(*first_us),
            read_count
        );
    }
}
