//! The delta-versus-full-clone parity replay: one scripted chain driven
//! through BOTH snapshot representations at once — the full-clone control
//! ([`ReadHubCache`]) and the parent-linked delta chain ([`DeltaSnapshotCache`])
//! — with every carry report field and every resolved value compared.
//!
//! The replay is the parity harness behind
//! `tests/bal_delta_snapshot.rs` and `bal_delta_snapshot_probe.rs`: the caller
//! supplies the chain (its stub answers the same words and the same raw lists
//! to both representations) and the blocks to advance; this module owns the
//! comparison, so "zero mismatches" means one place checked everything.

use std::collections::BTreeSet;

use alloy::primitives::{Address, B256, U256};

use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::delta_snapshot::{DeltaReport, DeltaSnapshotCache};
use degenbot_simulation::sim::evm2::read_hub::{CarryReport, ReadHub, ReadHubCache};
use degenbot_simulation::sim::ReadAnchor;

use crate::real_coverage::{Key, RealCoverage};

/// One replayed generation: its anchor, the parent hash the advance is handed,
/// its relevance set, and the identities read there.
#[derive(Debug, Clone)]
pub struct ParityBlock {
    /// The generation's own anchor (number AND hash).
    pub anchor: ReadAnchor,
    /// The parent hash from the block's header — the lineage input.
    pub parent_hash: B256,
    /// The caller's relevance set for this generation.
    pub relevance: Vec<Address>,
    /// The identities resolved and compared at this generation.
    pub reads: Vec<Key>,
}

/// What the replay compared, how deep the delta chain walked, and where the
/// two representations disagreed.
#[derive(Debug, Default)]
pub struct ParityResult {
    /// Generations advanced through both representations.
    pub generations: usize,
    /// Values and report fields compared.
    pub comparisons: usize,
    /// Every disagreement, named by generation and identity.
    pub mismatches: Vec<String>,
    /// The delta side's walk depth per generation, for the depth-bound claim.
    pub delta_depths: Vec<u32>,
}

/// Drive `blocks` through both representations over one chain and compare
/// everything they expose: the carry reports field by field, the snapshot
/// entry counts, and every resolved value.
pub async fn replay(provider: &AlloyProvider, blocks: &[ParityBlock]) -> ParityResult {
    let control = ReadHubCache::new(provider);
    let delta = DeltaSnapshotCache::new(provider);
    let mut result = ParityResult::default();
    for block in blocks {
        let (hub, control_report) = control
            .advance_bal(block.anchor, block.parent_hash, &block.relevance)
            .await;
        let (delta_hub, delta_report) = delta
            .advance_bal(block.anchor, block.parent_hash, &block.relevance)
            .await;
        result.generations = result.generations.saturating_add(1);
        result.delta_depths.push(delta_hub.chain_depth());
        compare_reports(block, &control_report, &delta_report, &mut result);
        result.comparisons = result.comparisons.saturating_add(1);
        if hub.memoized_slots() != delta_hub.snapshot_len() {
            result.mismatches.push(format!(
                "block {}: snapshot entries control {} vs delta {}",
                block.anchor.number,
                hub.memoized_slots(),
                delta_hub.snapshot_len()
            ));
        }
        let control_values = read_control(&hub, &block.reads).await;
        for (&(address, slot), control_value) in block.reads.iter().zip(&control_values) {
            let delta_value = delta_hub.read_storage(address, slot).await;
            result.comparisons = result.comparisons.saturating_add(1);
            let same = match (control_value, &delta_value) {
                (Ok(left), Ok(right)) => left == right,
                _ => false,
            };
            if !same {
                result.mismatches.push(format!(
                    "block {} {address}/{slot}: control {control_value:?} vs delta {delta_value:?}",
                    block.anchor.number
                ));
            }
        }
    }
    result
}

/// The control's read shape: register every read, drain one round, await.
async fn read_control(hub: &ReadHub<'_>, reads: &[Key]) -> Vec<Result<U256, String>> {
    let futures: Vec<_> = reads
        .iter()
        .map(|&(address, slot)| hub.read_storage(address, slot))
        .collect();
    hub.drain_round().await;
    let mut values = Vec::new();
    for read in futures {
        values.push(read.await.map_err(|error| error.to_string()));
    }
    values
}

/// Compare the two reports field by field — the timing fields are the point
/// of the benchmark, never of parity.
fn compare_reports(
    block: &ParityBlock,
    control: &CarryReport,
    delta: &DeltaReport,
    result: &mut ParityResult,
) {
    result.comparisons = result.comparisons.saturating_add(4);
    let fields = [
        (
            "outcome".to_string(),
            format!("{:?}", control.outcome),
            format!("{:?}", delta.outcome),
        ),
        (
            "writes".to_string(),
            format!("{:?}", control.writes),
            format!("{:?}", delta.writes),
        ),
        (
            "writes_overwritten".to_string(),
            control.writes_overwritten.to_string(),
            delta.writes_overwritten.to_string(),
        ),
        (
            "writes_skipped".to_string(),
            control.writes_skipped.to_string(),
            delta.writes_skipped.to_string(),
        ),
    ];
    for (field, left, right) in fields {
        if left != right {
            result.mismatches.push(format!(
                "block {} {field}: control {left} vs delta {right}",
                block.anchor.number
            ));
        }
    }
}

/// Every identity the capture wrote anywhere in its stretch — the read set a
/// real-fixture replay compares.
pub fn real_keys(real: &RealCoverage) -> BTreeSet<Key> {
    let mut keys = BTreeSet::new();
    for number in real.first()..=real.last() {
        keys.extend(real.writes_at(number).iter().copied());
    }
    keys
}

/// The real-fixture parity blocks: every captured block at its exact hash,
/// linked by its captured parent hash, reading every captured write identity
/// under the full tracked-address relevance set.
pub fn real_parity_blocks(real: &RealCoverage, keys: &BTreeSet<Key>) -> Vec<ParityBlock> {
    let relevance: Vec<Address> = keys.iter().map(|(address, _)| *address).collect();
    (real.first()..=real.last())
        .map(|number| ParityBlock {
            anchor: ReadAnchor::identified(
                number,
                real.hash(number).expect("the capture names every block"),
            ),
            parent_hash: real
                .parent_hash(number)
                .expect("the capture links every block"),
            relevance: relevance.clone(),
            reads: keys.iter().copied().collect(),
        })
        .collect()
}
