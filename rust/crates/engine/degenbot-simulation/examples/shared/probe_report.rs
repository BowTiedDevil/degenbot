//! The comparison report's pure arithmetic: the snapshot-hit classification,
//! the eliminable storage-wait ceiling, and the cross-leg read-digest gate.
//!
//! Everything here reads numbers the drivers recorded and produces numbers the
//! report prints, so the report's claims are pinned by tests over synthetic
//! inputs rather than by timing assertions.

use std::collections::{BTreeMap, BTreeSet};

use alloy::primitives::{Address, U256};

use crate::wall_ledger::{Digests, WallBucket, WallReport};

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
