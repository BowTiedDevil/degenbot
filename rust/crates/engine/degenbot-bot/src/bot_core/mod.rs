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
pub mod construction_io;
pub mod curve_data_provider_impl;
pub mod curve_state;
pub mod liquidity_verifier;
pub mod pool_builder;
pub mod pump_control;
pub mod pump_telemetry;
/// The registration cluster on the `Bot` facade (moved from the PyO3 shell,
/// ergo ND7GW7): CREATE2-verified pool/token registration, registry-of-record
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

// Re-export the merged V3/V4/Curve state types (ADR-003: BotState owns
// pool state; Curve is the ADR-003 "third family").
pub use ::degenbot_pools::aerodrome_v2_state::{
    AerodromeV2PoolIdentity, AerodromeV2PoolState, RegisterAerodromeV2PoolParams,
};
pub use ::degenbot_pools::curve_data_provider::{CurveDataProvider, CurveDataProviderError};
pub use ::degenbot_pools::curve_dy_io::{resolve_dy_inputs, CurveInputsError};
pub use ::degenbot_pools::rate_provider::{
    BalancerRateProvider, RateProviderError, StaticRateProvider,
};
pub use ::degenbot_pools::spec_bounds::{SpecValue, SpecViolation, UINT112_MAX};
pub use ::degenbot_pools::state_history::BalancesBlockDelta;
pub use ::degenbot_pools::v3_state::{
    v3_simulate_swap, BufferedV3LiquidityUpdate, BufferedV3PoolEvent, BufferedV3SwapEvent,
    ClSlotLayout, PoolTickCoverage, RegisterV3PoolError, RegisterV3PoolParams,
    RegistrationLifecycle, SimulateSwapError, V3PoolIdentity, V3PoolState, V3SwapOutcome,
    V3SwapUpdate,
};
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
    BuildFailure, BuildRefusal, HopSignature, RegistrationLedger, RegistrationOutcome,
    UnregistrableRecord, REGISTRATION_OUTCOMES,
};
pub use registration_lifecycle::{
    run_cl_v3_lifecycle, run_cl_v4_lifecycle, run_v3_registration_lifecycle,
    run_v4_registration_lifecycle, RegistrationLifecycleError,
};
pub use stage_handlers::{
    AffectedPaths, CandidateId, Finalize, FinalizeOutcome, Gate, GateOutcome, Publish,
    PublishOutcome, QuiesceOutcome, QuiesceVerdict, Resolve, Rewind, RewindOutcome, Simulate,
    SimulateOutcome, Solve, SolveOutcome, Stage, StageError, StageHandlers,
};
pub use verify_claims::{PoolVerifications, VerifyClaims};

pub use ::degenbot_pools::v4_state::{
    v4_simulate_swap, BufferedV4LiquidityUpdate, BufferedV4PoolEvent, BufferedV4SwapEvent,
    RegisterV4PoolError, RegisterV4PoolParams, V4PoolIdentity, V4PoolKey, V4PoolState, V4StateSync,
    V4SwapUpdate, AMOUNT_MODIFYING_HOOK_MASK, V4_DYNAMIC_FEE_FLAG,
};

// Re-export the ADR-004 typed TickMap boundary trait (V3 + V4 impls both live
// in `tick_map.rs`). State structs stay flat; only verifier/apply views are
// typed-narrowed.
pub use ::degenbot_pools::tick_map::{TickMap, TickMapMut};

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
// Pool registry sum type + V2 identity/state + token entry + swap-sim dispatch.
// **Relocated** to `degenbot-pools`; re-exported here at the historical
// `bot_core::*` paths so consumers resolve unchanged.
// Transient re-export — repointed at `degenbot_pools::*` natively.
// ---------------------------------------------------------------------------
pub use ::degenbot_pools::registry::{
    ConcentratedLiquidityPool, ConcentratedLiquidityPoolMut, PoolEntry, RegisteredPoolFamily,
    TokenEntry,
};
pub use ::degenbot_pools::simulate_swap::simulate_swap;
pub use ::degenbot_pools::v2_state::{
    RegisterV2PoolError, RegisterV2PoolParams, V2PoolIdentity, V2PoolState,
};
pub use ::degenbot_pools::TickInfo;

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
