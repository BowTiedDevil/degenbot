//! Value-only pool identity/state structs + stateless swap simulation for the
//! degenbot pool families — pure-Rust, `pyo3`-free.
//!
//! This crate owns the **pure-data, pure-math** layer of the pool subsystem:
//! the per-family `*PoolIdentity` / `*PoolState` value structs, the
//! `PoolEntry` sum type, the per-family `Register*PoolParams` /
//! `Register*PoolError` DTOs, the reorg-journal *data* (`ReorgJournal`,
//! `*BlockDelta`), the spec-bound validators, and the **stateless** swap
//! simulators (`v3_simulate_swap`, `v4_simulate_swap`, the V2 constant-product
//! dispatch, and the Curve/Balancer sims) that compute a swap outcome purely
//! from in-memory pool state with no chain / registry / async / tokio.
//!
//! Nothing here performs I/O. The I/O-shaped *interface* traits
//! (`TickWordFetcher`, `CurveDataProvider`, `BalancerRateProvider`) are defined
//! in this crate — but their RPC/DB *implementations* live in
//! `degenbot-bot`, exactly as `std::io::Read` defines an interface in the
//! standard library while its implementors (`File`, `BufReader`, …) live
//! elsewhere. This is the `std::io::Read` precedent: defining a capability
//! trait pulls no I/O and avoids a cyclic dependency (`pools` must not depend
//! on `degenbot-bot` or `degenbot-rpc`).
//!
//! ## Dividing line (per ADR-003 / ADR-005)
//!
//! > *Given all pool state in memory, can this compute deterministically with
//! > no chain / registry / async / tokio?* **Yes →** this crate
//! > (`degenbot-pools`). **No →** `degenbot-bot` (the `Bot`/`BotState` registry,
//! > the block pump, the reorg *coordinator*, the fetch-retry shells, the
//! > RPC/DB trait impls, the engine, the solvers).
//!
//! Concretely, the `BotState::simulate_swap_with_override` retry shell stays
//! in `degenbot-bot`: it looks up `self.pools.get(id)` (`PoolEntry`), calls the
//! stateless sim in this crate, and — for V3/V4 — catches a
//! `MissingTickWord(word)` *value* error, fetches the tick word via the (state
//! crate's) `TickWordFetcher` trait impl that the bot registered, and retries.
//! This is "Pattern B": the value crate returns a `MissingTickWord(i32)` value
//! error; the fetch-and-retry loop lives one layer up in the bot.
//!
//! ## Why this is a standalone crate (ADR-005 "standalone constraint")
//!
//! Previously these value types + stateless sims lived inline in
//! `degenbot-bot/src/bot_core/mod.rs` (a ~7500-line module) and its 22
//! submodules, inside the engine crate. That stranded standalone-usable pool
//! *data* and pool-family *swap math* inside the bot's I/O/registry surface —
//! a `cargo add degenbot` consumer wanting pool state + swap sims pulled the
//! engine, the block pump, RPC clients, and the solvers. Moving the value-only
//! layer out gives a clean Rust core that a standalone consumer (or the
//! `degenbot` umbrella) can depend on without the I/O umbrella, while
//! `degenbot-bot`'s `BotState` becomes a thin registry-lookup wrapper that
//! delegates each method to the value core in this crate.
//!
//! This crate is `pyo3`-free under its default features (enforced by `just
//! check-no-pyo3-in-cores`); it depends on `alloy`, `thiserror`, and the
//! per-family math leaf crates (`degenbot-v2-math`, `degenbot-cl-math`,
//! `degenbot-curve-math`, `degenbot-balancer-math`, `degenbot-solidly-math`)
//! plus `degenbot-uniswap` (for `DexVariant`). It is consumed by
//! `degenbot-bot` and re-exported by the `degenbot` umbrella for standalone
//! Rust consumers.
//!
//! ## Contents (added incrementally)
//!
//! The crate is populated by the `USPN7M` epic, one task per concern:
//!
//! - **trait definitions** (`TickWordFetcher`, `CurveDataProvider`,
//!   `BalancerRateProvider` + their error/return types + `StaticRateProvider`)
//! - **leaf value modules** (`spec_bounds`, `state_history`, `tick_bitmap`,
//!   `tick_map`)
//! - **per-family state structs** + `PoolEntry` / `V3FamilyPool` / `TickInfo` /
//!   `TokenEntry` + `Register*Pool{Params,Error}`
//! - **stateless swap sims** (`v3_simulate_swap`, `v4_simulate_swap`,
//!   `SimulateSwapError`, `V3SwapOutcome`, the V2/Curve/Balancer dispatch)
