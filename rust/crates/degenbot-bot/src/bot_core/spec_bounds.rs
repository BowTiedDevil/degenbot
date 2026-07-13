//! Spec-bound validation for pool registration admission.
//!
//! **Relocated** to `degenbot-pools` (pure value — `SpecViolation` /
//! `SpecValue` + the `validate_*` bounds are value-only, no `BotState`). The
//! full module content now lives in `degenbot_pools::spec_bounds`; re-exported
//! here at the historical `bot_core::spec_bounds` path so the bot's consumers
//! (the `register_v{2,3,4}_pool` admission checks) resolve unchanged. The
//! `BotState::register_*_pool` methods that CALL these validators stay in bot.
//!
//! Transient re-export — repointed at `degenbot_pools::spec_bounds` natively by
//! USPN7M/P2CKRL.

pub use ::degenbot_pools::spec_bounds::*;
