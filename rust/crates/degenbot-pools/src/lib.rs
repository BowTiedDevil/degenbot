//! Value-only pool identity/state structs + stateless swap simulation for the
//! degenbot pool families — pure-Rust, `pyo3`-free.
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
//! Nothing here performs I/O. The I/O-shaped *interface* traits
//! (`TickWordFetcher`, `CurveDataProvider`, `BalancerRateProvider`) are defined
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
//! > (`degenbot-pools`). **No →** `degenbot-bot` (the `Bot`/`BotState` registry,
//! > the block pump, the reorg *coordinator*, the fetch-retry shells, the
//! > RPC/DB trait impls, the engine, the solvers).
//!
//! Concretely, the `BotState::simulate_swap_with_override` retry shell stays
//! in `degenbot-bot`: it looks up `self.pools.get(id)` (`PoolEntry`), calls the
//! stateless sim in this crate, and — for V3/V4 — catches a
//! `MissingTickWord(word)` *value* error, fetches the tick word via the (state
//! crate's) `TickWordFetcher` trait impl that the bot registered, and retries.
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
//! engine, the block pump, RPC clients, and the solvers. Moving the value-only
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
//! ## Contents (added incrementally)
//!
//! The crate is populated by the `USPN7M` epic, one task per concern:
//!
//! - **trait definitions** (`TickWordFetcher`, `CurveDataProvider`,
//!   `BalancerRateProvider` + their error/return types + `StaticRateProvider`)
//! - **leaf value modules** (`spec_bounds`, `state_history`, `tick_bitmap`,
//!   `tick_map`)
//! - **per-family state structs** + `PoolEntry` / `ConcentratedLiquidityPool` / `TickInfo` /
//!   `TokenEntry` + `Register*Pool{Params,Error}`
//! - **stateless swap sims** (`v3_simulate_swap`, `v4_simulate_swap`,
//!   `SimulateSwapError`, `V3SwapOutcome`, the V2/Curve/Balancer dispatch)

pub mod aerodrome_v2_state;
pub mod balancer_stable_state;
pub mod balancer_weighted_state;
pub mod curve_data_provider;
pub mod curve_dy_io;
pub use curve_dy_io::{resolve_dy_inputs, CurveInputsError};
pub mod curve_state;
pub mod curve_strategies;
// Domain-math prose (sqrtPriceX96, Solidity, …) moved verbatim from
// `degenbot-bot/src/solvers/mobius_v3_int.rs`; mirrors the `#[allow]` on that
// module in `degenbot-bot/src/solvers/mod.rs`.
#[expect(clippy::doc_markdown)]
pub mod int_v3_hop;
pub mod liquidity_event;
pub mod liquidity_event_buffer;
pub mod pool;
pub mod rate_provider;
pub mod registry;
pub mod simulate_swap;
pub mod spec_bounds;
pub mod state_history;
/// Re-export the *`PancakeSwap` V3 fork* storage-slot encoders at the crate root
/// (the fork's layout diverges from Uniswap V3: two-word `slot0`, liquidity@5,
/// ticks@6, tickBitmap@7). A standalone consumer seeding/serving a pancake pool
/// directly MUST use these, never the Uniswap `v3_storage_slots` constants.
pub use v3_pancakeswap_storage_slots::{
    encode_pancake_v3_slot0_word1, pancake_v3_tick_bitmap_word_slot, pancake_v3_tick_mapping_slot,
    PANCAKE_V3_LIQUIDITY_SLOT, PANCAKE_V3_SLOT0_WORD0_SLOT, PANCAKE_V3_SLOT0_WORD1_SLOT,
    PANCAKE_V3_TICKS_MAPPING_SLOT, PANCAKE_V3_TICK_BITMAP_MAPPING_SLOT,
};
/// Re-export the V3 storage-slot encoders at the crate root so the Tier-3b
/// seeding layer + standalone consumers reach them without a long path.
pub use v3_storage_slots::{
    compute_v3_tick_bitmap_word, compute_v3_tick_bitmap_word_from_raw, decode_v3_slot0,
    encode_v3_liquidity_slot, encode_v3_slot0, encode_v3_slot0_fresh, encode_v3_tick_info_slot,
    sign_extend_int16, sign_extend_int24, v3_tick_bitmap_word_slot, v3_tick_mapping_slot,
    V3Slot0Parts,
};
/// Re-export the V4 storage-slot encoders at the crate root (Tier-3b seeding).
pub use v4_storage_slots::{
    compute_v4_tick_bitmap_word, decode_v4_slot0, encode_v4_liquidity_slot, encode_v4_slot0,
    encode_v4_tick_info_slot, v4_liquidity_slot, v4_pool_id, v4_pool_state_base_slot,
    v4_slot0_slot, v4_tick_bitmap_word_slot, v4_tick_mapping_slot, V4Slot0Parts,
};
pub mod tick_bitmap;
pub mod tick_fetch;
pub mod tick_map;
pub mod tick_map_verify;
pub mod v2_state;
pub mod v3_pancakeswap_storage_slots;
pub mod v3_state;
pub mod v3_storage_slots;
pub mod v4_state;
pub mod v4_storage_slots;

// Re-export the seam traits + their value-only error/return types at the crate
// root, so `degenbot-bot`'s transient shim modules can write
// `::degenbot_pools::{TickWordFetcher, …}` and so standalone consumers get a
// flat surface.
pub use curve_data_provider::{CurveDataProvider, CurveDataProviderError};
pub use rate_provider::{BalancerRateProvider, RateProviderError, StaticRateProvider};
pub use spec_bounds::{SpecValue, SpecViolation};
pub use state_history::ReorgPoolState;
pub use tick_bitmap::V3TickRangeForSolver;
pub use tick_fetch::{
    BootstrapTickError, BootstrapTickWord, FetchTickWordError, FetchedTickWord, TickBootstrapRpc,
    TickWordFetcher,
};

pub use pool::{
    BalanceVectorVariant, BalanceVectorView, ConcentratedLiquidityVariant,
    ConcentratedLiquidityView, Identity, Pool, ReservePairVariant, ReservePairView, Structure,
};

/// Liquidity data at an initialized tick.
///
/// Mirrors the Python `LiquidityAtTick` from `concentrated/types.py`. Used by
/// the V3/V4 pool-state structs and by [`tick_fetch::FetchedTickWord`]. Pulled
/// into this crate ahead of the full state-struct move (USPN7M/LTZ3TP)
/// because the `TickWordFetcher` seam returns `HashMap<i32, TickInfo>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TickInfo {
    /// The total liquidity that references this tick.
    pub liquidity_gross: alloy::primitives::U128,
    /// The liquidity delta for ticks entered from left to right.
    /// Positive for lower ticks, negative for upper ticks.
    pub liquidity_net: alloy::primitives::I256,
    /// The block at which this tick was last mutated (Mint/Burn event block,
    /// or the pool's registration block for genesis-seeded ticks). Mirrors the
    /// Python ``LiquidityAtTick.block`` field; preserved through the FFI
    /// round-trip (``update_tick_data`` writes it, ``tick_data_snapshot``
    /// reads it). The simulation math does NOT read this — it's diagnostic
    /// metadata + the snapshot round-trip's per-tick block contract.
    pub block: u64,
}

impl TickInfo {
    /// The `liquidity_net` value as a plain `i128`: the LOW 16 big-endian
    /// bytes of the 256-bit two's-complement — exactly the width of the
    /// on-chain `ticks(tick).liquidityNet` `int128` field.
    ///
    /// Documents the real trap this extraction guards (the path-13827 1-wei
    /// over-prediction incident, `docs/fixtures/v2_v3_v3_solver_divergence_
    /// 25641093.md`): a naive `i128::try_from(I256)` on a net whose high 128
    /// bits carry sign extension yields a different (or failing) value than
    /// the stored int128; the low-16-byte projection is the canonical
    /// on-chain width. All consumers of a stored `liquidity_net` as `i128`
    /// route through this helper.
    #[must_use]
    pub fn liquidity_net_i128(&self) -> i128 {
        let bytes = self.liquidity_net.to_be_bytes::<32>();
        let low: [u8; 16] = bytes[16..32].try_into().unwrap_or([0u8; 16]);
        i128::from_be_bytes(low)
    }
}

#[cfg(test)]
mod tick_info_tests {
    #![expect(clippy::unwrap_used)]
    use super::TickInfo;
    use alloy::primitives::{I256, U128, U256};

    fn tick_info(net: i128) -> TickInfo {
        TickInfo {
            liquidity_gross: U128::from(7u128),
            liquidity_net: I256::try_from(net).unwrap(),
            block: 0,
        }
    }

    /// The shared helper's output equals the per-site 16-byte extraction
    /// (the idiom being consolidated) for the trap-relevant boundary set,
    /// including negative nets whose high 128 bits carry sign extension.
    #[test]
    fn liquidity_net_i128_matches_low16_extraction() {
        for net in [
            -1i128,
            0i128,
            1i128,
            -5_000,
            5_000,
            i128::MAX,
            i128::MIN,
            -1 << 60,
            1 << 61,
        ] {
            let info = tick_info(net);
            // The reference extraction each site performs today.
            let bytes = info.liquidity_net.to_be_bytes::<32>();
            let low: [u8; 16] = bytes[16..32].try_into().unwrap_or([0u8; 16]);
            assert_eq!(
                info.liquidity_net_i128(),
                i128::from_be_bytes(low),
                "net={net}"
            );
        }
    }

    /// The convergence the sweep depends on: `divergence_probe`'s packed
    /// slot word (low 16 bytes re-unsigned into the HIGH 128 of the packed
    /// word) is byte-identical when derived through the helper's
    /// `i128 as u128` bit-pattern reinterpretation.
    #[test]
    fn liquidity_net_i128_covers_probe_packed_word() {
        let gross = U256::from(7u128);
        for net in [-1i128, 0i128, 42, -5_000, i128::MIN, i128::MAX] {
            let info = tick_info(net);
            let legacy = {
                let b = info.liquidity_net.to_be_bytes::<32>();
                gross | (U256::from_be_slice(&b[16..32]) << 128)
            };
            let via_helper = gross | (U256::from(info.liquidity_net_i128().cast_unsigned()) << 128);
            assert_eq!(legacy, via_helper, "net={net}");
        }
    }
}
