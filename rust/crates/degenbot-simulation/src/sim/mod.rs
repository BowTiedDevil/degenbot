//! Simulation domain — the in-process revm executor + the dispatch fan-out.
//!
//! `sim::evm` is the in-process EVM execution core (revm over the
//! `CacheDB<WarmCodeCache<BotStateDb<WrapDatabaseAsync<AlloyDB>>>` stack),
//! folded here from the retired `degenbot-evm` crate (ADR-019 D4 — the
//! accidental two-crate split is resolved by colocation, not a re-export
//! bridge).
pub mod evm;
