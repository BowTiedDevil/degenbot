//! The idle-block retention measurement: the delta-chain snapshot driven over
//! both committed real write-coverage captures and one synthetic long run, at
//! the unbounded control and at thresholds 1, 2, 4, and 8 blocks, reporting
//! what retention saves (live entries, memory) and what it costs (carried-hit
//! loss, extra dynamic fetches, advance/compaction cost). No timing is
//! asserted anywhere: the numbers are the report.
//!
//! # What is driven
//!
//! Per driver and per retention setting: every block of the driver's stretch
//! advances at its exact hash with its real (or synthetic-real) parent linkage
//! and its exact write identities, and the candidate read stream is a
//! deterministic script. The real captures run the shared read-heavy script
//! ([`read_selected`] — one bit per `(block, identity)`, so most identities
//! are re-read within any few-block window); the synthetic long run runs a
//! hot/warm/BAL-only stream (a hot ring read every block, a warm ring every
//! eighth block, and a never-read tail) — the two shapes bracket the policy's
//! price: what a threshold costs a stream that re-reads inside its window
//! versus one that straddles it, and that dropping never-read entries is free
//! at every threshold. The unbounded delta ([`Retention::Unbounded`]) is the
//! CONTROL every threshold is compared against.
//!
//! # Correctness leg, and the negative probe
//!
//! Before measuring, every run replays with verification on: EVERY identity at
//! EVERY anchor is compared against an INDEPENDENT COLD READ of the chain —
//! through the provider's batch primitive, a wire shape the delta's serial
//! dynamic fallback never uses — and any served value that differs from the
//! chain's word there is a hard failure (nonzero exit).
//! `BAL_RETENTION_PLANT_DIVERGENCE=1` arms a one-wei skew on the prototype's
//! serial storage answers (the `eth_getStorageAt` singles only its dynamic
//! fallback produces), and the same comparison must CATCH the planted
//! divergence and exit nonzero, proving the comparison has teeth.
//!
//! # Knobs
//!
//! `BAL_RETENTION_THRESHOLDS` (default `1,2,4,8`) is the threshold matrix —
//! the unbounded control is always measured beside it;
//! `BAL_RETENTION_SYNTH_BLOCKS` (default `300`), `BAL_RETENTION_SYNTH_KEYS`
//! (default `2000`), and `BAL_RETENTION_SYNTH_WAVE` (default `4`) size the
//! synthetic long run (each identity is written in one wave of that many
//! blocks and never again — the transient-activity shape retention is for);
//! `BAL_RETENTION_PLANT_DIVERGENCE` is the negative probe. Measurements land
//! in `logs/bal_retention_result.md`.

#![expect(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a benchmark probe prints its report and fails loudly on a broken fixture"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use alloy::primitives::{Address, U256};
use alloy::rpc::json_rpc::{RequestPacket, Response, ResponsePacket};
use alloy::transports::{TransportError, TransportFut};

use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::delta_snapshot::{
    DeltaSnapshotCache, Retention, DEFAULT_MAX_DEPTH,
};
use degenbot_simulation::sim::ReadAnchor;

#[path = "shared/modeled_chain.rs"]
#[expect(dead_code)]
mod modeled_chain;
#[path = "shared/real_coverage.rs"]
#[expect(dead_code)]
mod real_coverage;

use modeled_chain::{block_hash, read_selected, write_selected, ChainState, Coverage, StubChain};
use real_coverage::{RealBlock, RealCoverage};

/// The one account whose slots form the synthetic run's identities.
const ADDRESS: Address = Address::new([0x21; 20]);

/// Counting allocations around each measured run — the evidence of what the
/// chain holds as entries expire.
struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

// SAFETY: the counter delegates every operation to the system allocator and
// only observes sizes, never pointer contents or lifetimes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// One observed allocation.
fn record(size: usize) {
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    ALLOC_BYTES.fetch_add(u64::try_from(size).unwrap_or(u64::MAX), Ordering::Relaxed);
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The allocation counter's current byte total.
fn allocated_bytes() -> u64 {
    ALLOC_BYTES.load(Ordering::Relaxed)
}

/// The process's resident bytes — the coarse memory evidence beside the
/// allocation counter.
fn rss_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|text| text.split_whitespace().nth(1).map(str::to_string))
        .and_then(|pages| pages.parse::<u64>().ok())
        .map_or(0, |pages| pages.saturating_mul(4096))
}

/// The benchmark node behind one run: the modeled chain, per-shape request
/// counters, and the optional planted skew on the serial answers.
///
/// The two `eth_getStorageAt` shapes are counted apart on purpose: SINGLES are
/// the delta's dynamic fallback, BATCH MEMBERS are the independent cold reads
/// — so "dynamic fetches" is a wire fact, and the negative probe can skew one
/// shape without touching the other.
#[derive(Clone)]
struct BenchChain {
    inner: StubChain,
    singles: Arc<AtomicU64>,
    batched: Arc<AtomicU64>,
    armed: Arc<AtomicBool>,
}

impl BenchChain {
    fn new(chain: ChainState, armed: bool) -> Self {
        Self {
            inner: StubChain {
                chain: Arc::new(chain),
                plant: Arc::new(AtomicBool::new(false)),
            },
            singles: Arc::new(AtomicU64::new(0)),
            batched: Arc::new(AtomicU64::new(0)),
            armed: Arc::new(AtomicBool::new(armed)),
        }
    }

    fn provider(&self) -> AlloyProvider {
        let client = alloy::rpc::client::ClientBuilder::default().transport(self.clone(), true);
        let root: alloy::providers::RootProvider = alloy::providers::RootProvider::new(client);
        AlloyProvider::from_provider(Arc::new(root))
    }

    /// Zero the shape counters at a run's start.
    fn reset(&self) {
        self.singles.store(0, Ordering::Relaxed);
        self.batched.store(0, Ordering::Relaxed);
    }

    /// `(dynamic fetches, cold batch members)` since the last reset.
    fn counts(&self) -> (u64, u64) {
        (
            self.singles.load(Ordering::Relaxed),
            self.batched.load(Ordering::Relaxed),
        )
    }
}

impl tower::Service<RequestPacket> for BenchChain {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        let serial_storage =
            matches!(&req, RequestPacket::Single(one) if one.method() == "eth_getStorageAt");
        let batch_storage = matches!(&req, RequestPacket::Batch(many) if many.iter().any(|one| one.method() == "eth_getStorageAt"));
        if serial_storage {
            self.singles.fetch_add(1, Ordering::Relaxed);
        } else if batch_storage {
            self.batched.fetch_add(1, Ordering::Relaxed);
        }
        let mut inner = self.inner.clone();
        let armed = Arc::clone(&self.armed);
        let pending = inner.call(req);
        Box::pin(async move {
            let mut packet = pending.await?;
            if armed.load(Ordering::Relaxed) && serial_storage {
                if let ResponsePacket::Single(response) = &mut packet {
                    skew_response(response);
                }
            }
            Ok(packet)
        })
    }
}

/// The planted one-wei skew on one answered word.
fn skew_response(response: &mut Response) {
    if let alloy::rpc::json_rpc::ResponsePayload::Success(raw) = &response.payload {
        let Ok(serde_json::Value::String(text)) =
            serde_json::from_str::<serde_json::Value>(raw.get())
        else {
            return;
        };
        let Ok(word) = text.parse::<U256>() else {
            return;
        };
        let skewed = format!("{:#x}", word + U256::from(1u8));
        response.payload = alloy::rpc::json_rpc::ResponsePayload::Success(
            serde_json::value::to_raw_value(&serde_json::Value::String(skewed))
                .expect("a skewed answer serializes"),
        );
    }
}

/// One run's measurements.
#[derive(Default)]
struct Sample {
    reads: usize,
    fetches: u64,
    live_max: usize,
    live_final: usize,
    held_final: usize,
    alloc_bytes: u64,
    rss_after: u64,
    advance_ns: Vec<u64>,
    compactions: usize,
    compaction_ns: u64,
    dropped: usize,
}

impl Sample {
    /// Reads answered without a dynamic fetch.
    fn hits(&self) -> usize {
        self.reads
            .saturating_sub(usize::try_from(self.fetches).unwrap_or(usize::MAX))
    }
}

/// Nanoseconds as an integer, from a measured wall.
fn nanos(wall: std::time::Duration) -> u64 {
    u64::try_from(wall.as_nanos()).expect("the wall fits")
}

/// One sorted sample's value at a rational quantile.
fn percentile(sorted: &[u64], numerator: usize, denominator: usize) -> u64 {
    let index = sorted.len().saturating_sub(1) * numerator / denominator;
    sorted.get(index).copied().unwrap_or(0)
}

/// Drive one retention setting over one driver's stretch: advance every block
/// at its exact hash, run the candidate read script, and — when `verify` —
/// compare every served value at every anchor against an independent cold
/// read. Returns the run's measurements and every mismatch (empty is the
/// only passing verdict).
async fn drive(
    chain: &BenchChain,
    real: &RealCoverage,
    retention: Retention,
    verify: bool,
    reads: fn(u64, Address, U256) -> bool,
) -> (Sample, Vec<String>) {
    let provider = chain.provider();
    let cache =
        DeltaSnapshotCache::with_max_depth_and_retention(&provider, DEFAULT_MAX_DEPTH, retention);
    let keys: BTreeSet<(Address, U256)> = (real.first()..=real.last())
        .flat_map(|number| real.writes_at(number).iter().copied())
        .collect();
    let key_vec: Vec<(Address, U256)> = keys.iter().copied().collect();
    let relevance: Vec<Address> = keys.iter().map(|(address, _)| *address).collect();
    let mut sample = Sample::default();
    let mut mismatches = Vec::new();
    chain.reset();
    let alloc_before = allocated_bytes();

    for number in real.first()..=real.last() {
        let anchor =
            ReadAnchor::identified(number, real.hash(number).expect("the driver names hashes"));
        let hub = if number == real.first() {
            cache.for_anchor(anchor)
        } else {
            let started = Instant::now();
            let (hub, report) = cache
                .advance_bal(
                    anchor,
                    real.parent_hash(number).expect("the driver links"),
                    &relevance,
                )
                .await;
            sample.advance_ns.push(nanos(started.elapsed()));
            if let Some(compaction) = report.compaction {
                sample.compactions = sample.compactions.saturating_add(1);
                sample.compaction_ns = sample
                    .compaction_ns
                    .saturating_add(nanos(compaction.elapsed));
                sample.dropped = sample.dropped.saturating_add(compaction.dropped);
            }
            hub
        };
        sample.live_max = sample.live_max.max(hub.live_len());
        for &(address, slot) in &key_vec {
            if reads(number, address, slot) {
                let served = hub
                    .read_storage(address, slot)
                    .await
                    .expect("the modeled chain answers");
                sample.reads = sample.reads.saturating_add(1);
                if verify {
                    let cold = cold_read(&provider, address, slot, number).await;
                    if served != cold {
                        mismatches.push(format!(
                            "block {number} {address}/{slot}: read {served} vs cold {cold}"
                        ));
                    }
                }
            }
        }
        if verify {
            let cold = provider
                .get_storage_batch_results_at(&key_vec, number)
                .await
                .expect("the cold batch answers");
            for (index, &(address, slot)) in key_vec.iter().enumerate() {
                let cold = cold[index].clone().expect("the cold member answers");
                if let Some(served) = hub.snapshot_value(address, slot) {
                    if served != cold {
                        mismatches.push(format!(
                            "block {number} {address}/{slot}: snapshot {served} vs cold {cold}"
                        ));
                    }
                }
            }
        }
        sample.live_final = hub.live_len();
        sample.held_final = hub.snapshot_len();
    }

    sample.alloc_bytes = allocated_bytes().saturating_sub(alloc_before);
    sample.rss_after = rss_bytes();
    let (fetches, _cold_members) = chain.counts();
    sample.fetches = fetches;
    (sample, mismatches)
}

/// The synthetic run's candidate stream: the hot ring (one identity in
/// sixteen) is read EVERY block, the warm ring (the next three in sixteen)
/// once every eight blocks, and the rest is pure BAL-only — written and never
/// read. The rings make the policy's price visible per stream shape: what a
/// threshold does to re-reads INSIDE its window versus ones that straddle it,
/// and that dropping never-read entries is free at every threshold.
fn synth_read(number: u64, _address: Address, slot: U256) -> bool {
    let ring = slot.as_limbs()[0] % 16;
    match ring {
        0 => true,
        1..=3 => number % 8 == ring,
        _ => false,
    }
}

/// One independent cold read of the chain's word at an anchor — the batch
/// primitive, a wire shape the delta's serial fallback never uses.
async fn cold_read(provider: &AlloyProvider, address: Address, slot: U256, number: u64) -> U256 {
    let cold = provider
        .get_storage_batch_results_at(&[(address, slot)], number)
        .await
        .expect("the cold batch answers");
    cold[0].clone().expect("the cold member answers")
}

/// The synthetic long run: `keys` identities of one account, each written in
/// one wave of `wave` consecutive blocks and never again, at the exact hashes
/// of a linked synthetic chain.
fn synthetic_driver(first: u64, blocks: u64, keys: u64, wave: u64) -> RealCoverage {
    let wave = wave.max(1);
    let span = blocks.saturating_sub(wave).max(1);
    let mut captured = Vec::new();
    for number in first..first + blocks {
        let mut writes = Vec::new();
        for key in 0..keys {
            let slot = U256::from(key);
            let birth = first + (key * 37) % span;
            if number >= birth && number < birth + wave && write_selected(number, ADDRESS, slot) {
                writes.push((ADDRESS, slot));
            }
        }
        writes.sort_unstable();
        captured.push(RealBlock {
            number,
            hash: block_hash(number),
            parent_hash: block_hash(number.saturating_sub(1)),
            writes,
            other_writes: 0,
        });
    }
    RealCoverage::from_blocks(vec![ADDRESS], captured).expect("the synthetic capture folds")
}

/// Print one driver's report table across the measured settings.
fn report_driver(name: &str, columns: &[(String, Sample)]) {
    println!("## {name}");
    let header: Vec<String> = columns.iter().map(|(label, _)| label.clone()).collect();
    println!("| metric | {} |", header.join(" | "));
    println!("| {} |", vec!["---"; columns.len() + 1].join(" | "));
    let row = |name: &str, pick: &dyn Fn(&Sample) -> String| {
        let cells: Vec<String> = columns.iter().map(|(_, sample)| pick(sample)).collect();
        println!("| {name} | {} |", cells.join(" | "));
    };
    let control = columns.first().map(|(_, sample)| sample);
    row("candidate reads", &|s| s.reads.to_string());
    row("dynamic fetches", &|s| s.fetches.to_string());
    row("carried hits (reads - fetches)", &|s| s.hits().to_string());
    row("carried-hit loss vs unbounded", &|s| {
        control.map_or_else(String::new, |c| {
            c.hits().saturating_sub(s.hits()).to_string()
        })
    });
    row("extra fetches vs unbounded", &|s| {
        control.map_or_else(String::new, |c| {
            s.fetches.saturating_sub(c.fetches).to_string()
        })
    });
    row("live entries max", &|s| s.live_max.to_string());
    row("live entries final", &|s| s.live_final.to_string());
    row("held entries final", &|s| s.held_final.to_string());
    row("run allocations (KiB)", &|s| {
        (s.alloc_bytes / 1024).to_string()
    });
    row("rss after run (KiB)", &|s| (s.rss_after / 1024).to_string());
    row("advance wall total (ms)", &|s| {
        (s.advance_ns.iter().sum::<u64>() / 1_000_000).to_string()
    });
    row("advance wall p50 (us)", &|s| {
        let mut walls = s.advance_ns.clone();
        walls.sort_unstable();
        (percentile(&walls, 1, 2) / 1_000).to_string()
    });
    row("advance wall p95 (us)", &|s| {
        let mut walls = s.advance_ns.clone();
        walls.sort_unstable();
        (percentile(&walls, 19, 20) / 1_000).to_string()
    });
    row("compaction folds", &|s| s.compactions.to_string());
    row("compaction wall total (ms)", &|s| {
        (s.compaction_ns / 1_000_000).to_string()
    });
    row("entries dropped by folds", &|s| s.dropped.to_string());
    println!();
}

/// One driver measured across the unbounded control and every threshold.
async fn measure_driver(
    name: &str,
    driver: &RealCoverage,
    armed: bool,
    thresholds: &[u64],
    verify: bool,
    reads: fn(u64, Address, U256) -> bool,
) -> Vec<String> {
    let mut columns = Vec::new();
    let mut mismatches = Vec::new();
    let mut settings: Vec<(String, Retention)> =
        vec![("unbounded".to_string(), Retention::Unbounded)];
    for &idle in thresholds {
        settings.push((format!("T={idle}"), Retention::IdleBlocks(idle)));
    }
    for (label, retention) in settings {
        let keys: BTreeSet<(Address, U256)> = (driver.first()..=driver.last())
            .flat_map(|number| driver.writes_at(number).iter().copied())
            .collect();
        let chain = ChainState::build_coverage(
            &keys,
            driver.first(),
            driver.last(),
            driver,
            Coverage::Real,
        )
        .expect("a chain over the driver's stretch");
        let bench = BenchChain::new(chain, armed);
        let (sample, run_mismatches) = drive(&bench, driver, retention, verify, reads).await;
        columns.push((label, sample));
        mismatches.extend(run_mismatches);
    }
    report_driver(name, &columns);
    mismatches
}

#[tokio::main]
async fn main() {
    let planted = std::env::var("BAL_RETENTION_PLANT_DIVERGENCE").is_ok_and(|value| value == "1");
    let thresholds: Vec<u64> = std::env::var("BAL_RETENTION_THRESHOLDS")
        .unwrap_or_else(|_| "1,2,4,8".to_string())
        .split(',')
        .map(|part| part.trim().parse().expect("a threshold is a number"))
        .collect();
    let synth_blocks = std::env::var("BAL_RETENTION_SYNTH_BLOCKS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(300);
    let synth_keys = std::env::var("BAL_RETENTION_SYNTH_KEYS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2_000);
    let synth_wave = std::env::var("BAL_RETENTION_SYNTH_WAVE")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4);
    println!(
        "thresholds {thresholds:?} | synthetic {synth_blocks} blocks x {synth_keys} keys, wave {synth_wave} | max_depth {DEFAULT_MAX_DEPTH}"
    );

    // The negative probe: one verified run with the skew armed must be CAUGHT.
    if planted {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/bal_real_coverage.json"
        );
        let real = RealCoverage::load(fixture).expect("the committed capture parses");
        let keys: BTreeSet<(Address, U256)> = (real.first()..=real.last())
            .flat_map(|number| real.writes_at(number).iter().copied())
            .collect();
        let chain =
            ChainState::build_coverage(&keys, real.first(), real.last(), &real, Coverage::Real)
                .expect("a chain over the captured stretch");
        let bench = BenchChain::new(chain, true);
        let (_, mismatches) =
            drive(&bench, &real, Retention::IdleBlocks(2), true, read_selected).await;
        assert!(
            !mismatches.is_empty(),
            "the planted one-wei divergence must be caught"
        );
        println!(
            "negative probe: the cold-read comparison caught the planted divergence ({} mismatches)",
            mismatches.len()
        );
        std::process::exit(1);
    }

    // The correctness leg and the real-fixture measurements: both committed
    // captures, verified against independent cold reads at every anchor.
    for fixture in [
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/bal_real_coverage.json"
        ),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/bal_real_coverage_broader.json"
        ),
    ] {
        let real = RealCoverage::load(fixture).expect("the committed capture parses");
        let mismatches =
            measure_driver(fixture, &real, false, &thresholds, true, read_selected).await;
        assert!(
            mismatches.is_empty(),
            "{}: {} mismatches vs cold reads: {:?}",
            fixture,
            mismatches.len(),
            mismatches
        );
        println!("{fixture}: zero mismatches vs independent cold reads");
    }

    // The synthetic long run: verified spot-check first, then the measured
    // matrix (verification's cold reads would pollute the run's allocations).
    let synth = synthetic_driver(1_000, synth_blocks, synth_keys, synth_wave);
    let mismatches = measure_driver(
        "synthetic long run (verified spot-check)",
        &synth,
        false,
        &[2],
        true,
        synth_read,
    )
    .await;
    assert!(
        mismatches.is_empty(),
        "synthetic: {} mismatches vs cold reads: {:?}",
        mismatches.len(),
        mismatches
    );
    println!("synthetic long run: zero mismatches vs independent cold reads");
    measure_driver(
        "synthetic long run (measured)",
        &synth,
        false,
        &thresholds,
        false,
        synth_read,
    )
    .await;
}
