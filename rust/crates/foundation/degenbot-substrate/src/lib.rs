//! The strategy substrate — the one home for the surface strategies compose
//! (ADR-067): the live state owner [`BotState`], the planning workspace
//! (`planning::Workspace`), pool ingress, the connector index, executor hop
//! views, the sim-anchor oracle, the route registry, the session object
//! registry, the sign-time nonce authority and the per-strategy lane it mints,
//! the driver spawn seam, and the command-stream path-info projection. Both consumers compose it as a peer:
//! `degenbot-bot` (the pump + engine host) and `degenbot-strategy` (the strategy plane). PyO3-free by
//! role; the binding crate reaches it through `degenbot-bot` re-exports.
//!
//! Application-owned telemetry (the pipeline instruments registry) reaches
//! the substrate through the [`telemetry_port`] function-pointer bundle,
//! installed once at boot by the host crate.
//!
//! This module is the composition root — [`RegistryCore`], [`BotState`], and the
//! re-exports consumers name those types and their error types through. A
//! `BotState` method lives with the capability it orchestrates: registry reads
//! and clocks in [`pool_registry`], key resolution in [`pool_lookup`],
//! quarantine in [`quarantine_gate`], the reorg journal in [`reorg_journal`],
//! concentrated-liquidity apply and backfill replay in
//! [`concentrated_liquidity_orchestration`], swap reads in [`swap_simulation`],
//! swap-call encoding in [`swap_encoding`], and the per-family registration and
//! apply rows in [`reserve_pair_orchestration`] / [`balance_vector_orchestration`].

pub mod apply_telemetry;
pub mod balance_vector_orchestration;
pub mod concentrated_liquidity_orchestration;
pub mod concentrated_liquidity_route;
pub mod connector_index;
pub mod divergence_probe;
pub mod driver_spawn;
pub mod epoch;
pub mod epoch_delta;
pub mod executor_hop;
pub mod log_dispatcher;
pub mod nonce;
pub mod path_info;
pub mod planning;
pub mod pool_ingress;
pub mod pool_lookup;
pub mod pool_registry;

pub use pool_lookup::PoolLookup;
pub mod quarantine_gate;
pub mod registration_anchor;
pub mod registration_door;
pub mod registration_gate;
pub mod reorg_journal;
pub mod reserve_pair_orchestration;
pub mod resolve;
pub mod route_registry;
pub mod session_registry;
pub mod sim_anchor;
pub mod sparse_apply_gate;
pub mod stance;
pub mod state_lock;
pub mod swap_encoding;
pub mod swap_simulation;
pub mod telemetry_port;
pub mod tick_assembly;

// Re-export the pool-registry + family-state types the substrate modules
// share (the same set `degenbot-bot::bot_core` re-exports at its historical
// paths — ADR-003: family states live in `degenbot-pools`).
pub use ::degenbot_pools::simulate_swap;
pub use ::degenbot_pools::RegisterBalancerStablePoolParams;
pub use ::degenbot_pools::RegisterBalancerWeightedPoolParams;
pub use ::degenbot_pools::{
    check_metapool_liquidity, check_pool_liquidity, resolve_dy_inputs, CurveInputsError,
};
pub use ::degenbot_pools::{
    v3_simulate_swap, BufferedV3LiquidityUpdate, BufferedV3PoolEvent, BufferedV3SwapEvent,
    ConcentratedLiquiditySlotLayout, PoolTickCoverage, RegisterV3PoolError, RegisterV3PoolParams,
    RegistrationLifecycle, SimulateSwapError, V3PoolIdentity, V3PoolState, V3StateSync,
    V3SwapOutcome, V3SwapUpdate,
};
pub use ::degenbot_pools::{
    AerodromeV2PoolIdentity, AerodromeV2PoolState, RegisterAerodromePoolError,
    RegisterAerodromeV2PoolParams,
};
pub use ::degenbot_pools::{BalancerRateProvider, RateProviderError, StaticRateProvider};
pub use ::degenbot_pools::{BalancesBlockDelta, JournalError, ReorgPoolState};
pub use ::degenbot_pools::{BuildFailure, RegisterRefusal, SpecRefusal};
pub use ::degenbot_pools::{
    ConcentratedLiquidityPool, ConcentratedLiquidityPoolMut, PoolEntry, PoolEntrySlice,
    RegisteredPoolFamily, TokenEntry,
};
pub use ::degenbot_pools::{CurveDataProvider, CurveDataProviderError};
pub use ::degenbot_pools::{CurvePoolIdentity, CurvePoolState, RegisterCurvePoolParams};
pub use ::degenbot_pools::{
    RegisterV2PoolError, RegisterV2PoolParams, V2PoolIdentity, V2PoolState,
};
pub use ::degenbot_pools::{SpecValue, SpecViolation, UINT112_MAX};
pub use ::degenbot_pools::{TickMap, TickMapMut};

pub use ::degenbot_pools::TickInfo;
pub use ::degenbot_pools::{
    v4_simulate_swap, BufferedV4LiquidityUpdate, BufferedV4PoolEvent, BufferedV4SwapEvent,
    RegisterV4PoolError, RegisterV4PoolParams, V4PoolIdentity, V4PoolKey, V4PoolState, V4StateSync,
    V4SwapUpdate, AMOUNT_MODIFYING_HOOK_MASK, V4_DYNAMIC_FEE_FLAG,
};
pub use ::degenbot_pools::{PoolRefusal, PoolStateUpdate};
pub use ::degenbot_uniswap::v2_encoding::{encode_v2_swap, EncodedCall};
// The subscription topic filter is transport knowledge; the
// dispatcher's defensive re-check consumes the same list the ingestion crate
// filters with.
pub use ::degenbot_ingestion::RELEVANT_TOPICS;
pub use ::degenbot_math::curve::{CurveBasePoolPort, CurveSwapError};

pub use balance_vector_orchestration::BotCurveBasePoolPort;
pub use concentrated_liquidity_orchestration::{
    ConcentratedLiquidityApplyError, ConcentratedLiquidityOrchestration, InstallWordOutcome,
    RegisteredV4, StagedWordFetch,
};
pub use divergence_probe::{TrackedSlotKind, TrackedSlotProbe};
pub use epoch::{BlockContext, BlockMetadata, Epoch, StaleEpoch};
pub use epoch_delta::EpochDelta;
pub use registration_door::{PoolRegistration, RegistrationVerdict};
pub use route_registry::RouteRegistry;
pub use sim_anchor::{NoSimAnchor, SimAnchorOracle, SimAnchorState, NO_SIM_ANCHOR};
pub use swap_encoding::EncodeSwapError;

pub use crate::session_registry::PoolIdentity;

use alloy::primitives::Address;
use hashbrown::HashMap;

#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests;

/// The shared registry core - the family-agnostic field set every structural
/// family reads: the pool tables, the token registry, the id counter, the
/// reorg journal depth, and the ADR-040 quarantine set. `BotState` composes
/// this core alongside the family capabilities ([`ConcentratedLiquidityOrchestration`]),
/// and every `impl BotState` row that reads it is the front for one capability:
/// [`pool_registry`] (tables + clocks), [`pool_lookup`] (key resolution),
/// [`quarantine_gate`], [`reorg_journal`], and the per-family orchestration
/// modules.
struct RegistryCore {
    /// Pool registry: `pool_id` -> `PoolEntry`.
    pools: HashMap<u64, PoolEntry>,
    /// Pool contract address -> `pool_id`.
    pool_addresses: HashMap<Address, u64>,
    /// Token registry: address -> `TokenEntry`.
    tokens: HashMap<Address, TokenEntry>,
    /// Auto-incrementing pool ID.
    next_pool_id: u64,
    /// Reorg journal depth (in blocks) for every pool - one mainnet epoch
    /// by default (ADR-003). Applied uniformly to V2/V3/V4.
    journal_depth: usize,
    /// ADR-040 quarantine set: pool ids currently EXCLUDED from solve
    /// resolution (tainted-by-desync surfaces). Family-agnostic - spans
    /// V2/V3/V4 and every other family because the gate sits in the
    /// projection dispatcher, not per state type. Each transition bumps the
    /// pool's `state_nonce` (dirties it) so cached hop projections and
    /// in-flight candidates invalidate.
    quarantined_pools: hashbrown::HashSet<u64>,
}

/// The single owner of all runtime state.
///
/// All pool data, token metadata, engines, and encoded results live here.
/// Python holds `PyBot` — an `Arc` pointing here.
///
/// ADR-006 D4: `BotState` is the pure-data submodule (no I/O) behind the
/// thin `Bot` orchestrator facade. `pub` — callers cross the
/// orchestrator seam; `BotState` is a private deep module with its own test
/// seam.
pub struct BotState {
    /// The shared registry core: the pool tables, the token registry, the
    /// id counter, the journal depth, and the quarantine set.
    registry: RegistryCore,
    /// The concentrated-liquidity (V3 + V4) orchestration capability: the
    /// dual liquidity buffers, `v4_pool_ids`, the state-view registry, the
    /// registration gate, the snapshot seed block, the pump delivery
    /// cutoff, and the per-pool event horizons.
    concentrated_liquidity: ConcentratedLiquidityOrchestration,
}

impl BotState {
    /// Create a new, empty `BotState` with the default 32-block reorg journal.
    #[must_use]
    pub fn new() -> Self {
        Self::with_journal_depth(32)
    }

    /// Create a new, empty `BotState` with a custom reorg journal depth.
    #[must_use]
    pub fn with_journal_depth(journal_depth: usize) -> Self {
        Self {
            registry: RegistryCore {
                pools: HashMap::new(),
                pool_addresses: HashMap::new(),
                tokens: HashMap::new(),
                next_pool_id: 1,
                journal_depth,
                quarantined_pools: hashbrown::HashSet::new(),
            },
            concentrated_liquidity: ConcentratedLiquidityOrchestration::new(),
        }
    }
}

impl Default for BotState {
    fn default() -> Self {
        Self::new()
    }
}
