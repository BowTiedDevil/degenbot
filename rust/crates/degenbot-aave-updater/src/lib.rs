//! Pure-Rust Aave V3 updater chunk-loop — the transactional apply core
//! (epic `AZGJUN`, task `CXRGX4`).
//!
//! This is the apply half of the standalone-Rust `aave_update` core (mirroring
//! `degenbot-pool-updater`'s `apply_chunk_writes_on_conn`). It is the
//! **transactional core**: pure, synchronous, fixture-testable, NO RPC, NO
//! `pyo3`, NO `database_path`, NO `open_for_writes`.
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
//! # The TWO roles this crate plays in the epic
//!
//! - **Transactional apply core** (this file): the pure apply loop, callable
//!   on any `&Connection`, fixture-tested with atomicity round-trip tests.
//! - *(future `run_aave_update` orchestrator — sibling `6SWY4R`)*: RPC fetch +
//!   decode into `AaveChunkEvent` + the outer chunk-advance loop over the
//!   `aave_v3_markets.last_update_block` cursor.
//!
//! No `pyo3` (enforced by `just check-no-pyo3-in-cores`). The `PyO3` seam will
//! live in `degenbot-python`.

pub mod operations;
pub mod operations_parser;
pub mod processors;
mod run;
pub mod transaction_processor;

pub use processors::{
    ProcessorError, RayDivMode, RoundingStrategy, ScaledTokenBurnResult, ScaledTokenEventData,
    ScaledTokenMintResult, ScaledTokenProcessor,
};
pub use run::{apply_aave_chunk_writes_on_conn, AaveChunkEvent, AaveChunkWriteReport};
pub use transaction_processor::{process_transaction, ProcessTxError};
