//! `PyO3` wrapper for the `UniswapEngine` — snapshot `#[pymethods]` slice.
//!
//! Split out of the former monolithic `py_binding.rs` (ergo UG6FKN task 74W2Z6),
//! mirroring `crates/degenbot-bot/src/solvers/uniswap_engine/`'s per-concern
//! layout. `PyO3` allows multiple `#[pymethods] impl PyUniswapArbEngine { … }`
//! blocks per type, so each concern file contributes one slice.
//!
//! ## Retired surface (epic `XEANMB`)
//! RUQ637's `SnapshotStore` fields + the `load_*_from_py` / `clear_*_snapshot`
//! ingestion surface are RETIRED. The in-memory `SnapshotStore` was a
//! boot-time freeze of the DB cut; epic `XEANMB` replaced it with a WAL held
//! read transaction (`SnapshotDb`) so every per-pool `fetch_liquidity_map`
//! during `build_paths` shares one frozen DB snapshot. Per-pool tick data is
//! now read through the Db arm of `assemble_*_tick_map` (the held tx) for the
//! DB path, or fetched via the Chain arm (RPC) for the non-DB path. The
//! non-DB (file/memory) path sets `snapshot_seed_block` (the auto-backfill
//! anchor `S`) directly via the `snapshot_seed_block` setter — `start()`
//! computes `S = min(newest_block)` + sets it before `subscribe()` so
//! `after_subscribe` advances the engine phase to `SnapshotLoaded`.
//!
//! DADWUP retired the per-pool ingestion surface; XEANMB retires the
//! whole-dict ingestion surface too. This slice is now intentionally empty —
//! the `#[pymethods]` snapshot methods are gone + the file remains as a home
//! for the module-level documentation of the retirement.

// No `#[pymethods]` here — the snapshot ingestion surface is retired (XEANMB).
