//! **Relocated** to `degenbot-pools` (the family state structs + inherent
//! value methods are value-only). The full module content now lives in
//! `degenbot_pools::v4_state`; re-exported here at the historical
//! `bot_core::v4_state` path so consumers resolve unchanged. Transient re-export —
//! repointed at `degenbot_pools::v4_state` natively by USPN7M/P2CKRL.

pub use ::degenbot_pools::v4_state::*;
