//! The full-clone versus delta-chain snapshot measurement: the same BAL
//! carry-forward advances driven through the production full-clone control
//! ([`ReadHubCache::advance_bal`]) and the parent-linked delta-chain prototype
//! ([`DeltaSnapshotCache::advance_bal`]) at realistic working-set sizes, with
//! advance latency, single-thread lookup, compaction cost, walk depth, and
//! RSS/allocation evidence reported as p50/p95 over repeated runs.
//!
//! # What is driven
//!
//! Per size N and per repetition: one parent generation carrying N settled
//! entries (its list carries N final writes, relevance admitting them all),
//! then `max_depth + 2` linked child advances whose lists each carry one final
//! write over the working set. The control clones its whole N-entry map on
//! EVERY child advance; the delta chain links a small delta and pays one
//! compaction fold when the walk bound is reached (the sequence's last
//! advance — its cost is reported apart). Lookups are hits over the working
//! set at the final generation, single-threaded, through each representation's
//! own read seam. No lock-contention or concurrent-reader claim is measured
//! here, and no timing is asserted: the numbers are the report.
//!
//! # Correctness leg, and the negative probe
//!
//! Before measuring, the committed real write-coverage capture
//! (`tests/fixtures/bal_real_coverage.json`) replays through BOTH
//! representations via [`delta_parity`]; any value or report mismatch is a
//! hard failure (nonzero exit). `BAL_DELTA_PLANT_DIVERGENCE=1` arms a one-wei
//! skew on the prototype's serial storage answers (the single
//! `eth_getStorageAt` shape only its dynamic fallback uses — the control reads
//! storage in batches), and the same replay must CATCH the planted divergence
//! and exit nonzero, proving the comparison has teeth.
//!
//! # Knobs
//!
//! `BAL_DELTA_SIZES` (default `2500,25000,100000,1000000`) is the working-set
//! matrix; `BAL_DELTA_REPS` (default `11`) the repetitions; `BAL_DELTA_LOOKUPS`
//! (default `10000`) the per-rep lookup count; `BAL_DELTA_MAX_DEPTH` (default
//! `2`) the compaction threshold — kept low so one fold lands inside a few
//! generations even at 1m entries; `BAL_DELTA_PLANT_DIVERGENCE` the negative
//! probe. Measurements land in `logs/bal_delta_snapshot_result.md`.

#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    reason = "a benchmark probe prints its report and fails loudly on a broken fixture"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Instant;

use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::ClientBuilder;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};
use serde_json::value::to_raw_value;
use serde_json::Value;

use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::delta_snapshot::DeltaSnapshotCache;
use degenbot_simulation::sim::evm2::read_hub::ReadHubCache;
use degenbot_simulation::sim::ReadAnchor;

#[path = "shared/delta_parity.rs"]
mod delta_parity;
#[path = "shared/modeled_chain.rs"]
#[expect(dead_code)]
mod modeled_chain;
#[path = "shared/real_coverage.rs"]
#[expect(dead_code)]
mod real_coverage;

use modeled_chain::{bal_bytes, ChainState, Coverage, StubChain};
use real_coverage::RealCoverage;

/// The one account whose N slots form the measured working set.
const ADDRESS: Address = Address::new([0x11; 20]);

/// The anchor height the parent generation carries at.
const HEIGHT: u64 = 100;

/// Counting allocations around each measured advance — the evidence a delta
/// child never copies its parent's map.
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

/// The allocation counter's current totals.
fn allocations() -> (u64, u64) {
    (
        ALLOCATIONS.load(Ordering::Relaxed),
        ALLOC_BYTES.load(Ordering::Relaxed),
    )
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

/// The benchmark node: the raw list at each exact hash, and one constant
/// storage word (every measured read is a snapshot hit and never reaches it).
#[derive(Debug, Clone, Default)]
struct BenchStub {
    bals: Arc<Mutex<HashMap<B256, Vec<u8>>>>,
}

impl BenchStub {
    fn serve_bal_at(&self, hash: B256, bytes: Vec<u8>) {
        self.bals
            .lock()
            .expect("the list table locks")
            .insert(hash, bytes);
    }

    fn provider(&self) -> AlloyProvider {
        let client = ClientBuilder::default().transport(self.clone(), true);
        let root: RootProvider = RootProvider::new(client);
        AlloyProvider::from_provider(Arc::new(root))
    }

    fn answer(&self, req: &SerializedRequest) -> Response {
        let payload = match req.method() {
            "debug_getRawBlockAccessList" => {
                let params: Value =
                    serde_json::from_str(req.params().expect("a BAL request carries params").get())
                        .expect("BAL params are JSON");
                let hash: B256 = params
                    .get(0)
                    .and_then(Value::as_str)
                    .expect("the block hash is the first param")
                    .parse()
                    .expect("the block hash parses");
                match self.bals.lock().expect("the list table locks").get(&hash) {
                    Some(raw) => ResponsePayload::Success(
                        to_raw_value(&Bytes::copy_from_slice(raw)).expect("bytes serialize"),
                    ),
                    None => ResponsePayload::Failure(ErrorPayload {
                        code: -32_000,
                        message: "no block access list at that hash".into(),
                        data: None,
                    }),
                }
            }
            "eth_getStorageAt" => ResponsePayload::Success(
                to_raw_value(&Value::String("0x1".to_string())).expect("a word serializes"),
            ),
            other => panic!("the benchmark reads no {other}"),
        };
        Response {
            id: req.id().clone(),
            payload,
        }
    }
}

impl tower::Service<RequestPacket> for BenchStub {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        let stub = self.clone();
        Box::pin(async move {
            match req {
                RequestPacket::Single(one) => Ok(ResponsePacket::Single(stub.answer(&one))),
                RequestPacket::Batch(many) => Ok(ResponsePacket::Batch(
                    many.iter().map(|one| stub.answer(one)).collect(),
                )),
            }
        })
    }
}

/// A transport wrapper that skews the prototype's serial storage answers by
/// one wei when armed — the planted divergence the negative probe must catch.
/// The control reads storage in JSON-RPC batches, so only the prototype's
/// serial fallback is ever skewed.
#[derive(Clone)]
struct SkewChain {
    inner: StubChain,
    armed: Arc<AtomicBool>,
}

impl SkewChain {
    fn provider(&self) -> AlloyProvider {
        let client = ClientBuilder::default().transport(self.clone(), true);
        let root: RootProvider = RootProvider::new(client);
        AlloyProvider::from_provider(Arc::new(root))
    }
}

impl tower::Service<RequestPacket> for SkewChain {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        let serial_storage =
            matches!(&req, RequestPacket::Single(one) if one.method() == "eth_getStorageAt");
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
    if let ResponsePayload::Success(raw) = &response.payload {
        let Ok(Value::String(text)) = serde_json::from_str::<Value>(raw.get()) else {
            return;
        };
        let Ok(word) = text.parse::<U256>() else {
            return;
        };
        let skewed = format!("{:#x}", word + U256::from(1u8));
        response.payload = ResponsePayload::Success(
            to_raw_value(&Value::String(skewed)).expect("a skewed answer serializes"),
        );
    }
}

/// The parent's N final writes: slot `i` holds word `i + 1`.
fn parent_writes(size: usize) -> Vec<(Address, U256, U256)> {
    (0..size)
        .map(|index| {
            (
                ADDRESS,
                U256::from(index),
                U256::from(u64::try_from(index).expect("the size fits")) + U256::from(1u8),
            )
        })
        .collect()
}

/// One child generation's single final write over the working set.
fn child_writes(step: u64) -> Vec<(Address, U256, U256)> {
    vec![(ADDRESS, U256::ZERO, U256::from(1_000 + step))]
}

/// The block hash generation `step` is keyed by.
fn block_hash(step: u64) -> B256 {
    B256::from(U256::from(step + 1).to_be_bytes::<32>())
}

/// The deterministic lookup key over the working set.
fn lookup_slot(index: usize, size: usize) -> U256 {
    U256::from(u64::try_from(index.wrapping_mul(2_654_435_761) % size).expect("the slot fits"))
}

/// One repetition's measurements, in nanoseconds unless noted.
#[derive(Default)]
struct Sample {
    control_advance_ns: u64,
    control_clone_ns: u64,
    control_alloc_bytes: u64,
    control_lookup_ns: u64,
    control_rss_parent: u64,
    control_rss_child: u64,
    delta_advance_ns: u64,
    delta_link_ns: u64,
    delta_alloc_bytes: u64,
    delta_lookup_ns: u64,
    delta_rss_parent: u64,
    delta_rss_child: u64,
    compact_advance_ns: u64,
    compact_ns: u64,
    compact_entries: usize,
    max_walk: u32,
}

/// The per-size matrix's knobs.
struct Knobs {
    reps: usize,
    lookups: usize,
    max_depth: u32,
}

/// Nanoseconds as an integer, from a measured wall.
fn nanos(wall: std::time::Duration) -> u64 {
    u64::try_from(wall.as_nanos()).expect("the wall fits")
}

/// Drive one repetition through the full-clone control.
async fn control_rep(provider: &AlloyProvider, knobs: &Knobs, size: usize, sample: &mut Sample) {
    let control = ReadHubCache::new(provider);
    control
        .advance_bal(
            ReadAnchor::identified(HEIGHT, block_hash(0)),
            B256::ZERO,
            &[ADDRESS],
        )
        .await;
    sample.control_rss_parent = rss_bytes();
    let (_, alloc_before) = allocations();
    let started = Instant::now();
    let (mut hub, report) = control
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, block_hash(1)),
            block_hash(0),
            &[ADDRESS],
        )
        .await;
    sample.control_advance_ns = nanos(started.elapsed());
    sample.control_clone_ns = nanos(report.clone_time);
    let (_, alloc_after) = allocations();
    sample.control_alloc_bytes = alloc_after.saturating_sub(alloc_before);
    sample.control_rss_child = rss_bytes();
    for step in 2..=u64::from(knobs.max_depth) + 2 {
        let (next, _report) = control
            .advance_bal(
                ReadAnchor::identified(HEIGHT + step, block_hash(step)),
                block_hash(step - 1),
                &[ADDRESS],
            )
            .await;
        hub = next;
    }
    let lookup_started = Instant::now();
    for lookup in 0..knobs.lookups {
        let value = hub
            .read_storage(ADDRESS, lookup_slot(lookup, size))
            .await
            .expect("a memo hit answers");
        assert!(value > U256::ZERO, "the working set holds real words");
    }
    sample.control_lookup_ns = nanos(lookup_started.elapsed());
}

/// Drive one repetition through the delta chain.
async fn delta_rep(provider: &AlloyProvider, knobs: &Knobs, size: usize, sample: &mut Sample) {
    let delta = DeltaSnapshotCache::with_max_depth(provider, knobs.max_depth);
    delta
        .advance_bal(
            ReadAnchor::identified(HEIGHT, block_hash(0)),
            B256::ZERO,
            &[ADDRESS],
        )
        .await;
    sample.delta_rss_parent = rss_bytes();
    let (_, alloc_before) = allocations();
    let started = Instant::now();
    let (mut hub, report) = delta
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, block_hash(1)),
            block_hash(0),
            &[ADDRESS],
        )
        .await;
    sample.delta_advance_ns = nanos(started.elapsed());
    sample.delta_link_ns = nanos(report.link_time);
    let (_, alloc_after) = allocations();
    sample.delta_alloc_bytes = alloc_after.saturating_sub(alloc_before);
    sample.delta_rss_child = rss_bytes();
    let last_step = u64::from(knobs.max_depth) + 2;
    for step in 2..=last_step {
        let started = Instant::now();
        let (next, report) = delta
            .advance_bal(
                ReadAnchor::identified(HEIGHT + step, block_hash(step)),
                block_hash(step - 1),
                &[ADDRESS],
            )
            .await;
        if step == last_step {
            sample.compact_advance_ns = nanos(started.elapsed());
            let compaction = report.compaction.expect("the last link folds the chain");
            sample.compact_ns = nanos(compaction.elapsed);
            sample.compact_entries = compaction.entries;
        }
        hub = next;
    }
    sample.max_walk = hub.chain_depth();
    let lookup_started = Instant::now();
    for lookup in 0..knobs.lookups {
        let value = hub
            .read_storage(ADDRESS, lookup_slot(lookup, size))
            .await
            .expect("a chain hit answers");
        assert!(value > U256::ZERO, "the working set holds real words");
    }
    sample.delta_lookup_ns = nanos(lookup_started.elapsed());
}

/// One sorted sample's value at a rational quantile.
fn percentile(sorted: &[u64], numerator: usize, denominator: usize) -> u64 {
    let index = sorted.len().saturating_sub(1) * numerator / denominator;
    sorted.get(index).copied().unwrap_or(0)
}

/// Print one time metric's p50/p95 row, in microseconds.
fn time_row(name: &str, samples: &[Sample], pick: impl Fn(&Sample) -> u64) {
    let mut values: Vec<u64> = samples.iter().map(pick).collect();
    values.sort_unstable();
    println!(
        "| {name} (us) | {:.1} | {:.1} |",
        f64::from(u32::try_from(percentile(&values, 1, 2) / 1_000).expect("the wall fits")),
        f64::from(u32::try_from(percentile(&values, 19, 20) / 1_000).expect("the wall fits"))
    );
}

/// Print one byte metric's p50 row, in KiB.
fn byte_row(name: &str, samples: &[Sample], pick: impl Fn(&Sample) -> u64) {
    let mut values: Vec<u64> = samples.iter().map(pick).collect();
    values.sort_unstable();
    println!(
        "| {name} (KiB) | {:.1} |",
        f64::from(u32::try_from(percentile(&values, 1, 2) / 1_024).expect("the size fits"))
    );
}

/// Print one size's report table.
fn report_size(size: usize, samples: &[Sample]) {
    println!("## working set {size} entries");
    println!("| metric | p50 | p95 |");
    println!("| --- | --- | --- |");
    time_row("control advance wall", samples, |s| s.control_advance_ns);
    time_row("control clone", samples, |s| s.control_clone_ns);
    time_row("delta advance wall (link)", samples, |s| s.delta_advance_ns);
    time_row("delta link", samples, |s| s.delta_link_ns);
    time_row("delta advance wall (compacting)", samples, |s| {
        s.compact_advance_ns
    });
    time_row("delta compaction fold", samples, |s| s.compact_ns);
    time_row("control lookup", samples, |s| s.control_lookup_ns);
    time_row("delta lookup", samples, |s| s.delta_lookup_ns);
    println!("| metric | p50 | |");
    byte_row("control advance allocations", samples, |s| {
        s.control_alloc_bytes
    });
    byte_row("delta advance allocations", samples, |s| {
        s.delta_alloc_bytes
    });
    byte_row("control rss after parent", samples, |s| {
        s.control_rss_parent
    });
    byte_row("delta rss after parent", samples, |s| s.delta_rss_parent);
    byte_row("control rss after child", samples, |s| s.control_rss_child);
    byte_row("delta rss after child", samples, |s| s.delta_rss_child);
    let entries = samples.iter().map(|s| s.compact_entries).max().unwrap_or(0);
    let walk = samples.iter().map(|s| s.max_walk).max().unwrap_or(0);
    println!("| compaction fold entries | {entries} | |");
    println!("| max walk (node maps below own) | {walk} | |");
}

/// Run the size matrix and print the report.
async fn benchmark(knobs: &Knobs, sizes: &[usize]) {
    for &size in sizes {
        let stub = BenchStub::default();
        stub.serve_bal_at(block_hash(0), bal_bytes(&parent_writes(size)));
        for step in 1..=u64::from(knobs.max_depth) + 2 {
            stub.serve_bal_at(block_hash(step), bal_bytes(&child_writes(step)));
        }
        let provider = stub.provider();
        let mut samples = Vec::new();
        for _ in 0..knobs.reps {
            let mut sample = Sample::default();
            control_rep(&provider, knobs, size, &mut sample).await;
            delta_rep(&provider, knobs, size, &mut sample).await;
            samples.push(sample);
        }
        report_size(size, &samples);
    }
}

/// The correctness leg: the committed real capture replayed through both
/// representations, with the negative probe's skew armed or not.
async fn correctness_leg(planted: bool) -> Vec<String> {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/bal_real_coverage.json"
    );
    let real = RealCoverage::load(fixture).expect("the committed capture parses");
    let keys = delta_parity::real_keys(&real);
    let chain = ChainState::build_coverage(&keys, real.first(), real.last(), &real, Coverage::Real)
        .expect("a chain over the captured stretch");
    let stub = SkewChain {
        inner: StubChain {
            chain: Arc::new(chain),
            plant: Arc::new(AtomicBool::new(false)),
        },
        armed: Arc::new(AtomicBool::new(planted)),
    };
    let blocks = delta_parity::real_parity_blocks(&real, &keys);
    let result = delta_parity::replay(&stub.provider(), &blocks).await;
    println!(
        "real-fixture replay: {} generations, {} comparisons, {} mismatches",
        result.generations,
        result.comparisons,
        result.mismatches.len()
    );
    result.mismatches
}

#[tokio::main]
async fn main() {
    let planted = std::env::var("BAL_DELTA_PLANT_DIVERGENCE").is_ok_and(|value| value == "1");
    let mismatches = correctness_leg(planted).await;
    if planted {
        assert!(
            !mismatches.is_empty(),
            "the planted one-wei divergence must be caught"
        );
        println!("negative probe: the parity replay caught the planted divergence");
        std::process::exit(1);
    }
    assert!(
        mismatches.is_empty(),
        "the real-fixture replay must agree on every value: {mismatches:?}"
    );
    let sizes: Vec<usize> = std::env::var("BAL_DELTA_SIZES")
        .unwrap_or_else(|_| "2500,25000,100000,1000000".to_string())
        .split(',')
        .map(|part| part.trim().parse().expect("a size is a number"))
        .collect();
    let knobs = Knobs {
        reps: std::env::var("BAL_DELTA_REPS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(11),
        lookups: std::env::var("BAL_DELTA_LOOKUPS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(10_000),
        max_depth: std::env::var("BAL_DELTA_MAX_DEPTH")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(2),
    };
    println!(
        "sizes {:?} | reps {} | lookups {} | max_depth {}",
        sizes, knobs.reps, knobs.lookups, knobs.max_depth
    );
    benchmark(&knobs, &sizes).await;
}
