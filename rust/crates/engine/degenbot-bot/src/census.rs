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

/// One drain-path happening, data-carrying where a metric needs a value.
/// One variant per instrument answer; `instruments::record` is the sole
/// dispatcher.
#[derive(Debug, Clone, Copy)]
pub enum CensusEvent {
    /// The epoch race closed: header accepted → Published-stage dispatch
    /// (ADR-041 §3 item 1 — submission/delivery subscribe at this edge).
    HeaderPublished { secs: f64 },
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
        binding: crate::arb_engine::fleet_status::FleetBinding,
        oversubscribed: bool,
    },
    /// A batch of `n` candidates entered the simulate fan-out.
    CandidatesFanOut { n: u64 },
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
}
