//! The paired relevance analysis: the production-derived relevance set beside
//! the recorded-read oracle, measured on the SAME recorded candidates.
//!
//! # Why a pair, and nothing else
//!
//! The pre-simulation `relevance` field is what the live run KNEW when it had to
//! decide what to snapshot. The recorded read addresses are what the run turned
//! out to READ. Scoping one replay by each and holding everything else fixed —
//! the same rows, the same real write identities, the same candidate order, the
//! same W/F shape, the same price point, the same values — isolates the
//! relevance effect: the two legs differ in exactly one input.
//!
//! # What lives here
//!
//! Pure arithmetic over numbers the drivers measured: the set pair and its
//! difference, the block's real writes split by whether a set can seed them,
//! one anchor's measured reuse, the density-regime fold, and the re-read
//! gap/retention arithmetic. The probes own the measuring; this module owns the
//! arithmetic the report prints, so every claim is pinned by a test over
//! synthetic inputs instead of by a timing assertion.

use std::collections::{BTreeMap, BTreeSet};

use alloy::primitives::Address;

use degenbot_simulation::sim::evm2::delta_snapshot::ReadOrigin;

use crate::probe_report::StorageAccounting;
use crate::real_coverage::Key;
use crate::recorded_corpus::{candidate_addresses, classify_density, DensityRegime, Recorded};
use crate::session_drive::ReadEvent;

/// Which derivation scoped one replay's relevance set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelevanceVariant {
    /// The simulation seam's pre-simulation `relevance` field: what the live run
    /// knew before the candidate ran.
    Production,
    /// The recorded read addresses: what the run turned out to read. An ORACLE,
    /// because the live run did not have it when it had to decide.
    Oracle,
}

impl RelevanceVariant {
    /// Both variants, production first — the report's column order.
    pub const ALL: [Self; 2] = [Self::Production, Self::Oracle];

    /// The label every table prints beside a number the set scoped.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Oracle => "recorded-read-oracle",
        }
    }
}

/// One anchor's relevance sets: the same recorded candidates under both
/// derivations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorSets {
    /// The anchor the candidates were simulated at.
    pub block: u64,
    /// The union of the anchor's rows' production `relevance` sets, first-seen
    /// order, deduped.
    pub production: Vec<Address>,
    /// The anchor's distinct recorded read addresses, first-seen order.
    pub oracle: Vec<Address>,
    /// Recorded candidates at the anchor — the identical fan-out both sets
    /// describe.
    pub candidates: usize,
}

impl AnchorSets {
    /// The set one variant scopes a replay's snapshot with.
    #[must_use]
    pub fn set(&self, variant: RelevanceVariant) -> &[Address] {
        match variant {
            RelevanceVariant::Production => &self.production,
            RelevanceVariant::Oracle => &self.oracle,
        }
    }

    /// The anchor's density regime, from its fan-out size.
    #[must_use]
    pub const fn regime(&self) -> DensityRegime {
        classify_density(self.candidates)
    }

    /// What the two sets disagree about.
    #[must_use]
    pub fn difference(&self) -> SetDifference {
        let production: BTreeSet<Address> = self.production.iter().copied().collect();
        let oracle: BTreeSet<Address> = self.oracle.iter().copied().collect();
        SetDifference {
            excluded_read: self
                .oracle
                .iter()
                .copied()
                .filter(|address| !production.contains(address))
                .collect(),
            unread_relevant: self
                .production
                .iter()
                .copied()
                .filter(|address| !oracle.contains(address))
                .collect(),
            shared: self
                .production
                .iter()
                .filter(|address| oracle.contains(*address))
                .count(),
        }
    }
}

/// What two relevance sets disagree about at one anchor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetDifference {
    /// Addresses the recorded walk READ that production relevance excludes: the
    /// post-read-only addresses a production-scoped snapshot cannot seed for.
    pub excluded_read: Vec<Address>,
    /// Production-relevant addresses the anchor's walk never read: seeds the
    /// read oracle cannot see, and a snapshot the oracle scoped would miss.
    pub unread_relevant: Vec<Address>,
    /// Addresses both derivations name.
    pub shared: usize,
}

/// Pair every anchor's candidates under both derivations.
///
/// # Errors
///
/// A row carrying no `relevance` field makes the production union incomplete,
/// and an incomplete union reported as production relevance is exactly the
/// fallback this pairing must never perform: the anchor is refused with the
/// block named.
pub fn pair_anchors(fanouts: &[(u64, Vec<Recorded>)]) -> Result<Vec<AnchorSets>, String> {
    let mut pairs = Vec::with_capacity(fanouts.len());
    for (block, rows) in fanouts {
        let mut seen: BTreeSet<Address> = BTreeSet::new();
        let mut production: Vec<Address> = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let Some(addresses) = row.relevance.as_ref() else {
                return Err(format!(
                    "block {block}: recorded candidate {index} carries no `relevance` field — \
                     pairing it would report a recorded-read oracle as production relevance"
                ));
            };
            for address in addresses {
                if seen.insert(*address) {
                    production.push(*address);
                }
            }
        }
        pairs.push(AnchorSets {
            block: *block,
            production,
            oracle: candidate_addresses(rows),
            candidates: rows.len(),
        });
    }
    Ok(pairs)
}

/// One block's real write identities split by whether a relevance set names
/// their address: the identities a set can SEED, and the ones only a key the
/// snapshot already holds can still apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriteSplit {
    /// Writes whose address the relevance set names — seedable at an unknown
    /// slot.
    pub inside: usize,
    /// Writes whose address the set omits — applied only where the slot is
    /// already known, otherwise skipped and later read dynamically.
    pub outside: usize,
}

/// Split one block's captured writes by relevance membership.
#[must_use]
pub fn write_split(writes: &[Key], relevance: &[Address]) -> WriteSplit {
    let named: BTreeSet<Address> = relevance.iter().copied().collect();
    let mut split = WriteSplit::default();
    for (address, _slot) in writes {
        if named.contains(address) {
            split.inside += 1;
        } else {
            split.outside += 1;
        }
    }
    split
}

/// Classify one anchor's distinct keys by the answer path the leg TRACED: a key
/// the anchor's own final writes carry is a final-write hit whatever the
/// snapshot said, a key EVERY read of which the chain answered is carried, and a
/// key any read of which took the round path is a dynamic member — the member is
/// paid once per identity per anchor, however many candidates share it. Unlike
/// [`storage_accounting`](crate::probe_report::storage_accounting) this reads the
/// leg's own answers, so it measures an expiring policy too instead of modelling
/// the held set.
///
/// A key the leg traced NO read for pays nothing (no member was dispatched), so
/// a leg that skipped a recorded key shows up as `keys` exceeding the three
/// classes — which the delta-versus-control gate turns red rather than hiding.
#[must_use]
pub fn traced_accounting(
    keys: &BTreeSet<Key>,
    applied: &BTreeSet<Key>,
    reads: &[ReadEvent],
) -> StorageAccounting {
    let mut accounting = StorageAccounting {
        keys: keys.len(),
        ..StorageAccounting::default()
    };
    for key in keys {
        if applied.contains(key) {
            accounting.final_write += 1;
            continue;
        }
        let mut answered = false;
        let mut every_read_hit = true;
        for read in reads.iter().filter(|read| read.key == *key) {
            answered = true;
            every_read_hit = every_read_hit && read.origin == Some(ReadOrigin::Snapshot);
        }
        if !answered {
            continue;
        }
        if every_read_hit {
            accounting.carried += 1;
        } else {
            accounting.dynamic += 1;
        }
    }
    accounting
}

/// One anchor's measured reuse under one relevance set and one retention
/// policy: the numbers a report compares production against oracle on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnchorReuse {
    /// The anchor's recorded keys split by where their answer came from.
    pub storage: StorageAccounting,
    /// Final writes applied as new snapshot entries (applied not overwriting a
    /// served value).
    pub seeds: usize,
    /// Final writes that replaced a value the snapshot already served.
    pub overwrites: usize,
    /// Final writes left unknown: an unknown slot whose address the relevance
    /// set omits.
    pub skipped: usize,
    /// Storage round members the anchor's fan-out paid.
    pub storage_members: u64,
    /// Account round members the anchor's fan-out paid.
    pub account_members: u64,
    /// Read rounds drained at the anchor.
    pub rounds: u64,
    /// Memo hits the hub reported, read calls (repeats included).
    pub memo_hits: u64,
    /// Same-anchor duplicates a round coalesced.
    pub coalesced: u64,
    /// Snapshot entries after the anchor's advance — a gauge.
    pub snapshot: usize,
    /// Retained (live) entries after the anchor, where the lane expires them.
    pub live: Option<usize>,
}

impl AnchorReuse {
    /// Fold another anchor's measurement into a leg or regime total: counters
    /// sum, gauges take the maximum.
    pub fn add(&mut self, other: Self) {
        self.storage.add(other.storage);
        self.seeds = self.seeds.saturating_add(other.seeds);
        self.overwrites = self.overwrites.saturating_add(other.overwrites);
        self.skipped = self.skipped.saturating_add(other.skipped);
        self.storage_members = self.storage_members.saturating_add(other.storage_members);
        self.account_members = self.account_members.saturating_add(other.account_members);
        self.rounds = self.rounds.saturating_add(other.rounds);
        self.memo_hits = self.memo_hits.saturating_add(other.memo_hits);
        self.coalesced = self.coalesced.saturating_add(other.coalesced);
        self.snapshot = self.snapshot.max(other.snapshot);
        self.live = match (self.live, other.live) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (Some(live), None) | (None, Some(live)) => Some(live),
            (None, None) => None,
        };
    }
}

/// The same anchor measured under both relevance sets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReuseComparison {
    /// The production-scoped replay.
    pub production: AnchorReuse,
    /// The oracle-scoped replay.
    pub oracle: AnchorReuse,
}

/// A signed difference `left - right` over two counts.
fn signed_delta(left: usize, right: usize) -> i64 {
    i64::try_from(left)
        .unwrap_or(i64::MAX)
        .saturating_sub(i64::try_from(right).unwrap_or(i64::MAX))
}

impl ReuseComparison {
    /// A pairing of the same anchor's two measurements.
    #[must_use]
    pub const fn new(production: AnchorReuse, oracle: AnchorReuse) -> Self {
        Self { production, oracle }
    }

    /// Dynamic storage members production pays MINUS the oracle's: positive
    /// where production LOST reuse (more fetches), negative where it gained.
    #[must_use]
    pub fn dynamic_delta(&self) -> i64 {
        signed_delta(self.production.storage.dynamic, self.oracle.storage.dynamic)
    }

    /// Carried hits production reports MINUS the oracle's.
    #[must_use]
    pub fn carried_delta(&self) -> i64 {
        signed_delta(self.production.storage.carried, self.oracle.storage.carried)
    }

    /// Final-write hits production reports MINUS the oracle's.
    #[must_use]
    pub fn final_write_delta(&self) -> i64 {
        signed_delta(
            self.production.storage.final_write,
            self.oracle.storage.final_write,
        )
    }

    /// New BAL seeds production applies MINUS the oracle's.
    #[must_use]
    pub fn seed_delta(&self) -> i64 {
        signed_delta(self.production.seeds, self.oracle.seeds)
    }

    /// Known-slot overwrites production applies MINUS the oracle's: a write to a
    /// slot the snapshot already serves applies whatever the relevance set says,
    /// so only the address membership of the two sets can move this count.
    #[must_use]
    pub fn overwrite_delta(&self) -> i64 {
        signed_delta(self.production.overwrites, self.oracle.overwrites)
    }

    /// Final writes left unknown production reports MINUS the oracle's.
    #[must_use]
    pub fn skip_delta(&self) -> i64 {
        signed_delta(self.production.skipped, self.oracle.skipped)
    }

    /// Snapshot entries production holds MINUS the oracle's, at the anchor.
    #[must_use]
    pub fn snapshot_delta(&self) -> i64 {
        signed_delta(self.production.snapshot, self.oracle.snapshot)
    }

    /// Storage round members production pays MINUS the oracle's.
    #[must_use]
    pub fn storage_member_delta(&self) -> i64 {
        i64::try_from(self.production.storage_members)
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(self.oracle.storage_members).unwrap_or(i64::MAX))
    }

    /// Rounds production drains MINUS the oracle's.
    #[must_use]
    pub fn round_delta(&self) -> i64 {
        i64::try_from(self.production.rounds)
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(self.oracle.rounds).unwrap_or(i64::MAX))
    }
}

/// One density regime's paired reuse: every anchor the corpus recorded in the
/// regime, with both variants' measurements and the same anchor set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegimeReuse {
    /// The regime every anchor of the row lands in.
    pub regime: DensityRegime,
    /// Anchors the corpus recorded in the regime.
    pub anchors: usize,
    /// Recorded candidates over those anchors.
    pub candidates: usize,
    /// Production-relevant addresses over the regime's anchors, summed.
    pub production_addresses: usize,
    /// Oracle addresses over the regime's anchors, summed.
    pub oracle_addresses: usize,
    /// Read addresses production relevance excludes, summed.
    pub excluded_read: usize,
    /// Production-relevant addresses the walks never read, summed.
    pub unread_relevant: usize,
    /// The regime's paired measurement.
    pub reuse: ReuseComparison,
}

/// Fold per-anchor measurements into the density regimes the corpus recorded,
/// in first-appearance order.
///
/// # Errors
///
/// An anchor either map is missing is a broken measurement, never a silent zero:
/// the fold refuses it with the block named.
pub fn fold_regimes(
    pairs: &[AnchorSets],
    production: &BTreeMap<u64, AnchorReuse>,
    oracle: &BTreeMap<u64, AnchorReuse>,
) -> Result<Vec<RegimeReuse>, String> {
    let mut order: Vec<DensityRegime> = Vec::new();
    let mut rows: BTreeMap<DensityRegime, RegimeReuse> = BTreeMap::new();
    for pair in pairs {
        let Some(measured) = production.get(&pair.block) else {
            return Err(format!(
                "the production leg measured no reuse at block {}",
                pair.block
            ));
        };
        let Some(measured_oracle) = oracle.get(&pair.block) else {
            return Err(format!(
                "the oracle leg measured no reuse at block {}",
                pair.block
            ));
        };
        let regime = pair.regime();
        let row = rows.entry(regime).or_insert_with(|| {
            order.push(regime);
            RegimeReuse {
                regime,
                anchors: 0,
                candidates: 0,
                production_addresses: 0,
                oracle_addresses: 0,
                excluded_read: 0,
                unread_relevant: 0,
                reuse: ReuseComparison::default(),
            }
        });
        let difference = pair.difference();
        row.anchors += 1;
        row.candidates += pair.candidates;
        row.production_addresses += pair.production.len();
        row.oracle_addresses += pair.oracle.len();
        row.excluded_read += difference.excluded_read.len();
        row.unread_relevant += difference.unread_relevant.len();
        row.reuse.production.add(*measured);
        row.reuse.oracle.add(*measured_oracle);
    }
    Ok(order
        .into_iter()
        .filter_map(|regime| rows.remove(&regime))
        .collect())
}

/// One re-read observation: the gap since the same identity's previous read at
/// the same lane, and which side of the delta seam answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReRead {
    /// The block the re-read was made at.
    pub block: u64,
    /// Blocks since the identity's previous read in the same leg.
    pub gap: u64,
    /// Whether the delta chain answered (rather than a round).
    pub hit: bool,
}

/// Every re-read in one traced leg, in identity then block order.
///
/// A leg whose lane does not classify its answers (the full-clone control)
/// traces no origin and contributes no re-reads.
#[must_use]
pub fn re_reads(reads: &[ReadEvent]) -> Vec<ReRead> {
    let mut by_key: BTreeMap<Key, Vec<(u64, bool)>> = BTreeMap::new();
    for event in reads {
        let Some(origin) = event.origin else {
            continue;
        };
        by_key
            .entry(event.key)
            .or_default()
            .push((event.block, origin == ReadOrigin::Snapshot));
    }
    let mut re_reads = Vec::new();
    for (_, mut history) in by_key {
        history.sort_unstable_by_key(|(block, _)| *block);
        for window in history.windows(2) {
            let (previous, _) = window[0];
            let (block, hit) = window[1];
            re_reads.push(ReRead {
                block,
                gap: block.saturating_sub(previous),
                hit,
            });
        }
    }
    re_reads
}

/// Nearest-rank percentiles of one re-read gap sample.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GapStats {
    /// How many re-reads the sample holds.
    pub count: usize,
    /// The nearest-rank 50th percentile gap.
    pub p50: u64,
    /// The nearest-rank 95th percentile gap.
    pub p95: u64,
    /// The nearest-rank 99th percentile gap.
    pub p99: u64,
    /// The largest gap in the sample.
    pub max: u64,
}

/// Summarize one gap sample; an empty sample stays zeroed.
#[must_use]
pub fn summarize(gaps: Vec<u64>) -> GapStats {
    if gaps.is_empty() {
        return GapStats::default();
    }
    let mut gaps = gaps;
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

/// What one retention window did, measured against the same variant's unbounded
/// leg.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetentionDelta {
    /// Chain-hit re-reads the unbounded leg reported.
    pub hits: usize,
    /// Hit re-reads whose gap exceeds the window: the re-reads the window is
    /// PREDICTED to convert.
    pub over_window: usize,
    /// Reads the windowed leg answered with a round after a positive gap: the
    /// conversions it actually made. A write between two reads refreshes the
    /// entry, so a re-read over the window need not convert — the measured
    /// number is the authority, the predicted one the check.
    pub converted: usize,
    /// Extra storage members the windowed leg paid.
    pub extra_members: u64,
    /// Chain hits the windowed leg lost.
    pub lost_hits: usize,
    /// Live entries at the last anchor: unbounded, then windowed.
    pub live: (usize, usize),
}

/// Measure one variant's retention window against its unbounded control.
#[must_use]
pub fn retention_delta(
    unbounded_reads: &[ReadEvent],
    windowed_reads: &[ReadEvent],
    unbounded: &AnchorReuse,
    windowed: &AnchorReuse,
    window: u64,
) -> RetentionDelta {
    let reads = re_reads(unbounded_reads);
    let hits = reads.iter().filter(|read| read.hit).count();
    let over_window = reads
        .iter()
        .filter(|read| read.hit && read.gap > window)
        .count();
    let conversions = re_reads(windowed_reads);
    let converted = conversions
        .iter()
        .filter(|read| !read.hit && read.gap > 0)
        .count();
    let windowed_hits = conversions.iter().filter(|read| read.hit).count();
    RetentionDelta {
        hits,
        over_window,
        converted,
        extra_members: windowed
            .storage_members
            .saturating_sub(unbounded.storage_members),
        lost_hits: hits.saturating_sub(windowed_hits),
        live: (
            unbounded.live.unwrap_or_default(),
            windowed.live.unwrap_or_default(),
        ),
    }
}

/// One leg's measured totals, folded from its per-anchor map.
#[must_use]
pub fn total(reuse: &BTreeMap<u64, AnchorReuse>) -> AnchorReuse {
    let mut total = AnchorReuse::default();
    for measured in reuse.values() {
        total.add(*measured);
    }
    total
}
