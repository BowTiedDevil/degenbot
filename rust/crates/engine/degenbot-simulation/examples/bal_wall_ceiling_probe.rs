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
//!   MEV-relevant new writes only (each anchor's relevance set is the production
//!   derivation its trace rows carry — the set the seam knew BEFORE simulation —
//!   or, for a legacy row, its recorded candidate addresses as a labelled ORACLE
//!   fallback; the report prints which).
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
//! # Density segmentation
//!
//! The corpus's anchors are segmented into the density regimes they actually
//! record (`recorded_corpus::segments_by_density`: sparse under fifteen
//! candidates, moderate to fifty, burst above), and every regime segment
//! publishes its read-set diversity (new versus repeated keys, key age,
//! survival, distinct contracts) and its real write overlap beside its
//! cold-versus-carry replay: wall, eliminable storage wait, and the carry's
//! reuse. A regime the corpus never recorded is named absent, never fabricated.
//!
//! # Usage
//!
//! ```sh
//! just bal-wall-ceiling-probe                     # both modes x both price points, unplanted
//! BAL_WALL_COVERAGE=real just bal-wall-ceiling-probe    # one mode (synthetic | real | both)
//! BAL_WALL_LEGS=cold,carry just bal-wall-ceiling-probe  # the legs (cold and carry; seed optional)
//! BAL_WALL_TRACE=/tmp/bal_broader_trace.log \
//! BAL_WALL_FIXTURE=rust/crates/engine/degenbot-simulation/tests/fixtures/bal_real_coverage_broader.json \
//! BAL_WALL_COVERAGE=real BAL_WALL_LEGS=cold,carry just bal-wall-ceiling-probe
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
#[expect(dead_code)]
mod session_drive;
#[path = "shared/wall_ledger.rs"]
#[expect(dead_code)]
mod wall_ledger;

use modeled_chain::{write_selected, ChainState, Coverage, Key, StubChain};
use probe_report::{
    attribute_segment_spans, classify_hits, cross_check_live, eliminable_ceiling, identity_gate,
    segment_ceiling, storage_accounting, write_overlap, CeilingReport, HitSplit, StorageAccounting,
    WorkUnit,
};
use real_coverage::RealCoverage;
use recorded_corpus::{
    anchor_keys, anchor_profiles, anchor_relevance, corpus_relevance_source, group_by_anchor,
    load_rows, read_call_counts, segments_by_density, shape, universe, AnchorProfile, CorpusShape,
    DensityRegime, Recorded, Relevance, Segment, DEFAULT_TRACE,
};
use session_drive::{drive_lanes, LanePlan, LaneTrace, LegFacts, ReadLane};
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

/// The live `bal_carry_probe` accounting each recorded corpus produced — the
/// real mode's cross-check target, measured by the live probe over the same
/// stretch. Any discrepancy is a failing gate, never a hidden one.
struct LiveProfile {
    /// The corpus stretch's canonical blocks.
    first: u64,
    last: u64,
    /// The per-anchor distinct-key accounting the live probe reported.
    accounting: StorageAccounting,
}

const LIVE_PROFILES: &[LiveProfile] = &[
    LiveProfile {
        first: 26_375_033,
        last: 26_375_053,
        accounting: StorageAccounting {
            keys: 5_938,
            carried: 3_261,
            final_write: 136,
            dynamic: 2_541,
        },
    },
    LiveProfile {
        first: 26_380_992,
        last: 26_381_018,
        accounting: StorageAccounting {
            keys: 6_478,
            carried: 3_711,
            final_write: 164,
            dynamic: 2_603,
        },
    },
];

/// The corpus stretch's live accounting to cross-check against, when the
/// stretch carries one.
fn live_profile(first: u64, last: u64) -> Option<&'static StorageAccounting> {
    LIVE_PROFILES
        .iter()
        .find(|profile| profile.first == first && profile.last == last)
        .map(|profile| &profile.accounting)
}

/// One replay leg's cache policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LegKind {
    /// A new empty hub per candidate anchor.
    Cold,
    /// Exact-anchor final writes seeded into each empty hub.
    Seed,
    /// Parent-linked carry-forward through every block of the range.
    Carry,
}

/// The segment each block's work units are attributed to: a block's chain
/// maintenance is charged to the segment of the fan-out it serves next, and
/// the tail past the last anchor stays with the last anchor's segment.
struct SegmentMap {
    to_segment: BTreeMap<u64, usize>,
}

impl SegmentMap {
    /// Build the attribution over `first..=last` from the segments' anchors.
    fn build(segments: &[Segment], first: u64, last: u64) -> Self {
        let mut anchor_segments: Vec<(u64, usize)> = segments
            .iter()
            .enumerate()
            .flat_map(|(index, segment)| {
                segment
                    .anchors
                    .iter()
                    .map(move |block| (*block, index))
                    .collect::<Vec<_>>()
            })
            .collect();
        anchor_segments.sort_unstable();
        let mut to_segment: BTreeMap<u64, usize> = BTreeMap::new();
        let mut anchor = anchor_segments.iter().peekable();
        for number in first..=last {
            while anchor.peek().is_some_and(|(block, _)| *block < number) {
                anchor.next();
            }
            let segment = anchor
                .peek()
                .map(|(_, segment)| *segment)
                .or_else(|| anchor_segments.last().map(|(_, segment)| *segment))
                .expect("a segmented corpus holds at least one anchor");
            to_segment.insert(number, segment);
        }
        Self { to_segment }
    }

    /// The segment a block's work units belong to.
    fn segment_of(&self, block: u64) -> usize {
        self.to_segment
            .get(&block)
            .copied()
            .expect("every corpus block is attributed to a segment")
    }
}

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
    /// The legs to drive, in report order (`BAL_WALL_LEGS`).
    legs: Vec<LegKind>,
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

/// Fold `BAL_WALL_LEGS` (comma-separated `cold`, `seed`, `carry`; default all
/// three) into the legs to drive. The cold-versus-carry comparison needs both
/// of its legs; the seed leg is the optional third.
///
/// # Errors
///
/// A leg name this probe does not implement, or a selection without both
/// comparison legs, is a broken invocation, never a silent default.
fn parse_legs() -> Result<Vec<LegKind>, String> {
    let raw = std::env::var("BAL_WALL_LEGS").unwrap_or_else(|_| "cold,seed,carry".into());
    let mut legs = Vec::new();
    for name in raw
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let leg = match name {
            "cold" => LegKind::Cold,
            "seed" => LegKind::Seed,
            "carry" => LegKind::Carry,
            other => {
                return Err(format!(
                    "BAL_WALL_LEGS={other}: expected cold, seed, or carry"
                ))
            }
        };
        if !legs.contains(&leg) {
            legs.push(leg);
        }
    }
    if !legs.contains(&LegKind::Cold) || !legs.contains(&LegKind::Carry) {
        return Err("BAL_WALL_LEGS must drive both cold and carry".to_string());
    }
    Ok(legs)
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
            legs: parse_legs()?,
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

/// One leg work unit's measurement: the spans it recorded and the wall it
/// spent.
struct UnitFact {
    /// The span slice the unit recorded.
    unit: WorkUnit,
    /// The wall the unit spent, measured around its own work.
    wall: Duration,
}

/// One anchor's replay measurement folded for the per-segment report.
struct AnchorFold {
    block: u64,
    hits: HitSplit,
    storage: StorageAccounting,
}

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
    /// Every work unit's span slice and measured wall (the segment attribution).
    units: Vec<UnitFact>,
    /// Every anchor's own hit split and distinct-key accounting.
    anchors: Vec<AnchorFold>,
    /// The per-segment gross bucket sums the units fold into.
    segment_gross: Vec<[u64; 6]>,
    /// Snapshot clone wall summed over the advances, and the largest one.
    clone_ns: u64,
    clone_ns_max: u64,
    /// Snapshot entries after each advance: the memory growth series.
    snapshot_growth: Vec<(u64, usize)>,
    /// Resident memory at the leg's start and end, when the platform reports it.
    resident_kb: (Option<u64>, Option<u64>),
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
            units: Vec::new(),
            anchors: Vec::new(),
            segment_gross: Vec::new(),
            clone_ns: 0,
            clone_ns_max: 0,
            snapshot_growth: Vec::new(),
            resident_kb: (None, None),
            digests: Digests::new(),
        }
    }

    /// Fold one anchor's drive into the leg.
    fn fold(&mut self, block: u64, facts: &LegFacts, hits: HitSplit, storage: StorageAccounting) {
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
        self.anchors.push(AnchorFold {
            block,
            hits,
            storage,
        });
        self.digests.extend(facts.digests.clone());
    }

    /// Close the leg: reconcile its ledger, fold its units into per-segment
    /// gross sums, and summarize its completions.
    fn finish(mut self, ledger: &WallLedger, segments: usize) -> Result<Self, String> {
        self.wall_ns = ledger.offset();
        let spans = ledger.spans();
        self.wall = reconcile(&spans, self.wall_ns)?;
        self.completions = completion_stats(ledger.completions())?;
        let units: Vec<WorkUnit> = self.units.iter().map(|fact| fact.unit).collect();
        self.segment_gross = attribute_segment_spans(&units, &spans, segments)?;
        Ok(self)
    }
}

/// The legs of one latency point.
struct PointRun {
    storage: Duration,
    account: Duration,
    plant_storage: Duration,
    cold: LegSummary,
    /// The optional same-anchor seeding leg (`BAL_WALL_LEGS`).
    seed: Option<LegSummary>,
    carry: LegSummary,
    ceiling: CeilingReport,
}

impl PointRun {
    /// The driven legs, in report order.
    fn legs(&self) -> Vec<&LegSummary> {
        let mut legs = vec![&self.cold];
        if let Some(seed) = &self.seed {
            legs.push(seed);
        }
        legs.push(&self.carry);
        legs
    }
}

// ── The legs ──────────────────────────────────────────────────────────────

fn leg_plan(config: &Config) -> LanePlan {
    LanePlan {
        limits: config.limits,
        shared_hub: true,
        bounded: true,
        policy: DrainPolicy::SessionWide,
        trace: if debug_enabled() {
            LaneTrace::Progress
        } else {
            LaneTrace::Off
        },
    }
}

/// Cold per candidate anchor: a new empty hub for every anchor's fan-out.
async fn drive_cold(
    config: &Config,
    mode: &ModeChain<'_>,
    fanouts: &[(u64, Vec<Recorded>)],
    provider: &AlloyProvider,
    node: Node,
    map: &SegmentMap,
    segments: usize,
) -> Result<LegSummary, String> {
    let ledger = WallLedger::new();
    let mut summary = LegSummary::new("cold");
    for (block, rows) in fanouts {
        let hub = ReadHub::new(provider, ReadAnchor::identified(*block, mode.hash(*block)?));
        let first_span = ledger.spans().len();
        let started = Instant::now();
        let facts = drive_lanes(
            "cold",
            leg_plan(config),
            rows,
            0,
            provider,
            node,
            Some(ReadLane::Full(&hub)),
            &ledger,
        )
        .await?;
        summary.units.push(UnitFact {
            unit: WorkUnit {
                first_span,
                end_span: ledger.spans().len(),
                segment: map.segment_of(*block),
            },
            wall: started.elapsed(),
        });
        let hits = classify_hits(
            &read_call_counts(rows),
            &BTreeSet::new(),
            &BTreeSet::new(),
            facts.memo_hits,
        );
        let storage = storage_accounting(&anchor_keys(rows), &BTreeSet::new(), &BTreeSet::new());
        summary.fold(*block, &facts, hits, storage);
    }
    summary.finish(&ledger, segments)
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
    map: &SegmentMap,
    segments: usize,
) -> Result<LegSummary, String> {
    let ledger = WallLedger::new();
    let mut summary = LegSummary::new("seed");
    for (block, rows) in fanouts {
        let hub = ReadHub::new(provider, ReadAnchor::identified(*block, mode.hash(*block)?));
        let unit_start = ledger.spans().len();
        let unit_started = Instant::now();
        let raw = provider
            .get_raw_block_access_list(mode.hash(*block)?)
            .await
            .map_err(|error| format!("list fetch at {block}: {error}"))?;
        let seed = decode_bal_seed(&raw, &BalPlan::default())
            .map_err(|error| format!("list decode at {block}: {error}"))?;
        ledger.record_fixed(WallBucket::BalFetchDecode, node.bal());
        ledger.record_since(WallBucket::BalFetchDecode, unit_started);
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
            Some(ReadLane::Full(&hub)),
            &ledger,
        )
        .await?;
        summary.units.push(UnitFact {
            unit: WorkUnit {
                first_span: unit_start,
                end_span: ledger.spans().len(),
                segment: map.segment_of(*block),
            },
            wall: unit_started.elapsed(),
        });
        let hits = classify_hits(
            &read_call_counts(rows),
            &seeded_keys,
            &BTreeSet::new(),
            facts.memo_hits,
        );
        let storage = storage_accounting(&anchor_keys(rows), &seeded_keys, &BTreeSet::new());
        summary.fold(*block, &facts, hits, storage);
    }
    summary.finish(&ledger, segments)
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
    let clone_ns = u64::try_from(report.clone_time.as_nanos()).unwrap_or(u64::MAX);
    summary.clone_ns = summary.clone_ns.saturating_add(clone_ns);
    summary.clone_ns_max = summary.clone_ns_max.max(clone_ns);
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

/// The process's resident memory in KiB, when the platform reports it: the
/// snapshot's memory growth beside the entry counts, reported, never asserted.
fn resident_kb() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages.saturating_mul(4))
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
    map: &SegmentMap,
    segments: usize,
) -> Result<LegSummary, String> {
    let ledger = WallLedger::new();
    let mut summary = LegSummary::new("carry");
    summary.resident_kb = (resident_kb(), None);
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
    println!(
        "relevance scope: {} (the carry's seed set)",
        corpus_relevance_source(fanouts.iter().flat_map(|(_, rows)| rows.iter())).label()
    );
    for number in first..=last {
        // The anchor's relevance set, as the trace records it: the production
        // union the seam derived before simulation, or the labelled recorded-read
        // oracle fallback for a legacy row.
        let relevance = anchors
            .get(&number)
            .map_or_else(Relevance::default, |rows| anchor_relevance(rows));
        let unit_start = ledger.spans().len();
        let unit_started = Instant::now();
        let (hub, report) = cache
            .advance_bal(
                ReadAnchor::identified(number, mode.hash(number)?),
                mode.parent_hash(number)?,
                &relevance.addresses,
            )
            .await;
        let advance_wall = unit_started.elapsed();
        fold_advance(&ledger, node, &mut summary, &report, advance_wall);
        summary.units.push(UnitFact {
            unit: WorkUnit {
                first_span: unit_start,
                end_span: ledger.spans().len(),
                segment: map.segment_of(number),
            },
            wall: advance_wall,
        });
        summary.snapshot_growth.push((number, hub.memoized_slots()));
        let write_keys: BTreeSet<Key> = report.writes.iter().copied().collect();
        if let Some(rows) = anchors.get(&number) {
            let unit_start = ledger.spans().len();
            let unit_started = Instant::now();
            let facts = drive_lanes(
                "carry",
                leg_plan(config),
                rows,
                0,
                provider,
                node,
                Some(ReadLane::Full(hub.as_ref())),
                &ledger,
            )
            .await?;
            summary.units.push(UnitFact {
                unit: WorkUnit {
                    first_span: unit_start,
                    end_span: ledger.spans().len(),
                    segment: map.segment_of(number),
                },
                wall: unit_started.elapsed(),
            });
            let keys = anchor_keys(rows);
            let hits = classify_hits(
                &read_call_counts(rows),
                &write_keys,
                &snapshot_keys,
                facts.memo_hits,
            );
            let storage = storage_accounting(&keys, &write_keys, &snapshot_keys);
            summary.fold(number, &facts, hits, storage);
            snapshot_keys.extend(keys);
        }
        snapshot_keys.extend(write_keys);
    }
    summary.resident_kb.1 = resident_kb();
    summary.finish(&ledger, segments)
}

/// Run the selected legs of one latency point and compute its ceiling.
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
    map: &SegmentMap,
    segments: usize,
    observed: &mut Vec<(String, Digests)>,
) -> Result<PointRun, String> {
    let node = Node::new(storage, account).with_plant(plant_storage);
    let cold = drive_cold(config, mode, fanouts, provider, node, map, segments).await?;
    let seed = if config.legs.contains(&LegKind::Seed) {
        Some(drive_seed(config, mode, fanouts, provider, node, map, segments).await?)
    } else {
        None
    };
    if let Some(flag) = plant {
        flag.store(true, Ordering::Relaxed);
    }
    let carry = drive_carry(config, mode, fanouts, provider, node, map, segments).await;
    if let Some(flag) = plant {
        flag.store(false, Ordering::Relaxed);
    }
    let carry = carry?;
    let mut policies: Vec<(&'static str, WallReport)> = Vec::new();
    if let Some(seed) = &seed {
        policies.push(("seed", seed.wall.clone()));
    }
    policies.push(("carry", carry.wall.clone()));
    let ceiling = eliminable_ceiling(&cold.wall, &policies)?;
    observed.push(("cold".to_string(), cold.digests.clone()));
    if let Some(seed) = &seed {
        observed.push((seed.label.to_string(), seed.digests.clone()));
    }
    observed.push((carry.label.to_string(), carry.digests.clone()));
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
    let seed_note = run.seed.as_ref().map_or_else(String::new, |seed| {
        let removed = run
            .cold
            .wall
            .bucket(WallBucket::StorageWait)
            .saturating_sub(seed.wall.bucket(WallBucket::StorageWait));
        let share = removed.saturating_mul(1_000) / run.cold.wall_ns.max(1);
        format!(
            "; seed removes {}us = {}.{:01}%",
            us(removed),
            share / 10,
            share % 10
        )
    });
    println!(
        "  eliminable storage-wait ceiling (over a cold wall of {}us): {} removes {}us = \
         {}.{:01}% (gross {}us = {}.{:01}%){seed_note}",
        us(run.cold.wall_ns),
        run.ceiling.policy,
        us(run.ceiling.eliminated_ns),
        run.ceiling.share_thousandths / 10,
        run.ceiling.share_thousandths % 10,
        us(run.ceiling.eliminated_gross_ns),
        run.ceiling.gross_share_thousandths / 10,
        run.ceiling.gross_share_thousandths % 10,
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
    for leg in run.legs() {
        print_leg_row(leg);
    }
    for leg in run.legs() {
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
        for (base_leg, planted_leg) in base.legs().into_iter().zip(planted.legs()) {
            let delta = |bucket: WallBucket| -> i64 {
                let before = i64::try_from(pick(base_leg, bucket)).unwrap_or(i64::MAX);
                let after = i64::try_from(pick(planted_leg, bucket)).unwrap_or(i64::MAX);
                (after - before) / 1_000
            };
            println!(
                "  {:<5} {:>10} {:>11} {:>11} {:>9} {:>10} {:>10}",
                base_leg.label,
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
    let mut legs = vec![("cold", &point.cold.storage)];
    if let Some(seed) = &point.seed {
        legs.push(("seed", &seed.storage));
    }
    legs.push(("carry", &point.carry.storage));
    for (leg, accounting) in legs {
        println!(
            "    {leg:<5} keys {} carried {} final-write {} dynamic {}",
            accounting.keys, accounting.carried, accounting.final_write, accounting.dynamic
        );
    }
}

/// The real mode's carry accounting must reproduce the live `bal_carry_probe`'s
/// counts for this corpus — a discrepancy is a failing gate, never hidden, and
/// a stretch the live probe never measured has no offline verdict to trust.
fn report_live_cross_check(
    first: u64,
    last: u64,
    accounting: &StorageAccounting,
) -> Result<(), String> {
    let Some(live) = live_profile(first, last) else {
        return Err(format!(
            "the live bal_carry_probe holds no accounting for {first}..={last}: measure the stretch \
             live before replaying its real coverage"
        ));
    };
    match cross_check_live(accounting, live) {
        Ok(()) => {
            println!(
                "  live cross-check: GREEN (keys {}, carried {}, final-write {}, dynamic {} — the \
                 bal_carry_probe's counts)",
                accounting.keys, accounting.carried, accounting.final_write, accounting.dynamic
            );
            Ok(())
        }
        Err(error) => {
            println!("  live cross-check: RED ({error})");
            Err(error)
        }
    }
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
    if let (Some(synthetic_seed), Some(real_seed)) = (&synthetic.seed, &real.seed) {
        row(
            "seed wall_us / storage_us",
            format!(
                "{}/{}",
                us(synthetic_seed.wall_ns),
                bucket(synthetic_seed, WallBucket::StorageWait)
            ),
            format!(
                "{}/{}",
                us(real_seed.wall_ns),
                bucket(real_seed, WallBucket::StorageWait)
            ),
        );
    }
    row(
        "carry wall_us / storage_us",
        format!(
            "{}/{}",
            us(synthetic.carry.wall_ns),
            bucket(&synthetic.carry, WallBucket::StorageWait)
        ),
        format!(
            "{}/{}",
            us(real.carry.wall_ns),
            bucket(&real.carry, WallBucket::StorageWait)
        ),
    );
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
    if let (Some(synthetic_seed), Some(real_seed)) = (&synthetic.seed, &real.seed) {
        row(
            "seed keys (distinct: carried/final-write/dyn)",
            format!(
                "{}/{}/{}",
                synthetic_seed.storage.carried,
                synthetic_seed.storage.final_write,
                synthetic_seed.storage.dynamic
            ),
            format!(
                "{}/{}/{}",
                real_seed.storage.carried, real_seed.storage.final_write, real_seed.storage.dynamic
            ),
        );
    }

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

/// A ratio as `whole.milli` for the report's mean columns.
fn milli(sum: u64, count: u64) -> String {
    let scaled = sum.saturating_mul(1_000) / count.max(1);
    format!("{}.{:03}", scaled / 1_000, scaled % 1_000)
}

/// The corpus's density segmentation: the per-anchor profiles, one segment per
/// recorded regime with its read-set diversity and real write overlap, and the
/// regimes the corpus never recorded named as absent.
fn print_segments(
    corpus: &CorpusShape,
    profiles: &[AnchorProfile],
    segments: &[Segment],
    real: &RealCoverage,
) -> Result<(), String> {
    println!();
    println!(
        "corpus shape: {} candidates, {} candidate anchors, {} candidate-anchor keys, {} anchored \
         keys, {} global keys, {} contracts",
        corpus.candidates,
        corpus.candidate_anchors,
        corpus.candidate_anchor_keys,
        corpus.anchored_keys,
        corpus.global_keys,
        corpus.contracts
    );
    println!("per-anchor density and read-set novelty:");
    println!(
        "  {:>8}  {:<8} {:>6} {:>8} {:>7} {:>5} {:>8}",
        "block", "regime", "cands", "reads", "keys", "new", "contracts"
    );
    for profile in profiles {
        let regime = profile.regime.label();
        let block = profile.block;
        let cands = profile.candidates;
        let reads = profile.read_calls;
        let keys = profile.keys;
        let new = profile.new_keys;
        let contracts = profile.contracts;
        println!(
            "  {block:>8}  {regime:<8} {cands:>6} {reads:>8} {keys:>7} {new:>5} {contracts:>8}"
        );
    }
    println!(
        "per-segment density regimes (age = blocks since a key's corpus-first appearance; writes \
         = read-set keys the segment's real lists write / all such writes):"
    );
    println!(
        "  {:<8} {:>4} {:>6} {:>8} {:>7} {:>5} {:>8} {:>7} {:>8} {:>7} {:>8} {:>8} {:>8} {:>11}",
        "regime",
        "anc",
        "cands",
        "reads",
        "keys",
        "new",
        "repeated",
        "contr",
        "age-mean",
        "age-max",
        "survive",
        "recurred",
        "surviving",
        "writes"
    );
    for segment in segments {
        let span_first = *segment.anchors.first().expect("a segment holds an anchor");
        let span_last = *segment.anchors.last().expect("a segment holds an anchor");
        let overlap = write_overlap(&segment.keys, real, span_first, span_last)?;
        let regime = segment.regime.label();
        let anchors = segment.anchors.len();
        let cands = segment.candidates;
        let reads = segment.read_calls;
        let keys = segment.keys.len();
        let new = segment.new_keys;
        let repeated = segment.repeated_keys;
        let contracts = segment.contracts;
        let age_mean = milli(segment.key_age_sum_blocks, segment.key_age_pairs);
        let age_max = segment.key_age_max_blocks;
        let key_count = u64::try_from(keys).unwrap_or(u64::MAX);
        let survive = milli(segment.survival_anchor_sum, key_count);
        let recurred = segment.recurred_keys;
        let surviving = segment.surviving_keys;
        let written = overlap.written_in_span;
        let span_writes = overlap.span_writes;
        println!(
            "  {regime:<8} {anchors:>4} {cands:>6} {reads:>8} {keys:>7} {new:>5} {repeated:>8} \
             {contracts:>7} {age_mean:>8} {age_max:>7} {survive:>8} {recurred:>8} {surviving:>8} \
             {written:>6}/{span_writes:<4}"
        );
    }
    let absent: Vec<&str> = [
        DensityRegime::Sparse,
        DensityRegime::Moderate,
        DensityRegime::Burst,
    ]
    .into_iter()
    .filter(|regime| !segments.iter().any(|segment| segment.regime == *regime))
    .map(DensityRegime::label)
    .collect();
    if absent.is_empty() {
        println!("density regimes: sparse, moderate, and burst all recorded in this corpus");
    } else {
        println!(
            "density regimes absent from this corpus, never fabricated: {}",
            absent.join(", ")
        );
    }
    Ok(())
}

/// One latency point's per-segment cold-versus-carry view: the segment's work
/// units' measured wall, the gross storage wait the carry removes, and the
/// carry's distinct-key and call-level reuse by regime.
fn print_segment_replay(run: &PointRun, segments: &[Segment]) -> Result<(), String> {
    println!(
        "  per-segment cold vs carry (wall = the segment's work units measured; storage = gross \
         spans; share over the segment's cold wall):"
    );
    println!(
        "  {:<8} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>7} | {:>6} {:>8} {:>8} {:>7} | {:>8} {:>8}",
        "regime",
        "cold_us",
        "carry_us",
        "d_us",
        "cold_stor",
        "carry_stor",
        "eliminated",
        "share",
        "keys",
        "carried",
        "final-wr",
        "dynamic",
        "hit-carry",
        "hit-final"
    );
    for (index, segment) in segments.iter().enumerate() {
        let unit_wall = |leg: &LegSummary| -> u64 {
            leg.units
                .iter()
                .filter(|fact| fact.unit.segment == index)
                .map(|fact| u64::try_from(fact.wall.as_nanos()).unwrap_or(u64::MAX))
                .fold(0u64, u64::saturating_add)
        };
        let cold_wall = unit_wall(&run.cold);
        let carry_wall = unit_wall(&run.carry);
        let delta = i64::try_from(us(cold_wall)).unwrap_or(i64::MAX)
            - i64::try_from(us(carry_wall)).unwrap_or(i64::MAX);
        let cold_storage = run.cold.segment_gross[index][WallBucket::StorageWait.index()];
        let carry_storage = run.carry.segment_gross[index][WallBucket::StorageWait.index()];
        let ceiling = segment_ceiling(cold_storage, carry_storage, cold_wall)?;
        let mut storage = StorageAccounting::default();
        let mut hits = HitSplit::default();
        for fold in run
            .carry
            .anchors
            .iter()
            .filter(|fold| segment.anchors.contains(&fold.block))
        {
            storage.add(fold.storage);
            hits.add(fold.hits);
        }
        let regime = segment.regime.label();
        let cold_wall = us(cold_wall);
        let carry_wall = us(carry_wall);
        let cold_storage = us(cold_storage);
        let carry_storage = us(carry_storage);
        let eliminated = us(ceiling.eliminated_gross_ns);
        let share_whole = ceiling.share_thousandths / 10;
        let share_tenth = ceiling.share_thousandths % 10;
        let keys = storage.keys;
        let carried = storage.carried;
        let final_write = storage.final_write;
        let dynamic = storage.dynamic;
        let hit_carry = hits.carried;
        let hit_final = hits.final_write;
        println!(
            "  {regime:<8} {cold_wall:>10} {carry_wall:>10} {delta:>10} {cold_storage:>10} \
             {carry_storage:>10} {eliminated:>10} {share_whole:>4}.{share_tenth}% | {keys:>6} \
             {carried:>8} {final_write:>8} {dynamic:>7} | {hit_carry:>8} {hit_final:>8}"
        );
    }
    Ok(())
}

/// The carry leg's snapshot growth, clone cost, and resident readout: the
/// memory the working set and the relevant writes drive.
fn print_memory(carry: &LegSummary) {
    let Some((first_block, first_entries)) = carry.snapshot_growth.first().copied() else {
        return;
    };
    let (last_block, last_entries) = carry
        .snapshot_growth
        .last()
        .copied()
        .expect("a growth series holds its tail");
    let peak = carry
        .snapshot_growth
        .iter()
        .map(|(_, entries)| *entries)
        .max()
        .expect("a growth series holds a peak");
    let resident = |kb: Option<u64>| kb.map_or_else(|| "-".to_string(), |kb| kb.to_string());
    println!(
        "  carry memory: snapshot {first_entries}@{first_block} -> {last_entries}@{last_block} \
         entries (peak {peak}), clone {:.2?} total / {:.2?} max, resident {} -> {} KiB",
        Duration::from_nanos(carry.clone_ns),
        Duration::from_nanos(carry.clone_ns_max),
        resident(carry.resident_kb.0),
        resident(carry.resident_kb.1),
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
    let corpus = shape(&rows);
    let profiles = anchor_profiles(&rows);
    let segments = segments_by_density(&rows);
    let map = SegmentMap::build(&segments, first, last);
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

    print_segments(&corpus, &profiles, &segments, &real)?;

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
                &map,
                segments.len(),
                &mut observed,
            )
            .await?;
            print_point(&run);
            print_segment_replay(&run, &segments)?;
            print_memory(&run.carry);
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
                if let Err(error) = report_live_cross_check(first, last, &point.carry.storage) {
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
