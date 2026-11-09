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
