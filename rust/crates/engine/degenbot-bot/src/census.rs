//! The pipeline census event vocabulary — the drain path's one emit language.
//!
//! Every pipeline stage (the block pump, the solve cycle, the fleet seats,
//! the delivery/submission arms) reports happenings as a closed
//! [`CensusEvent`]; `instruments::record` is the one door. The census alone
//! decides which instrument, labels, and buckets answer an event: emit sites
//! never name a metric, never touch `pipeline()`, and carry no None-guard —
//! the guard folded inside `record` (one branch per observation, the same
//! discipline as `log::debug!`).
//!
//! Variant names are drawn from the pipeline's own vocabulary — the ADR-041
//! stage table, the Batch outcome record, `GLOSSARY.md` — never from the
//! metric an event feeds. The metric names and their verbatim
//! Prometheus/Grafana couplings live in `instruments.rs` and nowhere else;
//! `tests/metric_name_parity.rs` pins that coupling.
//!
//! Label payloads are `&'static str` by construction: the
//! `tests/metric_cardinality.rs` gate admits only closed `&'static` value
//! sets, so an event payload that could not be `&'static` is that gate's
//! refusal arriving at compile time.

use crate::bot_core::registration_ledger::{
    OutcomeLabel, RegistrationOutcome, RegistrationUnitOutcome,
};

/// One drain-path happening, data-carrying where a metric needs a value.
/// One variant per instrument answer; `instruments::record` is the sole
/// dispatcher.
#[derive(Debug, Clone, Copy)]
pub enum CensusEvent {
    /// The epoch race closed: header accepted → Published-stage dispatch
    /// (ADR-041 §3 item 1 — submission/delivery subscribe at this edge).
    HeaderPublished { secs: f64 },
    /// The block's solve cycle finished: header accepted → the settle/drain
    /// solve's completion (the block-to-solve gap the soak percentiles read;
    /// distinct from the epoch race above, which ends at Published dispatch).
    BlockSolved { secs: f64 },
    /// The Streaming stage's wait: first relevant log → quiesce/tombstone.
    StreamingStageWait { secs: f64 },
    /// One stale-epoch work item dropped by the rewind-generation fail-fast
    /// (ADR-041 I3). Sustained non-zero = the stage cycle chronically spills.
    StaleEpochDrop,
    /// Header accepted → first relevant log delivered (pre-solve gap leg 1).
    FirstLogDelivered { secs: f64 },
    /// First → last relevant log delivered (pre-solve gap leg 2, burst jitter).
    LogBurstDelivered { secs: f64 },
    /// Last relevant log → settle decision (pre-solve gap leg 3, debounce wait).
    SettleDecided { secs: f64 },
    /// One log-decode phase duration.
    LogDecodePhase { secs: f64 },
    /// One successful state-apply phase duration.
    StateApplyPhase { secs: f64 },
    /// One accepted block header.
    HeaderAccepted,
    /// One relevant-topic log dispatched by the pump.
    LogDispatched,
    /// One successful pool apply.
    LogApplied,
    /// One relevant-topic log that decoded but matched no registered pool.
    LogUnmatched,
    /// One reorg episode entered (`EnterReorg`).
    ReorgWindowEntered,
    /// One per-pool journal restore that CHANGED state.
    PoolStateUnwound,
    /// Rollback depth at reorg-episode entry, in blocks.
    ReorgDepth { blocks: u64 },
    /// One log event discarded by the recovery-anchor rule.
    RecoveryAnchorDropped,
    /// One forward log admitted LATE (past its block's D1 tombstone) and
    /// dropped un-applied via the benign late-admit path.
    LateLogAdmitted,
    /// The settle window the timers are currently armed with (ms), observed
    /// once per settle point.
    QuiesceWindowArmed { ms: u64 },
    /// One WS log event received by the pump (pre topic-filter).
    WsLogReceived,
    /// One relevant-topic log a decoder recognized.
    LogDecoded,
    /// One relevant-topic log that matched no decoder (undecoded topic0).
    LogUndecoded,
    /// CFS throttle deltas since the previous sample. Zero deltas are
    /// skipped inside the census (no per-block sampler noise).
    CgroupThrottleDelta { events: u64, usecs: u64 },
    /// One executed backfill range.
    BackfillExecuted,
    /// Signed `pool_state_head - engine_clock` divergence, in blocks.
    HeadLag { blocks: i64 },
    /// Age (seconds) of the newest accepted header; grows on a feed stall.
    HeaderAge { secs: f64 },
    /// Age (seconds) of the newest log applied to state; grows on a freeze.
    ApplyAge { secs: f64 },
    /// One flat sample of the searcher feed's closed 7-field status
    /// snapshot; the caller's tick owns the cadence and passes counter
    /// DELTAS since its previous sample.
    BackrunFeedSample {
        connected: bool,
        seconds_since_event: Option<f64>,
        frames: u64,
        dropped_ring: u64,
        rejected_parse: u64,
        rejected_chain_id: u64,
        reconnects: u64,
    },
    /// One frame reached its terminal decision; `reason` is empty for a
    /// plain `bid` (the pipeline's closed `Decision` reason set).
    BackrunFrameDecided {
        decision: &'static str,
        reason: &'static str,
    },
    /// The engine-`Mutex` hold of a dirty solve cycle closed (T2). `arm` is
    /// the cycle-span vocabulary (`detached` | `in_cycle` | `skipped_empty` |
    /// `shed`).
    SolveMutexHeld { secs: f64, arm: &'static str },
    /// One dirty-carrying solve cycle finished (same `arm` attribution as
    /// [`CensusEvent::SolveMutexHeld`] — the pairing answers "what does
    /// degradation cost" per cycle).
    SolveCycleRan { secs: f64, arm: &'static str },
    /// One per-path solve closure finished (gate + decomposed solver).
    PerPathSolved { secs: f64 },
    /// One per-path profit-envelope gate evaluation finished.
    PerPathGated { secs: f64 },
    /// One solve cycle carried dirty work.
    SolveCycleDirty,
    /// The engine's current registered-path count.
    RegisteredPaths { count: u64 },
    /// The host intake backlog depth for `role`.
    IntakeBacklog { role: &'static str, depth: u64 },
    /// One worker-census row: declared workers/slots of `resource` (the
    /// census registry ids — a small closed set).
    WorkerCensusRow {
        resource: &'static str,
        workers: f64,
    },
    /// FF-T5: the resolved fleet profile — one series, value 1, the labels
    /// carry the tier.
    FleetProfileResolved {
        profile: &'static str,
        binding: crate::arb_engine::FleetBinding,
        oversubscribed: bool,
    },
    /// `n` candidates entered a simulation arm: `arm` is the closed
    /// `fanout` | `inline` set naming the arm the batch landed on (the fan-out
    /// adds its pre-filter batch size; the inline/streaming arm adds one per
    /// candidate at the inline-sim seam).
    CandidatesEntered { n: u64, arm: &'static str },
    /// One solver concentrated-liquidity-hop correction (input/forward clamp
    /// or output align).
    SolverClampApplied,
    /// One distinct engine failure of the given closed-set kind
    /// (`crate::telemetry::error_kind`).
    EngineError { kind: &'static str },
    /// One per-path simulation finished.
    PathSimulated { secs: f64 },
    /// One simulation outcome (`profitable`, `not_profitable`, `error`, ...).
    SimulateVerdict { verdict: &'static str },
    /// One candidate's economics at submit entry (wei as f64 — dashboards
    /// chart magnitudes, not exact wei).
    CandidateEconomics { gross_wei: f64, net_wei: f64 },
    /// One candidate's simulated gas.
    CandidateGas { gas: u64 },
    /// One submit outcome (`submitted`, `skipped_pools_claimed`,
    /// `skipped_dry_run`, ...).
    SubmitOutcome { outcome: &'static str },
    /// PRG-2: one registration-candidate skip; `reason` is the small closed
    /// skip set (`v4-admission`, `path-cap`, `dup`, ...).
    RegistrationSkip { reason: &'static str },
    /// Candidate loop start → broadcast latency.
    SubmitLatency { secs: f64 },
    /// Confirmed net profit added (wei).
    ProfitRealized { wei: f64 },
    /// Un-submitted candidate net profit added (wei).
    ProfitMissed { wei: f64 },
    /// One monitor outcome (`confirmed`, `expired`).
    MonitorOutcome { outcome: &'static str },
    /// One delivery attempt's classified failure: the arm's verdict
    /// (`release` | `hold` | `promote`) with the class that decided it
    /// (the delivery taxonomy's closed labels, or `none` for a set with no
    /// recorded failure).
    DeliveryVerdict {
        class: &'static str,
        verdict: &'static str,
    },
    /// One delivery-unknown hold entered: a signed submission whose delivery
    /// attempt left its fate open now freezes the nonce it was signed against.
    DeliveryHoldEntered { strategy: &'static str },
    /// One delivery-unknown hold the head sweep released: the slot moved into
    /// replace-or-dominate and the ledger record closed stale.
    DeliveryHoldSwept { strategy: &'static str },
    /// One lease the head sweep released because the chain had passed its
    /// nonce: one stale reservation no longer freezes every lane's stamp.
    NonceLeaseSwept { strategy: &'static str },
    /// One lane parked instead of signing: its own delivery-unknown hold owns
    /// the slot, so the frames in hand wait for chain evidence or an explicit
    /// replacement.
    DeliverySlotParked { strategy: &'static str },
    /// One solved candidate the pre-sim gas floor suppressed: its margin does
    /// not cover the path's measured execution gas at the block's base fee (the
    /// sub-gas class that ships and then fails verification as no-profit).
    ///
    /// `seam` is the closed set naming the enforcement point that owns the
    /// decision: `inline` (the lane walk refused the candidate its simulation —
    /// the seam that prevents the work) or `delivery` (the delivery window
    /// refused it a batch entry, the only gate when no inline gate runs). A
    /// candidate floored at the inline seam is floored again by the window, and
    /// that echo is the same decision: it is counted once, here, labelled with
    /// the seam that made it.
    AdmissionFloored { seam: &'static str },
    /// Detached merge-pipe in-flight depth (stragglers sent but not yet
    /// applied/dropped), sampled at enqueue + per disposition.
    MergeInFlight { count: u64 },
    /// One detached straggler DROPPED by the Q1a stale/deregister gate.
    StragglerDropped,
    /// One detached outcome LOST to a dead merge drain (failed pipe send, or
    /// drained as the panicked sidecar shut down).
    StragglerSendFailed,
    /// One merge-seat panic caught by the sidecar guard.
    MergePanic,
    /// One solve cycle SHED by capacity-modulated admission.
    CycleShed,
    /// `n` retained admission keys expired by the retention window.
    AdmissionKeysExpired { n: u64 },
    /// One detached straggler applied to the results map.
    StragglerApplied,
    /// One solve cycle DEGRADED to the in-cycle arm (the cap verdict at
    /// begin).
    CycleDegraded,
    /// One state-lock acquisition wait (`site` is the closed `LockSite` set,
    /// `mode` is `read` | `write`).
    StateLockWait {
        site: &'static str,
        mode: &'static str,
        secs: f64,
    },
    /// One post-acquisition state-lock guard hold (same labels as
    /// [`CensusEvent::StateLockWait`]).
    StateLockHold {
        site: &'static str,
        mode: &'static str,
        secs: f64,
    },
    /// Per-block log funnel — the pump's header-epilogue snapshot
    /// (`closing_block` = the header number at which the ledger closed).
    EpochLogsClosed {
        seen: u64,
        received: u64,
        applied: u64,
        ignored: u64,
        closing_block: u64,
    },
    /// One publish-cycle duration (first relevant log → publish).
    PublishCycleDone { secs: f64 },
    /// One Rewind entered (`EnterReorg`).
    RewindEntered,
    /// One Rewind open→close duration.
    RewindClosed { secs: f64 },
    /// ADR-040: one error-outcome sim tallied by its closed reason.
    SimErrorReason { reason: &'static str },
    /// ADR-040: quarantine depth after a quarantine/release transition.
    QuarantineDepth { count: usize },
    /// Close-out: current resident set bytes (drift-watch).
    ResidentSetBytes { bytes: u64 },
    /// One epoch's `WalkMemo` stats observation (the per-epoch stats tap:
    /// four drained deltas plus the live negative-entry gauge).
    WalkMemoEpoch {
        probes: u64,
        hits: u64,
        cache_plays: u64,
        negatives_played: u64,
        negative_entries: u64,
    },
    /// One storage-prefetch lane report: the plan's attribution (`derived`
    /// path-derived pairs + `learned` capture-learned complement), the batch's
    /// member count (`requested` — the slots the memo did not already hold),
    /// the entries actually seeded, and the closed degrade reason when the lane
    /// backed off (`no_runtime` | `rpc_failed`).
    StoragePrefetch {
        derived: u64,
        learned: u64,
        requested: u64,
        seeded: u64,
        skipped: Option<&'static str>,
    },
    /// One JIT/AOT compilation attempt completed: its kind (`jit` | `aot`),
    /// whether it succeeded, and its wall time. Emitted by the backend's own
    /// completion callback, so it is exact per attempt rather than sampled.
    JitCompilation {
        kind: &'static str,
        success: bool,
        secs: f64,
    },
    /// The JIT backend's lookup counters since the previous sample, plus its
    /// live gauges (a DELTA event, like `CgroupThrottleDelta`: the holder
    /// samples its own cumulative snapshot and the census only ever adds
    /// deltas, so the emit cadence cannot skew the totals).
    JitBackendSample {
        lookup_hits: u64,
        lookup_misses: u64,
        resident_programs: u64,
        code_bytes: u64,
    },
}

// ── The registration-skip label collapse ───────────────────────────────────

impl CensusEvent {
    /// The registration-skip fact one folded unit outcome reports — the ONE
    /// fine-to-coarse label collapse, beside the event it serves. `reason` is
    /// a member of the metric's closed label set; a free-form breakdown label
    /// collapses to `other`. `None` records no skip fact: a created
    /// registration or a counted rejection (the counts carry it).
    pub(crate) fn registration_skip(outcome: &RegistrationUnitOutcome) -> Option<Self> {
        use RegistrationUnitOutcome as Unit;
        let reason = match outcome {
            Unit::Skip { label, .. } => match label {
                OutcomeLabel::Vocabulary(labelled) => coarse_label(labelled),
                OutcomeLabel::FreeForm(_) => "other",
            },
            Unit::Cap => "path-cap",
            Unit::RegisterFailed { .. } => "register-fail",
            Unit::Registered { created: false, .. } => "dup",
            Unit::Registered { created: true, .. } | Unit::Reject { .. } => return None,
        };
        Some(Self::RegistrationSkip { reason })
    }
}

/// The metric label one bounded outcome reports — the cardinality collapse
/// onto the closed set; per-error-class detail stays in the outcome
/// breakdown, never in label cardinality.
fn coarse_label(outcome: &RegistrationOutcome) -> &'static str {
    match outcome {
        RegistrationOutcome::Dup => "dup",
        RegistrationOutcome::PathCap => "path-cap",
        RegistrationOutcome::DirectionMismatch
        | RegistrationOutcome::UnknownPoolType
        | RegistrationOutcome::V4NoHash => "candidate-invalid",
        RegistrationOutcome::V4HookRejected | RegistrationOutcome::V4DynamicFeeRejected => {
            "v4-admission"
        }
        RegistrationOutcome::BuildV2Refused
        | RegistrationOutcome::BuildV3Refused
        | RegistrationOutcome::BuildV4Refused => "pool-build-error",
        RegistrationOutcome::RegisterFailed => "register-fail",
        RegistrationOutcome::Registered
        | RegistrationOutcome::PathRejected
        | RegistrationOutcome::Refused(_) => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::CensusEvent;
    use crate::bot_core::registration_ledger::{
        LifecyclePrecondition, OutcomeLabel, RefusalKind, RegistrationOutcome, RegistrationPhase,
        RegistrationUnitOutcome,
    };

    fn skip_of(label: RegistrationOutcome) -> RegistrationUnitOutcome {
        RegistrationUnitOutcome::Skip {
            label: OutcomeLabel::Vocabulary(label),
            counts_as_skip: true,
            detail: None,
        }
    }

    fn reason_of(outcome: &RegistrationUnitOutcome) -> Option<&'static str> {
        CensusEvent::registration_skip(outcome).and_then(|event| match event {
            CensusEvent::RegistrationSkip { reason } => Some(reason),
            _ => None,
        })
    }

    /// The one fine-to-coarse label collapse, pinned value by value: the
    /// metric's closed label set and which outcome reports each member.
    /// Build refusals report `pool-build-error` — the collapsed arm's intent.
    #[test]
    fn every_outcome_reports_its_registration_skip_label() {
        let cases = [
            (RegistrationOutcome::Registered, "other"),
            (RegistrationOutcome::Dup, "dup"),
            (RegistrationOutcome::PathCap, "path-cap"),
            (RegistrationOutcome::DirectionMismatch, "candidate-invalid"),
            (RegistrationOutcome::UnknownPoolType, "candidate-invalid"),
            (RegistrationOutcome::V4NoHash, "candidate-invalid"),
            (RegistrationOutcome::V4HookRejected, "v4-admission"),
            (RegistrationOutcome::V4DynamicFeeRejected, "v4-admission"),
            (RegistrationOutcome::PathRejected, "other"),
            (RegistrationOutcome::BuildV2Refused, "pool-build-error"),
            (RegistrationOutcome::BuildV3Refused, "pool-build-error"),
            (RegistrationOutcome::BuildV4Refused, "pool-build-error"),
            (RegistrationOutcome::RegisterFailed, "register-fail"),
        ];
        for (outcome, expected) in cases {
            assert_eq!(
                reason_of(&skip_of(outcome.clone())),
                Some(expected),
                "{outcome:?}"
            );
        }
        // A refusal rides its own kind-tag vocabulary, never a metric label.
        let refused = RegistrationOutcome::Refused(RefusalKind::LifecyclePrecondition {
            phase: RegistrationPhase::Lifecycle,
            reason: LifecyclePrecondition::MissingVerifyProvider,
        });
        assert_eq!(reason_of(&skip_of(refused)), Some("other"));
        // A free-form driver label collapses for the metric; the breakdown
        // keeps it verbatim.
        assert_eq!(
            reason_of(&RegistrationUnitOutcome::Skip {
                label: OutcomeLabel::FreeForm("driver-owned-tag".to_owned()),
                counts_as_skip: true,
                detail: None,
            }),
            Some("other")
        );
    }

    /// Which unit outcomes report a skip fact at all: both dedup producers
    /// fold to one outcome and one fact; a created registration and a counted
    /// rejection record none — the counts carry them.
    #[test]
    fn skip_class_outcomes_report_a_fact_and_the_others_do_not() {
        assert_eq!(reason_of(&RegistrationUnitOutcome::Cap), Some("path-cap"));
        assert_eq!(
            reason_of(&RegistrationUnitOutcome::RegisterFailed { detail: None }),
            Some("register-fail")
        );
        assert_eq!(
            reason_of(&RegistrationUnitOutcome::Registered {
                created: false,
                v4_hops: 0
            }),
            Some("dup")
        );
        assert_eq!(
            reason_of(&RegistrationUnitOutcome::Registered {
                created: true,
                v4_hops: 0
            }),
            None
        );
        assert_eq!(
            reason_of(&RegistrationUnitOutcome::Reject { detail: None }),
            None
        );
    }
}

// ── The test-only capture at the census door ──────────────────────────────

/// The internal seam a module's own tests use to assert what a fold reported
/// at the census door, without standing up the instrument set.
#[cfg(test)]
pub(crate) mod test_probe {
    use super::CensusEvent;
    use std::cell::RefCell;

    std::thread_local! {
        static RECORDED: RefCell<Vec<CensusEvent>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn record(event: CensusEvent) {
        RECORDED.with(|recorded| recorded.borrow_mut().push(event));
    }

    pub(crate) fn take() -> Vec<CensusEvent> {
        RECORDED.with(|recorded| std::mem::take(&mut *recorded.borrow_mut()))
    }
}
