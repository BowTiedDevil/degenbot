//! The real-candidate READ-SET oracle replay's pure arithmetic: the corpus
//! selection, the per-read comparison against an independent cold leg, and the
//! census a report prints.
//!
//! The replay answers ONE question per selected candidate read: does the
//! generation built from the candidate's own production relevance set answer
//! the word an INDEPENDENT read of the same exact block answers? The corpus
//! carries read identities, not calldata, amounts or deployment state, so a
//! green comparison is storage-read parity — never executable path or verdict
//! parity.
//!
//! # The relevance set is the candidate's own
//!
//! A row's relevance set is what the simulation seam derived from the
//! candidate's path and the deployment's settlement facts BEFORE the candidate
//! ran ([`row_relevance`]). Building a scope from the row's recorded reads
//! instead would replay an oracle the live run never had: a captured read is
//! only known after it happened. A legacy row has no such set, and its fallback
//! is labelled [`RelevanceSource::RecordedReadOracle`] so a report can never
//! print it as production relevance.

use std::collections::{BTreeMap, BTreeSet};

use alloy::primitives::{Address, U256};
use serde_json::Value;

use crate::recorded_corpus::{
    anchor_keys, parse_trace_line, row_relevance, Recorded, Relevance, RelevanceSource, TRACE_TAG,
};

/// One tracked read identity.
pub type Key = (Address, U256);

/// One recorded trace row with the candidate identity the corpus model drops.
///
/// `cand` names the candidate the seam simulated, and it repeats across rows
/// (the same candidate re-simulated, or recorded again at the same anchor), so
/// a DISTINCT candidate is the `(block, cand)` pair.
#[derive(Clone, Debug)]
pub struct TaggedRow {
    /// The row's `cand` field.
    pub candidate: String,
    /// The row itself.
    pub row: Recorded,
}

/// The row's candidate identity, read from the same JSON object
/// [`parse_trace_line`] parses.
fn candidate_of(line: &str) -> Option<String> {
    let start = line.find(TRACE_TAG)? + TRACE_TAG.len();
    let value: Value = serde_json::from_str(line[start..].trim()).ok()?;
    value.get("cand")?.as_str().map(str::to_string)
}

/// Load every recorded row beside its candidate identity, failing loudly when
/// the corpus is absent or carries no trace rows.
///
/// # Errors
///
/// A missing file, or a file with no parseable `fanout-trace` row, is a corpus
/// failure rather than an empty replay.
pub fn load_tagged_rows(path: &str) -> Result<Vec<TaggedRow>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        format!("read the recorded fan-out trace {path}: {error} (this replay has no corpus without it)")
    })?;
    let rows: Vec<TaggedRow> = text
        .lines()
        .filter_map(|line| {
            let row = parse_trace_line(line)?;
            let candidate = candidate_of(line)?;
            Some(TaggedRow { candidate, row })
        })
        .collect();
    if rows.is_empty() {
        return Err(format!("no `{TRACE_TAG}` rows in {path}"));
    }
    Ok(rows)
}

/// One selected candidate row: the relevance set the seam derived BEFORE the
/// candidate ran, and the identities the candidate recorded reading.
#[derive(Clone, Debug)]
pub struct SelectedRow {
    /// The candidate identity (`cand`) the row was recorded under.
    pub candidate: String,
    /// The row's production relevance set, or its labelled oracle fallback.
    pub relevance: Relevance,
    /// The row's distinct recorded read identities, sorted.
    pub keys: Vec<Key>,
}

/// One selected anchor: its block, its recorded fan-out width, and the rows
/// the selection kept there.
#[derive(Clone, Debug)]
pub struct SelectedAnchor {
    /// The candidate anchor.
    pub block: u64,
    /// Recorded candidates at the anchor — the fan-out width a report prints
    /// so a two-row leg is never read as the whole fan-out.
    pub fanout: usize,
    /// The selected rows, widest-and-most-diverse first.
    pub rows: Vec<SelectedRow>,
}

/// A bounded, deterministic corpus selection: anchors spread across the
/// recorded block range, and per anchor the rows whose read sets add the most
/// unseen identities.
#[derive(Clone, Debug)]
pub struct Selection {
    /// The selected anchors, in block order.
    pub anchors: Vec<SelectedAnchor>,
}

impl Selection {
    /// Selected trace rows.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.anchors.iter().map(|anchor| anchor.rows.len()).sum()
    }

    /// Distinct `(block, cand)` identities among the selected rows.
    #[must_use]
    pub fn distinct_candidates(&self) -> BTreeSet<(u64, String)> {
        let mut candidates = BTreeSet::new();
        for anchor in &self.anchors {
            for row in &anchor.rows {
                candidates.insert((anchor.block, row.candidate.clone()));
            }
        }
        candidates
    }

    /// Distinct relevance addresses over the selected rows.
    #[must_use]
    pub fn relevance_addresses(&self) -> BTreeSet<Address> {
        self.anchors
            .iter()
            .flat_map(|anchor| anchor.rows.iter())
            .flat_map(|row| row.relevance.addresses.iter().copied())
            .collect()
    }

    /// Distinct read identities over the selected rows.
    #[must_use]
    pub fn keys(&self) -> BTreeSet<Key> {
        self.anchors
            .iter()
            .flat_map(|anchor| anchor.rows.iter())
            .flat_map(|row| row.keys.iter().copied())
            .collect()
    }

    /// Distinct addresses the selected rows read.
    #[must_use]
    pub fn read_addresses(&self) -> BTreeSet<Address> {
        self.keys()
            .into_iter()
            .map(|(address, _)| address)
            .collect()
    }

    /// Selected rows whose relevance set is the recorded-read oracle fallback
    /// rather than the pre-simulation production derivation.
    #[must_use]
    pub fn oracle_rows(&self) -> usize {
        self.anchors
            .iter()
            .flat_map(|anchor| anchor.rows.iter())
            .filter(|row| row.relevance.source == RelevanceSource::RecordedReadOracle)
            .count()
    }
}

/// The row's distinct recorded read identities.
fn key_set(row: &Recorded) -> BTreeSet<Key> {
    anchor_keys(std::slice::from_ref(row))
}

/// One anchor's rows, widest-and-most-diverse first: each pick adds the most
/// identities no earlier pick carried, then the widest set, then the smallest
/// candidate identity and file index — a deterministic order with no reliance
/// on hash iteration.
fn pick_rows(rows: &[TaggedRow], indices: &[usize], wanted: usize) -> Vec<usize> {
    let key_sets: Vec<BTreeSet<Key>> = indices
        .iter()
        .map(|&index| key_set(&rows[index].row))
        .collect();
    let mut remaining: Vec<usize> = (0..indices.len()).collect();
    let mut union: BTreeSet<Key> = BTreeSet::new();
    let mut chosen: Vec<usize> = Vec::new();
    while chosen.len() < wanted && !remaining.is_empty() {
        remaining.sort_by(|&left, &right| {
            let new_left = key_sets[left].difference(&union).count();
            let new_right = key_sets[right].difference(&union).count();
            new_right
                .cmp(&new_left)
                .then_with(|| key_sets[right].len().cmp(&key_sets[left].len()))
                .then_with(|| {
                    rows[indices[left]]
                        .candidate
                        .cmp(&rows[indices[right]].candidate)
                })
                .then_with(|| indices[left].cmp(&indices[right]))
        });
        let pick = remaining.remove(0);
        union.extend(key_sets[pick].iter().copied());
        chosen.push(indices[pick]);
    }
    chosen
}

/// The evenly spread anchor indices over `len` anchors — first and last
/// included whenever two or more are wanted.
fn spread(len: usize, wanted: usize) -> Vec<usize> {
    if wanted == 0 || len == 0 {
        return Vec::new();
    }
    if wanted >= len {
        return (0..len).collect();
    }
    if wanted == 1 {
        return vec![0];
    }
    let mut picked: Vec<usize> = Vec::new();
    for step in 0..wanted {
        let index = step * (len - 1) / (wanted - 1);
        if picked.last() != Some(&index) {
            picked.push(index);
        }
    }
    picked
}

/// Select a bounded, diverse corpus slice: `anchors_wanted` anchors spread
/// across the recorded block range, and up to `rows_per_anchor` rows per
/// anchor chosen for read-set diversity.
///
/// # Errors
///
/// A zero anchor or row budget, or a corpus with no row, has nothing to
/// replay.
pub fn select(
    rows: &[TaggedRow],
    anchors_wanted: usize,
    rows_per_anchor: usize,
) -> Result<Selection, String> {
    if anchors_wanted == 0 || rows_per_anchor == 0 {
        return Err(format!(
            "the selection budget ({anchors_wanted} anchors x {rows_per_anchor} rows) must be non-zero"
        ));
    }
    let mut by_block: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    for (index, tagged) in rows.iter().enumerate() {
        by_block.entry(tagged.row.block).or_default().push(index);
    }
    if by_block.is_empty() {
        return Err("the corpus has no row to select".to_string());
    }
    let blocks: Vec<u64> = by_block.keys().copied().collect();
    let mut anchors = Vec::new();
    for pick in spread(blocks.len(), anchors_wanted) {
        let block = blocks[pick];
        let indices = &by_block[&block];
        let rows = pick_rows(rows, indices, rows_per_anchor)
            .into_iter()
            .map(|index| {
                let tagged = &rows[index];
                SelectedRow {
                    candidate: tagged.candidate.clone(),
                    relevance: row_relevance(&tagged.row),
                    keys: key_set(&tagged.row).into_iter().collect(),
                }
            })
            .collect();
        anchors.push(SelectedAnchor {
            block,
            fanout: indices.len(),
            rows,
        });
    }
    Ok(Selection { anchors })
}

/// Which side of a generation answered one read: the snapshot the BAL seeded
/// (no RPC), or the dynamic fallback (one round member).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOrigin {
    /// The generation's chain held the identity.
    Snapshot,
    /// The identity was unknown or expired, so a round answered it.
    Dynamic,
}

impl ReadOrigin {
    /// The label a report prints.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Snapshot => "served",
            Self::Dynamic => "dynamic",
        }
    }
}

/// One read's generation-side answer, before the cold comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadAnswer {
    /// The identity read.
    pub key: Key,
    /// Which side of the generation answered it.
    pub origin: ReadOrigin,
    /// The value the generation answered.
    pub value: U256,
}

/// One disagreement between a generation's answer and the independent cold
/// read at the same exact block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    /// The candidate the read belonged to.
    pub candidate: String,
    /// The identity that disagreed.
    pub key: Key,
    /// Which side of the generation answered it.
    pub origin: ReadOrigin,
    /// The generation's value.
    pub generation: U256,
    /// The cold value, when the cold leg answered at all.
    pub cold: Option<U256>,
}

/// What one leg's comparison measured.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Comparison {
    /// Reads compared.
    pub compared: usize,
    /// Reads the generation's snapshot answered.
    pub served: usize,
    /// Reads the dynamic fallback answered.
    pub dynamic: usize,
    /// Reads whose cold value is zero — compared like any other value.
    pub zeros: usize,
    /// Zero-valued reads the snapshot served.
    pub served_zeros: usize,
    /// Every disagreement.
    pub mismatches: Vec<Mismatch>,
}

/// Compare every generation answer against the independent cold leg: a missing
/// cold answer is a disagreement too, never a skipped identity.
#[must_use]
pub fn compare(candidate: &str, answers: &[ReadAnswer], cold: &BTreeMap<Key, U256>) -> Comparison {
    let mut comparison = Comparison {
        compared: answers.len(),
        ..Comparison::default()
    };
    for answer in answers {
        let cold_value = cold.get(&answer.key).copied();
        match answer.origin {
            ReadOrigin::Snapshot => comparison.served += 1,
            ReadOrigin::Dynamic => comparison.dynamic += 1,
        }
        if cold_value == Some(U256::ZERO) {
            comparison.zeros += 1;
            if answer.origin == ReadOrigin::Snapshot {
                comparison.served_zeros += 1;
            }
        }
        if cold_value != Some(answer.value) {
            comparison.mismatches.push(Mismatch {
                candidate: candidate.to_string(),
                key: answer.key,
                origin: answer.origin,
                generation: answer.value,
                cold: cold_value,
            });
        }
    }
    comparison
}

/// The negative control: add one wei to the first generation-side answer, so a
/// comparator that actually reads the generation's value must report a
/// difference. Returns whether an answer was there to skew.
pub fn plant_one_wei(answers: &mut [ReadAnswer]) -> bool {
    match answers.first_mut() {
        Some(answer) => {
            answer.value = answer.value.wrapping_add(U256::from(1u8));
            true
        }
        None => false,
    }
}

/// How many of a leg's read identities the anchor's own block access list
/// writes — the read-set overlap a report states instead of implying.
#[must_use]
pub fn write_overlap(keys: &[Key], writes: &BTreeSet<Key>) -> usize {
    keys.iter().filter(|key| writes.contains(key)).count()
}

/// FNV-1a over arbitrary bytes — a stable fingerprint for the reproduction
/// commands, not a cryptographic digest.
#[must_use]
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The fingerprint a report prints.
#[must_use]
pub fn fingerprint(bytes: &[u8]) -> String {
    format!("0x{:016x}", fnv1a(bytes))
}

/// The selection's canonical text — block, candidate, relevance addresses,
/// read identities, in selection order — so the selection fingerprint pins
/// exactly the rows a run compared.
#[must_use]
pub fn selection_material(selection: &Selection) -> String {
    let mut lines: Vec<String> = Vec::new();
    for anchor in &selection.anchors {
        lines.push(format!("{}:{}", anchor.block, anchor.fanout));
        for row in &anchor.rows {
            lines.push(format!(
                "  {} {} {}",
                row.candidate,
                row.relevance.source.label(),
                row.relevance
                    .addresses
                    .iter()
                    .map(|address| format!("{address:#x}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ));
            for (address, slot) in &row.keys {
                lines.push(format!("    {address:#x}/{slot:#x}"));
            }
        }
    }
    let mut material = lines.join("\n");
    material.push('\n');
    material
}
