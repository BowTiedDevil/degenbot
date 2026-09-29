//! The Batch outcome record + its typed verdict vocabulary (glossary:
//! "Batch outcome record" — "The typed result of one candidate's ordered pass
//! through the pipeline ... delivered as the stream both first-class consumers
//! drain ... One record vocabulary, so a driver's remaining responsibility is
//! display.").
//!
//! Field set = the union of what the two drains read (the spike IIDYXK
//! attribution): the Python renderers read every stage + `path_info`; the
//! Rust bot's `HeartbeatSink` reads only `block`. Batch-level counters are
//! NOT record fields — they are exact folds ([`fold_counters`]).

use std::collections::BTreeMap;

use alloy::primitives::U256;
use degenbot_arbitrage::SimFailure;
use degenbot_executor::composers::{HopInfo, PathInfo};

/// The typed result of one candidate's ordered pass through the pipeline.
#[derive(Debug, Clone)]
pub struct BatchOutcome {
    /// Keys every stage; read by all four Python renderers.
    pub path_id: u64,
    /// Block the batch was consumed at (the Rust drain's only read today —
    /// the liveness heartbeat).
    pub block: u64,
    /// Stage 1 — the assembly verdict.
    pub assembly: AssemblyVerdict,
    /// Stage 2 — the simulate verdict (`None` only when the candidate never
    /// reached the sim stage, or the fan-out dropped it post-assembly — the
    /// solve-snapshot staleness gate and the per-batch cap drop rows the way
    /// today's dispatch outcome does: silently, with no record).
    pub simulate: Option<SimulateVerdict>,
    /// Stage 3 — the submit receipt or typed failure (`Some` only after a
    /// gas-profitable sim reaches the submit lane). No drain reads inside
    /// this slot today; the glossary mandates the slot.
    pub submit: Option<SubmitVerdict>,
    /// Display context: `path_type` + hop detail (read by the profit-log,
    /// failure, and diag renderers).
    pub path_info: PathInfoView,
}

/// Stage 1 — one variant per pre-sim cause the two consumers apply today.
/// Stable `label()` per the `DispatchDecision` precedent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssemblyVerdict {
    /// The row entered the sim (or payload) stage.
    Assembled,
    /// `hop_outputs` was empty — not an encodable path (the `[sim-none]` skip).
    SkipEmptyHops,
    /// The row's `path_id` did not resolve to a registered `PathInfo`.
    ///
    /// Decision (task A6SXEH a): the two consumers disagreed — the `PyO3` seam
    /// raised `ValueError`, the settlement bot folded the miss into
    /// `SkipEmptyHops`. Unified: a resolve miss is a TYPED SKIP + counted,
    /// never an abort. It is not batch corruption: a path de-registered
    /// between solve and dispatch is a normal race, and the row is otherwise
    /// well-formed. The loud-abort rule is reserved for the two corruption
    /// arms ([`AssemblyError`]): a row whose per-hop lengths disagree with the
    /// RESOLVED path's hop count (raw arm — the batch and the registry
    /// diverged), and a PAYLOAD-row resolve miss (`merge_payload_results` /
    /// `assemble_batch`'s payload arm — a payload row is engine-born, so a
    /// miss there evidences registry divergence). Both re-raise in the
    /// driver's frame, for both consumers.
    SkipResolveMiss,
    /// The engine already simulated this path inline; its record comes from
    /// the payload arm, not the FFI sim batch (the batch-local dedup).
    SkipPayloadServed,
    /// `PathSuppression::is_suppressed` held (the cross-block failure registry).
    SkipSuppressed,
    /// Dropped by the thin-margin pre-filter.
    SkipThinMargin,
    /// Every surviving hop routed through a pool flagged `SolverCalc` within
    /// the decay window.
    SkipDivergentPool,
    /// A hop's input token is FoT-confirmed.
    SkipFeeOnTransfer,
}

impl AssemblyVerdict {
    /// The stable machine label (the driver-diff column).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Assembled => "assembled",
            Self::SkipEmptyHops => "skip-empty-hops",
            Self::SkipResolveMiss => "skip-resolve-miss",
            Self::SkipPayloadServed => "skip-payload-served",
            Self::SkipSuppressed => "skip-suppressed",
            Self::SkipThinMargin => "skip-thin-margin",
            Self::SkipDivergentPool => "skip-divergent-pool",
            Self::SkipFeeOnTransfer => "skip-fee-on-transfer",
        }
    }
}

/// Stage 2 — the per-candidate simulation outcome.
#[derive(Debug, Clone)]
pub enum SimulateVerdict {
    /// The sim committed and net profit reached the floor — every field of
    /// the receipt is renderer-read.
    Profitable(SimReceipt),
    /// Onchain-valid but below the net threshold — the receipt is the same
    /// value the core sorts on.
    GasUnprofitable(SimReceipt),
    /// A per-candidate failure record (the renderer's `[sim-fail]` input).
    /// Boxed: `SimFailure` is far larger than the other variants' payloads.
    Failed(Box<FailureDetail>),
    /// An unrecoverable per-path exception (counted, not propagated — the
    /// core's `return_exceptions=True` tolerance).
    Exception,
}

/// The renderer-read projection of a profitable sim result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimReceipt {
    /// Gross on-chain profit (wei).
    pub gross_profit: U256,
    /// Net profit = gross − gas cost (wei).
    pub net_profit: U256,
    /// The simulate's raw `gasUsed` (UN-inflated; the 1.5× margin is applied
    /// at submit time).
    pub gas_used: u64,
    /// The market-aware priority fee.
    pub priority_fee: u128,
}

/// The per-candidate failure detail. One source of truth: the core fan-out's
/// [`SimFailure`] already carries every field the failure renderers read
/// (bucket, fail index, revert bytes, reverting frame, captured/reverted
/// swaps, call trace, balance legs, log count) — the alias keeps the record
/// vocabulary named without duplicating the struct.
pub type FailureDetail = SimFailure;

/// Stage 3 — the submit receipt or typed failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitVerdict {
    /// The tx was broadcast.
    Submitted,
    /// The candidate reached the submit lane and did not broadcast.
    Failed(FailureKind),
}

/// The typed submit-failure taxonomy (the settlement bot's `FailureKind` —
/// the in-repo precedent; that local copy re-points here at its cut-over).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// A revert label produced by `classify_revert`.
    Revert,
    /// `no-profit` — the sim ran but the path was unprofitable.
    NoProfit,
    /// `int128-overflow` — a V4 amount exceeded `int128`.
    Int128Overflow,
    /// `encode-failed` — the encoder refused the path.
    EncodeFailed,
    /// `rpc-failed` — an RPC failure (sim cold-miss or broadcast failure).
    RpcFailed,
    /// `stale` — the solve snapshot advanced before the stage ran.
    Stale,
    /// Anything else (never silently dropped).
    Other,
}

impl FailureKind {
    /// Classify a `FailBuckets` label (the shared revert taxonomy).
    #[must_use]
    pub fn from_bucket(bucket: &str) -> Self {
        match bucket {
            "no-profit" => Self::NoProfit,
            "int128-overflow" => Self::Int128Overflow,
            "encode-failed" => Self::EncodeFailed,
            "rpc-failed" => Self::RpcFailed,
            "stale" => Self::Stale,
            "empty" | "numeric-revert" => Self::Revert,
            other => {
                // The taxonomy's custom-error + Error(string) + Panic labels
                // are all non-orchestration strings; treat them as reverts.
                if other.starts_with("unknown:0x")
                    || other.starts_with("short:")
                    || other.starts_with("Panic(")
                    || other.starts_with("Error(")
                    || other.ends_with("NotSettled")
                {
                    Self::Revert
                } else {
                    Self::Other
                }
            }
        }
    }

    /// Map a submit-lane skip reason onto the taxonomy.
    ///
    /// The lane's typed skips (pools already claimed, dry-run, inject-code
    /// guard) are orchestration outcomes, not sim failures — `Other` carries
    /// them; a broadcast RPC failure is `RpcFailed`. No drain reads inside
    /// the submit slot today (the spike's attribution table), so this mapping
    /// is the contract, not a parity surface.
    #[must_use]
    pub fn from_skip_reason(reason: &degenbot_submission::SkipReason) -> Self {
        match reason {
            degenbot_submission::SkipReason::BroadcastFailed(_) => Self::RpcFailed,
            degenbot_submission::SkipReason::PoolsClaimed
            | degenbot_submission::SkipReason::DryRun
            | degenbot_submission::SkipReason::InjectCode => Self::Other,
        }
    }

    /// The stable machine label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Revert => "revert",
            Self::NoProfit => "no-profit",
            Self::Int128Overflow => "int128-overflow",
            Self::EncodeFailed => "encode-failed",
            Self::RpcFailed => "rpc-failed",
            Self::Stale => "stale",
            Self::Other => "other",
        }
    }
}

/// Display context for one candidate: the combined pool-type label
/// (`"V2-V3"`, `"V4-V2"`, … — the renderers' `path_type`) + the typed hops.
#[derive(Debug, Clone)]
pub struct PathInfoView {
    /// The combined pool-type label.
    pub path_type: String,
    /// The typed hops (family + that variant's fields — the renderers read
    /// per-variant fields directly off `HopInfo`).
    pub hops: Vec<HopInfo>,
}

impl PathInfoView {
    /// The view for a row that never resolved a path (a pre-resolve skip —
    /// there is nothing to display).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            path_type: String::new(),
            hops: Vec::new(),
        }
    }
}

impl From<&PathInfo> for PathInfoView {
    fn from(path: &PathInfo) -> Self {
        let mut names: Vec<&'static str> = Vec::with_capacity(path.hops.len());
        for hop in &path.hops {
            names.push(match hop {
                HopInfo::V2(_) => "V2",
                HopInfo::V3(_) => "V3",
                HopInfo::V4(_) => "V4",
            });
        }
        Self {
            path_type: names.join("-"),
            hops: path.hops.clone(),
        }
    }
}

/// The batch-level tallies the drains render — exact folds over a batch's
/// records, never record fields.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BatchCounters {
    /// Candidates that reached the sim stage (post pre-filters + cap).
    pub candidate_count: usize,
    /// Simulated paths that committed and reached the profit floor.
    pub profitable_count: usize,
    /// Onchain-valid sims below the net threshold.
    pub gas_unprofitable_count: usize,
    /// Candidates that produced a failure record.
    pub fail_count: usize,
    /// Per-path exceptions (counted, unattributed by the fan-out).
    pub exception_count: usize,
    /// Suppression-skip count.
    pub suppressed_count: usize,
    /// Thin-margin-skip count.
    pub thin_dropped: usize,
    /// Pool-divergence-skip count.
    pub divergent_dropped: usize,
    /// Fee-on-transfer-skip count.
    pub fot_dropped: usize,
    /// Empty-hop-skip count (the `[sim-none]` log's input).
    pub empty_hop_count: usize,
    /// Resolve-miss-skip count.
    pub resolve_miss_count: usize,
    /// Payload-served-skip count (raw rows superseded by the payload arm).
    pub payload_served_count: usize,
    /// Failure bucket → count (the `[sim] ... by reason` fold).
    pub fail_buckets: BTreeMap<String, usize>,
}

/// Fold a batch's records into the render counters.
#[must_use]
pub fn fold_counters(batch: &[BatchOutcome]) -> BatchCounters {
    let mut c = BatchCounters::default();
    for o in batch {
        match o.assembly {
            AssemblyVerdict::Assembled => {}
            AssemblyVerdict::SkipEmptyHops => c.empty_hop_count += 1,
            AssemblyVerdict::SkipResolveMiss => c.resolve_miss_count += 1,
            AssemblyVerdict::SkipPayloadServed => c.payload_served_count += 1,
            AssemblyVerdict::SkipSuppressed => c.suppressed_count += 1,
            AssemblyVerdict::SkipThinMargin => c.thin_dropped += 1,
            AssemblyVerdict::SkipDivergentPool => c.divergent_dropped += 1,
            AssemblyVerdict::SkipFeeOnTransfer => c.fot_dropped += 1,
        }
        match &o.simulate {
            Some(SimulateVerdict::Profitable(_)) => {
                c.candidate_count += 1;
                c.profitable_count += 1;
            }
            Some(SimulateVerdict::GasUnprofitable(_)) => {
                c.candidate_count += 1;
                c.gas_unprofitable_count += 1;
            }
            Some(SimulateVerdict::Failed(f)) => {
                c.candidate_count += 1;
                c.fail_count += 1;
                *c.fail_buckets.entry(f.bucket.clone()).or_insert(0) += 1;
            }
            Some(SimulateVerdict::Exception) => {
                c.candidate_count += 1;
                c.exception_count += 1;
            }
            None => {}
        }
    }
    c
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "tests assert on known-valid inputs; parse_address fixtures are valid"
)]
mod tests {
    use super::*;
    use degenbot_core::address_utils::parse_address;
    use degenbot_executor::composers::V2HopInfo;

    fn v2_path() -> PathInfo {
        let pool = parse_address("0x1111111111111111111111111111111111111111").unwrap();
        let t0 = parse_address("0x2222222222222222222222222222222222222222").unwrap();
        let t1 = parse_address("0x3333333333333333333333333333333333333333").unwrap();
        PathInfo::new(vec![HopInfo::V2(V2HopInfo {
            pool_address: pool,
            token0_address: t0,
            token1_address: t1,
            fee: 30,
            zfo: true,
        })])
    }

    #[test]
    fn assembly_verdict_labels_are_stable() {
        assert_eq!(AssemblyVerdict::Assembled.label(), "assembled");
        assert_eq!(AssemblyVerdict::SkipEmptyHops.label(), "skip-empty-hops");
        assert_eq!(
            AssemblyVerdict::SkipResolveMiss.label(),
            "skip-resolve-miss"
        );
        assert_eq!(
            AssemblyVerdict::SkipPayloadServed.label(),
            "skip-payload-served"
        );
        assert_eq!(AssemblyVerdict::SkipSuppressed.label(), "skip-suppressed");
        assert_eq!(AssemblyVerdict::SkipThinMargin.label(), "skip-thin-margin");
        assert_eq!(
            AssemblyVerdict::SkipDivergentPool.label(),
            "skip-divergent-pool"
        );
        assert_eq!(
            AssemblyVerdict::SkipFeeOnTransfer.label(),
            "skip-fee-on-transfer"
        );
    }

    #[test]
    fn failure_kind_taxonomy_matches_settlement_bot_precedent() {
        assert_eq!(FailureKind::from_bucket("no-profit"), FailureKind::NoProfit);
        assert_eq!(
            FailureKind::from_bucket("int128-overflow"),
            FailureKind::Int128Overflow
        );
        assert_eq!(
            FailureKind::from_bucket("encode-failed"),
            FailureKind::EncodeFailed
        );
        assert_eq!(
            FailureKind::from_bucket("rpc-failed"),
            FailureKind::RpcFailed
        );
        assert_eq!(FailureKind::from_bucket("stale"), FailureKind::Stale);
        assert_eq!(FailureKind::from_bucket("empty"), FailureKind::Revert);
        assert_eq!(
            FailureKind::from_bucket("CurrencyNotSettled"),
            FailureKind::Revert
        );
        assert_eq!(FailureKind::from_bucket("other"), FailureKind::Other);
        assert_eq!(FailureKind::Revert.label(), "revert");
        assert_eq!(FailureKind::RpcFailed.label(), "rpc-failed");
    }

    #[test]
    fn path_info_view_derives_the_type_label_and_keeps_hops() {
        let view = PathInfoView::from(&v2_path());
        assert_eq!(view.path_type, "V2");
        assert_eq!(view.hops.len(), 1);
    }

    #[test]
    fn counters_are_exact_folds_over_records() {
        let path = PathInfoView::from(&v2_path());
        let receipt = SimReceipt {
            gross_profit: U256::from(100_u64),
            net_profit: U256::from(90_u64),
            gas_used: 300_000,
            priority_fee: 2,
        };
        let failure = FailureDetail {
            path_id: 3,
            bucket: "no-profit".to_string(),
            fail_index: None,
            revert_data: alloy::primitives::Bytes::new(),
            reverting_frame: None,
            captured_swaps: Vec::new(),
            log_full_count: 0,
            reverted_swaps: Vec::new(),
            optimal_input: 0,
            hop_outputs: Vec::new(),
            call_trace: Vec::new(),
            weth_before: 0,
            weth_after: 0,
            eth_before: 0,
            eth_after: 0,
            erc6909_before: 0,
            erc6909_after: 0,
        };
        let batch = vec![
            BatchOutcome {
                path_id: 1,
                block: 100,
                assembly: AssemblyVerdict::Assembled,
                simulate: Some(SimulateVerdict::Profitable(receipt)),
                submit: Some(SubmitVerdict::Submitted),
                path_info: path.clone(),
            },
            BatchOutcome {
                path_id: 2,
                block: 100,
                assembly: AssemblyVerdict::SkipSuppressed,
                simulate: None,
                submit: None,
                path_info: path.clone(),
            },
            BatchOutcome {
                path_id: 3,
                block: 100,
                assembly: AssemblyVerdict::Assembled,
                simulate: Some(SimulateVerdict::Failed(Box::new(failure))),
                submit: None,
                path_info: path,
            },
        ];
        let c = fold_counters(&batch);
        assert_eq!(c.candidate_count, 2);
        assert_eq!(c.profitable_count, 1);
        assert_eq!(c.fail_count, 1);
        assert_eq!(c.suppressed_count, 1);
        assert_eq!(c.fail_buckets.get("no-profit"), Some(&1));
    }
}
