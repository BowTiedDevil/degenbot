//! The comparison report's pure arithmetic: the snapshot-hit classification,
//! the eliminable storage-wait ceiling, and the cross-leg read-digest gate.
//!
//! Everything here reads numbers the drivers recorded and produces numbers the
//! report prints, so the report's claims are pinned by tests over synthetic
//! inputs rather than by timing assertions.

use std::collections::{BTreeMap, BTreeSet};

use alloy::primitives::{Address, U256};

use crate::real_coverage::RealCoverage;
use crate::wall_ledger::{Digests, Span, WallBucket, WallReport};

/// One anchor's snapshot hits split across their sources. The first two
/// classes come from the corpus's read-call counts and the policy's own key
/// sets; `other` is the hub counter's residual (same-anchor repeats of an
/// identity an earlier candidate fetched, and re-served account rows).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HitSplit {
    /// Hits of identities the anchor's own final writes seeded or overwrote.
    pub final_write: u64,
    /// Hits of identities the snapshot already held from earlier in the run.
    pub carried: u64,
    /// Memo hits neither class above explains.
    pub other: u64,
}

impl HitSplit {
    /// Fold another anchor's split into a leg total.
    pub fn add(&mut self, other: Self) {
        self.final_write += other.final_write;
        self.carried += other.carried;
        self.other += other.other;
    }
}

/// Classify one anchor's memo hits: every recorded read call of an identity the
/// anchor's own final writes carry is a final-write hit, every recorded read
/// call the snapshot already held (and the writes did not overwrite) is a
/// carried hit, and the hub's counter minus those two is the residual.
pub fn classify_hits(
    read_calls: &BTreeMap<(Address, U256), u64>,
    final_write_keys: &BTreeSet<(Address, U256)>,
    carried_keys: &BTreeSet<(Address, U256)>,
    memo_hits: u64,
) -> HitSplit {
    let mut split = HitSplit::default();
    for (key, calls) in read_calls {
        if final_write_keys.contains(key) {
            split.final_write += calls;
        } else if carried_keys.contains(key) {
            split.carried += calls;
        }
    }
    split.other = memo_hits.saturating_sub(split.final_write + split.carried);
    split
}

/// One anchor's storage accounting in the LIVE probe's shape: the anchor's
/// distinct recorded keys split by where their answer came from — the anchor's
/// own final writes, the carried snapshot, or a dynamic fetch — so the replay's
/// per-anchor counts are the same counts `bal_carry_probe` reports. (The
/// call-level [`HitSplit`] is a different denominator: every recorded read
/// call, repeats included.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StorageAccounting {
    /// The anchor's distinct recorded keys.
    pub keys: usize,
    /// Keys answered by a value the snapshot already held from earlier in the
    /// run (the anchor's own writes excepted).
    pub carried: usize,
    /// Keys answered by the anchor block's own final writes.
    pub final_write: usize,
    /// Keys neither class explains: the dynamic batch members they cost.
    pub dynamic: usize,
}

impl StorageAccounting {
    /// Fold another anchor's accounting into a leg total.
    pub fn add(&mut self, other: Self) {
        self.keys += other.keys;
        self.carried += other.carried;
        self.final_write += other.final_write;
        self.dynamic += other.dynamic;
    }
}

/// Classify one anchor's distinct keys the way the live probe counts them: a
/// key the anchor's own final writes carry is a final-write hit even when the
/// snapshot also held it, every other key the snapshot held is a carried hit,
/// and the rest are dynamic members.
pub fn storage_accounting(
    keys: &BTreeSet<(Address, U256)>,
    final_write_keys: &BTreeSet<(Address, U256)>,
    snapshot_keys: &BTreeSet<(Address, U256)>,
) -> StorageAccounting {
    let mut accounting = StorageAccounting {
        keys: keys.len(),
        ..StorageAccounting::default()
    };
    for key in keys {
        if final_write_keys.contains(key) {
            accounting.final_write += 1;
        } else if snapshot_keys.contains(key) {
            accounting.carried += 1;
        } else {
            accounting.dynamic += 1;
        }
    }
    accounting
}

/// One latency point's eliminable storage-wait ceiling: the storage wait the
/// best measured policy removes from the cold leg, absolute and as a share of
/// the cold leg's wall. It is a CEILING because realized wall improvement can
/// never exceed the wait the schedule stops paying; concurrency may hide part
/// of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CeilingReport {
    /// The policy whose residual storage wait is the lowest.
    pub policy: &'static str,
    /// Partitioned storage wait removed versus the cold leg.
    pub eliminated_ns: u64,
    /// The same removal over the gross span sums (lane-time freed).
    pub eliminated_gross_ns: u64,
    /// `eliminated_ns` in thousandths of the cold leg's wall.
    pub share_thousandths: u64,
    /// `eliminated_gross_ns` in thousandths of the cold leg's wall.
    pub gross_share_thousandths: u64,
    /// The cold leg's wall the shares are taken over.
    pub cold_wall_ns: u64,
}

/// Compute the ceiling of one latency point: cold minus the best policy's
/// storage wait, over the cold leg's wall.
///
/// # Errors
///
/// A ceiling over no measured policy, or over a cold leg with no wall, is not
/// a number this report may print.
pub fn eliminable_ceiling(
    cold: &WallReport,
    policies: &[(&'static str, WallReport)],
) -> Result<CeilingReport, String> {
    if policies.is_empty() {
        return Err("the ceiling needs at least one measured policy".to_string());
    }
    if cold.wall_ns == 0 {
        return Err("the cold leg's wall is zero".to_string());
    }
    let storage = WallBucket::StorageWait;
    let (policy, best) = policies
        .iter()
        .min_by_key(|(_, report)| report.bucket(storage))
        .ok_or_else(|| "the policy list is non-empty".to_string())?;
    let eliminated_ns = cold.bucket(storage).saturating_sub(best.bucket(storage));
    let eliminated_gross_ns = cold
        .gross_bucket(storage)
        .saturating_sub(best.gross_bucket(storage));
    let share = |ns: u64| -> u64 { ns.saturating_mul(1_000) / cold.wall_ns };
    Ok(CeilingReport {
        policy,
        eliminated_ns,
        eliminated_gross_ns,
        share_thousandths: share(eliminated_ns),
        gross_share_thousandths: share(eliminated_gross_ns),
        cold_wall_ns: cold.wall_ns,
    })
}

/// Compare every leg's read digests against the baseline: the same recorded
/// candidate must carry the same read-value digest in every leg, whatever its
/// cache policy served.
pub fn identity_gate(baseline: &Digests, legs: &[(String, Digests)]) -> Vec<String> {
    let mut mismatches = Vec::new();
    for (label, digests) in legs {
        if digests.is_empty() {
            mismatches.push(format!("{label}: replayed no candidate"));
            continue;
        }
        for (key, got) in digests {
            match baseline.get(key) {
                None => mismatches.push(format!("{label}: {key:?} is not in the baseline")),
                Some(want) if want != got => {
                    mismatches.push(format!("{label}: {key:?} digest {got} != baseline {want}"));
                }
                Some(_) => {}
            }
        }
    }
    mismatches
}

/// One leg work unit's owned span slice, and the segment its wall is
/// attributed to. A unit is one block advance or one anchor's fan-out drive:
/// the spans it recorded are its wall, whatever clock the recorder stamped on
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkUnit {
    /// Index of the first span the unit recorded (inclusive).
    pub first_span: usize,
    /// Index one past the unit's last span.
    pub end_span: usize,
    /// The segment the unit's wall is attributed to.
    pub segment: usize,
}

/// Attribute every recorded span to the work unit that recorded it and fold
/// gross bucket sums per segment — the per-regime wall view, computed from the
/// same spans the leg's own reconciliation reads.
///
/// # Errors
///
/// A unit naming a segment the run never built, a unit range that runs
/// backwards or escapes the span list, or a span no unit owns is a broken
/// instrumentation record, never a silently clipped one.
pub fn attribute_segment_spans(
    units: &[WorkUnit],
    spans: &[Span],
    segments: usize,
) -> Result<Vec<[u64; 6]>, String> {
    let mut gross = vec![[0u64; 6]; segments];
    let mut cursor = 0usize;
    for unit in units {
        if unit.segment >= segments {
            return Err(format!(
                "work unit {unit:?} names segment {} of {segments}",
                unit.segment
            ));
        }
        if unit.end_span < unit.first_span || unit.end_span > spans.len() {
            return Err(format!(
                "work unit {unit:?} is not a span slice of {} spans",
                spans.len()
            ));
        }
        if unit.first_span != cursor {
            return Err(format!("span {cursor} is not owned by any work unit"));
        }
        for span in &spans[unit.first_span..unit.end_span] {
            let bucket = span.bucket.index();
            let wall = span.end_ns.saturating_sub(span.start_ns);
            gross[unit.segment][bucket] = gross[unit.segment][bucket].saturating_add(wall);
        }
        cursor = unit.end_span;
    }
    if cursor != spans.len() {
        return Err(format!("span {cursor} is not owned by any work unit"));
    }
    Ok(gross)
}

/// One segment's eliminable storage-wait ceiling over the gross span sums: the
/// storage wait the carry stops paying in the segment's own work units, over
/// the segment's cold wall.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SegmentCeiling {
    /// Gross storage-wait spans the carry removes versus cold.
    pub eliminated_gross_ns: u64,
    /// `eliminated_gross_ns` in thousandths of the segment's cold wall.
    pub share_thousandths: u64,
    /// The segment's cold wall the share is taken over.
    pub cold_wall_ns: u64,
}

/// Compute one segment's ceiling from the two legs' gross storage-wait sums.
///
/// # Errors
///
/// A segment with no cold wall has no share to report.
pub fn segment_ceiling(
    cold_storage_gross_ns: u64,
    carry_storage_gross_ns: u64,
    cold_wall_ns: u64,
) -> Result<SegmentCeiling, String> {
    if cold_wall_ns == 0 {
        return Err("the segment's cold wall is zero".to_string());
    }
    let eliminated_gross_ns = cold_storage_gross_ns.saturating_sub(carry_storage_gross_ns);
    Ok(SegmentCeiling {
        eliminated_gross_ns,
        share_thousandths: eliminated_gross_ns.saturating_mul(1_000) / cold_wall_ns,
        cold_wall_ns,
    })
}

/// How much of a read set the real block lists write over one block span.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriteOverlap {
    /// Distinct identities in the read set.
    pub keys: usize,
    /// Read-set identities the span's lists write at least once.
    pub written_in_span: usize,
    /// Write identities over the read set inside the span, repeats across
    /// blocks included.
    pub span_writes: usize,
}

/// Measure the captured write coverage of `keys` over the blocks `first..=last`.
///
/// # Errors
///
/// A block the capture never folded has no write set to measure against.
pub fn write_overlap(
    keys: &BTreeSet<(Address, U256)>,
    real: &RealCoverage,
    first: u64,
    last: u64,
) -> Result<WriteOverlap, String> {
    let mut written: BTreeSet<(Address, U256)> = BTreeSet::new();
    let mut span_writes = 0usize;
    for number in first..=last {
        if real.block(number).is_none() {
            return Err(format!(
                "the fixture captures no block {number} of the span"
            ));
        }
        for &(address, slot) in real.writes_at(number) {
            if keys.contains(&(address, slot)) {
                written.insert((address, slot));
                span_writes += 1;
            }
        }
    }
    Ok(WriteOverlap {
        keys: keys.len(),
        written_in_span: written.len(),
        span_writes,
    })
}

/// Compare the replay's per-anchor accounting against the live probe's counts
/// for the same corpus: the offline numbers reproduce the live pipeline's
/// decisions exactly, or the gate fails.
pub fn cross_check_live(
    replay: &StorageAccounting,
    live: &StorageAccounting,
) -> Result<(), String> {
    if replay == live {
        return Ok(());
    }
    Err(format!(
        "the replay's carry accounting (keys {}, carried {}, final-write {}, dynamic {}) does not \
         match the live bal_carry_probe ({}, {}, {}, {})",
        replay.keys,
        replay.carried,
        replay.final_write,
        replay.dynamic,
        live.keys,
        live.carried,
        live.final_write,
        live.dynamic
    ))
}
