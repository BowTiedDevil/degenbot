//! `degenbot-aave` — the Aave V3 domain crate: the updater chunk-loop + the
//! position-analysis math.
//!
//! Three sibling concerns live here, each in its own submodule:
//! - [`updater`] — the chunk loop in two phases over one durability point:
//!   connection-free resolution (exact reads and pure reduction against the
//!   chunk's `ChunkSeed` and `ProjectionView`) and the synchronous, RPC-free
//!   transactional apply. NO `pyo3`, NO `database_path`, NO `open_for_writes`.
//! - [`analysis`] — the pure position health-factor / LTV / eMode / isolation
//!   math.
//! - [`wad_ray_math`] + [`percentage_math`] — the Aave V3 fixed-point math
//!   libraries (`WadRayMath` / `PercentageMath`)
//! - [`positions`] — the Aave side of the session's position seam: a
//!   `degenbot_core::session_positions::PositionObserver` over the state this
//!   crate's updater maintains, so a session can read one account's position
//!   without the engine depending on this integration.
//!
//! The updater is the standalone-Rust `aave_update` core: exact-read resolution
//! plus the transactional apply (mirroring `degenbot-pool-updater`'s chunk loop).
//!
//! # The chunk-boundary commit invariant (inviolable)
//!
//! The committed database holds ONLY fully-verified values committed at chunk
//! boundaries. Debugging/diagnostic work MUST NEVER edit, patch, coalesce, or
//! hand-fix a value in a committed DB in place — any such edit breaks the
//! chunk-boundary invariant and silently poisons the baseline: every
//! downstream compare becomes non-deterministic and all future drives are
//! corrupted. Experiments run against a THROWAWAY temp DB; the verified
//! baseline is rebuilt ONLY by re-driving from genesis, never by mutating it.
//!
//! # The chunk-atomicity invariant (`docs/architecture/chunk-atomicity.md`)
//! (the whole point)
//!
//! ONE `rusqlite::Connection`, ONE `Transaction` per chunk. Every apply of a
//! chunk goes through [`apply_aave_chunk_writes_on_conn`] on ONE borrowed
//! `Connection` (the caller's `Transaction` derefs to one) + the caller's
//! `Transaction::commit` / drop-rollback is the single point of durability.
//! Any `?` early-return (a `UNIQUE` violation, a decode failure, ...) leaves
//! the caller's `Transaction` uncommitted → it drops → the whole chunk
//! reverts → `last_update_block` unchanged → a restart re-processes the chunk
//! clean (no skipped blocks, no partial commit).
//!
//! # Why this design (the two-writer hazard it fixes)
//!
//! The earlier per-`#[pyfunction]` `PyO3` seam (the `db_get_or_create_*` /
//! `db_apply_*` pyfunctions in `degenbot-python/src/db/aave.rs`) opened
//! `DegenbotDb::open_for_writes(database_path)` PER CALL + committed
//! immediately on its OWN connection. That was a SECOND writer on the `SQLite`
//! file while the `aave_update` driver's `SQLAlchemy` `Session` held an open
//! mid-chunk write transaction (pending ORM flushes) — empirically proven to
//! silently corrupt: wrong-row re-fetch via the `SQLAlchemy` identity-map,
//! silently-lost ORM writes, no `SQLITE_BUSY` serialization. The "one
//! state owner" invariant was violated.
//!
//! The fix is structural: route the chunk's projection writes through ONE
//! `Transaction` via the `_on_conn` variants of the `write.rs` apply fns. The
//! identity rows resolution needs are interned synchronously by the chunk seed
//! on the caller's connection before that transaction opens, so a rolled-back
//! chunk leaves only inert rows behind (ADR-105 D4). This crate owns nothing
//! about the connection lifecycle — the caller (`AaveMarketWriter::update`
//! orchestrator) opens ONE `DegenbotDb`, loads the seed, begins ONE
//! `Transaction`, calls [`apply_aave_chunk_writes_on_conn`], and commits.
//!
//! # The TWO roles the [`updater`] module plays
//!
//! - **Transactional apply core** ([`updater::apply_aave_chunk_writes_on_conn`]):
//!   the pure apply loop, callable on any `&Connection`, fixture-tested with
//!   atomicity round-trip tests.
//! - **`AaveMarketWriter::update` orchestrator** ([`crate::AaveMarketWriter::update`]): RPC
//!   fetch + decode into canonical deltas (configuration and operation effects
//!   alike) + the outer chunk-advance loop over
//!   the `aave_v3_markets.last_update_block` cursor.
//!
//! No `pyo3` (enforced by `just check-no-pyo3-in-cores`). The `PyO3` seam lives
//! in `degenbot-python`.

pub mod analysis;
#[cfg(test)]
#[expect(clippy::expect_used, clippy::panic)]
// reason: a fixture reader refuses by panicking — a missing, malformed, or
// unknown-schema capture is a harness defect that must stop the test, never
// shrink its coverage.
pub(crate) mod fixtures;
pub mod market_writer;
pub mod percentage_math;
pub mod positions;
pub mod updater;
pub mod wad_ray_math;

// Re-export the Aave V3 math libraries at the crate root so in-crate +
// standalone consumers resolve `degenbot_aave::ray_mul` etc. without the
// module prefix (the same symbol surface `degenbot-evm-math` re-exported
// before the relocation).
pub use percentage_math::{
    percent_div, percent_div_ceil, percent_mul, percent_mul_ceil, percent_mul_floor, PercentError,
    HALF_PERCENTAGE_FACTOR, PERCENTAGE_FACTOR,
};
pub use wad_ray_math::{
    ray_div, ray_div_ceil, ray_div_floor, ray_mul, ray_mul_ceil, ray_mul_floor, ray_to_wad,
    wad_div, wad_mul, wad_to_ray, RayRounding, WadRayError, HALF_RAY, HALF_WAD, MAX_UINT256, RAY,
    WAD, WAD_RAY_RATIO,
};

// Re-export the updater surface at the crate root so the PyO3 seam +
// standalone consumers resolve `degenbot_aave::AaveMarketWriter::update` etc. without
// the `updater::` prefix. The analysis surface is likewise re-exported.
pub use market_writer::{market_lock_path, AaveMarketWriter, MarketReset};
pub use positions::AavePositionObserver;
pub use updater::*;
