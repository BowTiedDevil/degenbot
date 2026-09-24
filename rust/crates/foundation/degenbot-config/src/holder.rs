//! The process-wide typed `BotConfig` holder.
//!
//! Exactly one site reads the environment for `DEGENBOT_*` keys: the
//! 12-factor loader. The boot path loads the config once and installs the
//! resulting value via [`install`]; every former environment-reading call
//! site (which previously read per-key env vars at call time) consults
//! [`config`] and gets a typed schema field. This module performs no
//! environment access itself — it is a value holder (the ADR-021 tripwire
//! pattern, "carries data, never the environment").
//!
//! Tests that never install a config observe the schema defaults, which is
//! byte-compatible with running in a clean environment. (Per-process A/B
//! testing and a second bot instance install distinct values before the
//! first engine construction — the ground the retired solver RUNTIME
//! OnceLock used to hold process-wide.)

use std::sync::{Arc, OnceLock};

use crate::schema::BotConfig;

static CFG: OnceLock<Arc<BotConfig>> = OnceLock::new();

/// Install the loaded config (first caller wins — the boot path installs
/// before any pump/engine construction; later calls are no-ops and return
/// `false`).
#[must_use]
pub fn install(cfg: Arc<BotConfig>) -> bool {
    CFG.set(cfg).is_ok()
}

/// The installed config, or the schema defaults when no owner installed one
/// (tests / standalone constructions in a clean environment).
#[must_use]
pub fn config() -> &'static BotConfig {
    config_arc().as_ref()
}

/// The same value as [`config`], behind the process-shared Arc — the
/// construction-time packer (`ArbitrageEngine::with_core`) clones this into
/// the engine instead of building a fresh default, so production boots
/// observe the loader's value for every construction stance.
#[must_use]
pub fn config_arc() -> &'static Arc<BotConfig> {
    static DEFAULT: OnceLock<Arc<BotConfig>> = OnceLock::new();
    CFG.get()
        .unwrap_or_else(|| DEFAULT.get_or_init(|| Arc::new(BotConfig::default())))
}

/// Was a config installed by a real boot (vs test defaults)?
#[must_use]
pub fn installed() -> bool {
    CFG.get().is_some()
}
