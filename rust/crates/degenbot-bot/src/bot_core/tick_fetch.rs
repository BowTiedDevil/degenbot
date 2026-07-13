//! Fetch-callback seam for sparse tick-map swap simulation.
//!
//! **Relocated.** The `TickWordFetcher` trait + `FetchedTickWord` +
//! `FetchTickWordError` now live in `degenbot-pools` (the `TickWordFetcher`
//! is held as an `Option<Arc<dyn TickWordFetcher>>` on V3/V4 `PoolState`, so
//! the trait type must be visible to the pool-state crate — `std::io::Read`
//! precedent). Re-exported here at the historical path so the bot's consumers
//! resolve unchanged. The RPC/DB *implementations* + the `PyTickWordFetcher`
//! `PyO3` adapter still live in `degenbot-bot` / `degenbot-python`.
//!
//! Transient re-export — the shim file is removed and consumers are repointed
//! at `degenbot_pools::` natively by USPN7M/P2CKRL.

pub use ::degenbot_pools::{FetchTickWordError, FetchedTickWord, TickWordFetcher};
