//! **Relocated** to `degenbot-pools` (the family state structs + inherent
//! value methods are value-only). The full module content now lives in
//! `degenbot_pools::aerodrome_v2_state`; re-exported here at the historical
//! `bot_core::aerodrome_v2_state` path so consumers resolve unchanged. Transient re-export —
//! repointed at `degenbot_pools::aerodrome_v2_state` natively by USPN7M/P2CKRL.

pub use ::degenbot_pools::aerodrome_v2_state::*;
