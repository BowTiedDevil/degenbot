//! Shared support for the engine-lab development package.
//!
//! This package owns the bot, simulation, arbitrage, JIT, fan-out, and BAL
//! instruments. Three rules keep it a laboratory rather than a product:
//!
//! - **It is not a production operator CLI.** Nothing shipped, reachable from a
//!   product crate, or documented as a supported *product* entry point may live
//!   here. A development-tool recipe is not a product surface: the documented
//!   `just aot-prepare-executor` path is a tool entry point, and it stays
//!   documented.
//! - **The two heavy lanes stay separate.** The JIT engine and the bot's
//!   Prometheus exporter are independent forwards with independent
//!   `required-features` on the programs that need them, so a default check
//!   compiles neither. There is no combined profiler feature, and no lane is
//!   spelled `--all-features`.
//! - **The exporter lane is the bot's, not the solver's.** `/metrics` scraping
//!   belongs to `prometheus`; solver-phase function timing belongs to
//!   `degenbot-solver-lab`.
//!
//! The library target is the shared support the instruments build on: the
//! corpus, modeled chain, dispatch, read oracle, wall ledger, and report
//! shaping, so those modules are imported rather than `#[path]`-included from an
//! example directory.
//!
//! Lane features: `evm2-jit`, `prometheus`.
