//! Fetch-callback seam for sparse tick-map swap simulation (ADR-005 sparse-map
//! feature parity) — the **live-pump miss path**.
//!
//! `v3_simulate_swap` / `v4_simulate_swap` return
//! `SimulateSwapError::MissingTickWord` when `coverage == Sparse` and the walk
//! enters a tick-bitmap word whose bitmap has not been fetched. Rather than
//! silently producing a wrong amount, the calc path delegates the recovery to
//! a [`TickWordFetcher`]: on a miss it calls the fetcher for the missing
//! `word`, merges the result into the pool state (growing `known_bitmap_words`),
//! and retries — mirroring the Python companion's `MissingLiquidityData` →
//! `_tick_data_fetcher` → retry loop.
//!
//! The fetcher *returns* the fetched data (it does not write back into
//! `BotState` itself), so the calc holds no lock across the fetch — the Rust
//! call site (or the `PyO3` adapter wrapping a Python fetcher) merges the
//! result. This keeps the seam re-entrancy-safe (a Python fetcher that does
//! RPC cannot deadlock by re-entering `BotState`).
//!
//! ## Why this trait lives in `degenbot-pools` (not the bot)
//!
//! This crate owns the V3/V4 pool-state structs (`V3PoolState`,
//! `V4PoolState`), which hold an `Option<Arc<dyn TickWordFetcher>>` field — so
//! the trait type must be visible to this crate. Following the `std::io::Read`
//! precedent (defining an interface pulls no I/O), the trait *definition* +
//! its value-only error/return types live here, while the RPC/DB
//! *implementations* (and the `PyTickWordFetcher` `PyO3` adapter) live in
//! `degenbot-bot` / `degenbot-python`.
//!
//! ## Relationship to `super::bootstrap::TickBootstrapRpc`
//!
//! The bootstrap trait (sibling module) serves the **registration-time** Chain
//! arm of `assemble_*_tick_map` — keyed by *address* (the pool's contract
//! address is known BEFORE `register_*_pool` assigns the internal `pool_id`).
//! This miss-path trait serves the **live-pump miss-detection path** during
//! swap simulation — keyed by *`pool_id`* (already-registered pools). The two
//! traits share the same choreography (compute word → fetch bitmap → enumerate
//! bits → fetch ticks) but differ in key type and call site. See
//! `docs/migration-guides/chain-bootstrap-tick-map.md` §3 for the
//! consolidation rationale (keep two traits for now; consolidate post-`XEANMB`).

use hashbrown::HashMap;

use crate::TickInfo;

/// A fetched tick-bitmap word's data, returned by [`TickWordFetcher`].
///
/// `ticks` may be empty — a fetched word whose bitmap is all-zero has no
/// initialized ticks, but the `word` is nonetheless now "known" (the caller
/// records it in `known_bitmap_words` so it is not re-fetched). This mirrors
/// the Python bitmap-store rule: in sparse mode a region is unknown unless its
/// word key is in the lazy-loaded map, regardless of the bitmap value.
#[derive(Debug, Clone)]
pub struct FetchedTickWord {
    /// The tick-bitmap word position that was fetched (`tick_position(tick.div_euclid(spacing))`).
    pub word: i32,
    /// The initialized ticks within this word (`{tick: TickInfo}`). May be empty.
    pub ticks: HashMap<i32, TickInfo>,
}

/// Why a [`TickWordFetcher`] could not fulfil a missing-word fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchTickWordError {
    /// The fetch itself failed (RPC error, not found, etc.).
    FetchFailed,
    /// The requested `word` is outside the fetcher's supported range.
    OutOfRange,
}

/// A callback that fetches a missing tick-bitmap word's data on demand.
///
/// Implementations may do I/O (RPC) — the calc path holds no `BotState` lock
/// across the call (it merges the returned [`FetchedTickWord`] separately), so a
/// fetcher that re-enters `BotState` (e.g. a Python fetcher writing via
/// `update_tick_data`) is re-entrancy-safe as long as the fetcher returns here
/// rather than mutating state in place.
///
/// `Send + Sync` so it can live behind an `Arc`/`RwLock` on the engine/pool side.
pub trait TickWordFetcher: Send + Sync + std::fmt::Debug {
    /// Fetch the missing tick-bitmap word `word` for pool `pool_id` at `block`.
    ///
    /// Returns the word's initialized ticks (which the caller merges into the
    /// pool state + records the word as known), or an error if the fetch
    /// failed.
    ///
    /// # Errors
    ///
    /// Returns [`FetchTickWordError::FetchFailed`] if the underlying fetch
    /// (e.g. RPC) failed, or [`FetchTickWordError::OutOfRange`] if `word` is
    /// outside the fetcher's supported range.
    fn fetch_missing_tick_word(
        &self,
        pool_id: u64,
        word: i32,
        block: u64,
    ) -> Result<FetchedTickWord, FetchTickWordError>;
}
