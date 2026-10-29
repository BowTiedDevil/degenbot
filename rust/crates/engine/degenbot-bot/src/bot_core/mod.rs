//! The application core: the pump, stage machinery, registration lifecycle,
//! and the thin `Bot` orchestrator facade (ADR-006 D4). The state owner and
//! every substrate seam it shares with strategies —
//! [`degenbot_substrate::BotState`], the planning workspace, pool ingress, the
//! connector index, executor hop views, the session object registry — live in
//! `degenbot-substrate` (ADR-067); consumers resolve it directly.

pub(crate) mod backrun_resolver;
pub(crate) mod balancer_stable_state;
pub(crate) mod balancer_weighted_state;
pub(crate) mod block_pump;
pub(crate) mod bot;
/// The build cluster on the `Bot` facade (moved from the PyO3 shell):
/// the build-and-register entry points per family, the
/// swap-simulation calc entries, and the construction-DB open — the shell
/// keeps only arg parsing + `PyErr` mapping + the single-flight claim.
pub(crate) mod build_register;
pub(crate) mod construction_io;
pub(crate) mod curve_data_provider_impl;
pub(crate) mod curve_state;
/// The snapshot + pump-lifecycle + block-stream cluster on the `Bot`
/// facade: the retained-tx snapshot open/teardown pair, the driver
/// subscribe/resume/stop wrappers, the verification-config setters, and the
/// once-only block-stream hand-off. The cluster's typed errors are door items
/// of this module, beside the `Bot` facade they belong to.
pub(crate) mod lifecycle;
pub(crate) mod liquidity_verifier;
pub(crate) mod pool_builder;
/// The pool-handle ops cluster on the shared state lock: the calc /
/// simulate / encode surface `PyLiquidityPool` drives, with the staged
/// fetch choreography and typed failure vocabulary the PyO3 shell maps
/// through `bot::errmap`.
pub(crate) mod pool_ops;
/// The pool-refresh core entry: the update path's one fetch/compare/apply
/// dispatcher keyed by `pool_id` (the shell's `Pool.refresh_via` face).
pub(crate) mod refresh;

pub(crate) mod pump_control;
pub(crate) mod pump_telemetry;
/// The registration cluster on the `Bot` facade (moved from the PyO3 shell):
/// CREATE2-verified pool/token registration, registry-of-record
/// payload shaping, skip-label collapsing, and the tick-map assembly entry
/// points the shell's `assemble_*` methods drive.
pub(crate) mod registration;
/// The registration outcome vocabulary + its four negative memos: the one
/// owner of the bounded tags the Rust driver and the Python registration
/// pipeline both read.
pub(crate) mod registration_ledger;
pub(crate) mod registration_lifecycle;
pub(crate) mod reorg_coordinator;
/// The WS-log-shape → `alloy::rpc::types::Log` reconstruction the offline
/// pump→dispatch drive path uses (moved from the PyO3 shell).
pub(crate) mod rpc_log;
/// The engine session's strategy-plane boot: the one home of the
/// mint → register → attach → install → start ordering the composition root
/// drives, with the plane's own values injected. Returns handles; never runs a
/// consume loop (ADR-050 D9).
pub(crate) mod session_boot;
/// The at-most-once verification claim — the one owner of the claim policy
/// (claim-if-absent / wait-if-present / release-on-settlement).
pub(crate) mod verify_claims;

/// The bounded retry dance for transient registration-verify failures.
pub(crate) mod verification_retry;

pub(crate) mod snapshot_verify;
pub(crate) mod solve_anchor;

#[expect(clippy::unwrap_used)]
#[cfg(test)]
mod backrun_resolver_tests;
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]
#[cfg(test)]
mod snapshot_load_tests;
pub(crate) mod stage_handlers;
pub(crate) mod stage_machine;
pub(crate) mod stage_telemetry;

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
    OutcomeLabel, PathAdmission, PipelineReport, PolicyVerdict, PreBuildIdentity, RefusalKind,
    RegisterRefusal, RegistrationLedger, RegistrationOutcome, RegistrationPhase,
    RegistrationUnitOutcome, SnapshotLoadReason, UnregistrableRecord, UnsupportedSubject,
    VerifyOrigin, REFUSAL_KIND_TAGS, REGISTRATION_OUTCOMES,
};
pub use registration_lifecycle::{run_registration_lifecycle, run_verify_lifecycle};

/// The engine session's strategy-plane boot: the mint → register → attach →
/// install → start ordering the composition root drives, with the plane's own
/// values injected. The boot returns handles and never runs a consume loop
/// (ADR-050 D9).
pub use session_boot::{
    EngineSession, FacetBoot, HeadTriggerComposer, HostedArmBoot, HostedBoot, SessionBootError,
    SessionConstruction,
};

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

/// The per-chain orchestrator facade: the one entry both drivers hold, over
/// the shared `BotState` (ADR-006 D4).
pub use bot::Bot;
pub use rpc_log::build_rpc_log;

// ---------------------------------------------------------------------------
// The door (ADR-084 D2): a consumer names one segment below `bot_core`, never
// an implementation module beneath it. Every submodule above is `pub(crate)`;
// the items the drivers actually use are re-exported here, so the PyO3 shell,
// the submission engine, the console, and a `cargo add degenbot` consumer read
// one surface.
// ---------------------------------------------------------------------------

/// The build-and-register swap-calc rejections the shell maps onto its typed
/// exceptions.
pub use build_register::{CalcTokensInError, CalcTokensOutError};

/// The construction I/O seam: the handle, the two adapters, the pacing types,
/// and the read traits the construction door's dispatch runs over.
pub use construction_io::{
    AlloyRpcConstruction, BlockSelector, BlockSelectorError, ConstructionIo, DbConstruction,
    DegenbotDbConstruction, NoDb, PacedRead, RpcConstruction, TickFetchPace,
};

/// The `Bot` facade's snapshot + block-stream error vocabulary.
pub use lifecycle::{BlockStreamError, CloseSnapshotTxError, SnapshotOpenError};

/// Liquidity verification against a live tick map.
pub use liquidity_verifier::verify_live_map;

/// The pool-handle ops cluster: the calc / simulate / encode entries a
/// `PyLiquidityPool`-shaped handle drives, and their typed failure.
pub use pool_ops::{
    calculate_tokens_in, calculate_tokens_in_for_pair, calculate_tokens_out,
    calculate_tokens_out_for_pair, calculate_tokens_out_with_fetch, curve_data_provider,
    encode_swap, pool_family, simulate_mainline, simulate_override_with_fetch, SwapOpError,
};

/// The construction door (ADR-078 D2) and the per-family build verbs behind
/// it, with the identity/verdict payloads they hand back.
pub use pool_builder::builder::{
    build_aerodrome_v2, build_balancer_stable, build_balancer_weighted, build_curve_pool,
    build_erc20_metadata, build_v2, build_v3, build_v4, derive_hook_flags, fetch_balancer_factory,
    fetch_erc20_metadata_batch, probe_balancer_type, probe_pool_type, resolve_v4_identity,
    BalancerFamily, PoolBuilderError, PoolFamily, V4BuildResult, V4PoolBuildIdentity,
    V4PoolBuildOverrides,
};
pub use pool_builder::construction_door::{ConstructionVerdict, PoolConstruction};
pub use pool_builder::curve_choreography::fetch_curve_balances;
pub use pool_builder::parity::IdentityDivergence;
pub use pool_builder::route::{
    BuiltIdentity, ConstructedPool, ConstructionRefusal, ConstructionRoute, V3RouteInputs,
};

/// The pool-refresh entry: the update path's one fetch/compare/apply
/// dispatcher keyed by `pool_id`.
pub use refresh::{refresh_pool, RefreshError, RefreshOutcome};

/// Registration: the fleet-intake guard, the address/tick shaping the shell's
/// argument parsing delegates to, and the V4 echo payload.
pub use registration::{
    ensure_registration_fleet_hosted, parse_address_str, tick_rows, RegistrationIntakeNotHosted,
    V4RegisteredEcho,
};
pub use reorg_coordinator::{ReorgCoordinator, ReorgError, ReorgOutcome};

/// The stage-machine telemetry seam: the observer a driver wires its stage
/// spans and counters through.
pub use stage_telemetry::StageTelemetry;

/// The bounded retry dance for transient registration-verify failures.
pub use verification_retry::{
    is_retryable, retry_policy_from_config, retry_verification_call, RetryPolicy,
};
