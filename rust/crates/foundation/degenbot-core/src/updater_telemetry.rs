//! The updater→application telemetry port.
//!
//! The updater crates (`degenbot-pool-updater`, `degenbot-aave`) sit BELOW the
//! engine: the architecture gates
//! (`crates/facade/degenbot/tests/architecture_gates.rs`) pin that an
//! integration may not depend on `degenbot-bot`, while the instruments
//! registry is application-owned (`degenbot_bot::instruments`, behind its
//! `otel` feature). The chunk loops therefore report the numeric twins of
//! their `degenbot.updater.*` trace spans — stage wall times, the write-lock
//! hold, and RPC round trips — through this function-pointer bundle the host
//! installs once at metrics boot ([`register`]; installed by
//! `degenbot_bot::metrics::init_global_metrics_with_addr`, so the port exists
//! exactly when the meter it forwards to can exist).
//!
//! Unregistered builds (pure-Rust consumers without the metrics stack)
//! observe every hook as absent — the same `pipeline() == None` no-op
//! discipline as `degenbot_substrate::telemetry_port`. Unlike that port, the
//! label vocabulary is not merely a documented string convention: the hooks
//! take the typed [`UpdaterKind`] × [`UpdaterStage`] closed sets, so a stray
//! label value cannot compile (ADR-043 §9 review, 2026-10-06 telemetry
//! dispatch approved by the project manager — full citation in
//! `degenbot-bot`'s `tests/metric_cardinality.rs::ALLOWED_LABELS`).
//! Per-chunk detail (chain, block range, counts) belongs in the
//! `degenbot.updater.*` TRACE span fields, never in metric labels (the
//! instruments cardinality law).
//!
//! The port is PASSIVE telemetry: every call site already owns the measured
//! `Instant` stage spans (the replay-bench baseline measurement); reporting
//! the number costs one branch behind a `OnceLock`. No hook may move the SQL
//! statement stream, the RPC request stream, or a rollback boundary — the
//! golden gates (statement ledgers, DB dumps, RPC round-trip/byte counters)
//! pin all three.

use std::sync::OnceLock;

/// The emitting updater's identity — the closed value set of the metric
/// label `updater` (ADR-043 §9 review, 2026-10-06 telemetry dispatch; see
/// `degenbot-bot`'s `tests/metric_cardinality.rs`). Call sites pass a
/// variant; the single enum→`&'static str` conversion is the host install's
/// [`UpdaterKind::label`] call, so a stray value cannot compile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdaterKind {
    /// The pool updater (`degenbot-pool-updater`).
    Pool,
    /// The Aave updater (`degenbot-aave`).
    Aave,
}

impl UpdaterKind {
    /// The closed-set metric label string for this updater.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pool => "pool",
            Self::Aave => "aave",
        }
    }
}

/// The updater chunk-loop stage — the closed value set of the metric label
/// `stage` (ADR-043 §9 review, 2026-10-06 telemetry dispatch; see
/// `degenbot-bot`'s `tests/metric_cardinality.rs`). A superset of the four
/// stages the two emitters report today: `chunk` and `cleanup` are the
/// reviewed reserve names matching the `degenbot.updater.*` trace-span stage
/// vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdaterStage {
    /// Whole-chunk framing work.
    Chunk,
    /// RPC log fetch.
    Fetch,
    /// Decode + compute.
    Compute,
    /// Verification.
    Verify,
    /// Apply + commit.
    Apply,
    /// Post-commit cleanup.
    Cleanup,
}

impl UpdaterStage {
    /// The closed-set metric label string for this stage.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Chunk => "chunk",
            Self::Fetch => "fetch",
            Self::Compute => "compute",
            Self::Verify => "verify",
            Self::Apply => "apply",
            Self::Cleanup => "cleanup",
        }
    }
}

/// Host-installed updater telemetry hooks. Call sites pass the typed closed
/// set variants; the metric-label strings exist only behind
/// [`UpdaterKind::label`] / [`UpdaterStage::label`].
#[derive(Clone, Copy)]
pub struct UpdaterInstruments {
    /// Observe one chunk stage's wall time ([`UpdaterKind`] ×
    /// [`UpdaterStage`] — the closed `updater`/`stage` label sets).
    pub observe_stage: fn(updater: UpdaterKind, stage: UpdaterStage, seconds: f64),
    /// Observe one write-lock hold — `transaction()` open → commit/drop —
    /// for `updater`.
    pub observe_lock_hold: fn(updater: UpdaterKind, seconds: f64),
    /// Add `count` JSON-RPC round trips attributed to `updater`'s run (the
    /// production twin of the cassette gates' round-trip counters).
    pub add_rpc_round_trips: fn(updater: UpdaterKind, count: u64),
}

static PORT: OnceLock<Option<UpdaterInstruments>> = OnceLock::new();

/// Install (or explicitly clear) the telemetry bundle. Later calls are
/// no-ops (`OnceLock`).
pub fn register(instruments: Option<UpdaterInstruments>) {
    let _ = PORT.set(instruments);
}

/// The installed bundle, or `None` when the host never registered one.
#[must_use]
pub fn updaters() -> Option<&'static UpdaterInstruments> {
    PORT.get().and_then(|p| p.as_ref())
}

impl UpdaterInstruments {
    /// Observe one chunk stage's wall time.
    pub fn observe_stage(&self, updater: UpdaterKind, stage: UpdaterStage, seconds: f64) {
        (self.observe_stage)(updater, stage, seconds);
    }

    /// Observe one write-lock hold.
    pub fn observe_lock_hold(&self, updater: UpdaterKind, seconds: f64) {
        (self.observe_lock_hold)(updater, seconds);
    }

    /// Add JSON-RPC round trips.
    pub fn add_rpc_round_trips(&self, updater: UpdaterKind, count: u64) {
        (self.add_rpc_round_trips)(updater, count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    static OBSERVED_STAGES: Mutex<Vec<(UpdaterKind, UpdaterStage)>> = Mutex::new(Vec::new());
    static OBSERVED_HOLDS: Mutex<Vec<(UpdaterKind, f64)>> = Mutex::new(Vec::new());
    static OBSERVED_ROUNDS: Mutex<Vec<(UpdaterKind, u64)>> = Mutex::new(Vec::new());

    #[expect(
        clippy::unwrap_used,
        reason = "test fixtures fail loudly on a poisoned lock"
    )]
    fn capture_stage(updater: UpdaterKind, stage: UpdaterStage, _seconds: f64) {
        OBSERVED_STAGES.lock().unwrap().push((updater, stage));
    }

    #[expect(
        clippy::unwrap_used,
        reason = "test fixtures fail loudly on a poisoned lock"
    )]
    fn capture_hold(updater: UpdaterKind, seconds: f64) {
        OBSERVED_HOLDS.lock().unwrap().push((updater, seconds));
    }

    #[expect(
        clippy::unwrap_used,
        reason = "test fixtures fail loudly on a poisoned lock"
    )]
    fn capture_rounds(updater: UpdaterKind, count: u64) {
        OBSERVED_ROUNDS.lock().unwrap().push((updater, count));
    }

    /// The delegate methods forward to the fn pointers with the typed
    /// closed-set labels carried verbatim — the wiring the host install
    /// relies on. The bundle is `Copy`, so this needs no process-global
    /// registration (which would poison the `None` assertion below).
    #[test]
    #[expect(clippy::unwrap_used, reason = "test fixtures fail loudly")]
    fn delegates_forward_to_the_fn_pointers() {
        let bundle = UpdaterInstruments {
            observe_stage: capture_stage,
            observe_lock_hold: capture_hold,
            add_rpc_round_trips: capture_rounds,
        };
        bundle.observe_stage(UpdaterKind::Pool, UpdaterStage::Fetch, 0.25);
        bundle.observe_stage(UpdaterKind::Aave, UpdaterStage::Apply, 90.0);
        bundle.observe_lock_hold(UpdaterKind::Pool, 0.01);
        bundle.add_rpc_round_trips(UpdaterKind::Aave, 7);

        assert_eq!(
            OBSERVED_STAGES.lock().unwrap().as_slice(),
            &[
                (UpdaterKind::Pool, UpdaterStage::Fetch),
                (UpdaterKind::Aave, UpdaterStage::Apply),
            ]
        );
        assert_eq!(
            OBSERVED_HOLDS.lock().unwrap().as_slice(),
            &[(UpdaterKind::Pool, 0.01)]
        );
        assert_eq!(
            OBSERVED_ROUNDS.lock().unwrap().as_slice(),
            &[(UpdaterKind::Aave, 7)]
        );
    }

    /// The label strings are exactly the reviewed closed sets (ADR-043 §9:
    /// `updater` ∈ {`pool`, `aave`}; `stage` ∈ {`chunk`, `fetch`, `compute`,
    /// `verify`, `apply`, `cleanup`}) — the metric-cardinality gate's
    /// provenance comment points here.
    #[test]
    fn label_strings_match_the_reviewed_closed_sets() {
        assert_eq!(
            [UpdaterKind::Pool.label(), UpdaterKind::Aave.label()],
            ["pool", "aave"]
        );
        assert_eq!(
            [
                UpdaterStage::Chunk.label(),
                UpdaterStage::Fetch.label(),
                UpdaterStage::Compute.label(),
                UpdaterStage::Verify.label(),
                UpdaterStage::Apply.label(),
                UpdaterStage::Cleanup.label(),
            ],
            ["chunk", "fetch", "compute", "verify", "apply", "cleanup"]
        );
    }

    /// This test binary never registers the port, so the accessor is `None` —
    /// the unregistered-build no-op discipline the no-metrics consumers rely
    /// on. (Nothing above calls [`register`]; a registered test would make
    /// this assert order-dependent.)
    #[test]
    fn unregistered_builds_observe_every_hook_as_absent() {
        assert!(updaters().is_none());
    }
}
