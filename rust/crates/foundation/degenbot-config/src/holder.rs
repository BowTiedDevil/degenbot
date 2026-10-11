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
//!
//! # Snapshot, not a one-shot
//!
//! [`install`] keeps PRODUCTION first-wins semantics: the first boot to
//! install owns the snapshot and every later `install` is a no-op. What
//! changed is the storage SHAPE: the slot is a swappable pointer rather than
//! a `OnceLock`, so the sanctioned TEST seam [`reinstall_for_tests`] can
//! re-govern the snapshot from a fresh env+file load. The seam does not exist
//! to change production behavior — no production code calls it — but a
//! `OnceLock` cannot be cleared, and the suite needs exactly that re-govern.
//!
//! ## Safety contract of [`reinstall_for_tests`]
//!
//! Previously-constructed engines keep their minted snapshots: an engine
//! clones the [`config_arc`] value (or reads a typed field) at CONSTRUCTION,
//! never through a live holder pointer, so a reinstall re-governs only future
//! resolutions. The process-lifetime resources sized from the first snapshot
//! (the shared io runtime, the solve/io worker pools) are deliberately NOT
//! re-sized by the seam — the same "process-lifetime, not per-engine"
//! boundary the io runtime has always had.
//!
//! Every published snapshot is leaked to a `'static` reference (the previous
//! `OnceLock` gave the one value it held the same process-lifetime
//! guarantee); [`reinstall_for_tests`] therefore leaks one `Arc<BotConfig>`
//! per reinstall, bounded by the test that drives it.

use std::sync::{Arc, RwLock};

use crate::schema::BotConfig;

/// The INSTALLED snapshot: `None` until a real boot installs one (or the test
/// seam reinstalls). A bare read never sets it, so [`installed`] keeps meaning
/// "a boot installed a config" rather than "someone read the default".
static CFG: RwLock<Option<&'static Arc<BotConfig>>> = RwLock::new(None);

/// The schema-default fallback, built on first read when nothing was
/// installed. Held SEPARATELY from [`CFG`] so reading the default never makes
/// [`installed`] report a boot.
static DEFAULT: RwLock<Option<&'static Arc<BotConfig>>> = RwLock::new(None);

/// Read a snapshot slot, ignoring a poisoned writer (a panic while swapping
/// must not brick every later config read).
fn read_slot(slot: &RwLock<Option<&'static Arc<BotConfig>>>) -> Option<&'static Arc<BotConfig>> {
    *slot
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Publish `cfg` into `slot` as a process-lifetime reference.
fn write_slot(
    slot: &RwLock<Option<&'static Arc<BotConfig>>>,
    cfg: Arc<BotConfig>,
) -> &'static Arc<BotConfig> {
    let leaked: &'static Arc<BotConfig> = Box::leak(Box::new(cfg));
    *slot
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(leaked);
    leaked
}

/// Install the loaded config (first caller wins — the boot path installs
/// before any pump/engine construction; later calls are no-ops and return
/// `false`).
#[must_use]
pub fn install(cfg: Arc<BotConfig>) -> bool {
    let mut slot = CFG
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if slot.is_some() {
        return false;
    }
    let leaked: &'static Arc<BotConfig> = Box::leak(Box::new(cfg));
    *slot = Some(leaked);
    true
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
    if let Some(installed) = read_slot(&CFG) {
        return installed;
    }
    if let Some(default) = read_slot(&DEFAULT) {
        return default;
    }
    write_slot(&DEFAULT, Arc::new(BotConfig::default()))
}

/// Was a config installed by a real boot (vs test defaults)?
#[must_use]
pub fn installed() -> bool {
    read_slot(&CFG).is_some()
}

/// TEST-ONLY: re-govern the process-wide snapshot from a fresh load.
///
/// Unreachable from a production boot path (no production code calls it; the
/// only callers are the sanctioned test seam and the Rust guard test), and
/// deliberately loud. Unlike [`install`], this is NOT first-wins — overwriting
/// the snapshot is the whole point. Returns the newly published Arc so a
/// caller can assert identity. See the module docs for the safety contract
/// (previously-constructed engines keep their minted snapshots).
#[doc(hidden)]
#[must_use]
pub fn reinstall_for_tests(cfg: Arc<BotConfig>) -> &'static Arc<BotConfig> {
    write_slot(&CFG, cfg)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{config, config_arc, install, installed, reinstall_for_tests};
    use crate::schema::BotConfig;

    /// install-once stays first-wins; the test seam re-governs future reads.
    ///
    /// ONE test owns the process-global slot (the crate's unit tests run on
    /// parallel threads), so no second writer can interleave.
    #[test]
    fn install_is_first_wins_and_the_test_seam_re_governs() {
        assert!(!installed(), "nothing has installed in this binary yet");

        let first = Arc::new(BotConfig::default());
        assert!(
            install(Arc::clone(&first)),
            "the first install owns the slot"
        );
        assert!(installed());
        assert!(
            Arc::ptr_eq(config_arc(), &first),
            "the installed Arc is handed back verbatim"
        );

        let mut second = BotConfig::default();
        second.session.chain_id = Some(4242);
        assert!(
            !install(Arc::new(second)),
            "a second install is a no-op (first-wins)"
        );
        assert!(
            Arc::ptr_eq(config_arc(), &first),
            "the first install still owns the slot"
        );

        let mut reinstated = BotConfig::default();
        reinstated.session.chain_id = Some(777);
        let leaked = reinstall_for_tests(Arc::new(reinstated));
        assert!(installed());
        assert!(Arc::ptr_eq(config_arc(), leaked));
        assert_eq!(config().session.chain_id, Some(777));
    }
}
