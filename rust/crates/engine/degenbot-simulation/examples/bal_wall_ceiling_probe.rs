#![expect(
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "a benchmark probe prints its report and fails loudly on a broken fixture"
)]

//! The deterministic wall-clock ceiling benchmark of BAL carry-forward: the
//! recorded production fan-out read walks replayed under three cache policies
//! — cold per anchor, cold plus exact-anchor BAL write seeding, and
//! parent-linked carry-forward — with candidate wall time attributed to six
//! buckets and the eliminable storage-wait share computed per latency point.
//!
//! # What is driven
//!
//! The corpus is the recorded `fanout-trace` stream (444 candidates, 14 anchors
//! over the blocks 26375033..=26375053): per candidate, the read identities the
//! simulation seam drained and the wall it measured (`sim_ns`). Every leg
//! drives the SAME candidates in the same order through the same session
//! scheduler ([`session_drive`]: W lanes, F slots, one shared hub, one drain
//! role) under the same modeled RPC prices, and spends each candidate's
//! recorded execution as wall. The legs differ ONLY in the hub they are handed:
//!
//! - `cold` — a new empty hub per candidate anchor; every read takes the
//!   ordinary batch path;
//! - `seed` — a new empty hub per candidate anchor seeded with that block's
//!   exact final writes (the production fetch/decode/seed pipeline, timed);
//! - `carry` — one [`ReadHubCache`] advanced block by block through EVERY
//!   intermediate block of the range ([`ReadHubCache::advance_bal`]), an
//!   unbroken parent chain carrying the settled working set forward with the
//!   MEV-relevant new writes only (each anchor's relevance set is its own
//!   recorded candidate addresses).
//!
//! # The modeled chain, and what this cannot prove
//!
//! The node is the stub transport in [`modeled_chain`]: one deterministic
//! chain whose blocks write a partial, per-identity-selected set of the
//! corpus's identities, with values derived from the SAME chain the modeled
//! lists carry. Every candidate's read-value digest must therefore be
//! identical across the three legs — the cross-leg digest gate proves the
//! policies replay the same reads and that a carry never serves a stale parent
//! value. What the replay does NOT establish is full EVM verdict parity: the
//! corpus carries no calldata, no opcode runs, and the blocks' real write
//! coverage is modeled (the corpus records reads only). Value drift and verdict
//! parity are the live probe's (`bal_carry_probe`) and the broader-corpus
//! experiment's questions.
//!
//! # Coverage modes, and the real-coverage fixture
//!
//! The write coverage itself is the replay's one swapped variable: every run
//! reports `synthetic` (the modeled write bit, ~half the tracked identities per
//! block) beside `real` — the EXACT write identities of the 21 real block
//! access lists, captured once from the live node into the dev-only fixture
//! ([`real_coverage`]: format, regeneration command) and replayed offline at
//! the blocks' real hashes with their real parent linkage. Candidate order,
//! recorded `sim_ns`, W/F, the price points, and READ VALUES are identical
//! across the modes — the shared value chain drifts only where both coverages
//! write (see [`modeled_chain`]), so the comparison isolates write coverage and
//! the cross-mode digest gate proves the values stayed put. The real mode's
//! per-anchor storage/hit accounting is cross-checked against the live
//! `bal_carry_probe`'s counts and fails loud on any discrepancy.
//!
//! # Wall-time attribution
//!
//! Every modeled wait, every spent execution, and every measured phase (list
//! fetch/decode, snapshot clone/update) is one span in the leg's
//! [`wall_ledger::WallLedger`]. [`wall_ledger::reconcile`] partitions the
//! leg's measured wall into the six buckets — recorded execution, storage
//! wait, account wait, list fetch/decode, snapshot clone/update, and
//! scheduler/other — charging every wall moment once (concurrency resolved by
//! bucket precedence, the resolved double occupancy reported as overlap), so
//! the buckets sum to the measured wall and no policy's work is charged to
//! another's bucket. Timing is reported, never asserted.
//!
//! # The ceiling
//!
//! The eliminable storage-wait ceiling is the storage wait the best policy
//! removes from the cold leg, absolute and as a share of the cold leg's wall —
//! a ceiling because realized wall improvement can never exceed the wait the
//! schedule stops paying. It is reported at the 200/400us and 700/1200us price
//! points with W=2/F=8, beside each leg's bucket split, time to first
//! candidate, p50/p95 completion, round/member counts, and snapshot-hit
//! classification.
//!
//! # Gates
//!
//! The cross-leg read-digest gate must be GREEN in every coverage mode: every
//! leg, every latency point, every recorded candidate — and the cross-mode gate
//! must agree on the values (the cold baselines' digests are identical across
//! the modes). `BAL_WALL_PLANT_DIVERGENCE=1` answers storage one wei high
//! during the carry leg only, which must turn the gate RED and the process exit
//! nonzero. `BAL_WALL_PLANT_STORAGE_DELAY_US=500` plants an extra storage delay
//! and runs the first latency point unplanted and planted, reporting the bucket
//! and ceiling deltas (the planted delay lands only in the storage-wait
//! bucket).
//!
//! # Usage
//!
//! ```sh
//! just bal-wall-ceiling-probe                     # both modes x both price points, unplanted
//! BAL_WALL_COVERAGE=real just bal-wall-ceiling-probe    # one mode (synthetic | real | both)
//! BAL_WALL_TRACE=/tmp/fanout_trace.log \
//! BAL_WALL_STORAGE_US=700 BAL_WALL_ACCOUNT_US=1200 \
//! BAL_WALL_W=2 BAL_WALL_F=8 just bal-wall-ceiling-probe
//! BAL_WALL_PLANT_STORAGE_DELAY_US=500 just bal-wall-ceiling-probe   # first point, with deltas
//! BAL_WALL_PLANT_DIVERGENCE=1 just bal-wall-ceiling-probe           # exits 1
//! just bal-real-coverage-capture                  # regenerate the fixture (live node, dev-only)
//! ```
//!
//! `BAL_WALL_FIXTURE` relocates the fixture (default
//! `tests/fixtures/bal_real_coverage.json` in this crate); `BAL_WALL_DEBUG`
//! traces lane progress.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};

use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::async_fanout::{DrainPolicy, FanoutLimits};
use degenbot_simulation::sim::evm2::bal_prefetch::{decode_bal_seed, BalPlan};
use degenbot_simulation::sim::evm2::read_hub::{CarryOutcome, CarryReport, ReadHub, ReadHubCache};
use degenbot_simulation::sim::ReadAnchor;

#[path = "shared/modeled_chain.rs"]
#[expect(dead_code)]
mod modeled_chain;
#[path = "shared/probe_report.rs"]
mod probe_report;
#[path = "shared/real_coverage.rs"]
mod real_coverage;
#[path = "shared/recorded_corpus.rs"]
#[expect(dead_code)]
mod recorded_corpus;
#[path = "shared/session_drive.rs"]
mod session_drive;
#[path = "shared/wall_ledger.rs"]
#[expect(dead_code)]
mod wall_ledger;

use modeled_chain::{write_selected, ChainState, Coverage, Key, StubChain};
use probe_report::{
    classify_hits, eliminable_ceiling, identity_gate, storage_accounting, CeilingReport, HitSplit,
    StorageAccounting,
};
use real_coverage::RealCoverage;
use recorded_corpus::{
    anchor_keys, candidate_addresses, group_by_anchor, load_rows, read_call_counts, universe,
    Recorded, DEFAULT_TRACE,
};
use session_drive::{drive_lanes, LanePlan, LegFacts};
use wall_ledger::{
    completion_stats, reconcile, CompletionStats, Digests, Node, WallBucket, WallLedger, WallReport,
};

// ── Configuration ─────────────────────────────────────────────────────────

/// The optimistic model's prices.
const OPTIMISTIC: (u64, u64) = (200, 400);

/// The pessimistic model's prices.
const PESSIMISTIC: (u64, u64) = (700, 1200);

/// The fixture's default home: the committed compact capture of the corpus
/// stretch (the raw list responses are never stored).
const DEFAULT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/bal_real_coverage.json"
);

/// The live `bal_carry_probe` accounting this corpus produced — the real
/// mode's cross-check. Any discrepancy is a failing gate, never a hidden one.
const LIVE_ANCHOR_KEYS: usize = 5_938;
const LIVE_CARRIED_HITS: usize = 3_261;
const LIVE_FINAL_WRITE_HITS: usize = 136;
const LIVE_DYNAMIC_MEMBERS: usize = 2_541;

fn env_us(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_micros)
}

fn env_count(name: &str, fallback: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|count| *count > 0)
        .unwrap_or(fallback)
}

/// What one invocation drives.
struct Config {
    trace: String,
    /// The real write-coverage fixture the `real` mode and the shared value
    /// chain are built over.
    fixture: String,
    /// Set by the dev-only capture phase: write the fixture from the live node
    /// and exit without replaying.
    capture: bool,
    /// The coverage modes to report, in report order.
    modes: Vec<(&'static str, Coverage)>,
    /// The `(storage, account)` latency points to report.
    points: Vec<(Duration, Duration)>,
    limits: FanoutLimits,
    /// The negative probe's planted extra storage delay.
    plant_storage: Duration,
    /// Some(flag) when the divergence plant is armed: raised for the carry leg
    /// only, and the matrix narrows to the first point.
    plant: Option<Arc<AtomicBool>>,
}

/// One replay mode's block identities: its coverage's hashes and the shared
/// fixture.
struct ModeChain<'a> {
    label: &'static str,
    coverage: Coverage,
    real: &'a RealCoverage,
}

impl ModeChain<'_> {
    fn hash(&self, number: u64) -> Result<B256, String> {
        self.coverage.hash(self.real, number)
    }

    fn parent_hash(&self, number: u64) -> Result<B256, String> {
        self.coverage.parent_hash(self.real, number)
    }
}

impl Config {
    /// Fold the invocation's env knobs into one configuration.
    ///
    /// # Errors
    ///
    /// A `BAL_WALL_COVERAGE` value that names no coverage mode is a broken
    /// invocation, never a silent default.
    fn from_env() -> Result<Self, String> {
        let storage = env_us("BAL_WALL_STORAGE_US");
        let account = env_us("BAL_WALL_ACCOUNT_US");
        let points = match (storage, account) {
            (Some(storage), Some(account)) => vec![(storage, account)],
            (Some(storage), None) => vec![(storage, Duration::from_micros(OPTIMISTIC.1))],
            (None, Some(account)) => vec![(Duration::from_micros(OPTIMISTIC.0), account)],
            (None, None) => vec![
                (
                    Duration::from_micros(OPTIMISTIC.0),
                    Duration::from_micros(OPTIMISTIC.1),
                ),
                (
                    Duration::from_micros(PESSIMISTIC.0),
                    Duration::from_micros(PESSIMISTIC.1),
                ),
            ],
        };
        let modes = match std::env::var("BAL_WALL_COVERAGE") {
            Ok(value) if value == "synthetic" => vec![("synthetic", Coverage::Synthetic)],
            Ok(value) if value == "real" => vec![("real", Coverage::Real)],
            Ok(value) if value == "both" || value.is_empty() => {
                vec![("synthetic", Coverage::Synthetic), ("real", Coverage::Real)]
            }
            Ok(value) => {
                return Err(format!(
                    "BAL_WALL_COVERAGE={value}: expected synthetic, real, or both"
                ));
            }
            Err(_) => vec![("synthetic", Coverage::Synthetic), ("real", Coverage::Real)],
        };
        Ok(Self {
            trace: std::env::var("BAL_WALL_TRACE").unwrap_or_else(|_| DEFAULT_TRACE.into()),
            fixture: std::env::var("BAL_WALL_FIXTURE").unwrap_or_else(|_| DEFAULT_FIXTURE.into()),
            capture: std::env::var("BAL_WALL_FIXTURE_CAPTURE").is_ok(),
            modes,
            points,
            limits: FanoutLimits::new(env_count("BAL_WALL_W", 2), env_count("BAL_WALL_F", 8)),
            plant_storage: env_us("BAL_WALL_PLANT_STORAGE_DELAY_US").unwrap_or(Duration::ZERO),
            plant: std::env::var("BAL_WALL_PLANT_DIVERGENCE")
                .is_ok()
                .then(|| Arc::new(AtomicBool::new(false))),
        })
    }
}

/// Whether `BAL_WALL_DEBUG` asks for the lane progress trace.
fn debug_enabled() -> bool {
    std::env::var("BAL_WALL_DEBUG").is_ok()
}

// ── One leg's report ──────────────────────────────────────────────────────

/// What one policy's leg measured at one latency point.
struct LegSummary {
    label: &'static str,
    /// The leg's measured wall.
    wall_ns: u64,
    /// The reconciled bucket attribution.
    wall: WallReport,
    /// Time to first candidate and the completion percentiles.
    completions: CompletionStats,
    /// The drives' own measured wall, summed over anchors: the bucket
    /// attribution's independent cross-check.
    drives_wall_ns: u64,
    lanes: usize,
    slots: Option<usize>,
    peak_active: usize,
    rounds: u64,
    storage_members: u64,
    account_members: u64,
    coalesced: u64,
    memo_hits: u64,
    hits: HitSplit,
    /// Final writes applied to the snapshot (carry) or seeded (seed).
    writes_applied: u64,
    /// Final writes left unknown: outside the relevance set (carry).
    writes_skipped: u64,
    /// Settled parent words cloned across the chain (carry).
    carried_values: u64,
    /// Block access lists fetched (seed: one per anchor; carry: one per block).
    bal_fetches: u64,
    /// The distinct-key per-anchor storage accounting (the live probe's shape).
    storage: StorageAccounting,
    digests: Digests,
}

impl LegSummary {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            wall_ns: 0,
            wall: WallReport::default(),
            completions: CompletionStats::default(),
            drives_wall_ns: 0,
            lanes: 0,
            slots: None,
            peak_active: 0,
            rounds: 0,
            storage_members: 0,
            account_members: 0,
            coalesced: 0,
            memo_hits: 0,
            hits: HitSplit::default(),
            writes_applied: 0,
            writes_skipped: 0,
            carried_values: 0,
            bal_fetches: 0,
            storage: StorageAccounting::default(),
            digests: Digests::new(),
        }
    }

    /// Fold one anchor's drive into the leg.
    fn fold(&mut self, facts: &LegFacts, hits: HitSplit, storage: StorageAccounting) {
        assert_eq!(
            facts.label, self.label,
            "a drive's label names the leg it belongs to"
        );
        self.drives_wall_ns = self
            .drives_wall_ns
            .saturating_add(u64::try_from(facts.wall.as_nanos()).unwrap_or(u64::MAX));
        self.lanes = self.lanes.max(facts.lanes);
        self.slots = facts.slots.or(self.slots);
        self.peak_active = self.peak_active.max(facts.peak_active);
        self.rounds += facts.rounds;
        self.storage_members += facts.storage_fetches;
        self.account_members += facts.account_fetches;
        self.coalesced += facts.coalesced;
        self.memo_hits += facts.memo_hits;
        self.hits.add(hits);
        self.storage.add(storage);
        self.digests.extend(facts.digests.clone());
    }

    /// Close the leg: reconcile its ledger and summarize its completions.
    fn finish(mut self, ledger: &WallLedger) -> Result<Self, String> {
        self.wall_ns = ledger.offset();
        self.wall = reconcile(&ledger.spans(), self.wall_ns)?;
        self.completions = completion_stats(ledger.completions())?;
        Ok(self)
    }
}

/// The three legs of one latency point.
struct PointRun {
    storage: Duration,
    account: Duration,
    plant_storage: Duration,
    cold: LegSummary,
    seed: LegSummary,
    carry: LegSummary,
    ceiling: CeilingReport,
}

// ── The legs ──────────────────────────────────────────────────────────────

fn leg_plan(config: &Config) -> LanePlan {
    LanePlan {
        limits: config.limits,
        shared_hub: true,
        bounded: true,
        policy: DrainPolicy::SessionWide,
        debug: debug_enabled(),
    }
}

/// Cold per candidate anchor: a new empty hub for every anchor's fan-out.
async fn drive_cold(
    config: &Config,
    mode: &ModeChain<'_>,
    fanouts: &[(u64, Vec<Recorded>)],
    provider: &AlloyProvider,
    node: Node,
) -> Result<LegSummary, String> {
    let ledger = WallLedger::new();
    let mut summary = LegSummary::new("cold");
    for (block, rows) in fanouts {
        let hub = ReadHub::new(provider, ReadAnchor::identified(*block, mode.hash(*block)?));
        let facts = drive_lanes(
            "cold",
            leg_plan(config),
            rows,
            0,
            provider,
            node,
            Some(&hub),
            &ledger,
        )
        .await?;
        let hits = classify_hits(
            &read_call_counts(rows),
            &BTreeSet::new(),
            &BTreeSet::new(),
            facts.memo_hits,
        );
        let storage = storage_accounting(&anchor_keys(rows), &BTreeSet::new(), &BTreeSet::new());
        summary.fold(&facts, hits, storage);
    }
    summary.finish(&ledger)
}

/// Cold plus exact-anchor final writes: each anchor's block list is fetched
/// and decoded (the production pipeline) and its final writes seeded into the
/// anchor's empty hub, timed into the fetch/decode and snapshot buckets.
async fn drive_seed(
    config: &Config,
    mode: &ModeChain<'_>,
    fanouts: &[(u64, Vec<Recorded>)],
    provider: &AlloyProvider,
    node: Node,
) -> Result<LegSummary, String> {
    let ledger = WallLedger::new();
    let mut summary = LegSummary::new("seed");
    for (block, rows) in fanouts {
        let hub = ReadHub::new(provider, ReadAnchor::identified(*block, mode.hash(*block)?));
        let started = Instant::now();
        let raw = provider
            .get_raw_block_access_list(mode.hash(*block)?)
            .await
            .map_err(|error| format!("list fetch at {block}: {error}"))?;
        let seed = decode_bal_seed(&raw, &BalPlan::default())
            .map_err(|error| format!("list decode at {block}: {error}"))?;
        ledger.record_fixed(WallBucket::BalFetchDecode, node.bal());
        ledger.record_since(WallBucket::BalFetchDecode, started);
        let seeded_keys: BTreeSet<Key> = seed.writes.iter().map(|&(a, s, _)| (a, s)).collect();
        let started = Instant::now();
        let report = hub.seed_bal(&seed);
        ledger.record_since(WallBucket::SnapshotCloneUpdate, started);
        summary.bal_fetches += 1;
        summary.writes_applied = summary
            .writes_applied
            .saturating_add(u64::try_from(report.writes_seeded).unwrap_or(u64::MAX));
        let facts = drive_lanes(
            "seed",
            leg_plan(config),
            rows,
            0,
            provider,
            node,
            Some(&hub),
            &ledger,
        )
        .await?;
        let hits = classify_hits(
            &read_call_counts(rows),
            &seeded_keys,
            &BTreeSet::new(),
            facts.memo_hits,
        );
        let storage = storage_accounting(&anchor_keys(rows), &seeded_keys, &BTreeSet::new());
        summary.fold(&facts, hits, storage);
    }
    summary.finish(&ledger)
}

/// Time one advance's list fetch/decode and snapshot clone/update into the
/// leg's buckets and fold its write counters.
fn fold_advance(
    ledger: &WallLedger,
    node: Node,
    summary: &mut LegSummary,
    report: &CarryReport,
    advance_wall: Duration,
) {
    ledger.record_fixed(WallBucket::BalFetchDecode, node.bal());
    ledger.record_fixed(WallBucket::BalFetchDecode, report.bal_latency);
    ledger.record_fixed(WallBucket::SnapshotCloneUpdate, report.clone_time);
    let applied_wall = advance_wall
        .saturating_sub(report.bal_latency)
        .saturating_sub(report.clone_time);
    ledger.record_fixed(WallBucket::SnapshotCloneUpdate, applied_wall);
    summary.bal_fetches += 1;
    summary.writes_applied = summary
        .writes_applied
        .saturating_add(u64::try_from(report.writes.len()).unwrap_or(u64::MAX));
    summary.writes_skipped = summary
        .writes_skipped
        .saturating_add(u64::try_from(report.writes_skipped).unwrap_or(u64::MAX));
    if let CarryOutcome::Carried { carried_values } = &report.outcome {
        summary.carried_values = summary
            .carried_values
            .saturating_add(u64::try_from(*carried_values).unwrap_or(u64::MAX));
    }
}

/// Parent-linked carry-forward: every block of the range advanced through the
/// production `advance_bal` pipeline (list fetch/decode and snapshot
/// clone/update timed), the anchors' candidates driven against the carried
/// hub, and the MEV-relevant new-write rule left to the pipeline.
async fn drive_carry(
    config: &Config,
    mode: &ModeChain<'_>,
    fanouts: &[(u64, Vec<Recorded>)],
    provider: &AlloyProvider,
    node: Node,
) -> Result<LegSummary, String> {
    let ledger = WallLedger::new();
    let mut summary = LegSummary::new("carry");
    let cache = ReadHubCache::new(provider);
    let first = fanouts.first().map_or(0, |(block, _)| *block);
    let last = fanouts.last().map_or(0, |(block, _)| *block);
    let anchors: BTreeMap<u64, &[Recorded]> = fanouts
        .iter()
        .map(|(b, rows)| (*b, rows.as_slice()))
        .collect();
    // The snapshot's tracked identities: the working set carried forward and
    // every final write applied so far — the hit classification's denominator.
    let mut snapshot_keys: BTreeSet<Key> = BTreeSet::new();
    for number in first..=last {
        let relevance: Vec<Address> = anchors
            .get(&number)
            .map(|rows| candidate_addresses(rows))
            .unwrap_or_default();
        let started = Instant::now();
        let (hub, report) = cache
            .advance_bal(
                ReadAnchor::identified(number, mode.hash(number)?),
                mode.parent_hash(number)?,
                &relevance,
            )
            .await;
        let advance_wall = started.elapsed();
        fold_advance(&ledger, node, &mut summary, &report, advance_wall);
        let write_keys: BTreeSet<Key> = report.writes.iter().copied().collect();
        if let Some(rows) = anchors.get(&number) {
            let facts = drive_lanes(
                "carry",
                leg_plan(config),
                rows,
                0,
                provider,
                node,
                Some(&hub),
                &ledger,
            )
            .await?;
            let keys = anchor_keys(rows);
            let hits = classify_hits(
                &read_call_counts(rows),
                &write_keys,
                &snapshot_keys,
                facts.memo_hits,
            );
            let storage = storage_accounting(&keys, &write_keys, &snapshot_keys);
            summary.fold(&facts, hits, storage);
            snapshot_keys.extend(keys);
        }
        snapshot_keys.extend(write_keys);
    }
    summary.finish(&ledger)
}

/// Run the three legs of one latency point and compute its ceiling.
#[expect(
    clippy::too_many_arguments,
    reason = "the point's controls (prices, plant, observation sink) are the comparison's inputs"
)]
async fn run_point(
    config: &Config,
    mode: &ModeChain<'_>,
    fanouts: &[(u64, Vec<Recorded>)],
    provider: &AlloyProvider,
    storage: Duration,
    account: Duration,
    plant_storage: Duration,
    plant: Option<&Arc<AtomicBool>>,
    observed: &mut Vec<(String, Digests)>,
) -> Result<PointRun, String> {
    let node = Node::new(storage, account).with_plant(plant_storage);
    let cold = drive_cold(config, mode, fanouts, provider, node).await?;
    let seed = drive_seed(config, mode, fanouts, provider, node).await?;
    if let Some(flag) = plant {
        flag.store(true, Ordering::Relaxed);
    }
    let carry = drive_carry(config, mode, fanouts, provider, node).await;
    if let Some(flag) = plant {
        flag.store(false, Ordering::Relaxed);
    }
    let carry = carry?;
    let ceiling = eliminable_ceiling(
        &cold.wall,
        &[("seed", seed.wall.clone()), ("carry", carry.wall.clone())],
    )?;
    for summary in [&cold, &seed, &carry] {
        observed.push((summary.label.to_string(), summary.digests.clone()));
    }
    Ok(PointRun {
        storage,
        account,
        plant_storage,
        cold,
        seed,
        carry,
        ceiling,
    })
}

// ── Report ────────────────────────────────────────────────────────────────

fn us(ns: u64) -> u64 {
    ns / 1_000
}

/// The leg table's header; its column widths mirror [`print_leg_row`]'s
/// literal, which cannot share it (format strings are literals).
fn print_leg_header() {
    println!(
        "  {:<5} {:>9} {:>8} {:>8} {:>8} | {:>8} {:>9} {:>9} {:>8} {:>8} {:>8} | {:>8} {:>8} | {:>5} {:>4} {:>6} {:>6} {:>7} {:>7} | {:>6} {:>7} {:>7} {:>6} | {:>7} {:>6} {:>9}",
        "leg", "wall_us", "first_us", "p50_us", "p95_us", "exec_us", "storage_us",
        "account_us", "bal_us", "snap_us", "other_us", "gross_us", "overlap_us", "W/F",
        "peak", "coales", "rounds", "st-mem", "ac-mem", "memo", "carried", "final-wr",
        "other", "writes", "skip", "carried-v",
    );
}

fn print_leg_row(leg: &LegSummary) {
    let bucket = |bucket: WallBucket| us(leg.wall.bucket(bucket));
    let gross: u64 = leg.wall.gross.iter().sum();
    let slots = leg
        .slots
        .map_or_else(|| "-".to_string(), |slots| slots.to_string());
    println!(
        "  {:<5} {:>9} {:>8} {:>8} {:>8} | {:>8} {:>9} {:>9} {:>8} {:>8} {:>8} | {:>8} {:>8} | {:>5} {:>4} {:>6} {:>6} {:>7} {:>7} | {:>6} {:>7} {:>7} {:>6} | {:>7} {:>6} {:>9}",
        leg.label,
        us(leg.wall_ns),
        us(leg.completions.first_ns),
        us(leg.completions.p50_ns),
        us(leg.completions.p95_ns),
        bucket(WallBucket::RecordedExec),
        bucket(WallBucket::StorageWait),
        bucket(WallBucket::AccountWait),
        bucket(WallBucket::BalFetchDecode),
        bucket(WallBucket::SnapshotCloneUpdate),
        bucket(WallBucket::SchedulerOther),
        us(gross),
        us(leg.wall.overlap_ns),
        format!("{}/{}", leg.lanes, slots),
        leg.peak_active,
        leg.coalesced,
        leg.rounds,
        leg.storage_members,
        leg.account_members,
        leg.memo_hits,
        leg.hits.carried,
        leg.hits.final_write,
        leg.hits.other,
        leg.writes_applied,
        leg.writes_skipped,
        leg.carried_values,
    );
}

fn print_reconciliation(leg: &LegSummary) {
    let sum: u64 = leg.wall.buckets.iter().sum();
    let gross: u64 = leg.wall.gross.iter().sum();
    println!(
        "  reconcile {:<5}: buckets {}us == measured wall {}us (residual {}us, the reported \
         tolerance); scheduler/other {}us unattributed; the drives' own wall {}us; gross \
         lane-time {}us with {}us of double occupancy resolved by precedence and reported once",
        leg.label,
        us(sum),
        us(leg.wall_ns),
        us(sum.abs_diff(leg.wall_ns)),
        us(leg.wall.bucket(WallBucket::SchedulerOther)),
        us(leg.drives_wall_ns),
        us(gross),
        us(leg.wall.overlap_ns),
    );
}

fn print_ceiling(run: &PointRun) {
    let seed_removed = run
        .cold
        .wall
        .bucket(WallBucket::StorageWait)
        .saturating_sub(run.seed.wall.bucket(WallBucket::StorageWait));
    let seed_share = seed_removed.saturating_mul(1_000) / run.cold.wall_ns.max(1);
    println!(
        "  eliminable storage-wait ceiling (over a cold wall of {}us): {} removes {}us = \
         {}.{:01}% (gross {}us = {}.{:01}%); seed removes {}us = {}.{:01}%",
        us(run.cold.wall_ns),
        run.ceiling.policy,
        us(run.ceiling.eliminated_ns),
        run.ceiling.share_thousandths / 10,
        run.ceiling.share_thousandths % 10,
        us(run.ceiling.eliminated_gross_ns),
        run.ceiling.gross_share_thousandths / 10,
        run.ceiling.gross_share_thousandths % 10,
        us(seed_removed),
        seed_share / 10,
        seed_share % 10,
    );
}

fn print_point(run: &PointRun) {
    println!(
        "\nlatency point: storage {}us / account {}us (list fetch priced at {}us), planted \
         storage delay {}us",
        run.storage.as_micros(),
        run.account.as_micros(),
        run.account.as_micros(),
        run.plant_storage.as_micros(),
    );
    print_leg_header();
    for leg in [&run.cold, &run.seed, &run.carry] {
        print_leg_row(leg);
    }
    for leg in [&run.cold, &run.seed, &run.carry] {
        print_reconciliation(leg);
    }
    print_ceiling(run);
}

/// The planted-delay comparison: the bucket and ceiling deltas of one point
/// run unplanted and planted, over both the partitioned wall buckets and the
/// gross span sums. The deltas are reported, never asserted.
fn print_plant_delta(base: &PointRun, planted: &PointRun) {
    println!(
        "\nplanted storage-delay comparison ({}us per storage round trip, first latency point):",
        planted.plant_storage.as_micros()
    );
    let partitioned: fn(&LegSummary, WallBucket) -> u64 = |leg, bucket| leg.wall.bucket(bucket);
    let gross: fn(&LegSummary, WallBucket) -> u64 = |leg, bucket| leg.wall.gross_bucket(bucket);
    for (view, pick) in [
        ("partitioned wall buckets", partitioned),
        (
            "gross span sums (the plant is charged into storage-wait spans alone)",
            gross,
        ),
    ] {
        println!("  {view}:");
        println!(
            "  {:<5} {:>10} {:>11} {:>11} {:>9} {:>10} {:>10}",
            "leg",
            "exec_d_us",
            "storage_d_us",
            "account_d_us",
            "bal_d_us",
            "snap_d_us",
            "other_d_us"
        );
        for (label, base, planted) in [
            ("cold", &base.cold, &planted.cold),
            ("seed", &base.seed, &planted.seed),
            ("carry", &base.carry, &planted.carry),
        ] {
            let delta = |bucket: WallBucket| -> i64 {
                let before = i64::try_from(pick(base, bucket)).unwrap_or(i64::MAX);
                let after = i64::try_from(pick(planted, bucket)).unwrap_or(i64::MAX);
                (after - before) / 1_000
            };
            println!(
                "  {:<5} {:>10} {:>11} {:>11} {:>9} {:>10} {:>10}",
                label,
                delta(WallBucket::RecordedExec),
                delta(WallBucket::StorageWait),
                delta(WallBucket::AccountWait),
                delta(WallBucket::BalFetchDecode),
                delta(WallBucket::SnapshotCloneUpdate),
                delta(WallBucket::SchedulerOther),
            );
        }
    }
    println!(
        "  ceiling: {}us ({}.{:01}%) unplanted -> {}us ({}.{:01}%) planted",
        us(base.ceiling.eliminated_ns),
        base.ceiling.share_thousandths / 10,
        base.ceiling.share_thousandths % 10,
        us(planted.ceiling.eliminated_ns),
        planted.ceiling.share_thousandths / 10,
        planted.ceiling.share_thousandths % 10,
    );
}

fn print_header(
    config: &Config,
    mode: &ModeChain<'_>,
    candidates: usize,
    anchors: usize,
    first: u64,
    last: u64,
) {
    println!(
        "bal_wall_ceiling_probe [{}/coverage]: trace={} rows={candidates} anchors={anchors} blocks \
         {first}..={last} W={} F={} planted storage delay {}us",
        mode.label,
        config.trace,
        config.limits.workers(),
        config.limits.slots(),
        config.plant_storage.as_micros(),
    );
    println!(
        "  coverage: {} write identities per block list (fixture {}); read values are the \
         shared chain's, identical across coverage modes",
        mode.label, config.fixture
    );
    println!(
        "  legs: cold = new empty hub per candidate anchor; seed = exact-anchor final writes into \
         each empty hub; carry = unbroken parent chain through every intermediate block with the \
         MEV-relevant new writes only"
    );
    println!(
        "  model: one round trip per dispatched packet at the point's prices (a list fetch is one \
         packet at the account price); the recorded sim_ns is SPENT as wall inside the candidate's \
         slot; identical candidates, order, W/F bound, and prices in every leg"
    );
    println!(
        "  record: the buckets sum to the measured wall; recorded reads do NOT establish full EVM \
         verdict parity (no calldata, no opcodes, modeled write coverage); timing is reported, \
         never asserted"
    );
}

/// The cross-leg digest gate: every leg's every recorded candidate must carry
/// the cold baseline's read-value digest.
fn report_gate(
    observed: &[(String, Digests)],
    candidates: usize,
    label: &str,
) -> Result<(), String> {
    let mut baseline = Digests::new();
    let mut legs: Vec<(String, Digests)> = Vec::new();
    for (label, digests) in observed {
        if label == "cold" {
            baseline.extend(digests.clone());
        } else {
            legs.push((label.clone(), digests.clone()));
        }
    }
    if baseline.len() != candidates {
        return Err(format!(
            "the cold baseline replayed {} of {candidates} recorded candidates",
            baseline.len()
        ));
    }
    let mismatches = identity_gate(&baseline, &legs);
    println!();
    if mismatches.is_empty() {
        println!(
            "identity gate [{label}]: GREEN ({} legs x {} candidates)",
            legs.len(),
            baseline.len()
        );
        return Ok(());
    }
    println!(
        "identity gate [{label}]: RED ({} mismatches)",
        mismatches.len()
    );
    for mismatch in mismatches.iter().take(5) {
        println!("  {mismatch}");
    }
    Err("replay identity gate red".to_string())
}

/// The distinct-key per-anchor storage accounting: the live probe's counts
/// (one read per distinct recorded key), beside the call-level hit split the
/// leg table carries.
fn print_storage_accounting(label: &str, point: &PointRun) {
    println!(
        "  storage accounting [{label}] (distinct keys per anchor, the live bal_carry_probe's shape):"
    );
    for (leg, accounting) in [
        ("cold", &point.cold.storage),
        ("seed", &point.seed.storage),
        ("carry", &point.carry.storage),
    ] {
        println!(
            "    {leg:<5} keys {} carried {} final-write {} dynamic {}",
            accounting.keys, accounting.carried, accounting.final_write, accounting.dynamic
        );
    }
}

/// The real mode's carry accounting must be the live `bal_carry_probe`'s
/// counts for this corpus — a discrepancy is a failing gate, never hidden.
fn cross_check_live(accounting: &StorageAccounting) -> Result<(), String> {
    let matched = accounting.keys == LIVE_ANCHOR_KEYS
        && accounting.carried == LIVE_CARRIED_HITS
        && accounting.final_write == LIVE_FINAL_WRITE_HITS
        && accounting.dynamic == LIVE_DYNAMIC_MEMBERS;
    if matched {
        println!(
            "  live cross-check: GREEN (keys {}, carried {}, final-write {}, dynamic {} — the \
             bal_carry_probe's counts)",
            accounting.keys, accounting.carried, accounting.final_write, accounting.dynamic
        );
        return Ok(());
    }
    println!(
        "  live cross-check: RED (replay keys {}, carried {}, final-write {}, dynamic {} vs live \
         {LIVE_ANCHOR_KEYS}, {LIVE_CARRIED_HITS}, {LIVE_FINAL_WRITE_HITS}, {LIVE_DYNAMIC_MEMBERS})",
        accounting.keys, accounting.carried, accounting.final_write, accounting.dynamic
    );
    Err(format!(
        "real coverage's carry accounting (keys {}, carried {}, final-write {}, dynamic {}) does \
         not match the live bal_carry_probe ({LIVE_ANCHOR_KEYS}, {LIVE_CARRIED_HITS}, \
         {LIVE_FINAL_WRITE_HITS}, {LIVE_DYNAMIC_MEMBERS})",
        accounting.keys, accounting.carried, accounting.final_write, accounting.dynamic
    ))
}

/// The cross-mode value gate: the shared chain means both coverage modes
/// replay IDENTICAL read values, so their cold baselines' digests match
/// candidate for candidate.
fn report_cross_mode_values(left: (&str, &Digests), right: (&str, &Digests)) -> Result<(), String> {
    let mut mismatches = Vec::new();
    for (key, digest) in left.1 {
        match right.1.get(key) {
            Some(other) if other == digest => {}
            Some(other) => mismatches.push(format!(
                "{} {key:?} digest {digest} != {} {other}",
                left.0, right.0
            )),
            None => mismatches.push(format!(
                "{} replayed {key:?} but {} did not",
                left.0, right.0
            )),
        }
    }
    for key in right.1.keys() {
        if !left.1.contains_key(key) {
            mismatches.push(format!(
                "{} replayed {key:?} but {} did not",
                right.0, left.0
            ));
        }
    }
    if mismatches.is_empty() {
        println!(
            "cross-mode value gate: GREEN ({} candidates carry identical read values in {} and {})",
            left.1.len(),
            left.0,
            right.0
        );
        return Ok(());
    }
    println!(
        "cross-mode value gate: RED ({} mismatches)",
        mismatches.len()
    );
    for mismatch in mismatches.iter().take(5) {
        println!("  {mismatch}");
    }
    Err("cross-mode read values diverged".to_string())
}

/// Synthetic versus real coverage at one latency point: every quantity the
/// comparison isolates, side by side (the per-mode tables above carry the full
/// bucket splits and reconciliations).
#[expect(
    clippy::too_many_lines,
    reason = "the comparison rows and the decomposition read top-down beside the tables"
)]
fn print_coverage_comparison(
    storage: Duration,
    account: Duration,
    synthetic: &PointRun,
    real: &PointRun,
) {
    println!(
        "\ncoverage comparison at {}/{}us (synthetic modeled bits -> real captured identities):",
        storage.as_micros(),
        account.as_micros()
    );
    println!("  {:<44} {:>12} {:>12}", "metric", "synthetic", "real");
    let us = |ns: u64| (ns / 1_000).to_string();
    let row = |name: &str, left: String, right: String| {
        println!("  {name:<44} {left:>12} {right:>12}");
    };
    let ceiling = |run: &PointRun| {
        format!(
            "{} {}.{:01}%",
            run.ceiling.policy,
            run.ceiling.share_thousandths / 10,
            run.ceiling.share_thousandths % 10
        )
    };
    let bucket = |run: &LegSummary, choice: WallBucket| us(run.wall.bucket(choice));
    row(
        "eliminable storage-wait ceiling",
        ceiling(synthetic),
        ceiling(real),
    );
    row(
        "cold wall_us / storage_us",
        format!(
            "{}/{}",
            us(synthetic.cold.wall_ns),
            bucket(&synthetic.cold, WallBucket::StorageWait)
        ),
        format!(
            "{}/{}",
            us(real.cold.wall_ns),
            bucket(&real.cold, WallBucket::StorageWait)
        ),
    );
    for (name, left, right) in [
        ("seed wall_us / storage_us", &synthetic.seed, &real.seed),
        ("carry wall_us / storage_us", &synthetic.carry, &real.carry),
    ] {
        row(
            name,
            format!(
                "{}/{}",
                us(left.wall_ns),
                bucket(left, WallBucket::StorageWait)
            ),
            format!(
                "{}/{}",
                us(right.wall_ns),
                bucket(right, WallBucket::StorageWait)
            ),
        );
    }
    row(
        "carry buckets us (exec/account/bal/other)",
        format!(
            "{}/{}/{}/{}",
            bucket(&synthetic.carry, WallBucket::RecordedExec),
            bucket(&synthetic.carry, WallBucket::AccountWait),
            bucket(&synthetic.carry, WallBucket::BalFetchDecode),
            bucket(&synthetic.carry, WallBucket::SchedulerOther),
        ),
        format!(
            "{}/{}/{}/{}",
            bucket(&real.carry, WallBucket::RecordedExec),
            bucket(&real.carry, WallBucket::AccountWait),
            bucket(&real.carry, WallBucket::BalFetchDecode),
            bucket(&real.carry, WallBucket::SchedulerOther),
        ),
    );
    row(
        "carry snapshot_us / bal fetches",
        format!(
            "{}/{}",
            bucket(&synthetic.carry, WallBucket::SnapshotCloneUpdate),
            synthetic.carry.bal_fetches
        ),
        format!(
            "{}/{}",
            bucket(&real.carry, WallBucket::SnapshotCloneUpdate),
            real.carry.bal_fetches
        ),
    );
    row(
        "carry completion us (first/p50/p95)",
        format!(
            "{}/{}/{}",
            us(synthetic.carry.completions.first_ns),
            us(synthetic.carry.completions.p50_ns),
            us(synthetic.carry.completions.p95_ns),
        ),
        format!(
            "{}/{}/{}",
            us(real.carry.completions.first_ns),
            us(real.carry.completions.p50_ns),
            us(real.carry.completions.p95_ns),
        ),
    );
    row(
        "carry rounds / storage members",
        format!(
            "{}/{}",
            synthetic.carry.rounds, synthetic.carry.storage_members
        ),
        format!("{}/{}", real.carry.rounds, real.carry.storage_members),
    );
    row(
        "carry hits (calls: carried/final-write/other)",
        format!(
            "{}/{}/{}",
            synthetic.carry.hits.carried,
            synthetic.carry.hits.final_write,
            synthetic.carry.hits.other
        ),
        format!(
            "{}/{}/{}",
            real.carry.hits.carried, real.carry.hits.final_write, real.carry.hits.other
        ),
    );
    row(
        "carry keys (distinct: carried/final-write/dyn)",
        format!(
            "{}/{}/{}",
            synthetic.carry.storage.carried,
            synthetic.carry.storage.final_write,
            synthetic.carry.storage.dynamic
        ),
        format!(
            "{}/{}/{}",
            real.carry.storage.carried, real.carry.storage.final_write, real.carry.storage.dynamic
        ),
    );
    row(
        "carry writes applied / skipped",
        format!(
            "{}/{}",
            synthetic.carry.writes_applied, synthetic.carry.writes_skipped
        ),
        format!(
            "{}/{}",
            real.carry.writes_applied, real.carry.writes_skipped
        ),
    );
    row(
        "seed keys (distinct: carried/final-write/dyn)",
        format!(
            "{}/{}/{}",
            synthetic.seed.storage.carried,
            synthetic.seed.storage.final_write,
            synthetic.seed.storage.dynamic
        ),
        format!(
            "{}/{}/{}",
            real.seed.storage.carried, real.seed.storage.final_write, real.seed.storage.dynamic
        ),
    );

    println!("  ceiling decomposition (carried reuse vs optimistic modeled write coverage):");
    println!(
        "    cold is coverage-independent: {} distinct keys, {}us of storage wait at this point",
        real.cold.storage.keys,
        us(real.cold.wall.bucket(WallBucket::StorageWait))
    );
    println!(
        "    the anchors' own final writes answer {} distinct keys under the modeled ~50% \
         coverage and {} under the real lists; carried reuse answers {} vs {}",
        synthetic.carry.storage.final_write,
        real.carry.storage.final_write,
        synthetic.carry.storage.carried,
        real.carry.storage.carried
    );
    println!(
        "    so of the synthetic ceiling ({}.{:01}%), the optimistic write coverage's share is \
         the {}.{:01} points the real ceiling loses; the surviving {}.{:01}% is carried reuse \
         plus the real final writes",
        synthetic.ceiling.share_thousandths / 10,
        synthetic.ceiling.share_thousandths % 10,
        synthetic
            .ceiling
            .share_thousandths
            .saturating_sub(real.ceiling.share_thousandths)
            / 10,
        synthetic
            .ceiling
            .share_thousandths
            .saturating_sub(real.ceiling.share_thousandths)
            % 10,
        real.ceiling.share_thousandths / 10,
        real.ceiling.share_thousandths % 10
    );
}

/// The node the dev-only capture phase dials: the chain-1 endpoint env family,
/// or the conftest name. Neither means no declared endpoint — fail loud.
fn rpc_url() -> Result<String, String> {
    std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .or_else(|_| std::env::var("TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI"))
        .map_err(|_| {
            "set DEGENBOT_RPC_HTTP_CHAINID_1 (or TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI) \
             to an archive mainnet endpoint"
                .to_string()
        })
}

/// The dev-only capture phase: dial the declared node, fold the corpus
/// stretch's real write identities and parent linkage into the fixture, and
/// exit — the measured replay itself never touches the network.
async fn capture_fixture(
    config: &Config,
    keys: &BTreeSet<Key>,
    first: u64,
    last: u64,
) -> Result<(), String> {
    let tracked: BTreeSet<Address> = keys.iter().map(|(address, _)| *address).collect();
    let url = rpc_url()?;
    let provider = AlloyProvider::new(&url, 3)
        .await
        .map_err(|error| format!("connect {url}: {error}"))?;
    println!(
        "capturing the real write-coverage fixture for {first}..={last} from {url} ({} tracked \
         addresses)",
        tracked.len()
    );
    let real = real_coverage::capture(&provider, first, last, &tracked).await?;
    real.save(&config.fixture)?;
    let other: usize = (first..=last)
        .filter_map(|number| real.block(number))
        .map(|block| block.other_writes)
        .sum();
    println!(
        "wrote {}: {} blocks, {} kept write identities over the tracked addresses ({} writes \
         outside them, never applied by the carry's write policy)",
        config.fixture,
        last - first + 1,
        real.write_count(),
        other
    );
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "the run's capture phase, mode replays, and gates read top-down"
)]
async fn run() -> Result<(), String> {
    let config = Config::from_env()?;
    let rows = load_rows(&config.trace)?;
    let (keys, first, last) = universe(&rows);
    let fanouts = group_by_anchor(rows);
    let candidates: usize = fanouts.iter().map(|(_, rows)| rows.len()).sum();

    if config.capture {
        return capture_fixture(&config, &keys, first, last).await;
    }

    let real = RealCoverage::load(&config.fixture)?;
    if real.first() > first || real.last() < last {
        return Err(format!(
            "the fixture covers {}..={} but the corpus replays {first}..={last}",
            real.first(),
            real.last()
        ));
    }
    let plant = config
        .plant
        .clone()
        .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));

    let mut runs: Vec<(Duration, Duration, Duration)> = Vec::new();
    if config.plant_storage.is_zero() && config.plant.is_none() {
        for &(storage, account) in &config.points {
            runs.push((storage, account, Duration::ZERO));
        }
    } else {
        // A negative probe narrows to the first point; the storage-delay plant
        // runs that point unplanted and planted so its deltas are reportable.
        let (storage, account) = config.points[0];
        runs.push((storage, account, Duration::ZERO));
        if config.plant.is_none() {
            runs.push((storage, account, config.plant_storage));
        }
    }

    // One replay per coverage mode: identical candidates, order, W/F, prices,
    // and read values; only the block lists' write identities differ. Gates
    // fail the process after every mode's report is printed, so a red gate
    // never hides the other mode's numbers.
    let mut failures: Vec<String> = Vec::new();
    let mut modes: Vec<(&'static str, Vec<PointRun>, Digests)> = Vec::new();
    for &(label, coverage) in &config.modes {
        let mode = ModeChain {
            label,
            coverage,
            real: &real,
        };
        let chain = Arc::new(ChainState::build_coverage(
            &keys, first, last, &real, coverage,
        )?);
        let provider = StubChain {
            chain,
            plant: Arc::clone(&plant),
        }
        .provider();
        print_header(&config, &mode, candidates, fanouts.len(), first, last);

        let mut observed: Vec<(String, Digests)> = Vec::new();
        let mut completed: Vec<PointRun> = Vec::new();
        for &(storage, account, plant_storage) in &runs {
            let run = run_point(
                &config,
                &mode,
                &fanouts,
                &provider,
                storage,
                account,
                plant_storage,
                config.plant.as_ref(),
                &mut observed,
            )
            .await?;
            print_point(&run);
            completed.push(run);
        }
        if config.plant_storage > Duration::ZERO {
            if let [base, planted] = completed.as_slice() {
                print_plant_delta(base, planted);
            }
        }
        // The storage accounting is set arithmetic over one replay, identical
        // at every point: read it off the first point's legs.
        let mut cold_baseline = Digests::new();
        for (leg, digests) in &observed {
            if leg == "cold" {
                cold_baseline.extend(digests.clone());
            }
        }
        if let Some(point) = completed.first() {
            let drift: usize = (first..=last)
                .flat_map(|number| {
                    real.writes_at(number)
                        .iter()
                        .map(move |&(address, slot)| (number, address, slot))
                })
                .filter(|&(number, address, slot)| write_selected(number, address, slot))
                .count();
            println!(
                "  shared chain drift: {drift} captured writes the modeled bit also selects (the \
                 digest gate's stale-carry teeth)"
            );
            print_storage_accounting(label, point);
            if coverage == Coverage::Real {
                if let Err(error) = cross_check_live(&point.carry.storage) {
                    failures.push(error);
                }
            }
        }
        if let Err(error) = report_gate(&observed, candidates, label) {
            failures.push(error);
        }
        modes.push((label, completed, cold_baseline));
    }

    if let [(synthetic_label, synthetic, synthetic_values), (real_label, real_runs, real_values)] =
        modes.as_slice()
    {
        for (index, (left, right)) in synthetic.iter().zip(real_runs.iter()).enumerate() {
            let (storage, account, _) = runs[index];
            print_coverage_comparison(storage, account, left, right);
        }
        if let Err(error) = report_cross_mode_values(
            (*synthetic_label, synthetic_values),
            (*real_label, real_values),
        ) {
            failures.push(error);
        }
    } else {
        println!("\none coverage mode driven: no cross-mode comparison");
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("bal_wall_ceiling_probe: {error}");
        std::process::exit(1);
    }
}
