//! The application core: the pump, stage machinery, registration lifecycle,
//! and the thin `Bot` orchestrator facade (ADR-006 D4). The state owner and
//! every substrate seam it shares with strategies —
//! [`degenbot_substrate::BotState`], the planning workspace, pool ingress, the
//! connector index, executor hop views, the session object registry — live in
//! `degenbot-substrate` (ADR-067); consumers resolve it directly.

pub mod backrun_resolver;
pub mod balancer_stable_state;
pub mod balancer_weighted_state;
pub mod block_pump;
pub mod bot;
/// The build cluster on the `Bot` facade (moved from the PyO3 shell):
/// the build-and-register entry points per family, the
/// swap-simulation calc entries, and the construction-DB open — the shell
/// keeps only arg parsing + `PyErr` mapping + the single-flight claim.
pub mod build_register;
pub mod construction_io;
pub mod curve_data_provider_impl;
pub mod curve_state;
/// The snapshot + pump-lifecycle + block-stream cluster on the `Bot`
/// facade: the retained-tx snapshot open/teardown pair, the driver
/// subscribe/resume/stop wrappers, the verification-config setters, and the
/// once-only block-stream hand-off. The cluster's typed errors re-export
/// through `bot.rs` so the shell's import paths hold.
pub mod lifecycle;
pub mod liquidity_verifier;
pub mod pool_builder;
/// The pool-handle ops cluster on the shared state lock: the calc /
/// simulate / encode surface `PyLiquidityPool` drives, with the staged
/// fetch choreography and typed failure vocabulary the PyO3 shell maps
/// through `bot::errmap`.
pub mod pool_ops;
/// The pool-refresh core entry: the update path's one fetch/compare/apply
/// dispatcher keyed by `pool_id` (the shell's `Pool.refresh_via` face).
pub mod refresh;

pub mod pump_control;
pub mod pump_telemetry;
/// The registration cluster on the `Bot` facade (moved from the PyO3 shell):
/// CREATE2-verified pool/token registration, registry-of-record
/// payload shaping, skip-label collapsing, and the tick-map assembly entry
/// points the shell's `assemble_*` methods drive.
pub mod registration;
/// The registration outcome vocabulary + its four negative memos: the one
/// owner of the bounded tags the Rust driver and the Python registration
/// pipeline both read.
pub mod registration_ledger;
pub mod registration_lifecycle;
pub mod reorg_coordinator;
/// The WS-log-shape → `alloy::rpc::types::Log` reconstruction the offline
/// pump→dispatch drive path uses (moved from the PyO3 shell).
pub mod rpc_log;
/// The at-most-once verification claim — the one owner of the claim policy
/// (claim-if-absent / wait-if-present / release-on-settlement).
pub mod verify_claims;

/// The bounded retry dance for transient registration-verify failures.
pub mod verification_retry;

pub mod snapshot_verify;
pub(crate) mod solve_anchor;

#[expect(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]
#[cfg(test)]
mod snapshot_load_tests;
pub mod stage_handlers;
pub mod stage_machine;
pub mod stage_telemetry;

// The pools/substrate-owned types are home at `degenbot_pools::*` and
// `degenbot_substrate::*`; do not re-add compat re-exports in this module.
pub use balancer_stable_state::{
    BalancerStablePoolIdentity, BalancerStablePoolState, RegisterBalancerStablePoolParams,
};
pub use balancer_weighted_state::{
    BalancerWeightedPoolIdentity, BalancerWeightedPoolState, RegisterBalancerWeightedPoolParams,
};
// the block-clock channel type is a shared-kernel fact type
// (degenbot-core), not bot_runtime knowledge — the runtime’s engine merely
// relays header ticks through it and the PyO3 layer subscribes at the edge.
pub use curve_state::{CurvePoolIdentity, CurvePoolState, RegisterCurvePoolParams};
pub use degenbot_core::block_clock_pipe::{BlockClockPipe, BlockNotification};
pub use degenbot_ingestion::RELEVANT_TOPICS;

pub use pump_control::PumpControl;
pub use registration_ledger::{
    BuildFailure, BuildRefusal, HopSignature, IdentityMismatchShape, LifecyclePrecondition,
    RefusalKind, RegisterRefusal, RegistrationLedger, RegistrationOutcome, RegistrationPhase,
    SnapshotLoadReason, UnregistrableRecord, UnsupportedSubject, VerifyOrigin, REFUSAL_KIND_TAGS,
    REGISTRATION_OUTCOMES,
};
pub use registration_lifecycle::{run_registration_lifecycle, run_verify_lifecycle};

pub use stage_handlers::{
    AffectedPaths, CandidateId, Finalize, FinalizeOutcome, Gate, GateOutcome, Publish,
    PublishOutcome, QuiesceOutcome, QuiesceVerdict, Resolve, Rewind, RewindOutcome, Simulate,
    SimulateOutcome, Solve, SolveOutcome, Stage, StageError, StageHandlers,
};
pub use verify_claims::{PoolVerifications, VerifyClaims};

// Re-export the unified block stage machine : the
// per-block state map + decision producer + watchdogs + gates in ONE pure
// machine; the pump drives it (see `bot_core/stage_machine.rs`). The
// retired `BlockClock`/`PumpFSM` types are gone (hard cutover, Q6) —
// their sub-state lives inside `StageMachine`.
pub use stage_machine::{
    BlockState, CompletenessDecision, HeaderDecision, LogDecision, StageDecision, StageMachine,
    WatchdogPhase,
};

// ---------------------------------------------------------------------------
// The canonical pool-family taxonomy (ADR-059 D1). DEFINED in
// `degenbot-pathfinding` — the zero-dependency leaf that owns the graph
// discriminant, the persisted-`kind` roster, and the u8 FFI route — and
// re-exported here at the core door so both drivers read one contract. A
// canonical *defined* in `bot_core` would force the foundation consumers
// (`degenbot-pathfinding`, `degenbot-db`) to depend on this engine crate: a
// crate cycle. Same contract-door shape as the registration ledger (S12);
// Python derives its family sets from the FFI tag list the shell projects
// from this enum.
// ---------------------------------------------------------------------------
pub use ::degenbot_pathfinding::PoolKind;

// ---------------------------------------------------------------------------
// Bot — thin orchestrator facade (ADR-006 D4). Extracted to `bot.rs` (the
// lone ADR-006 D4 helper row not previously file-extracted; siblings
// `log_dispatcher`/`block_pump`/`solve_coordinator`/`reorg_coordinator`/...).
// Reachability path `degenbot_bot::bot_core::Bot` preserved by the re-export
// the 4 reachers (`block_pump`, `degenbot-python/bot/mod.rs`,
// `degenbot-python/bot/pump.rs` ×2) are byte-identical.
// ---------------------------------------------------------------------------

/// Block metadata included in each `ResultBatch`.
///
/// Passed from the pump's WS block header into the drain tick, then forwarded
/// to Python via the result batch channel. The value lives in
/// `degenbot-substrate` (the substrate's epoch + dispatcher consume it);
/// re-exported here so the `BlockPump` + `StageHandlers` seams keep their
/// `bot_core` path.
pub use bot::Bot;
pub use rpc_log::build_rpc_log;
