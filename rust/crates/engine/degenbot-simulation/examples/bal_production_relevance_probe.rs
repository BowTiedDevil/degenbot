//! The paired relevance replay: production-derived relevance versus the
//! recorded-read oracle on the SAME captured corpus.
//!
//! # The question
//!
//! The simulation seam now records, per candidate, the `relevance` address set
//! it derived from the candidate's path and settlement facts BEFORE the
//! candidate ran. The earlier BAL probes scoped their snapshots with the
//! recorded READ addresses instead — an oracle, because those are what the run
//! turned out to touch. This probe replays one captured corpus twice, changing
//! ONLY the set that scopes each anchor's snapshot, and measures what the
//! difference costs: seedable write identities, carried/final-write/dynamic
//! hits, fetch members, rounds, wall buckets, snapshot entries, and re-read
//! gaps — per anchor and per density regime, under `Retention::Unbounded` and
//! `Retention::IdleBlocks(8)`.
//!
//! # What holds fixed
//!
//! One corpus, one candidate order, one W/F lane shape, one price point, one
//! real write-coverage capture, one modeled value chain. Both variants advance
//! EVERY block of the captured stretch through the same transition, so parent
//! links and the applied write feed are identical; only `relevance` differs.
//! The cross-leg read-digest gate then proves the difference is reuse, not
//! values: every leg must answer every candidate with the same digest.
//!
//! # Gates
//!
//! 1. Every recorded row must carry `relevance` (the pairing refuses a legacy
//!    row rather than reporting its read addresses as production relevance), and
//!    every production-scoped replay prints `production` as its scope.
//! 2. The production full-clone leg's per-anchor storage accounting must equal
//!    the committed live `bal_carry_probe` accounting for the same stretch and
//!    relevance sets — the offline replay reproduces the live pipeline's
//!    decisions exactly, anchor by anchor.
//! 3. Each variant's delta legs must reproduce its full-clone control's
//!    per-anchor accounting under `Unbounded` retention (the delta chain is a
//!    storage-layout change, not a policy change).
//! 4. Every leg's read digests must agree (one modeled chain answers them all).
//!    `BAL_PROD_PLANT_DIVERGENCE=1` skews the delta legs' storage answers by one
//!    wei, and this gate must catch it (exit 1).
//!
//! # Knobs
//!
//! `BAL_PROD_TRACE` (default `/tmp/bal_production_trace.log`),
//! `BAL_PROD_FIXTURE` (default the committed
//! `tests/fixtures/bal_real_coverage_production.json`),
//! `BAL_PROD_STORAGE_US` / `BAL_PROD_ACCOUNT_US`, `BAL_PROD_W` / `BAL_PROD_F`,
//! `BAL_PROD_IDLE` (default 8), `BAL_PROD_PLANT_DIVERGENCE`. The trace and the
//! fixture are REQUIRED: an absent corpus is a hard failure, never an empty
//! replay. Measurements land in `logs/bal_production_relevance_result.md`.
//!
//! # Regenerating the fixture (dev-only, live node)
//!
//! ```sh
//! BAL_WALL_TRACE=/tmp/bal_production_trace.log \
//! BAL_WALL_FIXTURE=crates/engine/degenbot-simulation/tests/fixtures/bal_real_coverage_production.json \
//! BAL_WALL_FIXTURE_CAPTURE=1 just bal-real-coverage-capture
//! ```
//!
//! The capture runs through the same production decode the replay reads, folds
//! only the corpus's addresses, and stores no values; the replay itself never
//! touches the network.

#![expect(
    clippy::exit,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "a benchmark probe prints its report and fails loudly on a broken fixture"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};

use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::async_fanout::{DrainPolicy, FanoutLimits};
use degenbot_simulation::sim::evm2::delta_snapshot::{
    DeltaHub, DeltaReport, DeltaSnapshotCache, Retention,
};
use degenbot_simulation::sim::evm2::read_hub::{CarryOutcome, CarryReport, ReadHub, ReadHubCache};
use degenbot_simulation::sim::ReadAnchor;

#[path = "shared/modeled_chain.rs"]
#[expect(dead_code)]
mod modeled_chain;
#[path = "shared/probe_report.rs"]
#[expect(dead_code)]
mod probe_report;
#[path = "shared/real_coverage.rs"]
#[expect(dead_code)]
mod real_coverage;
#[path = "shared/recorded_corpus.rs"]
#[expect(dead_code)]
mod recorded_corpus;
#[path = "shared/relevance_pairs.rs"]
mod relevance_pairs;
#[path = "shared/session_drive.rs"]
#[expect(dead_code)]
mod session_drive;
#[path = "shared/wall_ledger.rs"]
#[expect(dead_code)]
mod wall_ledger;

use modeled_chain::{ChainState, Coverage, StubChain};
use probe_report::{cross_check_live, identity_gate, storage_accounting, StorageAccounting};
use real_coverage::RealCoverage;
use recorded_corpus::{
    anchor_keys, classify_density, corpus_relevance_source, group_by_anchor, load_rows, shape,
    universe, CorpusShape, DensityRegime, Key, Recorded,
};
use relevance_pairs::{
    fold_regimes, pair_anchors, re_reads, retention_delta, summarize, total, traced_accounting,
    write_split, AnchorReuse, AnchorSets, GapStats, RegimeReuse, RelevanceVariant, ReuseComparison,
    WriteSplit,
};
use session_drive::{drive_lanes, LanePlan, LaneTrace, ReadEvent, ReadLane};
use wall_ledger::{
    completion_stats, reconcile, CompletionStats, Digests, Node, WallBucket, WallLedger, WallReport,
};

/// The manager-assigned production-relevance corpus.
const DEFAULT_TRACE: &str = "/tmp/bal_production_trace.log";

/// The committed real write-coverage capture of that corpus's stretch.
const DEFAULT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/bal_real_coverage_production.json"
);

/// The manager's canonical block range: the replay advances every block of it,
/// candidate anchors and intermediate blocks alike, so the parent links are the
/// captured ones.
const RANGE: (u64, u64) = (26_382_028, 26_382_051);

/// The shape the manager's capture assignment pins. The parsed corpus must
/// reproduce every count exactly, or the replay would measure a different
/// corpus than the one described.
const PINNED_SHAPE: CorpusShape = CorpusShape {
    candidates: 560,
    candidate_anchors: 20,
    candidate_anchor_keys: 31_714,
    anchored_keys: 6_788,
    global_keys: 2_615,
    contracts: 89,
};

/// The live `bal_carry_probe` totals over the same stretch with the same
/// production relevance sets (`BAL_CARRY_TRACE=/tmp/bal_production_trace.log
/// BAL_CARRY_FIRST=26382028 BAL_CARRY_LAST=26382051 just bal-carry-probe`):
/// zero carried-versus-cold value mismatches, and the accounting the offline
/// production full-clone leg must reproduce exactly.
const LIVE_TOTAL: StorageAccounting = StorageAccounting {
    keys: 6_788,
    carried: 4_018,
    final_write: 163,
    dynamic: 2_607,
};

/// The live probe's per-anchor `(block, keys, carried, final-write hits, dynamic
/// members)`, in block order — the same run, anchor by anchor.
const LIVE_ANCHORS: &[(u64, usize, usize, usize, usize)] = &[
    (26_382_028, 253, 0, 3, 250),
    (26_382_029, 55, 44, 11, 0),
    (26_382_030, 49, 41, 8, 0),
    (26_382_031, 2_204, 115, 2, 2_087),
    (26_382_032, 369, 296, 5, 68),
    (26_382_034, 51, 30, 5, 16),
    (26_382_035, 493, 300, 9, 184),
    (26_382_036, 318, 304, 14, 0),
    (26_382_037, 320, 305, 15, 0),
    (26_382_038, 224, 212, 12, 0),
    (26_382_039, 232, 217, 15, 0),
    (26_382_040, 159, 158, 1, 0),
    (26_382_041, 443, 433, 10, 0),
    (26_382_043, 188, 179, 7, 2),
    (26_382_044, 315, 305, 10, 0),
    (26_382_045, 39, 37, 2, 0),
    (26_382_046, 27, 27, 0, 0),
    (26_382_047, 392, 374, 18, 0),
    (26_382_048, 307, 299, 8, 0),
    (26_382_051, 350, 342, 8, 0),
];

/// The optimistic latency point: one storage batch / one account batch.
const OPTIMISTIC: (u64, u64) = (200, 400);

/// The pessimistic latency point.
const PESSIMISTIC: (u64, u64) = (700, 1_200);

fn env_us(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_micros)
}

fn env_count(name: &str, fallback: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(fallback)
}

/// The replay's invocation knobs.
struct Knobs {
    trace: String,
    fixture: String,
    points: Vec<(Duration, Duration)>,
    limits: FanoutLimits,
    idle: u64,
    planted: bool,
}

impl Knobs {
    fn from_env() -> Self {
        let points = match (env_us("BAL_PROD_STORAGE_US"), env_us("BAL_PROD_ACCOUNT_US")) {
            (Some(storage), Some(account)) => vec![(storage, account)],
            _ => vec![
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
        Self {
            trace: std::env::var("BAL_PROD_TRACE").unwrap_or_else(|_| DEFAULT_TRACE.to_string()),
            fixture: std::env::var("BAL_PROD_FIXTURE")
                .unwrap_or_else(|_| DEFAULT_FIXTURE.to_string()),
            points,
            limits: FanoutLimits::new(env_count("BAL_PROD_W", 2), env_count("BAL_PROD_F", 8)),
            idle: u64::try_from(env_count("BAL_PROD_IDLE", 8)).unwrap_or(u64::MAX),
            planted: std::env::var("BAL_PROD_PLANT_DIVERGENCE").is_ok_and(|value| value == "1"),
        }
    }
}

/// The loaded corpus: the paired relevance sets, the fan-outs they describe, the
/// per-anchor read keys, and the tracked identity universe.
struct Corpus {
    shape: CorpusShape,
    pairs: Vec<AnchorSets>,
    fanouts: Vec<(u64, Vec<Recorded>)>,
    read_keys: BTreeMap<u64, BTreeSet<Key>>,
    keys: BTreeSet<Key>,
    first: u64,
    last: u64,
}

/// Load the corpus, refusing a trace the manager's assignment does not describe.
fn load_corpus(trace: &str) -> Result<Corpus, String> {
    let rows = load_rows(trace)?;
    let measured = shape(&rows);
    if measured != PINNED_SHAPE {
        return Err(format!(
            "the parsed corpus {measured:?} does not match the manager's pinned {PINNED_SHAPE:?}"
        ));
    }
    if !corpus_relevance_source(&rows).is_production() {
        return Err("the corpus is not entirely production-scoped relevance".to_string());
    }
    let (keys, first, last) = universe(&rows);
    if (first, last) != RANGE {
        return Err(format!(
            "the corpus spans {first}..={last}, not the manager's {}..={}",
            RANGE.0, RANGE.1
        ));
    }
    let fanouts = group_by_anchor(rows);
    let pairs = pair_anchors(&fanouts)?;
    let read_keys = fanouts
        .iter()
        .map(|(block, rows)| (*block, anchor_keys(rows)))
        .collect();
    Ok(Corpus {
        shape: measured,
        pairs,
        fanouts,
        read_keys,
        keys,
        first,
        last,
    })
}

impl Corpus {
    /// The anchor's distinct recorded read identities, when it recorded any.
    fn read_keys(&self, block: u64) -> Option<&BTreeSet<Key>> {
        self.read_keys.get(&block)
    }

    /// The anchor's paired set, when the corpus recorded candidates there.
    fn pair(&self, block: u64) -> Option<&AnchorSets> {
        self.pairs.iter().find(|pair| pair.block == block)
    }
}

/// One replay leg's cache-and-retention policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LegKind {
    /// The production full clone, read through its per-anchor hub.
    Full,
    /// The delta chain with round-coalesced misses, unbounded.
    Delta,
    /// The same delta chain under the idle-block window.
    DeltaIdle,
}

impl LegKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Delta => "delta",
            Self::DeltaIdle => "delta-idle",
        }
    }

    const fn delta(self) -> bool {
        matches!(self, Self::Delta | Self::DeltaIdle)
    }
}

/// The hub one advance installed, kept alive for the anchor's reads.
enum AdvanceHub<'p> {
    Full(Arc<ReadHub<'p>>),
    Delta(Arc<DeltaHub<'p>>),
}

/// What one block's advance applied and left behind.
struct AdvanceFacts {
    applied: BTreeSet<Key>,
    overwrites: usize,
    skipped: usize,
    snapshot: usize,
    live: Option<usize>,
    cold: bool,
}

/// One leg's measured record.
struct LegRecord {
    variant: RelevanceVariant,
    kind: LegKind,
    wall: WallReport,
    completions: CompletionStats,
    anchors: BTreeMap<u64, AnchorReuse>,
    /// The identities each block's final writes applied — the set the two
    /// variants can disagree on even when their counts coincide.
    applied: BTreeMap<u64, BTreeSet<Key>>,
    reads: Vec<ReadEvent>,
    digests: Digests,
    bal_fetches: u64,
    snapshot_final: usize,
    live_final: usize,
    cold_blocks: usize,
}

impl LegRecord {
    fn label(&self) -> String {
        format!("{}/{}", self.variant.label(), self.kind.label())
    }

    /// The leg's folded per-anchor totals.
    fn total(&self) -> AnchorReuse {
        total(&self.anchors)
    }
}

/// The six legs of one latency point.
struct LegSet {
    legs: Vec<LegRecord>,
}

impl LegSet {
    fn get(&self, variant: RelevanceVariant, kind: LegKind) -> Result<&LegRecord, String> {
        self.legs
            .iter()
            .find(|leg| leg.variant == variant && leg.kind == kind)
            .ok_or_else(|| format!("the point has no {}/{kind:?} leg", variant.label()))
    }
}

fn leg_plan(limits: FanoutLimits, trace: LaneTrace) -> LanePlan {
    LanePlan {
        limits,
        shared_hub: true,
        bounded: true,
        policy: DrainPolicy::SessionWide,
        trace,
    }
}

/// Time one full-clone advance into the leg's buckets.
fn charge_advance_full(ledger: &WallLedger, node: Node, report: &CarryReport, wall: Duration) {
    ledger.record_fixed(WallBucket::BalFetchDecode, node.bal());
    ledger.record_fixed(WallBucket::BalFetchDecode, report.bal_latency);
    ledger.record_fixed(WallBucket::SnapshotCloneUpdate, report.clone_time);
    let applied = wall
        .saturating_sub(report.bal_latency)
        .saturating_sub(report.clone_time);
    ledger.record_fixed(WallBucket::SnapshotCloneUpdate, applied);
}

/// Time one delta advance into the leg's buckets: the list fetch/decode, the
/// link, and the compaction fold a threshold advance pays.
fn charge_advance_delta(ledger: &WallLedger, node: Node, report: &DeltaReport, wall: Duration) {
    let compaction = report
        .compaction
        .map_or(Duration::ZERO, |fold| fold.elapsed);
    ledger.record_fixed(WallBucket::BalFetchDecode, node.bal());
    ledger.record_fixed(WallBucket::BalFetchDecode, report.bal_latency);
    ledger.record_fixed(WallBucket::SnapshotCloneUpdate, report.link_time);
    ledger.record_fixed(WallBucket::SnapshotCloneUpdate, compaction);
    let applied = wall
        .saturating_sub(report.bal_latency)
        .saturating_sub(report.link_time)
        .saturating_sub(compaction);
    ledger.record_fixed(WallBucket::SnapshotCloneUpdate, applied);
}

/// The anchor pair one block's advance is handed: the block's own exact-hash
/// anchor and its captured parent hash.
fn anchors(real: &RealCoverage, number: u64) -> Result<(ReadAnchor, B256), String> {
    Ok((
        ReadAnchor::identified(number, real.hash(number)?),
        real.parent_hash(number)?,
    ))
}

/// Drive one block's advance through the leg's cache and report what it applied.
async fn advance<'p>(
    control: Option<&ReadHubCache<'p>>,
    delta: Option<&DeltaSnapshotCache<'p>>,
    real: &RealCoverage,
    number: u64,
    relevance: &[Address],
    ledger: &WallLedger,
    node: Node,
) -> Result<(AdvanceHub<'p>, AdvanceFacts), String> {
    let (anchor, parent) = anchors(real, number)?;
    let started = Instant::now();
    match (control, delta) {
        (Some(control), _) => {
            let (hub, report) = control.advance_bal(anchor, parent, relevance).await;
            charge_advance_full(ledger, node, &report, started.elapsed());
            let facts = AdvanceFacts {
                applied: report.writes.iter().copied().collect(),
                overwrites: report.writes_overwritten,
                skipped: report.writes_skipped,
                snapshot: hub.memoized_slots(),
                live: None,
                cold: matches!(report.outcome, CarryOutcome::Cold { .. }),
            };
            Ok((AdvanceHub::Full(hub), facts))
        }
        (_, Some(delta)) => {
            let (hub, report) = delta.advance_bal(anchor, parent, relevance).await;
            charge_advance_delta(ledger, node, &report, started.elapsed());
            let facts = AdvanceFacts {
                applied: report.writes.iter().copied().collect(),
                overwrites: report.writes_overwritten,
                skipped: report.writes_skipped,
                snapshot: hub.snapshot_len(),
                live: Some(hub.live_len()),
                cold: matches!(report.outcome, CarryOutcome::Cold { .. }),
            };
            Ok((AdvanceHub::Delta(hub), facts))
        }
        (None, None) => Err("a leg owns exactly one cache".to_string()),
    }
}

/// What every leg of one latency point shares: the corpus, its capture, the
/// modeled chain, the price point, and the divergence plant.
struct LegContext<'a> {
    knobs: &'a Knobs,
    corpus: &'a Corpus,
    real: &'a RealCoverage,
    provider: &'a AlloyProvider,
    node: Node,
    plant: &'a Arc<AtomicBool>,
}

/// Drive one anchor's recorded candidates through the leg's lane, arming the
/// divergence plant only while a delta leg answers.
async fn drive_anchor(
    hub: &AdvanceHub<'_>,
    rows: &[Recorded],
    kind: LegKind,
    limits: FanoutLimits,
    ledger: &WallLedger,
    context: &LegContext<'_>,
) -> Result<session_drive::LegFacts, String> {
    let lane = match hub {
        AdvanceHub::Full(hub) => ReadLane::Full(hub.as_ref()),
        AdvanceHub::Delta(hub) => ReadLane::Delta(hub.as_ref()),
    };
    let trace = if kind.delta() {
        LaneTrace::Reads
    } else {
        LaneTrace::Off
    };
    context
        .plant
        .store(kind.delta() && context.knobs.planted, Ordering::Relaxed);
    let driven = drive_lanes(
        kind.label(),
        leg_plan(limits, trace),
        rows,
        0,
        context.provider,
        context.node,
        Some(lane),
        ledger,
    )
    .await;
    context.plant.store(false, Ordering::Relaxed);
    driven
}

/// Drive one leg over the corpus: every block advanced, every anchor's recorded
/// candidates read through the leg's lane.
async fn drive_leg(
    kind: LegKind,
    variant: RelevanceVariant,
    context: &LegContext<'_>,
) -> Result<LegRecord, String> {
    let LegContext {
        knobs,
        corpus,
        real,
        provider,
        node,
        plant,
    } = *context;
    let by_anchor: BTreeMap<u64, &[Recorded]> = corpus
        .fanouts
        .iter()
        .map(|(block, rows)| (*block, rows.as_slice()))
        .collect();
    let ledger = WallLedger::new();
    let mut record = LegRecord {
        variant,
        kind,
        wall: WallReport::default(),
        completions: CompletionStats::default(),
        anchors: BTreeMap::new(),
        applied: BTreeMap::new(),
        reads: Vec::new(),
        digests: Digests::new(),
        bal_fetches: 0,
        snapshot_final: 0,
        live_final: 0,
        cold_blocks: 0,
    };
    let mut snapshot_keys: BTreeSet<Key> = BTreeSet::new();
    let control = (kind == LegKind::Full).then(|| ReadHubCache::new(provider));
    let delta = kind.delta().then(|| match kind {
        LegKind::DeltaIdle => {
            DeltaSnapshotCache::with_retention(provider, Retention::IdleBlocks(knobs.idle))
        }
        _ => DeltaSnapshotCache::new(provider),
    });
    plant.store(false, Ordering::Relaxed);
    for number in corpus.first..=corpus.last {
        let relevance: &[Address] = corpus.pair(number).map_or(&[], |pair| pair.set(variant));
        let (hub, facts) = advance(
            control.as_ref(),
            delta.as_ref(),
            real,
            number,
            relevance,
            &ledger,
            node,
        )
        .await?;
        record.bal_fetches = record.bal_fetches.saturating_add(1);
        record.cold_blocks = record.cold_blocks.saturating_add(usize::from(facts.cold));
        record.snapshot_final = facts.snapshot;
        record.live_final = facts.live.unwrap_or(record.live_final);
        record.applied.insert(number, facts.applied.clone());
        snapshot_keys.extend(facts.applied.iter().copied());
        let Some(rows) = by_anchor.get(&number) else {
            continue;
        };
        let keys = corpus.read_keys(number).cloned().unwrap_or_default();
        let driven = drive_anchor(&hub, rows, kind, knobs.limits, &ledger, context).await?;
        // A delta leg classified the answer path of every read it made, so its
        // accounting is measured per policy — an expiring retention included. The
        // full-clone lane does not classify, so it is accounted from the held set,
        // and the delta-vs-full gate reconciles the two derivations.
        let storage = if kind.delta() {
            traced_accounting(&keys, &facts.applied, &driven.reads)
        } else {
            storage_accounting(&keys, &facts.applied, &snapshot_keys)
        };
        record.anchors.insert(
            number,
            AnchorReuse {
                storage,
                seeds: facts.applied.len().saturating_sub(facts.overwrites),
                overwrites: facts.overwrites,
                skipped: facts.skipped,
                storage_members: driven.storage_fetches,
                account_members: driven.account_fetches,
                rounds: driven.rounds,
                memo_hits: driven.memo_hits,
                coalesced: driven.coalesced,
                snapshot: facts.snapshot,
                live: facts.live,
            },
        );
        snapshot_keys.extend(keys);
        record.reads.extend(driven.reads);
        record.digests.extend(driven.digests);
    }
    record.wall = reconcile(&ledger.spans(), ledger.offset())?;
    record.completions = completion_stats(ledger.completions())?;
    Ok(record)
}

fn us(ns: u64) -> u64 {
    ns / 1_000
}

/// One leg's storage accounting totals.
fn accounting_of(anchors: &BTreeMap<u64, AnchorReuse>) -> StorageAccounting {
    let mut accounting = StorageAccounting::default();
    for measured in anchors.values() {
        accounting.add(measured.storage);
    }
    accounting
}

/// Print the corpus and the per-anchor relevance sets with their differences.
fn print_sets(corpus: &Corpus, real: &RealCoverage) {
    println!("### relevance sets per anchor (identical candidates, two derivations)");
    println!();
    println!(
        "| block | regime | cands | prod addr | oracle addr | shared | excluded | unread | real \
         writes | prod in/out | oracle in/out |"
    );
    println!("| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- |");
    let mut totals = [0usize; 8];
    for pair in &corpus.pairs {
        let difference = pair.difference();
        let writes = real.writes_at(pair.block);
        let production: WriteSplit = write_split(writes, &pair.production);
        let oracle: WriteSplit = write_split(writes, &pair.oracle);
        totals[0] += pair.production.len();
        totals[1] += pair.oracle.len();
        totals[2] += difference.shared;
        totals[3] += difference.excluded_read.len();
        totals[4] += difference.unread_relevant.len();
        totals[5] += writes.len();
        totals[6] += production.inside;
        totals[7] += oracle.inside;
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {}/{} | {}/{} |",
            pair.block,
            pair.regime().label(),
            pair.candidates,
            pair.production.len(),
            pair.oracle.len(),
            difference.shared,
            difference.excluded_read.len(),
            difference.unread_relevant.len(),
            writes.len(),
            production.inside,
            production.outside,
            oracle.inside,
            oracle.outside,
        );
    }
    println!(
        "| totals | | {} | {} | {} | {} | {} | {} | {} | {}/{} | {}/{} |",
        corpus.shape.candidates,
        totals[0],
        totals[1],
        totals[2],
        totals[3],
        totals[4],
        totals[5],
        totals[6],
        totals[5].saturating_sub(totals[6]),
        totals[7],
        totals[5].saturating_sub(totals[7]),
    );
    println!();
}

/// Print every excluded post-read-only address the anchor's list writes, plus
/// the count of exclusions the block's writes cannot reach.
fn print_exclusions(corpus: &Corpus, real: &RealCoverage) {
    println!("### excluded post-read-only addresses (read, outside production relevance)");
    println!();
    println!("| block | address | writes at the anchor | in the anchor's read keys |");
    println!("| ---: | --- | ---: | --- |");
    let (mut listed, mut unreachable) = (0usize, 0usize);
    for pair in &corpus.pairs {
        let Some(read_keys) = corpus.read_keys(pair.block) else {
            continue;
        };
        for address in &pair.difference().excluded_read {
            let writes = real
                .writes_at(pair.block)
                .iter()
                .filter(|(written, _)| written == address)
                .count();
            if writes == 0 {
                unreachable += 1;
                continue;
            }
            listed += 1;
            let read_slots = read_keys
                .iter()
                .any(|(key_address, _)| key_address == address);
            println!(
                "| {} | {address:#x} | {writes} | {} |",
                pair.block,
                read_slot_label(read_slots)
            );
        }
    }
    println!();
    println!(
        "{listed} exclusions the anchor's list writes (a seed a production-scoped snapshot cannot \
         make at an unknown slot); {unreachable} exclusions wrote nothing at their anchor"
    );
    println!();
}

const fn read_slot_label(read: bool) -> &'static str {
    if read {
        "yes"
    } else {
        "no"
    }
}

/// Print the per-anchor paired reuse under the two unbounded delta legs.
fn print_paired_anchors(
    corpus: &Corpus,
    production: &BTreeMap<u64, AnchorReuse>,
    oracle: &BTreeMap<u64, AnchorReuse>,
) {
    println!("### paired reuse per anchor — delta legs, unbounded retention");
    println!();
    println!(
        "| block | regime | carried P/O | final P/O | dynamic P/O | dDyn | seeds P/O | overwrites \
         P/O | skip P/O | members P/O | rounds P/O | snapshot P/O | live P/O |"
    );
    println!("| ---: | --- | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- |");
    for pair in &corpus.pairs {
        let (Some(measured), Some(measured_oracle)) =
            (production.get(&pair.block), oracle.get(&pair.block))
        else {
            continue;
        };
        let comparison = ReuseComparison::new(*measured, *measured_oracle);
        println!(
            "| {} | {} | {}/{} | {}/{} | {}/{} | {} | {}/{} | {}/{} | {}/{} | {}/{} | {}/{} | \
             {}/{} | {}/{} |",
            pair.block,
            pair.regime().label(),
            comparison.production.storage.carried,
            comparison.oracle.storage.carried,
            comparison.production.storage.final_write,
            comparison.oracle.storage.final_write,
            comparison.production.storage.dynamic,
            comparison.oracle.storage.dynamic,
            comparison.dynamic_delta(),
            comparison.production.seeds,
            comparison.oracle.seeds,
            comparison.production.overwrites,
            comparison.oracle.overwrites,
            comparison.production.skipped,
            comparison.oracle.skipped,
            comparison.production.storage_members,
            comparison.oracle.storage_members,
            comparison.production.rounds,
            comparison.oracle.rounds,
            comparison.production.snapshot,
            comparison.oracle.snapshot,
            comparison.production.live.unwrap_or_default(),
            comparison.oracle.live.unwrap_or_default(),
        );
    }
    println!();
}

/// Print where production relevance lost or gained reuse: the paired per-anchor
/// deltas, sign stated by the header.
fn print_reuse_deltas(
    corpus: &Corpus,
    production: &BTreeMap<u64, AnchorReuse>,
    oracle: &BTreeMap<u64, AnchorReuse>,
    applied: &BTreeMap<u64, BTreeSet<Key>>,
    applied_oracle: &BTreeMap<u64, BTreeSet<Key>>,
) {
    println!("### reuse deltas per anchor (production minus oracle) — delta legs, unbounded");
    println!();
    println!(
        "| block | regime | dCarried | dFinal | dDynamic | dSeeds | dOverwrites | dSkipped | \
         dSnapshot | dMembers | dRounds | applied P-only/O-only |"
    );
    println!("| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |");
    let mut totals = ReuseComparison::default();
    for pair in &corpus.pairs {
        let (Some(measured), Some(measured_oracle)) =
            (production.get(&pair.block), oracle.get(&pair.block))
        else {
            continue;
        };
        let comparison = ReuseComparison::new(*measured, *measured_oracle);
        totals.production.add(*measured);
        totals.oracle.add(*measured_oracle);
        let empty = BTreeSet::new();
        let theirs = applied_oracle.get(&pair.block).unwrap_or(&empty);
        let ours = applied.get(&pair.block).unwrap_or(&empty);
        let only_production = ours.difference(theirs).count();
        let only_oracle = theirs.difference(ours).count();
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {only_production}/{only_oracle} |",
            pair.block,
            pair.regime().label(),
            comparison.carried_delta(),
            comparison.final_write_delta(),
            comparison.dynamic_delta(),
            comparison.seed_delta(),
            comparison.overwrite_delta(),
            comparison.skip_delta(),
            comparison.snapshot_delta(),
            comparison.storage_member_delta(),
            comparison.round_delta(),
        );
    }
    let all_production: BTreeSet<Key> = applied.values().flatten().copied().collect();
    let all_oracle: BTreeSet<Key> = applied_oracle.values().flatten().copied().collect();
    let only_production = all_production.difference(&all_oracle).count();
    let only_oracle = all_oracle.difference(&all_production).count();
    println!(
        "| totals | | {} | {} | {} | {} | {} | {} | {} | {} | {} | {only_production}/{only_oracle} |",
        totals.carried_delta(),
        totals.final_write_delta(),
        totals.dynamic_delta(),
        totals.seed_delta(),
        totals.overwrite_delta(),
        totals.skip_delta(),
        totals.snapshot_delta(),
        totals.storage_member_delta(),
        totals.round_delta(),
    );
    println!();
    println!(
        "positive dDynamic/dMembers/dRounds means production paid more; positive dCarried/dFinal/         dSeeds/dSnapshot means production held more"
    );
    println!();
}

/// Print the regime fold: the same paired reuse aggregated by density regime.
fn print_regimes(
    corpus: &Corpus,
    regimes: &[RegimeReuse],
    production: &[ReadEvent],
    oracle: &[ReadEvent],
) {
    println!("### paired reuse per regime — delta legs, unbounded retention");
    println!();
    println!(
        "| regime | anchors | cands | prod addr | oracle addr | excluded | unread | carried P/O | \
         final P/O | dynamic P/O | dDyn | members P/O | rounds P/O | snapshot P/O | gap p50 P/O | \
         gap max P/O |"
    );
    println!(
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | --- | ---: | --- | --- | \
         --- | --- | --- |"
    );
    let production_gaps = regime_gaps(corpus, production);
    let oracle_gaps = regime_gaps(corpus, oracle);
    for regime in regimes {
        let comparison = &regime.reuse;
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {}/{} | {}/{} | {}/{} | {} | {}/{} | {}/{} | \
             {}/{} | {}/{} | {}/{} |",
            regime.regime.label(),
            regime.anchors,
            regime.candidates,
            regime.production_addresses,
            regime.oracle_addresses,
            regime.excluded_read,
            regime.unread_relevant,
            comparison.production.storage.carried,
            comparison.oracle.storage.carried,
            comparison.production.storage.final_write,
            comparison.oracle.storage.final_write,
            comparison.production.storage.dynamic,
            comparison.oracle.storage.dynamic,
            comparison.dynamic_delta(),
            comparison.production.storage_members,
            comparison.oracle.storage_members,
            comparison.production.rounds,
            comparison.oracle.rounds,
            comparison.production.snapshot,
            comparison.oracle.snapshot,
            gap_cell(production_gaps.get(&regime.regime), GapField::P50),
            gap_cell(oracle_gaps.get(&regime.regime), GapField::P50),
            gap_cell(production_gaps.get(&regime.regime), GapField::Max),
            gap_cell(oracle_gaps.get(&regime.regime), GapField::Max),
        );
    }
    println!();
}

/// The gap sample fields the regime table's cells print.
#[derive(Clone, Copy)]
enum GapField {
    P50,
    Max,
}

const fn gap_value(stats: GapStats, field: GapField) -> u64 {
    match field {
        GapField::P50 => stats.p50,
        GapField::Max => stats.max,
    }
}

fn gap_cell(stats: Option<&GapStats>, field: GapField) -> String {
    stats.map_or_else(
        || "-".to_string(),
        |stats| gap_value(*stats, field).to_string(),
    )
}

/// The re-read gap distribution per density regime for one traced leg.
fn regime_gaps(corpus: &Corpus, reads: &[ReadEvent]) -> BTreeMap<DensityRegime, GapStats> {
    let mut by_regime: BTreeMap<DensityRegime, Vec<u64>> = BTreeMap::new();
    for read in re_reads(reads) {
        let candidates = corpus.pair(read.block).map_or(0, |pair| pair.candidates);
        by_regime
            .entry(classify_density(candidates))
            .or_default()
            .push(read.gap);
    }
    by_regime
        .into_iter()
        .map(|(regime, gaps)| (regime, summarize(gaps)))
        .collect()
}

/// Print the retention comparison: both variants' unbounded and idle legs.
fn print_retention(knobs: &Knobs, set: &LegSet) -> Result<(), String> {
    println!(
        "### retention: unbounded versus IdleBlocks({}) — delta legs",
        knobs.idle
    );
    println!();
    println!(
        "| variant | leg | dynamic | storage members | chain hits | snapshot (final) | live |"
    );
    println!("| --- | --- | ---: | ---: | ---: | ---: | ---: |");
    for variant in RelevanceVariant::ALL {
        for kind in [LegKind::Delta, LegKind::DeltaIdle] {
            let leg = set.get(variant, kind)?;
            let measured = leg.total();
            let hits = re_reads(&leg.reads).iter().filter(|read| read.hit).count();
            println!(
                "| {} | {} | {} | {} | {} | {} | {} |",
                variant.label(),
                kind.label(),
                measured.storage.dynamic,
                measured.storage_members,
                hits,
                leg.snapshot_final,
                leg.live_final,
            );
        }
    }
    println!();
    println!(
        "| variant | hits | over-window hit re-reads | converted | lost hits | extra members | \
         live unbounded -> idle |"
    );
    println!("| --- | ---: | ---: | ---: | ---: | ---: | --- |");
    for variant in RelevanceVariant::ALL {
        let unbounded = set.get(variant, LegKind::Delta)?;
        let windowed = set.get(variant, LegKind::DeltaIdle)?;
        // The live gauge is the LAST anchor's, not the peak a fold reports.
        let mut unbounded_total = unbounded.total();
        unbounded_total.live = Some(unbounded.live_final);
        let mut windowed_total = windowed.total();
        windowed_total.live = Some(windowed.live_final);
        let delta = retention_delta(
            &unbounded.reads,
            &windowed.reads,
            &unbounded_total,
            &windowed_total,
            knobs.idle,
        );
        println!(
            "| {} | {} | {} | {} | {} | {} | {} -> {} |",
            variant.label(),
            delta.hits,
            delta.over_window,
            delta.converted,
            delta.lost_hits,
            delta.extra_members,
            delta.live.0,
            delta.live.1,
        );
    }
    println!();
    Ok(())
}

/// Print the anchors where the two variants' WINDOWED legs diverge: under
/// unbounded retention a seeding decision only bites at a slot's first
/// appearance, while an expiry makes an old slot unknown again and lets the
/// relevance set decide whether it is re-seeded.
fn print_idle_divergence(set: &LegSet) -> Result<(), String> {
    let production = set.get(RelevanceVariant::Production, LegKind::DeltaIdle)?;
    let oracle = set.get(RelevanceVariant::Oracle, LegKind::DeltaIdle)?;
    println!("#### anchors where the windowed legs diverge (seeds/overwrites/skips/snapshot)");
    println!();
    println!("| block | seeds P/O | overwrites P/O | skips P/O | snapshot P/O | live P/O |");
    println!("| ---: | --- | --- | --- | --- | --- |");
    let mut divergent = 0usize;
    for (block, measured) in &production.anchors {
        let Some(other) = oracle.anchors.get(block) else {
            return Err(format!(
                "the oracle windowed leg measured no anchor at {block}"
            ));
        };
        if measured.seeds == other.seeds
            && measured.overwrites == other.overwrites
            && measured.skipped == other.skipped
            && measured.snapshot == other.snapshot
        {
            continue;
        }
        divergent += 1;
        println!(
            "| {block} | {}/{} | {}/{} | {}/{} | {}/{} | {}/{} |",
            measured.seeds,
            other.seeds,
            measured.overwrites,
            other.overwrites,
            measured.skipped,
            other.skipped,
            measured.snapshot,
            other.snapshot,
            measured.live.unwrap_or_default(),
            other.live.unwrap_or_default(),
        );
    }
    println!();
    println!(
        "{divergent} of {} anchors diverge under the window; every other anchor applies the same \
         writes under both relevance sets",
        production.anchors.len()
    );
    println!();
    Ok(())
}

/// Print the wall buckets of every leg at one price point.
fn print_walls(set: &LegSet) {
    println!(
        "| leg | wall | exec | storage | account | bal | snapshot | other | rounds | storage \
             members | account members | bal fetches | p50 | p95 | cold blocks |"
    );
    println!(
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | \
         ---: | ---: | ---: |"
    );
    for leg in &set.legs {
        let measured = leg.total();
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            leg.label(),
            us(leg.wall.wall_ns),
            us(leg.wall.bucket(WallBucket::RecordedExec)),
            us(leg.wall.bucket(WallBucket::StorageWait)),
            us(leg.wall.bucket(WallBucket::AccountWait)),
            us(leg.wall.bucket(WallBucket::BalFetchDecode)),
            us(leg.wall.bucket(WallBucket::SnapshotCloneUpdate)),
            us(leg.wall.bucket(WallBucket::SchedulerOther)),
            measured.rounds,
            measured.storage_members,
            measured.account_members,
            leg.bal_fetches,
            us(leg.completions.p50_ns),
            us(leg.completions.p95_ns),
            leg.cold_blocks,
        );
    }
    println!();
}

/// Print the re-read gap distributions of both variants' delta legs.
fn print_gaps(set: &LegSet, knobs: &Knobs) -> Result<(), String> {
    println!("### re-read gaps — delta legs");
    println!();
    println!("| variant | leg | re-reads | p50 | p95 | p99 | max | chain hits | over the window |");
    println!("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
    for variant in RelevanceVariant::ALL {
        for kind in [LegKind::Delta, LegKind::DeltaIdle] {
            let leg = set.get(variant, kind)?;
            let reads = re_reads(&leg.reads);
            let stats = summarize(reads.iter().map(|read| read.gap).collect());
            let hits = reads.iter().filter(|read| read.hit).count();
            let over = reads
                .iter()
                .filter(|read| read.hit && read.gap > knobs.idle)
                .count();
            println!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                variant.label(),
                kind.label(),
                stats.count,
                stats.p50,
                stats.p95,
                stats.p99,
                stats.max,
                hits,
                over,
            );
        }
    }
    println!();
    Ok(())
}

/// Reconcile the offline production full-clone leg against the committed live
/// `bal_carry_probe` accounting, anchor by anchor.
fn reconcile_live(set: &LegSet) -> Result<(), String> {
    let leg = set.get(RelevanceVariant::Production, LegKind::Full)?;
    let offline = accounting_of(&leg.anchors);
    println!(
        "### live reconciliation — offline production full-clone leg vs live `bal_carry_probe`"
    );
    println!();
    println!("| accounting | offline replay | live probe |");
    println!("| --- | ---: | ---: |");
    for (label, offline, live) in [
        ("keys", offline.keys, LIVE_TOTAL.keys),
        ("carried", offline.carried, LIVE_TOTAL.carried),
        ("final-write", offline.final_write, LIVE_TOTAL.final_write),
        ("dynamic", offline.dynamic, LIVE_TOTAL.dynamic),
    ] {
        println!("| {label} | {offline} | {live} |");
    }
    println!();
    cross_check_live(&offline, &LIVE_TOTAL)?;
    let mut committed: StorageAccounting = StorageAccounting::default();
    for (block, keys, carried, final_write, dynamic) in LIVE_ANCHORS {
        let measured = leg
            .anchors
            .get(block)
            .ok_or_else(|| format!("the offline replay measured no anchor at block {block}"))?;
        let live = StorageAccounting {
            keys: *keys,
            carried: *carried,
            final_write: *final_write,
            dynamic: *dynamic,
        };
        cross_check_live(&measured.storage, &live)
            .map_err(|error| format!("block {block}: {error}"))?;
        committed.add(live);
    }
    cross_check_live(&committed, &LIVE_TOTAL)
        .map_err(|error| format!("the committed per-anchor live rows: {error}"))?;
    println!(
        "offline and live agree exactly: {} anchors, every key classified the same way",
        leg.anchors.len()
    );
    println!();
    Ok(())
}

/// Every leg must answer every candidate with the same read digest, and replay
/// the same candidate count.
fn gate_digests(set: &LegSet) -> Vec<String> {
    let Ok(reference) = set.get(RelevanceVariant::Production, LegKind::Full) else {
        return vec!["the point has no production full-clone reference leg".to_string()];
    };
    let legs: Vec<(String, Digests)> = set
        .legs
        .iter()
        .filter(|leg| leg.label() != reference.label())
        .map(|leg| (leg.label(), leg.digests.clone()))
        .collect();
    let mut mismatches = identity_gate(&reference.digests, &legs);
    for leg in &set.legs {
        if leg.digests.len() != reference.digests.len() {
            mismatches.push(format!(
                "{} replayed {} candidates, the reference {}",
                leg.label(),
                leg.digests.len(),
                reference.digests.len()
            ));
        }
    }
    mismatches
}

/// Each variant's unbounded delta leg must reproduce its full-clone control's
/// per-anchor accounting: relevance scoping changes reuse, never the split.
fn gate_delta_accounting(set: &LegSet) -> Vec<String> {
    let mut mismatches = Vec::new();
    for variant in RelevanceVariant::ALL {
        let Ok(full) = set.get(variant, LegKind::Full) else {
            mismatches.push(format!("{}: no full-clone control", variant.label()));
            continue;
        };
        let Ok(delta) = set.get(variant, LegKind::Delta) else {
            mismatches.push(format!("{}: no delta leg", variant.label()));
            continue;
        };
        for (block, measured) in &full.anchors {
            match delta.anchors.get(block) {
                Some(other) if other.storage == measured.storage => {}
                Some(other) => mismatches.push(format!(
                    "{} block {block}: full {:?} vs delta {:?}",
                    variant.label(),
                    measured.storage,
                    other.storage
                )),
                None => mismatches.push(format!(
                    "{} block {block}: the delta leg measured no anchor",
                    variant.label()
                )),
            }
        }
    }
    mismatches
}

/// Run one latency point: six legs, the gates, and the point's report.
async fn run_point(
    knobs: &Knobs,
    corpus: &Corpus,
    real: &RealCoverage,
    provider: &AlloyProvider,
    plant: &Arc<AtomicBool>,
    storage: Duration,
    account: Duration,
) -> Result<(LegSet, Vec<String>), String> {
    let context = LegContext {
        knobs,
        corpus,
        real,
        provider,
        node: Node::new(storage, account),
        plant,
    };
    let mut legs = Vec::new();
    for variant in RelevanceVariant::ALL {
        for kind in [LegKind::Full, LegKind::Delta, LegKind::DeltaIdle] {
            legs.push(drive_leg(kind, variant, &context).await?);
        }
    }
    let set = LegSet { legs };
    println!(
        "## latency point {}us storage / {}us account",
        storage.as_micros(),
        account.as_micros()
    );
    println!();
    let production = set
        .get(RelevanceVariant::Production, LegKind::Delta)?
        .anchors
        .clone();
    let oracle = set
        .get(RelevanceVariant::Oracle, LegKind::Delta)?
        .anchors
        .clone();
    let regimes = fold_regimes(&corpus.pairs, &production, &oracle)?;
    print_paired_anchors(corpus, &production, &oracle);
    print_reuse_deltas(
        corpus,
        &production,
        &oracle,
        &set.get(RelevanceVariant::Production, LegKind::Delta)?
            .applied,
        &set.get(RelevanceVariant::Oracle, LegKind::Delta)?.applied,
    );
    print_regimes(
        corpus,
        &regimes,
        &set.get(RelevanceVariant::Production, LegKind::Delta)?.reads,
        &set.get(RelevanceVariant::Oracle, LegKind::Delta)?.reads,
    );
    print_retention(knobs, &set)?;
    print_idle_divergence(&set)?;
    print_walls(&set);
    print_gaps(&set, knobs)?;
    let mut mismatches = gate_digests(&set);
    mismatches.extend(gate_delta_accounting(&set));
    Ok((set, mismatches))
}

async fn run() -> Result<(), String> {
    let knobs = Knobs::from_env();
    let corpus = load_corpus(&knobs.trace)?;
    let real = RealCoverage::load(&knobs.fixture)?;
    if real.first() > corpus.first || real.last() < corpus.last {
        return Err(format!(
            "the fixture covers {}..={} but the corpus replays {}..={}",
            real.first(),
            real.last(),
            corpus.first,
            corpus.last
        ));
    }
    let chain = ChainState::build_coverage(
        &corpus.keys,
        corpus.first,
        corpus.last,
        &real,
        Coverage::Real,
    )?;
    let plant = Arc::new(AtomicBool::new(false));
    let stub = StubChain {
        chain: Arc::new(chain),
        plant: Arc::clone(&plant),
    };
    let provider = stub.provider();
    println!(
        "trace {} | fixture {} | W={} F={} | idle window {} | planted {}",
        knobs.trace,
        knobs.fixture,
        knobs.limits.workers(),
        knobs.limits.slots(),
        knobs.idle,
        knobs.planted
    );
    println!(
        "corpus: {} candidates, {} anchors over {}..={}, {} candidate-anchor keys, {} anchored \
         keys, {} global keys, {} addresses",
        corpus.shape.candidates,
        corpus.shape.candidate_anchors,
        corpus.first,
        corpus.last,
        corpus.shape.candidate_anchor_keys,
        corpus.shape.anchored_keys,
        corpus.shape.global_keys,
        corpus.shape.contracts,
    );
    println!(
        "relevance scope: production (every row carries its field; the oracle leg is labelled \
         {})",
        RelevanceVariant::Oracle.label()
    );
    println!();
    print_sets(&corpus, &real);
    print_exclusions(&corpus, &real);

    let mut gate = Vec::new();
    for (index, (storage, account)) in knobs.points.iter().enumerate() {
        let (set, mismatches) = run_point(
            &knobs, &corpus, &real, &provider, &plant, *storage, *account,
        )
        .await?;
        if index == 0 {
            reconcile_live(&set)?;
        }
        gate.extend(mismatches);
    }

    if knobs.planted {
        assert!(
            !gate.is_empty(),
            "the planted one-wei divergence must be caught by the digest gate"
        );
        println!(
            "negative probe: the digest gate caught the planted divergence ({} findings)",
            gate.len()
        );
        std::process::exit(1);
    }
    assert!(
        gate.is_empty(),
        "every gate must hold: {} findings, first {:?}",
        gate.len(),
        gate.first()
    );
    println!(
        "gates: every leg read identical digests, both variants' unbounded delta accounting \
         equals the full-clone control, and the production full-clone leg reconciles with the live \
         probe"
    );
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("bal_production_relevance_probe: {error}");
        std::process::exit(1);
    }
}
