//! Simulation domain — the in-process revm engine.
//!
//! `sim::evm` is the in-process EVM execution core (revm over the
//! `CacheDB<WarmCodeCache<BotStateDb<WrapDatabaseAsync<AlloyDB>>>` stack),
//! folded here from the retired `degenbot-evm` crate (ADR-019 D4 — the
//! accidental two-crate split is resolved by colocation, not a re-export
//! bridge). The settlement-arbitrage strategy (the dispatch fan-out + the 7-call bundle)
//! lives in `degenbot-arbitrage` (ADR-019 D4/D7, decision R).
pub mod evm;

/// The evm2 pilot engine — an opt-in parallel implementation (`evm2`
/// feature) proving the migration seam behind the same `sim` domain. Off in
/// every default lane; hard cutover retires it again.
#[cfg(feature = "evm2")]
pub mod evm2;
