//! The recorded fan-out corpus: the `fanout-trace` rows every recorded-read
//! benchmark replays, and the key sets the comparison report is computed over.
//!
//! The corpus is the bot's own `fanout-trace` stream: per candidate, the read
//! identities the simulation seam drained and the wall it measured for the
//! candidate (`sim_ns`). It carries no calldata and no chain snapshot, so a
//! replay drives the read schedule and the recorded execution — never an EVM
//! verdict.

use std::collections::{BTreeMap, BTreeSet};

use alloy::primitives::{Address, U256};
use serde_json::Value;

/// The tag the bot's tracing formatter prefixes each recorded row with.
pub const TRACE_TAG: &str = "fanout-trace ";

/// The recorded corpus the benchmarks are defined over.
pub const DEFAULT_TRACE: &str = "/tmp/fanout_trace.log";

/// One recorded row's read groups: an address and the slots the trace lists
/// beside it.
pub type ReadGroups = Vec<(Address, Vec<U256>)>;

/// One tracked read identity.
pub type Key = (Address, U256);

/// One recorded candidate: the read walk the simulation seam drained and the
/// execution wall it measured.
#[derive(Clone, Debug)]
pub struct Recorded {
    /// The anchor the candidate was simulated at.
    pub block: u64,
    /// The wall the seam measured (encode + sequence + assess).
    pub sim_ns: u64,
    /// The candidate's recorded read walk, in recorded order.
    pub reads: ReadGroups,
}

/// Parse one log line's trace row (the substring after `fanout-trace `).
#[must_use]
pub fn parse_trace_line(line: &str) -> Option<Recorded> {
    let start = line.find(TRACE_TAG)? + TRACE_TAG.len();
    let value: Value = serde_json::from_str(line[start..].trim()).ok()?;
    let reads = value
        .get("reads")?
        .as_array()?
        .iter()
        .filter_map(|entry| {
            let mut parts = entry.as_str()?.split('|');
            let address = parts.next()?.parse::<Address>().ok()?;
            let slots = parts
                .next()?
                .split(',')
                .filter(|slot| !slot.is_empty())
                .filter_map(|slot| slot.parse::<U256>().ok())
                .collect();
            Some((address, slots))
        })
        .collect();
    Some(Recorded {
        block: value.get("block")?.as_u64()?,
        sim_ns: value.get("sim_ns")?.as_u64()?,
        reads,
    })
}

/// Load every recorded row, failing loudly when the corpus is absent.
///
/// # Errors
///
/// A missing file or a file with no trace rows is a corpus failure, never an
/// empty replay.
pub fn load_rows(path: &str) -> Result<Vec<Recorded>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        format!("read the recorded fan-out trace {path}: {error} (this benchmark has no corpus without it)")
    })?;
    let rows: Vec<Recorded> = text.lines().filter_map(parse_trace_line).collect();
    if rows.is_empty() {
        return Err(format!("no `{TRACE_TAG}` rows in {path}"));
    }
    Ok(rows)
}

/// Group the rows into fan-outs: one per recorded anchor, in file order.
#[must_use]
pub fn group_by_anchor(rows: Vec<Recorded>) -> Vec<(u64, Vec<Recorded>)> {
    let mut groups: Vec<(u64, Vec<Recorded>)> = Vec::new();
    for row in rows {
        match groups.last_mut() {
            Some((block, group)) if *block == row.block => group.push(row),
            _ => groups.push((row.block, vec![row])),
        }
    }
    groups
}

/// The rows' distinct read identities, sorted.
#[must_use]
pub fn anchor_keys(rows: &[Recorded]) -> BTreeSet<Key> {
    rows.iter()
        .flat_map(|row| {
            row.reads
                .iter()
                .flat_map(|(address, slots)| slots.iter().map(move |slot| (*address, *slot)))
        })
        .collect()
}

/// Every anchor's distinct recorded identities, sorted — the coalesced read set
/// one hub at that anchor would carry.
#[must_use]
pub fn anchors_keys(rows: &[Recorded]) -> BTreeMap<u64, Vec<Key>> {
    let mut anchors: BTreeMap<u64, BTreeSet<Key>> = BTreeMap::new();
    for row in rows {
        let keys = anchors.entry(row.block).or_default();
        for (address, slots) in &row.reads {
            for slot in slots {
                keys.insert((*address, *slot));
            }
        }
    }
    anchors
        .into_iter()
        .map(|(block, keys)| (block, keys.into_iter().collect()))
        .collect()
}

/// How many recorded read calls each identity carries — the denominator of the
/// snapshot-hit classification.
#[must_use]
pub fn read_call_counts(rows: &[Recorded]) -> BTreeMap<Key, u64> {
    let mut counts: BTreeMap<Key, u64> = BTreeMap::new();
    for row in rows {
        for (address, slots) in &row.reads {
            for slot in slots {
                *counts.entry((*address, *slot)).or_default() += 1;
            }
        }
    }
    counts
}

/// The rows' distinct candidate addresses, in first-seen order — an anchor's
/// relevance set for the carry-forward's new-write rule.
#[must_use]
pub fn candidate_addresses(rows: &[Recorded]) -> Vec<Address> {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for row in rows {
        for (address, _) in &row.reads {
            if seen.insert(*address) {
                ordered.push(*address);
            }
        }
    }
    ordered
}

/// The corpus's set analysis: the counts a corpus description pins, computed
/// from the parsed rows so a replay and the live probe compare the same
/// numbers instead of trusting a printed line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorpusShape {
    /// Recorded candidates (trace rows).
    pub candidates: usize,
    /// Candidate anchors: blocks the corpus recorded candidates at.
    pub candidate_anchors: usize,
    /// Recorded candidate-anchor keys: every recorded slot read, repeats
    /// included.
    pub candidate_anchor_keys: usize,
    /// Distinct identities per anchor, summed — the cold leg's anchored reads.
    pub anchored_keys: usize,
    /// Distinct identities over the whole corpus.
    pub global_keys: usize,
    /// Distinct addresses the corpus reads.
    pub contracts: usize,
}

/// Measure one corpus's shape.
#[must_use]
pub fn shape(rows: &[Recorded]) -> CorpusShape {
    let mut anchors: BTreeMap<u64, BTreeSet<Key>> = BTreeMap::new();
    let mut global: BTreeSet<Key> = BTreeSet::new();
    let mut contracts: BTreeSet<Address> = BTreeSet::new();
    let mut candidate_anchor_keys = 0usize;
    for row in rows {
        let anchor = anchors.entry(row.block).or_default();
        for (address, slots) in &row.reads {
            contracts.insert(*address);
            for slot in slots {
                candidate_anchor_keys += 1;
                anchor.insert((*address, *slot));
                global.insert((*address, *slot));
            }
        }
    }
    CorpusShape {
        candidates: rows.len(),
        candidate_anchors: anchors.len(),
        candidate_anchor_keys,
        anchored_keys: anchors.values().map(BTreeSet::len).sum(),
        global_keys: global.len(),
        contracts: contracts.len(),
    }
}

/// A candidate-density regime: the size class of an anchor's fan-out. The
/// thresholds are a declared taxonomy over fan-out size, not a fit to any
/// corpus, so segments from different corpora stay comparable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DensityRegime {
    /// A trickle anchor: at most [`SPARSE_MAX`] candidates.
    Sparse,
    /// An ordinary block: up to fifty candidates.
    Moderate,
    /// A burst block: at least [`BURST_MIN`] candidates.
    Burst,
}

/// The largest fan-out still [`DensityRegime::Sparse`].
pub const SPARSE_MAX: usize = 14;

/// The smallest fan-out already [`DensityRegime::Burst`].
pub const BURST_MIN: usize = 51;

impl DensityRegime {
    /// The short label the report prints.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sparse => "sparse",
            Self::Moderate => "moderate",
            Self::Burst => "burst",
        }
    }
}

/// Classify one anchor's fan-out into the declared density regimes.
#[must_use]
pub const fn classify_density(candidates: usize) -> DensityRegime {
    if candidates <= SPARSE_MAX {
        DensityRegime::Sparse
    } else if candidates < BURST_MIN {
        DensityRegime::Moderate
    } else {
        DensityRegime::Burst
    }
}

/// One anchor's measured place in the corpus: how big its fan-out was and how
/// much of its read set the corpus had never seen before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorProfile {
    /// The anchor's block.
    pub block: u64,
    /// The density regime its fan-out lands in.
    pub regime: DensityRegime,
    /// Recorded candidates at the anchor.
    pub candidates: usize,
    /// Recorded reads at the anchor, repeats included.
    pub read_calls: usize,
    /// Distinct identities the anchor reads.
    pub keys: usize,
    /// Identities whose corpus-first appearance is this anchor.
    pub new_keys: usize,
    /// Distinct addresses the anchor reads.
    pub contracts: usize,
}

/// One density regime's segment: every anchor the corpus recorded in the
/// regime, with the read-set diversity measured over their reads. The anchors
/// need not be contiguous — the segment is the regime, not a phase — and a
/// regime the corpus never recorded produces no segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    /// The density regime every anchor of the segment lands in.
    pub regime: DensityRegime,
    /// The segment's anchors' blocks, in block order.
    pub anchors: Vec<u64>,
    /// Recorded candidates over the segment's anchors.
    pub candidates: usize,
    /// Recorded reads over the segment's anchors, repeats included.
    pub read_calls: usize,
    /// Distinct identities the segment's anchors read.
    pub keys: BTreeSet<Key>,
    /// Keys whose corpus-first appearance is one of the segment's anchors.
    pub new_keys: usize,
    /// Keys the corpus read at an earlier anchor before this segment read them.
    pub repeated_keys: usize,
    /// Distinct addresses the segment's anchors read.
    pub contracts: usize,
    /// Blocks since the key's corpus-first appearance, summed over every
    /// distinct `(anchor, key)` read, and the count of those pairs.
    pub key_age_sum_blocks: u64,
    /// The count `key_age_sum_blocks` averages over.
    pub key_age_pairs: u64,
    /// The longest blocks-since-first-appearance any recorded read carried.
    pub key_age_max_blocks: u64,
    /// Anchors-that-read-the-key summed over the segment's distinct keys over
    /// the whole corpus: the carry-forward's reuse depth per key.
    pub survival_anchor_sum: u64,
    /// Segment keys more than one anchor reads.
    pub recurred_keys: usize,
    /// Segment keys some anchor past the segment's last anchor still reads.
    pub surviving_keys: usize,
}

/// The per-anchor read sets, and the per-key first appearance and anchor
/// membership, the segmentation measures over.
#[derive(Default)]
struct AnchorReads {
    candidates: usize,
    read_calls: usize,
    keys: BTreeSet<Key>,
    contracts: BTreeSet<Address>,
}

/// The segmentation's inputs: every anchor's reads beside the per-key history.
#[derive(Default)]
struct CorpusReads {
    anchors: BTreeMap<u64, AnchorReads>,
    first_seen: BTreeMap<Key, u64>,
    seen_at: BTreeMap<Key, BTreeSet<u64>>,
}

fn anchor_reads(rows: &[Recorded]) -> CorpusReads {
    let mut reads = CorpusReads::default();
    for row in rows {
        let anchor = reads.anchors.entry(row.block).or_default();
        anchor.candidates += 1;
        for (address, slots) in &row.reads {
            anchor.contracts.insert(*address);
            for slot in slots {
                anchor.read_calls += 1;
                let key = (*address, *slot);
                anchor.keys.insert(key);
                reads
                    .first_seen
                    .entry(key)
                    .and_modify(|first| *first = (*first).min(row.block))
                    .or_insert(row.block);
                reads.seen_at.entry(key).or_default().insert(row.block);
            }
        }
    }
    reads
}

/// Profile every anchor, in block order.
#[must_use]
pub fn anchor_profiles(rows: &[Recorded]) -> Vec<AnchorProfile> {
    let reads = anchor_reads(rows);
    reads
        .anchors
        .iter()
        .map(|(block, anchor)| AnchorProfile {
            block: *block,
            regime: classify_density(anchor.candidates),
            candidates: anchor.candidates,
            read_calls: anchor.read_calls,
            keys: anchor.keys.len(),
            new_keys: anchor
                .keys
                .iter()
                .filter(|key| reads.first_seen[*key] == *block)
                .count(),
            contracts: anchor.contracts.len(),
        })
        .collect()
}

/// Segment the anchors into the density regimes the corpus actually records,
/// one segment per regime in first-appearance order.
#[must_use]
pub fn segments_by_density(rows: &[Recorded]) -> Vec<Segment> {
    let reads = anchor_reads(rows);
    let mut order: Vec<DensityRegime> = Vec::new();
    let mut members: BTreeMap<DensityRegime, Vec<u64>> = BTreeMap::new();
    for (block, anchor) in &reads.anchors {
        let regime = classify_density(anchor.candidates);
        let entry = members.entry(regime).or_default();
        if entry.is_empty() {
            order.push(regime);
        }
        entry.push(*block);
    }
    order
        .into_iter()
        .map(|regime| segment(regime, &members[&regime], &reads))
        .collect()
}

/// Fold one regime's anchors into their segment.
fn segment(regime: DensityRegime, blocks: &[u64], reads: &CorpusReads) -> Segment {
    let mut keys: BTreeSet<Key> = BTreeSet::new();
    let mut contracts: BTreeSet<Address> = BTreeSet::new();
    let mut candidates = 0usize;
    let mut read_calls = 0usize;
    let mut age_sum = 0u64;
    let mut age_pairs = 0u64;
    let mut age_max = 0u64;
    let mut last = 0u64;
    for block in blocks {
        let anchor = &reads.anchors[block];
        last = last.max(*block);
        candidates += anchor.candidates;
        read_calls += anchor.read_calls;
        contracts.extend(anchor.contracts.iter().copied());
        for key in &anchor.keys {
            keys.insert(*key);
            let age = block.saturating_sub(reads.first_seen[key]);
            age_sum += age;
            age_pairs += 1;
            age_max = age_max.max(age);
        }
    }
    let new_keys = keys
        .iter()
        .filter(|key| blocks.contains(&reads.first_seen[*key]))
        .count();
    let mut survival_anchor_sum = 0u64;
    let mut recurred_keys = 0usize;
    let mut surviving_keys = 0usize;
    for key in &keys {
        let read_at = &reads.seen_at[key];
        survival_anchor_sum += u64::try_from(read_at.len()).unwrap_or(u64::MAX);
        recurred_keys += usize::from(read_at.len() > 1);
        surviving_keys += usize::from(read_at.iter().any(|block| *block > last));
    }
    Segment {
        regime,
        anchors: blocks.to_vec(),
        candidates,
        read_calls,
        keys: keys.clone(),
        new_keys,
        repeated_keys: keys.len() - new_keys,
        contracts: contracts.len(),
        key_age_sum_blocks: age_sum,
        key_age_pairs: age_pairs,
        key_age_max_blocks: age_max,
        survival_anchor_sum,
        recurred_keys,
        surviving_keys,
    }
}

/// The corpus's tracked identity universe and its block range, for the modeled
/// chain's write history.
#[must_use]
pub fn universe(rows: &[Recorded]) -> (BTreeSet<Key>, u64, u64) {
    let mut keys = BTreeSet::new();
    let mut first = u64::MAX;
    let mut last = u64::MIN;
    for row in rows {
        first = first.min(row.block);
        last = last.max(row.block);
        for (address, slots) in &row.reads {
            for slot in slots {
                keys.insert((*address, *slot));
            }
        }
    }
    (keys, first, last)
}
