//! `degenbot-aave` — the Aave V3 domain crate: the updater chunk-loop + the
//! position-analysis math.
//!
//! Two sibling concerns live here, each in its own submodule:
//! - [`updater`] — the transactional apply core (epic `AZGJUN`, task
//!   `CXRGX4`): pure, synchronous, fixture-testable, NO RPC, NO `pyo3`, NO
//!   `database_path`, NO `open_for_writes`.
//! - [`analysis`] — the pure position health-factor / LTV / eMode / isolation
//!   math (port of `src/degenbot/aave/analysis/core.py`).
//!
//! The updater is the apply half of the standalone-Rust `aave_update` core
//! (mirroring `degenbot-pool-updater`'s `apply_chunk_writes_on_conn`).
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
//! # The §3.4 atomicity invariant (the whole point)
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
//! The earlier per-`#[pyfunction]` `PyO3` seam (the 2QPBUJ `db_get_or_create_*` /
//! `db_apply_*` pyfunctions in `degenbot-python/src/db/aave.rs`) opened
//! `DegenbotDb::open_for_writes(database_path)` PER CALL + committed
//! immediately on its OWN connection. That was a SECOND writer on the `SQLite`
//! file while the `aave_update` driver's `SQLAlchemy` `Session` held an open
//! mid-chunk write transaction (pending ORM flushes) — empirically proven to
//! silently corrupt: wrong-row re-fetch via the `SQLAlchemy` identity-map,
//! silently-lost ORM writes, no `SQLITE_BUSY` serialization. The §3.4 "one
//! state owner" invariant was violated.
//!
//! The fix is structural: route the WHOLE chunk's writes through ONE
//! `Transaction` via the `_on_conn` variants of the `write.rs` apply/get-or-
//! create fns (extracted in CXRGX4). This crate owns nothing about the
//! connection lifecycle — the caller (the `run_aave_update` orchestrator,
//! sibling `6SWY4R`) opens ONE `DegenbotDb`, begins ONE `Transaction`, calls
//! [`apply_aave_chunk_writes_on_conn`], and commits.
//!
//! # The TWO roles the [`updater`] module plays in the epic
//!
//! - **Transactional apply core** ([`updater::apply_aave_chunk_writes_on_conn`]):
//!   the pure apply loop, callable on any `&Connection`, fixture-tested with
//!   atomicity round-trip tests.
//! - **`run_aave_update` orchestrator** ([`updater::run_aave_update`]): RPC
//!   fetch + decode into `AaveChunkEvent` + the outer chunk-advance loop over
//!   the `aave_v3_markets.last_update_block` cursor.
//!
//! No `pyo3` (enforced by `just check-no-pyo3-in-cores`). The `PyO3` seam lives
//! in `degenbot-python`.

pub mod analysis;
pub mod updater;

// Re-export the updater surface at the crate root so the PyO3 seam +
// standalone consumers resolve `degenbot_aave::run_aave_update` etc. without
// the `updater::` prefix. The analysis surface is likewise re-exported.
pub use updater::*;
