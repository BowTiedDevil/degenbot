//! Fetch-callback seam for sparse tick-map swap simulation (ADR-005 sparse-map
//! feature parity).
//!
//! [`v3_simulate_swap`](crate::bot_core::v3_simulate_swap) /
//! [`v4_simulate_swap`](crate::bot_core::v4_state::v4_simulate_swap) return
//! [`SimulateSwapError::MissingTickWord`](crate::bot_core::SimulateSwapError)
//! when `coverage == Sparse` and the walk enters a tick-bitmap word whose
//! bitmap has not been fetched. Rather than silently producing a wrong amount,
//! the calc path delegates the recovery to a [`TickWordFetcher`]: on a miss it
//! calls the fetcher for the missing `word`, merges the result into the pool
//! state (growing `known_bitmap_words`), and retries — mirroring the Python
//! companion's `MissingLiquidityData` → `_tick_data_fetcher` → retry loop.
//!
//! The fetcher *returns* the fetched data (it does not write back into
//! `BotState` itself), so the calc holds no lock across the fetch — the Rust
//! call site (or, in slice 3, the `PyO3` adapter wrapping a Python fetcher) merges
//! the result. This keeps the seam re-entrancy-safe (a Python fetcher that does
//! RPC cannot deadlock by re-entering `BotState`).

use std::collections::HashMap;

use crate::bot_core::TickInfo;

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
pub trait TickWordFetcher: Send + Sync {
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
