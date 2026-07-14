//! **Relocated** to `degenbot-pools` (the trait/generic core + its inherent
//! impls + value structs; `*BlockDelta` types + the state-dependent restore
//! methods co-locate with their family state which also moved to pools).
//! Re-exported here at the historical `bot_core::tick_map` path so consumers
//! resolve unchanged. Transient re-export — repointed at
//! `degenbot_pools::tick_map` natively by USPN7M/P2CKRL.

pub use ::degenbot_pools::tick_map::*;
