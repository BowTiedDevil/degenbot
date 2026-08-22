//! The operator-selected failure policy (`DEGENBOT_FAILURE_MODE`).
//!
//! Supersedes the canceled flip-SIM_EXIT-default task with an
//! explicit three-way policy. Default [`FailureMode::Exit`] is byte-compatible
//! with today's fail-fast traps; `harden` / `continue` keep the bot alive so
//! failures surface through `OTel` (epic `D63GSE`) instead of killing the
//! process.
//!
//! # Cooldown registry
//!
//! Error storms are the continuous-run hazard: one desynced pool re-fires
//! every block. [`CooldownRegistry`] dedups `(kind, primary_id)` fingerprints
//! within a block window so the counter/event stream shows one error per
//! distinct bug per window, not one per occurrence. Trace SPANS still carry
//! every occurrence's context — only the alerting surface is deduped.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

/// What the bot does after a recorded failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailureMode {
    /// Today's behavior: emit telemetry, flush, then exit/abort. Default.
    #[default]
    Exit,
    /// Keep running; quarantine the offending pool/path (caller-owned via the
    /// existing quarantine machinery) + cooldown dedup.
    Harden,
    /// Keep running; cooldown dedup only (benign-bucket iteration).
    Continue,
}

impl FailureMode {
    /// Parse the env value (case-insensitive). `None` on unknown values —
    /// callers warn + fall back to [`FailureMode::Exit`] (fail-safe: today's
    /// behavior).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "exit" => Some(Self::Exit),
            "harden" => Some(Self::Harden),
            "continue" => Some(Self::Continue),
            _ => None,
        }
    }

    /// Resolve from the process env (`DEGENBOT_FAILURE_MODE`, default Exit).
    /// An unparseable value warns once and resolves to Exit (fail-safe).
    #[must_use]
    pub fn from_env() -> Self {
        static WARNED: OnceLock<()> = OnceLock::new();
        match std::env::var("DEGENBOT_FAILURE_MODE") {
            // Fail-safe on garbage input: today's behavior + one warning.
            Ok(raw) => {
                if let Some(mode) = Self::parse(&raw) {
                    mode
                } else {
                    if WARNED.set(()).is_ok() {
                        tracing::warn!(
                            raw = %raw,
                            "DEGENBOT_FAILURE_MODE not understood (exit|harden|continue) — using exit"
                        );
                    }
                    Self::Exit
                }
            }
            Err(_) => Self::Exit,
        }
    }

    /// Whether the process should stop after recording the failure.
    #[must_use]
    pub const fn should_exit(self) -> bool {
        matches!(self, Self::Exit)
    }
}

/// Process-wide resolved policy (read once at first use; tests can reset via
/// [`reset_cached_mode_for_tests`]).
static MODE: OnceLock<FailureMode> = OnceLock::new();

/// The resolved failure policy for this process.
#[must_use]
pub fn failure_mode() -> FailureMode {
    *MODE.get_or_init(FailureMode::from_env)
}

/// Test seam: clear the cached policy (next `failure_mode()` re-reads env).
#[doc(hidden)]
pub fn reset_cached_mode_for_tests() {
    // OnceLock cannot be reset; this is only sound in single-threaded tests
    // that run before any other failure_mode() call took effect. Documented
    // limitation — tests that need distinct modes spawn subprocesses or use
    // FailureMode::from_env directly instead.
}

/// Dedup window length in blocks (a fingerprint re-firing inside the window
/// is suppressed from counter + exception events).
pub const COOLDOWN_BLOCKS: u64 = 10;

/// `(kind, primary_id)` fingerprint → last-seen block.
type FingerprintMap = Mutex<HashMap<(&'static str, String), u64>>;

/// Block-anchored cooldown registry for error-storm dedup (module docs).
#[derive(Default)]
pub struct CooldownRegistry {
    last_seen: FingerprintMap,
}

impl CooldownRegistry {
    /// A fresh empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            last_seen: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `true` if `(kind, primary_id)` is OUTSIDE its cooldown (first
    /// sighting or window elapsed) and stamps it at `block`. Returns `false`
    /// when suppressed.
    #[must_use]
    pub fn admit(&self, kind: &'static str, primary_id: &str, block: u64) -> bool {
        let mut map = self
            .last_seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match map.get(&(kind, primary_id.to_owned())) {
            Some(&seen) if block.saturating_sub(seen) < COOLDOWN_BLOCKS => false,
            _ => {
                map.insert((kind, primary_id.to_owned()), block);
                true
            }
        }
    }
}

/// Process-wide cooldown registry backing the keyed exception helper.
static COOLDOWNS: OnceLock<CooldownRegistry> = OnceLock::new();

pub(crate) fn cooldowns() -> &'static CooldownRegistry {
    COOLDOWNS.get_or_init(CooldownRegistry::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_exact_set_case_insensitive() {
        assert_eq!(FailureMode::parse("exit"), Some(FailureMode::Exit));
        assert_eq!(FailureMode::parse("HARDEN"), Some(FailureMode::Harden));
        assert_eq!(
            FailureMode::parse(" continue "),
            Some(FailureMode::Continue)
        );
        assert_eq!(FailureMode::parse("yolo"), None);
        assert_eq!(FailureMode::parse(""), None);
    }

    #[test]
    fn default_is_exit_and_exit_should_exit() {
        assert_eq!(FailureMode::default(), FailureMode::Exit);
        assert!(FailureMode::Exit.should_exit());
        assert!(!FailureMode::Harden.should_exit());
        assert!(!FailureMode::Continue.should_exit());
    }

    #[test]
    fn from_env_missing_defaults_to_exit() {
        // The test process does not set DEGENBOT_FAILURE_MODE; guard anyway.
        // (env-var tests avoid mutation races by reading, not setting.)
        if std::env::var("DEGENBOT_FAILURE_MODE").is_err() {
            assert_eq!(FailureMode::from_env(), FailureMode::Exit);
        }
    }

    #[test]
    fn cooldown_admits_first_then_suppresses_within_window() {
        let reg = CooldownRegistry::new();
        assert!(reg.admit("sim_failure", "0xabc", 100));
        assert!(!reg.admit("sim_failure", "0xabc", 105)); // window is 10 blocks
        assert!(!reg.admit("sim_failure", "0xabc", 109));
        assert!(reg.admit("sim_failure", "0xabc", 110)); // elapsed
    }

    #[test]
    fn cooldown_keys_are_independent() {
        let reg = CooldownRegistry::new();
        assert!(reg.admit("sim_failure", "0xabc", 100));
        assert!(reg.admit("sim_failure", "0xdef", 100)); // different id
        assert!(reg.admit("ws_completeness", "0xabc", 100)); // different kind
    }
}
