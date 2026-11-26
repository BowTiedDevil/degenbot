//! Shared support for the solver-lab development package.
//!
//! This package owns the solver and order-index replay, measurement, and stress
//! instruments. Three rules keep it a laboratory rather than a gate:
//!
//! - **Measurements never gate.** Nothing added here becomes a pass/fail gate,
//!   and no `just check-dev-tools` lane may be promoted into one: what lives
//!   here reports numbers, and a durable assertion belongs in the owning crate's
//!   integration tests. The `just test-golden-replay` recipe invokes the
//!   two-sided golden contract's durable assertions in `degenbot-pools`'
//!   owning integration tests through the canonical nextest lane, then runs
//!   this replay as the non-gating measurement/readout pass
//!   (`rust/tools/README.md` sections 8 and 8.1).
//! - **A small replay stays small.** Profiling, capture I/O, the cache lab, and
//!   the hull index are four separate opt-in forwards, so reading a capture
//!   never *enables* a profiler, the ledger feature, or a JIT lane. A forward is
//!   not a resolved tree, though: `degenbot-pools` carries the `hotpath` crate
//!   non-optionally (no-op stubs unless this package's `hotpath` forward turns
//!   it on) and `degenbot-db`'s bundled `rusqlite` rides along wherever the
//!   database crate is reached. No combined "all lab" feature exists, and no
//!   lane is spelled `--all-features`.
//! - **Pool profiling is not the bot exporter.** The `hotpath` lane is
//!   solver-phase function timing; the `/metrics` listener belongs to the bot
//!   exporter lane in `degenbot-engine-lab`.
//!
//! The library target is the shared support the instruments build on: capture
//! row parsing and the harness schemas, so a helper module is imported rather
//! than `#[path]`-included from an example directory.
//!
//! # Modules
//!
//! - [`paths`] — repository-root resolution (marker-resolved, no `..`
//!   arithmetic) and the pools fixture home the synthetic corpus generator
//!   writes into.
//! - `capture_rows`, `concentrated_liquidity_rows` — the capture-JSONL parsing
//!   the replay and readout instruments share, behind `capture-fixtures`
//!   because they reach the feature's fixture reader for the default capture
//!   path.
//! - `golden_regen` — over-shoot-only refresh planning, atomic fixture writes,
//!   and old/new human and JSON reports for the replay instrument.
//! - `budgets` — exact path 4/105 walk-count artifacts, fail-closed drift
//!   comparison, checked generation, and derivation-inclusive repetitions.
//!
//! Lane features: `capture-fixtures`, `cache-lab`, `telemetry` (the cache lab's
//! second build shape, never a bot-exporter lane), `hotpath`, `envelope`.

pub mod paths;

#[cfg(feature = "capture-fixtures")]
pub mod budgets;

#[cfg(feature = "capture-fixtures")]
pub mod capture_rows;

#[cfg(feature = "capture-fixtures")]
pub mod concentrated_liquidity_rows;

#[cfg(feature = "capture-fixtures")]
pub mod golden_regen;
