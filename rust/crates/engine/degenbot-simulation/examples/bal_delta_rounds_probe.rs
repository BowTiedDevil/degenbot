//! The round-coalesced delta read replay: the recorded candidate scheduler
//! driven over two live-captured corpora through the full-clone control and the
//! delta chain whose storage misses take the production read hub's rounds.
//!
//! # What is compared
//!
//! Per corpus and per latency point, three legs run the SAME schedule (one
//! candidate order, one lane shape, one cost model) and differ only in the read
//! lane a candidate's walk is handed:
//!
//! - `full` — the production full-clone carry-forward
//!   ([`ReadHubCache::advance_bal`]) and its per-anchor [`ReadHub`].
//! - `delta` — the parent-linked delta chain ([`DeltaSnapshotCache`]) under
//!   [`Retention::Unbounded`]: chain hits answer locally, misses register with
//!   the generation's own hub and settle back into that generation.
//! - `delta-idle` — the same delta chain under [`Retention::IdleBlocks`] at
//!   `BAL_DELTA_ROUNDS_IDLE` (default 8): the measured question "does the
//!   observed re-read gap distribution justify this window?", never an assumed
//!   one.
//!
//! The value gate is the read digests: every candidate's read-walk digest must
//! be identical in all three legs, because one modeled chain answers them all.
//! The negative probe (`BAL_DELTA_ROUNDS_PLANT_DIVERGENCE=1`) arms a one-wei
//! skew on the storage answers ONLY while the delta legs run, which the same
//! gate must catch.
//!
//! # What is reported
//!
//! Wall buckets (reconciled to the measured wall), candidate completion
//! percentiles, the round accounting (rounds, distinct members, memo hits,
//! coalesced duplicates), the delta's chain-hit versus dynamic split, and — the
//! input the retention decision needs — the candidate RE-READ GAP
//! distribution: p50/p95/p99/max over all re-reads, split between chain hits
//! and reads that took the round path, per density regime, with the number of
//! hit re-reads whose gap exceeds the idle window beside the measured extra
//! members. Timing is reported, never asserted.
//!
//! # Knobs
//!
//! `BAL_DELTA_ROUNDS_TRACES` (comma list, default the two committed corpora:
//! `/tmp/fanout_trace.log` and `/tmp/bal_broader_trace.log`),
//! `BAL_DELTA_ROUNDS_FIXTURES` (comma list of the matching real-coverage
//! captures, default by corpus index), `BAL_DELTA_ROUNDS_STORAGE_US` /
//! `BAL_DELTA_ROUNDS_ACCOUNT_US` (default both price points, 200/400us and
//! 700/1200us), `BAL_DELTA_ROUNDS_W` / `BAL_DELTA_ROUNDS_F` (default 2/8),
//! `BAL_DELTA_ROUNDS_IDLE` (default 8), `BAL_DELTA_ROUNDS_PLANT_DIVERGENCE`.
//! Both traces are REQUIRED: an absent corpus is a hard failure, never an empty
//! replay. Measurements land in `logs/bal_delta_rounds_result.md`.

#![expect(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a benchmark probe prints its report and fails loudly on a broken fixture"
)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};

use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::async_fanout::{DrainPolicy, FanoutLimits};
use degenbot_simulation::sim::evm2::delta_snapshot::Retention;
use degenbot_simulation::sim::evm2::delta_snapshot::{DeltaReport, DeltaSnapshotCache, ReadOrigin};
use degenbot_simulation::sim::evm2::read_hub::{CarryReport, ReadHub, ReadHubCache};
use degenbot_simulation::sim::ReadAnchor;

#[path = "shared/modeled_chain.rs"]
#[expect(dead_code)]
mod modeled_chain;
#[path = "shared/real_coverage.rs"]
#[expect(dead_code)]
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

use modeled_chain::{ChainState, Coverage, StubChain};
use real_coverage::RealCoverage;
use recorded_corpus::{
    anchor_relevance, classify_density, corpus_relevance_source, group_by_anchor, load_rows,
    universe, DensityRegime, Key, Recorded, Relevance,
};
use session_drive::{drive_lanes, LanePlan, LaneTrace, LegFacts, ReadEvent, ReadLane};
use wall_ledger::{
    completion_stats, reconcile, CompletionStats, Digests, Node, WallBucket, WallLedger, WallReport,
};

/// The committed corpora: the recorded scheduler traces and the real
/// write-coverage capture that keys each stretch's lists.
const CORPORA: &[(&str, &str)] = &[
    (
        "/tmp/fanout_trace.log",
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/bal_real_coverage.json"
        ),
    ),
    (
        "/tmp/bal_broader_trace.log",
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/bal_real_coverage_broader.json"
        ),
    ),
];

/// The optimistic latency point: one storage batch / one account batch.
const OPTIMISTIC: (u64, u64) = (200, 400);

/// The pessimistic latency point.
const PESSIMISTIC: (u64, u64) = (700, 1200);

/// One corpus: its recorded trace and the real-coverage capture of its stretch.
struct Corpus {
    trace: &'static str,
    fixture: &'static str,
}

/// The replay's invocation knobs.
struct Knobs {
    corpora: Vec<Corpus>,
    points: Vec<(Duration, Duration)>,
    limits: FanoutLimits,
    idle: u64,
    planted: bool,
}

/// One replay leg's cache policy.
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

    /// Only the delta legs answer through the chain seam, so only they carry
    /// the divergence plant.
    const fn delta(self) -> bool {
        matches!(self, Self::Delta | Self::DeltaIdle)
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
        .and_then(|value| value.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(fallback)
}

fn env_list(name: &str) -> Option<Vec<String>> {
    std::env::var(name).ok().map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect()
    })
}

impl Knobs {
    /// Fold the invocation's env knobs into one configuration.
    fn from_env() -> Self {
        let chosen = env_list("BAL_DELTA_ROUNDS_TRACES");
        let fixtures = env_list("BAL_DELTA_ROUNDS_FIXTURES");
        let traces: Vec<String> = chosen.unwrap_or_else(|| {
            CORPORA
                .iter()
                .map(|(trace, _)| (*trace).to_string())
                .collect()
        });
        let corpora = traces
            .iter()
            .enumerate()
            .map(|(index, trace)| Corpus {
                trace: Box::leak(trace.clone().into_boxed_str()),
                fixture: match fixtures.as_ref().and_then(|list| list.get(index)) {
                    Some(fixture) => Box::leak(fixture.clone().into_boxed_str()),
                    None => CORPORA
                        .get(index)
                        .map_or(CORPORA[0].1, |(_, fixture)| *fixture),
                },
            })
            .collect();
        let points = match (
            env_us("BAL_DELTA_ROUNDS_STORAGE_US"),
            env_us("BAL_DELTA_ROUNDS_ACCOUNT_US"),
        ) {
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
            corpora,
            points,
            limits: FanoutLimits::new(
                env_count("BAL_DELTA_ROUNDS_W", 2),
                env_count("BAL_DELTA_ROUNDS_F", 8),
            ),
            idle: u64::try_from(env_count("BAL_DELTA_ROUNDS_IDLE", 8)).unwrap_or(u64::MAX),
            planted: std::env::var("BAL_DELTA_ROUNDS_PLANT_DIVERGENCE")
                .is_ok_and(|value| value == "1"),
        }
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

/// What one leg accumulated across its corpus's anchors.
#[derive(Default)]
struct LegAccum {
    rounds: u64,
    storage_members: u64,
    account_members: u64,
    memo_hits: u64,
    coalesced: u64,
    peak_active: usize,
    digests: Digests,
    reads: Vec<ReadEvent>,
    snapshot_final: usize,
    live_final: usize,
}

impl LegAccum {
    fn fold(&mut self, facts: LegFacts) {
        self.rounds = self.rounds.saturating_add(facts.rounds);
        self.storage_members = self.storage_members.saturating_add(facts.storage_fetches);
        self.account_members = self.account_members.saturating_add(facts.account_fetches);
        self.memo_hits = self.memo_hits.saturating_add(facts.memo_hits);
        self.coalesced = self.coalesced.saturating_add(facts.coalesced);
        self.peak_active = self.peak_active.max(facts.peak_active);
        self.digests.extend(facts.digests);
        self.reads.extend(facts.reads);
    }

    /// Chain-hit reads versus reads that took the round path, over the leg's
    /// trace (a full-clone leg does not classify, and reports zeroes).
    fn hit_split(&self) -> (usize, usize) {
        let hits = self
            .reads
            .iter()
            .filter(|event| event.origin == Some(ReadOrigin::Snapshot))
            .count();
        (hits, self.reads.len().saturating_sub(hits))
    }
}

/// One leg's measured record.
struct LegRecord {
    label: &'static str,
    wall: WallReport,
    completions: CompletionStats,
    accum: LegAccum,
    bal_fetches: u64,
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

/// Drive the full-clone leg's advance for one block, returning its lane.
async fn advance_full<'p>(
    control: &ReadHubCache<'p>,
    real: &RealCoverage,
    number: u64,
    relevance: &[Address],
    ledger: &WallLedger,
    node: Node,
) -> Result<(Arc<ReadHub<'p>>, u64), String> {
    let (anchor, parent) = anchors(real, number)?;
    let started = Instant::now();
    let (hub, report) = control.advance_bal(anchor, parent, relevance).await;
    charge_advance_full(ledger, node, &report, started.elapsed());
    Ok((hub, 1))
}

/// Drive a delta leg's advance for one block.
async fn advance_delta<'p>(
    delta: &DeltaSnapshotCache<'p>,
    real: &RealCoverage,
    number: u64,
    relevance: &[Address],
    ledger: &WallLedger,
    node: Node,
) -> Result<
    (
        Arc<degenbot_simulation::sim::evm2::delta_snapshot::DeltaHub<'p>>,
        usize,
    ),
    String,
> {
    let (anchor, parent) = anchors(real, number)?;
    let started = Instant::now();
    let (hub, report) = delta.advance_bal(anchor, parent, relevance).await;
    charge_advance_delta(ledger, node, &report, started.elapsed());
    let entries = hub.snapshot_len();
    Ok((hub, entries))
}

/// Drive one leg over one corpus's whole stretch: every block advanced through
/// the leg's transition, the recorded candidates at every anchor read through
/// the leg's lane.
async fn drive_leg(
    kind: LegKind,
    knobs: &Knobs,
    fanouts: &[(u64, Vec<Recorded>)],
    real: &RealCoverage,
    provider: &AlloyProvider,
    node: Node,
    plant: &Arc<AtomicBool>,
) -> Result<LegRecord, String> {
    let by_anchor: BTreeMap<u64, &[Recorded]> = fanouts
        .iter()
        .map(|(block, rows)| (*block, rows.as_slice()))
        .collect();
    let first = fanouts.first().map_or(0, |(block, _)| *block);
    let last = fanouts.last().map_or(0, |(block, _)| *block);
    let ledger = WallLedger::new();
    let mut accum = LegAccum::default();
    let mut bal_fetches = 0u64;
    let control = (kind == LegKind::Full).then(|| ReadHubCache::new(provider));
    let delta = kind.delta().then(|| match kind {
        LegKind::DeltaIdle => {
            DeltaSnapshotCache::with_retention(provider, Retention::IdleBlocks(knobs.idle))
        }
        _ => DeltaSnapshotCache::new(provider),
    });

    plant.store(false, Ordering::Relaxed);
    for number in first..=last {
        // The anchor's relevance set: the production-derived union its trace rows
        // carry, or the labelled recorded-read ORACLE fallback for a legacy
        // corpus. Never re-derived here — a probe that guessed it would be
        // measuring its own guess.
        let relevance = by_anchor
            .get(&number)
            .map_or_else(Relevance::default, |rows| anchor_relevance(rows));
        let full_hub: Option<Arc<ReadHub<'_>>> = match control.as_ref() {
            Some(control) => {
                let (hub, charged) =
                    advance_full(control, real, number, &relevance.addresses, &ledger, node)
                        .await?;
                bal_fetches = bal_fetches.saturating_add(charged);
                accum.snapshot_final = hub.memoized_slots();
                Some(hub)
            }
            None => None,
        };
        let delta_hub = match delta.as_ref() {
            Some(delta) => {
                let (hub, _entries) =
                    advance_delta(delta, real, number, &relevance.addresses, &ledger, node).await?;
                bal_fetches = bal_fetches.saturating_add(1);
                accum.snapshot_final = hub.snapshot_len();
                accum.live_final = hub.live_len();
                Some(hub)
            }
            None => None,
        };
        let lane = match (&full_hub, &delta_hub) {
            (Some(hub), _) => ReadLane::Full(hub.as_ref()),
            (_, Some(hub)) => ReadLane::Delta(hub.as_ref()),
            _ => return Err("a leg owns exactly one read lane".to_string()),
        };
        if let Some(rows) = by_anchor.get(&number) {
            plant.store(kind.delta() && knobs.planted, Ordering::Relaxed);
            let facts = drive_lanes(
                kind.label(),
                leg_plan(knobs.limits, LaneTrace::Reads),
                rows,
                0,
                provider,
                node,
                Some(lane),
                &ledger,
            )
            .await;
            plant.store(false, Ordering::Relaxed);
            accum.fold(facts?);
        }
    }

    let spans = ledger.spans();
    let wall = reconcile(&spans, ledger.offset())?;
    let completions = completion_stats(ledger.completions())?;
    Ok(LegRecord {
        label: kind.label(),
        wall,
        completions,
        accum,
        bal_fetches,
    })
}

/// One re-read observation: the read's block, its gap since the previous read
/// of the same identity, and which side of the delta seam answered it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReRead {
    block: u64,
    gap: u64,
    origin: ReadOrigin,
}

/// Every re-read in one traced delta leg, in block order per identity.
fn re_reads(events: &[ReadEvent]) -> Vec<ReRead> {
    let mut by_key: BTreeMap<Key, Vec<(u64, ReadOrigin)>> = BTreeMap::new();
    for event in events {
        let Some(origin) = event.origin else {
            continue;
        };
        by_key
            .entry(event.key)
            .or_default()
            .push((event.block, origin));
    }
    let mut reads = Vec::new();
    for (_, mut history) in by_key {
        history.sort_unstable_by_key(|(block, _)| *block);
        for window in history.windows(2) {
            let (previous, _) = window[0];
            let (block, origin) = window[1];
            reads.push(ReRead {
                block,
                gap: block.saturating_sub(previous),
                origin,
            });
        }
    }
    reads
}

/// One re-read gap distribution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GapStats {
    count: usize,
    p50: u64,
    p95: u64,
    p99: u64,
    max: u64,
}

/// Nearest-rank percentiles over one gap sample.
fn summarize(mut gaps: Vec<u64>) -> GapStats {
    if gaps.is_empty() {
        return GapStats::default();
    }
    gaps.sort_unstable();
    let at = |percent: usize| {
        let rank = (gaps.len() * percent).div_ceil(100).max(1).min(gaps.len());
        gaps[rank - 1]
    };
    GapStats {
        count: gaps.len(),
        p50: at(50),
        p95: at(95),
        p99: at(99),
        max: gaps[gaps.len() - 1],
    }
}

impl GapStats {
    /// The row this distribution prints.
    fn row(&self) -> String {
        if self.count == 0 {
            return "     - |   - |   - |   - |   -".to_string();
        }
        format!(
            "{:>6} | {:>3} | {:>3} | {:>3} | {:>3}",
            self.count, self.p50, self.p95, self.p99, self.max
        )
    }
}

/// Every block's density regime, from the corpus's own fan-out sizes.
fn regimes(fanouts: &[(u64, Vec<Recorded>)]) -> BTreeMap<u64, DensityRegime> {
    fanouts
        .iter()
        .map(|(block, rows)| (*block, classify_density(rows.len())))
        .collect()
}

fn us(ns: u64) -> u64 {
    ns / 1_000
}

/// Print one corpus point's leg table.
fn print_legs(name: &str, record: &[LegRecord], idle: u64) {
    println!("### {name} — legs");
    println!();
    println!(
        "| leg | wall | exec | storage | account | bal | snapshot | other | first | p50 | p95 |"
    );
    println!("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
    for leg in record {
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            leg.label,
            us(leg.wall.wall_ns),
            us(leg.wall.bucket(WallBucket::RecordedExec)),
            us(leg.wall.bucket(WallBucket::StorageWait)),
            us(leg.wall.bucket(WallBucket::AccountWait)),
            us(leg.wall.bucket(WallBucket::BalFetchDecode)),
            us(leg.wall.bucket(WallBucket::SnapshotCloneUpdate)),
            us(leg.wall.bucket(WallBucket::SchedulerOther)),
            us(leg.completions.first_ns),
            us(leg.completions.p50_ns),
            us(leg.completions.p95_ns),
        );
    }
    println!();
    println!(
        "| leg | rounds | storage members | account members | memo hits | coalesced | \
         chain hits | dynamic reads | peak active | snapshot | live | bal fetches |"
    );
    println!(
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    );
    for leg in record {
        let (hits, dynamic) = leg.accum.hit_split();
        let hits = if leg.label == "full" {
            "n/a".to_string()
        } else {
            hits.to_string()
        };
        let dynamic = if leg.label == "full" {
            "n/a".to_string()
        } else {
            dynamic.to_string()
        };
        let live = if leg.label == "full" {
            "n/a".to_string()
        } else {
            leg.accum.live_final.to_string()
        };
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            leg.label,
            leg.accum.rounds,
            leg.accum.storage_members,
            leg.accum.account_members,
            leg.accum.memo_hits,
            leg.accum.coalesced,
            hits,
            dynamic,
            leg.accum.peak_active,
            leg.accum.snapshot_final,
            live,
            leg.bal_fetches,
        );
    }
    println!();
    println!("idle window measured: {idle} blocks");
    println!();
}

/// Print one corpus's re-read gap report and the measured idle-window cost.
fn print_gaps(
    name: &str,
    unbounded: &LegRecord,
    idle: &LegRecord,
    by_regime: &BTreeMap<u64, DensityRegime>,
    window: u64,
) {
    let reads = re_reads(&unbounded.accum.reads);
    let all: Vec<u64> = reads.iter().map(|read| read.gap).collect();
    let hits: Vec<u64> = reads
        .iter()
        .filter(|read| read.origin == ReadOrigin::Snapshot)
        .map(|read| read.gap)
        .collect();
    // In the UNBOUNDED chain a settled value always carries, so a round read is
    // never a re-read after a gap: every one is a same-anchor duplicate another
    // lane coalesced onto. The window is what turns a hit re-read into a round
    // read, which is why the converted slice is measured on the idle leg.
    let coalesced_duplicates = reads
        .iter()
        .filter(|read| read.origin == ReadOrigin::Hub && read.gap == 0)
        .count();
    let converted: Vec<u64> = re_reads(&idle.accum.reads)
        .iter()
        .filter(|read| read.origin == ReadOrigin::Hub && read.gap > 0)
        .map(|read| read.gap)
        .collect();
    let total_reads = unbounded.accum.reads.len();
    let hit_reads = unbounded
        .accum
        .reads
        .iter()
        .filter(|event| event.origin == Some(ReadOrigin::Snapshot))
        .count();

    println!("### {name} — candidate re-read gaps (delta, unbounded)");
    println!();
    println!("| slice | re-reads | p50 | p95 | p99 | max |");
    println!("| --- | ---: | ---: | ---: | ---: | ---: |");
    println!("| all re-reads | {}", summarize(all).row());
    println!(
        "| answered by the chain (hits) | {}",
        summarize(hits.clone()).row()
    );
    println!(
        "| answered by a round, same block (coalesced duplicate) | {coalesced_duplicates} | \
         -, -, -, - (gap 0) |"
    );
    println!(
        "| hit re-reads over the {window}-block window | {}",
        summarize(hits.iter().copied().filter(|gap| *gap > window).collect()).row()
    );
    println!(
        "| reads the window converted to a round | {}",
        summarize(converted).row()
    );
    for regime in [
        DensityRegime::Sparse,
        DensityRegime::Moderate,
        DensityRegime::Burst,
    ] {
        let gaps: Vec<u64> = reads
            .iter()
            .filter(|read| by_regime.get(&read.block) == Some(&regime))
            .map(|read| read.gap)
            .collect();
        if !gaps.is_empty() {
            println!("| {} anchors | {}", regime.label(), summarize(gaps).row());
        }
    }
    println!();
    println!(
        "reads {} = chain hits {} + dynamic {}",
        total_reads, hit_reads, unbounded.accum.storage_members
    );
    let over_window = hits.iter().filter(|gap| **gap > window).count();
    let extras = idle
        .accum
        .storage_members
        .saturating_sub(unbounded.accum.storage_members);
    println!(
        "IdleBlocks({window}): {over_window} hit re-reads have a gap over the window (predicted), \
         measured {} extra dynamic reads ({} -> {}) and {} fewer chain hits; live entries {} vs {}",
        extras,
        unbounded.accum.storage_members,
        idle.accum.storage_members,
        hit_reads.saturating_sub(
            idle.accum
                .reads
                .iter()
                .filter(|event| event.origin == Some(ReadOrigin::Snapshot))
                .count()
        ),
        unbounded.accum.live_final,
        idle.accum.live_final,
    );
    println!();
}

/// The first read two legs answered differently, named by block and identity —
/// the digest gate's diagnosis when a candidate's walk disagrees.
fn trace_mismatch(full: &[ReadEvent], delta: &[ReadEvent]) -> Option<String> {
    let mut left: Vec<_> = full
        .iter()
        .map(|event| (event.block, event.key, event.value))
        .collect();
    let mut right: Vec<_> = delta
        .iter()
        .map(|event| (event.block, event.key, event.value))
        .collect();
    left.sort_unstable();
    right.sort_unstable();
    for (a, b) in left.iter().zip(&right) {
        if a != b {
            return Some(format!(
                "block {} {}/{}: full {:#x} vs delta {:#x}",
                a.0, a.1 .0, a.1 .1, a.2, b.2
            ));
        }
    }
    if left.len() == right.len() {
        None
    } else {
        Some(format!("read counts {} vs {}", left.len(), right.len()))
    }
}

/// Compare two legs' digests, returning every candidate that disagrees.
fn digest_mismatches(reference: &Digests, other: &Digests) -> Vec<String> {
    let mut mismatches = Vec::new();
    for (key, digest) in reference {
        match other.get(key) {
            Some(other_digest) if other_digest == digest => {}
            Some(other_digest) => mismatches.push(format!(
                "block {} candidate {}: {digest} vs {other_digest}",
                key.0, key.1
            )),
            None => mismatches.push(format!("block {} candidate {}: missing", key.0, key.1)),
        }
    }
    if other.len() != reference.len() {
        mismatches.push(format!(
            "candidate count {} vs {}",
            reference.len(),
            other.len()
        ));
    }
    mismatches
}

/// Run one corpus at one latency point: all three legs, the value gate, and the
/// gap report.
#[expect(
    clippy::too_many_arguments,
    reason = "the point's inputs are the replay's controls: corpora, prices, leg set, observation sink"
)]
async fn run_point(
    knobs: &Knobs,
    corpus: &Corpus,
    fanouts: &[(u64, Vec<Recorded>)],
    real: &RealCoverage,
    provider: &AlloyProvider,
    plant: &Arc<AtomicBool>,
    storage: Duration,
    account: Duration,
) -> Vec<String> {
    let node = Node::new(storage, account);
    let mut record = Vec::new();
    for kind in [LegKind::Full, LegKind::Delta, LegKind::DeltaIdle] {
        record.push(
            drive_leg(kind, knobs, fanouts, real, provider, node, plant)
                .await
                .expect("the leg replays"),
        );
    }
    let name = format!(
        "{} — {} / {}us",
        corpus.trace,
        storage.as_micros(),
        account.as_micros()
    );
    println!("## {name}");
    println!();
    println!(
        "relevance scope: {} (the carry's seed set)",
        corpus_relevance_source(fanouts.iter().flat_map(|(_, rows)| rows.iter())).label()
    );
    print_legs(&name, &record, knobs.idle);

    let mut mismatches = Vec::new();
    let reference = &record[0].accum.digests;
    for leg in &record[1..] {
        for mismatch in digest_mismatches(reference, &leg.accum.digests) {
            mismatches.push(format!("{name} {}: {mismatch}", leg.label));
        }
        if let Some(diagnosis) = trace_mismatch(&record[0].accum.reads, &leg.accum.reads) {
            println!("leg {} first read divergence: {diagnosis}", leg.label);
        }
    }
    if !knobs.planted {
        print_gaps(&name, &record[1], &record[2], &regimes(fanouts), knobs.idle);
    }
    mismatches
}

#[tokio::main]
async fn main() {
    let knobs = Knobs::from_env();
    println!(
        "corpora {} | W={} F={} | idle window {} | points {:?} | planted {}",
        knobs.corpora.len(),
        knobs.limits.workers(),
        knobs.limits.slots(),
        knobs.idle,
        knobs
            .points
            .iter()
            .map(|(storage, account)| format!("{}/{}us", storage.as_micros(), account.as_micros()))
            .collect::<Vec<_>>(),
        knobs.planted
    );
    println!();

    let mut gate = Vec::new();
    for corpus in &knobs.corpora {
        let rows = load_rows(corpus.trace).expect("the recorded corpus loads");
        let (keys, first, last) = universe(&rows);
        let fanouts = group_by_anchor(rows);
        let real = RealCoverage::load(corpus.fixture).expect("the committed capture parses");
        let chain = ChainState::build_coverage(&keys, first, last, &real, Coverage::Real)
            .expect("a chain over the corpus's stretch");
        let plant = Arc::new(AtomicBool::new(false));
        let stub = StubChain {
            chain: Arc::new(chain),
            plant: Arc::clone(&plant),
        };
        let provider = stub.provider();
        for (storage, account) in &knobs.points {
            let mismatches = run_point(
                &knobs, corpus, &fanouts, &real, &provider, &plant, *storage, *account,
            )
            .await;
            gate.extend(mismatches);
        }
    }

    if knobs.planted {
        assert!(
            !gate.is_empty(),
            "the planted one-wei divergence must be caught by the digest gate"
        );
        println!(
            "negative probe: the digest gate caught the planted divergence ({})",
            gate.len()
        );
        std::process::exit(1);
    }
    assert!(
        gate.is_empty(),
        "every leg must read identical values: {} mismatches, first {:?}",
        gate.len(),
        gate.first()
    );
    println!(
        "value gate: every leg's read digests agree ({} corpora)",
        knobs.corpora.len()
    );
}
