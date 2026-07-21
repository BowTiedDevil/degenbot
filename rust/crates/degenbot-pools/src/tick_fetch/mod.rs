//! Fetch-callback seams for sparse tick-map reads.
//!
//! Two sibling traits serve two distinct callsites — see
//! `docs/migration-guides/chain-bootstrap-tick-map.md` §3 for why they are
//! kept separate for now (consolidation is a post-`XEANMB` follow-up):
//!
//! - [`miss::TickWordFetcher`] — the **live-pump miss path** during swap
//!   simulation (`v3_simulate_swap` / `v4_simulate_swap` →
//!   `MissingTickWord`). Keyed by the internal `pool_id` (already-registered
//!   pools). Owns the `FetchedTickWord` + `FetchTickWordError` value types.
//! - [`bootstrap::TickBootstrapRpc`] — the **registration-time** Chain arm of
//!   `assemble_{v3,v4}_tick_map` (the Store → Db → Chain precedence chain).
//!   Keyed by pool address (V3) or pool-manager + pool-id (V4) (the pool's
//!   identity is known BEFORE `register_*_pool` assigns the internal
//!   `pool_id`). Owns the `BootstrapTickWord` + `BootstrapTickError` value
//!   types.

pub mod bootstrap;
pub mod miss;

pub use bootstrap::{BootstrapTickError, BootstrapTickWord, TickBootstrapRpc};
pub use miss::{FetchTickWordError, FetchedTickWord, TickWordFetcher};
