//! Value-only pool identity/state structs, stateless swap simulation, and the
//! multi-hop solve tree for the degenbot pool families — pure-Rust, `pyo3`-free.
//!
//! This crate owns the **pure-data, pure-math** layer of the pool subsystem:
//! the per-family `*PoolIdentity` / `*PoolState` value structs, the
//! `PoolEntry` sum type, the per-family `Register*PoolParams` /
//! `Register*PoolError` DTOs, the reorg-journal *data* (`ReorgJournal`,
//! `*BlockDelta`), the spec-bound validators, and the **stateless** swap
//! simulators (`v3_simulate_swap`, `v4_simulate_swap`, the V2 constant-product
//! dispatch, and the Curve/Balancer sims) that compute a swap outcome purely
//! from in-memory pool state with no chain / registry / async / tokio.
//!
//! The [`solve`] tree is the same bargain applied to arbitrage: the
//! family-blind kernels (the golden-section template, the integer-verification
//! Möbius order, the profit-envelope gate and its caches) plus one solve entry
//! per family, all value-in / value-out over resolved hop state — no chain,
//! registry, async, or tokio. It is the ADR-073 endgame re-home of the dissolved
//! `degenbot-solvers` crate; the `degenbot` umbrella keeps the `solvers` name as
//! a re-export of [`solve`].
//!
//! Nothing here performs I/O. The I/O-shaped *interface* traits
//! (`TickWordSource`, `CurveDataProvider`, `BalancerRateProvider`) are defined
//! in this crate — but their RPC/DB *implementations* live in
//! `degenbot-bot`, exactly as `std::io::Read` defines an interface in the
//! standard library while its implementors (`File`, `BufReader`, …) live
//! elsewhere. This is the `std::io::Read` precedent: defining a capability
//! trait pulls no I/O and avoids a cyclic dependency (`pools` must not depend
//! on `degenbot-bot` or `degenbot-rpc`).
//!
//! ## Dividing line (per ADR-003 / ADR-005)
//!
//! > *Given all pool state in memory, can this compute deterministically with
//! > no chain / registry / async / tokio?* **Yes →** this crate
//! > (`degenbot-pools`, its solve tree included). **No →** `degenbot-bot` (the
//! > `Bot`/`BotState` registry, the block pump, the reorg *coordinator*, the
//! > fetch-retry shells, the RPC/DB trait impls, the engine).
//!
//! Concretely, the `BotState::simulate_swap_with_override` retry shell stays
//! in `degenbot-bot`: it looks up `self.pools.get(id)` (`PoolEntry`), calls the
//! stateless sim in this crate, and — for V3/V4 — catches a
//! `MissingTickWord(word)` *value* error, fetches the tick word via the (state
//! crate's) `TickWordSource` trait impl that the bot registered, and retries.
//! This is "Pattern B": the value crate returns a `MissingTickWord(i32)` value
//! error; the fetch-and-retry loop lives one layer up in the bot.
//!
//! ## Why this is a standalone crate (ADR-005 "standalone constraint")
//!
//! Previously these value types + stateless sims lived inline in
//! `degenbot-bot/src/bot_core/mod.rs` (a ~7500-line module) and its 22
//! submodules, inside the engine crate. That stranded standalone-usable pool
//! *data* and pool-family *swap math* inside the bot's I/O/registry surface —
//! a `cargo add degenbot` consumer wanting pool state + swap sims pulled the
//! engine, the block pump, and RPC clients. Moving the value-only
//! layer out gives a clean Rust core that a standalone consumer (or the
//! `degenbot` umbrella) can depend on without the I/O umbrella, while
//! `degenbot-bot`'s `BotState` becomes a thin registry-lookup wrapper that
//! delegates each method to the value core in this crate.
//!
//! This crate is `pyo3`-free under its default features (enforced by `just
//! check-no-pyo3-in-cores`); it depends on `alloy`, `thiserror`, and the
//! per-family math leaf crates (`degenbot-v2-math`, `degenbot-concentrated-liquidity-math`,
//! `degenbot-curve-math`, `degenbot-balancer-math`, `degenbot-solidly-math`)
//! plus `degenbot-uniswap` (for `DexVariant`). It is consumed by
//! `degenbot-bot` and re-exported by the `degenbot` umbrella for standalone
//! Rust consumers.
//!
//! ## Contents (ADR-084 W5: four capability modules at the root)
//!
//! - [`state`] - per-family value structs + storage slots, the tick
//!   map/bitmap machinery, spec-bound validators, the reorg-journal data, and
//!   the family-capability declarations
//! - [`families`] - the family read doors + projections (ADR-073), the CL
//!   state module (ADR-080), and the family event plumbing
//! - [`sim`] - the stateless swap simulators, the `PoolEntry` dispatch row +
//!   registry, the seam traits (whose implementations live one layer up), and
//!   the registration-refusal vocabulary
//! - [`solve`] - the family-blind kernels (golden-section template,
//!   integer-verification Mobius order, profit-envelope composition and
//!   caches) plus one solve entry per family module
//!
//! The crate root keeps the umbrella-grade re-exports (`TickInfo`, the
//! storage-slot encoders, the seam traits) so standalone consumers keep a
//! flat surface; the module trees beneath are the homes.

pub mod families;

pub mod sim;

/// The solver-kernel tree (ADR-073 endgame re-home): the family-blind kernels
/// (the golden-section template, the integer-verification Möbius order, the
/// envelope composition and its caches) live here beside the family read
/// modules, and each family's solve entry lives inside its family module
/// under [`families`].
pub mod solve;

pub mod state;

pub use sim::curve_dy_io::{
    check_metapool_liquidity, check_pool_liquidity, resolve_dy_inputs, CurveInputsError,
};

pub use families::buffered_event::{BufferedPoolEvent, PoolLiquidityUpdate};

pub use families::state_update::{PoolRefusal, PoolStateUpdate};

/// Re-export the *`PancakeSwap` V3 fork* storage-slot encoders at the crate root
/// (the fork's layout diverges from Uniswap V3: two-word `slot0`, liquidity@5,
/// ticks@6, tickBitmap@7). A standalone consumer seeding/serving a pancake pool
/// directly MUST use these, never the Uniswap `v3_storage_slots` constants.
pub use state::v3_pancakeswap_storage_slots::{
    encode_pancake_v3_slot0_word1, pancake_v3_tick_bitmap_word_slot, pancake_v3_tick_mapping_slot,
    PANCAKE_V3_LIQUIDITY_SLOT, PANCAKE_V3_SLOT0_WORD0_SLOT, PANCAKE_V3_SLOT0_WORD1_SLOT,
    PANCAKE_V3_TICKS_MAPPING_SLOT, PANCAKE_V3_TICK_BITMAP_MAPPING_SLOT,
};

/// Re-export the V3 storage-slot encoders at the crate root so the Tier-3b
/// seeding layer + standalone consumers reach them without a long path.
pub use state::v3_state::{ConcentratedLiquiditySlotLayout, V3PoolState};

pub use state::v3_storage_slots::{
    compute_v3_tick_bitmap_word, compute_v3_tick_bitmap_word_from_raw, decode_v3_slot0,
    encode_v3_liquidity_slot, encode_v3_slot0, encode_v3_slot0_fresh, encode_v3_tick_info_slot,
    sign_extend_int16, sign_extend_int24, v3_tick_bitmap_word_slot, v3_tick_mapping_slot,
    V3Slot0Parts,
};

/// Re-export the V4 storage-slot encoders at the crate root (Tier-3b seeding).
pub use state::v4_storage_slots::{
    compute_v4_tick_bitmap_word, decode_v4_slot0, encode_v4_liquidity_slot, encode_v4_slot0,
    encode_v4_tick_info_slot, v4_liquidity_slot, v4_pool_id, v4_pool_state_base_slot,
    v4_slot0_slot, v4_tick_bitmap_word_slot, v4_tick_mapping_slot, V4Slot0Parts,
};

// Re-export the seam traits + their value-only error/return types at the crate
// root, so `degenbot-bot`'s transient shim modules can write
// `::degenbot_pools::{TickWordSource, …}` and so standalone consumers get a
// flat surface.
pub use sim::curve_data_provider::{CurveDataProvider, CurveDataProviderError};

pub use sim::rate_provider::{BalancerRateProvider, RateProviderError, StaticRateProvider};

pub use state::spec_bounds::{SpecValue, SpecViolation};

pub use state::state_history::ReorgPoolState;

pub use state::tick_bitmap::V3TickRangeForSolver;

pub use sim::tick_fetch::{FetchedTickWord, TickWordFetchError, TickWordKey, TickWordSource};

pub use sim::registration_refusal::{BuildFailure, RegisterRefusal, SpecRefusal};

pub use sim::pool::{
    BalanceVectorVariant, BalanceVectorView, BinnedLiquidityVariant, ConcentratedLiquidityVariant,
    ConcentratedLiquidityView, Identity, Pool, ReservePairVariant, ReservePairView, Structure,
};

/// Liquidity data at an initialized tick.
///
/// Mirrors the Python `LiquidityAtTick` from `concentrated/types.py`. Used by
/// the V3/V4 pool-state structs and by [`tick_fetch::FetchedTickWord`]. Pulled
/// into this crate ahead of the full state-struct move
/// because the `TickWordSource` seam returns `HashMap<i32, TickInfo>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TickInfo {
    /// The total liquidity that references this tick.
    pub liquidity_gross: alloy::primitives::U128,
    /// The liquidity delta for ticks entered from left to right.
    /// Positive for lower ticks, negative for upper ticks. Stored at the
    /// on-chain width (`ticks(tick).liquidityNet` is `int128`) — the
    /// int128 narrowing from the previous `alloy::primitives::I256`.
    pub liquidity_net: i128,
    /// The block at which this tick was last mutated/confirmed — the event
    /// block, the registration seed block, or the bulk-sync block. Mirrors the
    /// Python ``LiquidityAtTick.block`` field; preserved through the FFI
    /// round-trip (``update_tick_data`` writes it, ``tick_data_snapshot``
    /// reads it). ``0`` is a legal stamp (a chain can launch at genesis), never
    /// a sentinel. The simulation math does NOT read this — it's diagnostic
    /// metadata + the snapshot round-trip's per-tick block contract.
    pub block: u64,
}

impl TickInfo {
    /// The `liquidity_net` value as a plain `i128`. The field is stored at
    /// the on-chain width (int128 narrowing), so this is now the identity
    /// projection; kept as the read seam for all consumers that used to
    /// route through the low-16-byte extraction of the old `I256` field.
    ///
    /// The trap this seam historically guarded (the path-13827 1-wei
    /// over-prediction incident, the 25641093 divergence fixture) is
    /// structurally retired: there is no wider representation left to
    /// mis-project.
    #[must_use]
    pub fn liquidity_net_i128(&self) -> i128 {
        self.liquidity_net
    }
}

#[cfg(test)]
mod tick_info_tests {
    use super::TickInfo;
    use alloy::primitives::U128;

    fn tick_info(net: i128) -> TickInfo {
        TickInfo {
            liquidity_gross: U128::from(7u128),
            liquidity_net: net,
            block: 0,
        }
    }

    /// The stored field IS the on-chain int128 width; the legacy
    /// low-16-byte projection of the old I256 field is now the identity.
    #[test]
    fn liquidity_net_i128_is_identity() {
        for net in [i128::MIN, -1, 0, 1, -5_000, 5_000, i128::MAX] {
            assert_eq!(tick_info(net).liquidity_net_i128(), net);
        }
    }
}
