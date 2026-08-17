//! Simulation domain — the in-process revm engine.
//!
//! `sim::evm` is the in-process EVM execution core (revm over the
//! `CacheDB<WarmCodeCache<BotStateDb<WrapDatabaseAsync<AlloyDB>>>` stack),
//! folded here from the retired `degenbot-evm` crate (ADR-019 D4 — the
//! accidental two-crate split is resolved by colocation, not a re-export
//! bridge). The backrun strategy (the dispatch fan-out + the 7-call bundle)
//! lives in `degenbot-settlement-strategy` (ADR-019 D4/D7, decision R).
pub mod evm;
