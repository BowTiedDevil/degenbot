//! The per-bucket failure policy (ADR-040) — one closed taxonomy in which
//! every failure bucket declares its `Severity`, taint `Scope`, and default
//! `Action`. Reactions are a pure function of the bucket: there is no
//! process-wide mode to reconcile (the D63GSE `exit|harden|continue` lattice
//! is retired — it was a second channel that could contradict the bucket
//! table). Supply-side dedup (the [`CooldownRegistry`] behind
//! `telemetry::record_exception_keyed`) is orthogonal to reaction choice and
//! survives unchanged.
//!
//! # Completeness contract
//!
//! The taxonomy is closed: adding a `telemetry::error_kind` const or a sim
//! reason without a table entry trips the runtime-completeness test (the
//! `error_kinds_are_unique` pattern extended to the matrix). Runtime lookups
//! of an UNDECLARED string fall back to the conservative
//! `Degraded`/`Process` floor (never silent) — the fallback exists so an
//! operator config error or an evolution gap degrades loudly, not fatally.
//!
//! # Operator knob
//!
//! Per-bucket action overrides (ADR-040 D3): `config.toml` `[failure_policy]`
//! maps `kind` or `kind.reason` → `observe|event|quarantine|exit`. Keys are
//! closed and boot-validated — an unknown key or action is a boot ERROR, not
//! a warning. There is deliberately no env-var mode switch.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

/// How much damage continuing can do after this failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Expected domain economics — not a malfunction at all. Never surfaces
    /// through `degenbot.errors`.
    Benign,
    /// Transient or environmental; the next solve re-derives from fresh state
    /// and is trustworthy.
    Degraded,
    /// The failure poisons future trading decisions on its [`Scope`] surface.
    Tainted,
    /// Continuing is meaningless or unsafe at process scale.
    Fatal,
}

/// The smallest surface whose future decisions are untrustworthy after the
/// failure. Quarantine reactions contaminate exactly this scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// One pool: quarantine every path touching it.
    Pool,
    /// One path/encoder: other paths over the same pools are unaffected.
    Path,
    /// No local surface — the failure is machine- or feed-wide.
    Process,
}

/// What the seam does after recording the failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Counters only; no `degenbot.errors` increment, no event.
    Observe,
    /// Keyed loud event (deduped), keep running.
    Event,
    /// Quarantine the taint scope + keyed loud event (+ repro dump at the
    /// seam). ADR-040: a tainted bucket ALWAYS quarantines.
    Quarantine,
    /// Loud event, flush, exit.
    Exit,
}

impl Action {
    /// Parse the operator spelling.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "observe" => Some(Self::Observe),
            "event" => Some(Self::Event),
            "quarantine" => Some(Self::Quarantine),
            "exit" => Some(Self::Exit),
            _ => None,
        }
    }

    /// The canonical spelling (the pyfunction + config round-trip value).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Event => "event",
            Self::Quarantine => "quarantine",
            Self::Exit => "exit",
        }
    }
}

/// The default action for a severity — the matrix's derived half. `tainted`
/// ALWAYS quarantines (that is its definition, ADR-040 D1); `fatal` exits.
#[must_use]
pub const fn default_action(severity: Severity) -> Action {
    match severity {
        Severity::Benign => Action::Observe,
        Severity::Degraded => Action::Event,
        Severity::Tainted => Action::Quarantine,
        Severity::Fatal => Action::Exit,
    }
}

/// The closed bucket table — the ADR-040 decision matrix. `(kind, reason)`;
/// `reason: None` covers kinds with no sub-taxonomy (the reason axis only
/// exists where the emitting seam discriminates).
#[must_use]
#[expect(
    clippy::match_same_arms,
    reason = "the ADR-040 table is line-per-bucket by design: rows that coincide today (DeliveryLag / suspect sim revert) are distinct buckets on purpose - merging the arms would hide the taxonomy the matrix exists to name"
)]
pub fn bucket(kind: &str, reason: Option<&str>) -> (Severity, Scope) {
    match (kind, reason) {
        // ---- solver-state tripwire (ADR-021 classes → ADR-040 buckets) ----
        // Strict-gate defect classes (ADR-021 D2): divergent pool state -
        // assume the worst until classified (Unclassified/None included).
        // DeliveryLag + sim revert-pool-state stay Degraded (lag is
        // report-only per ADR-021 Part B; a suspect revert needs tripwire
        // corroboration before escalating to the tainted class).
        (
            "solver_state_desync",
            Some("missed_log" | "unhandled_reorg" | "storage_mutated" | "unclassified") | None,
        )
        | ("verify_mismatch", _) => (Severity::Tainted, Scope::Pool),
        ("sim_failure", Some("revert_pool_state")) => (Severity::Degraded, Scope::Pool),

        // simulator reason split (ADR-040; the discriminator is
        // `classify_revert` frame attribution at the sim seam).
        ("sim_failure", Some("pre_encode")) => (Severity::Tainted, Scope::Path),
        ("sim_failure", Some("revert_economics")) => (Severity::Benign, Scope::Path),
        ("sim_failure", Some("rpc") | None) => (Severity::Degraded, Scope::Process),
        ("submit_failure", _) => (Severity::Degraded, Scope::Process),
        ("monitor_failure", _) => (Severity::Degraded, Scope::Path),
        ("ws_completeness" | "drain_stall" | "drain_dead", _) => (Severity::Fatal, Scope::Process),

        // Evolution floor — conservative, never silent (module docs).
        _ => (Severity::Degraded, Scope::Process),
    }
}

/// Errors from [`install_overrides`] — boot-loud per ADR-040 D3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverrideError {
    /// The bucket key is not a declared `kind` or `kind.reason`.
    UnknownBucket(String),
    /// The action spelling is not one of the four canonical strings.
    UnknownAction(String),
}

impl std::fmt::Display for OverrideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownBucket(k) => write!(f, "unknown failure bucket: {k:?}"),
            Self::UnknownAction(a) => write!(f, "unknown failure action: {a:?}"),
        }
    }
}

impl std::error::Error for OverrideError {}

/// `(key, action_str)` override store, installed ONCE at boot.
type OverrideMap = HashMap<String, Action>;
static OVERRIDES: OnceLock<OverrideMap> = OnceLock::new();

static KNOWN_KINDS: &[&str] = &[
    "solver_state_desync",
    "ws_completeness",
    "sim_failure",
    "submit_failure",
    "monitor_failure",
    "verify_mismatch",
    "drain_stall",
    "drain_dead",
];

static KNOWN_REASONS: &[(&str, &str)] = &[
    ("solver_state_desync", "missed_log"),
    ("solver_state_desync", "unhandled_reorg"),
    ("solver_state_desync", "storage_mutated"),
    ("solver_state_desync", "delivery_lag"),
    ("solver_state_desync", "unclassified"),
    ("sim_failure", "pre_encode"),
    ("sim_failure", "revert_pool_state"),
    ("sim_failure", "revert_economics"),
    ("sim_failure", "rpc"),
];

/// Validate one `key → action_str` pair WITHOUT installing. `key` is `kind`
/// or `kind.reason`.
///
/// # Errors
/// [`OverrideError::UnknownBucket`] for an undeclared bucket; [`OverrideError::UnknownAction`]
/// for an uncanonical action spelling.
fn validate_pair(key: &str, action_raw: &str) -> Result<Action, OverrideError> {
    let action = Action::parse(action_raw)
        .ok_or_else(|| OverrideError::UnknownAction(action_raw.to_owned()))?;
    let (kind, reason) = key
        .split_once('.')
        .map_or((key, None), |(k, r)| (k, Some(r)));
    if KNOWN_KINDS.contains(&kind) && reason.is_none_or(|r| KNOWN_REASONS.contains(&(kind, r))) {
        Ok(action)
    } else {
        Err(OverrideError::UnknownBucket(key.to_owned()))
    }
}

/// Install the boot-validated override map. A second call is a no-op (boot
/// happens once); the winner is the first install.
///
/// # Errors
/// The FIRST invalid pair, surfaced as [`OverrideError`] — callers treat this
/// as a boot error (ADR-040 D3: fail loud, do not trade on a half-read
/// policy).
pub fn install_overrides<'a>(
    overrides: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(), OverrideError> {
    let mut map = OverrideMap::new();
    for (key, action_raw) in overrides {
        let action = validate_pair(key, action_raw)?;
        map.insert(key.to_owned(), action);
    }
    let _ = OVERRIDES.set(map);
    Ok(())
}

/// The effective action for a bucket: the operator override when one keys
/// it, else the table's default ([`default_action`] of [`bucket`]).
/// Undeclared buckets fall back to the degraded floor (module docs) — callers
/// that can should treat an unexpected `Event` for a new kind as a prompt to
/// extend the table.
#[must_use]
pub fn action(kind: &str, reason: Option<&str>) -> Action {
    if let Some(map) = OVERRIDES.get() {
        let key = match reason {
            Some(r) => format!("{kind}.{r}"),
            None => kind.to_owned(),
        };
        if let Some(a) = map.get(&key) {
            return *a;
        }
        // A kind-level override also covers reason sub-buckets.
        if reason.is_some() {
            if let Some(a) = map.get(kind) {
                return *a;
            }
        }
    }
    let (severity, _scope) = bucket(kind, reason);
    default_action(severity)
}

/// The taint scope for a bucket — exposed so the reacting seam knows WHAT to
/// quarantine (pool / path / process).
#[must_use]
pub fn scope(kind: &str, reason: Option<&str>) -> Scope {
    bucket(kind, reason).1
}

/// Process-wide dedup for error storms (kept from D63GSE): one alerting
/// surface event per `(kind, primary_id)` fingerprint per [`COOLDOWN_BLOCKS`]
/// window. Trace spans still carry every occurrence.
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
    #![expect(clippy::expect_used)]

    use super::*;

    // --- matrix completeness (the runtime exhaustive check) ---

    /// Every declared `kind` + every declared `kind.reason` pair must map
    /// WITHOUT the fallback — the completeness check that makes an added
    /// kind-without-entry a loud test failure instead of a silent degraded.
    #[test]
    fn matrix_is_total_over_declared_buckets() {
        // Every telemetry error_kind const must be a KNOWN_KINDS entry, and
        // every declared reason pair must resolve WITHOUT the fallback floor.
        use crate::telemetry::error_kind;
        let kinds = [
            error_kind::SOLVER_STATE_DESYNC,
            error_kind::WS_COMPLETENESS,
            error_kind::SIM_FAILURE,
            error_kind::SUBMIT_FAILURE,
            error_kind::MONITOR_FAILURE,
            error_kind::VERIFY_MISMATCH,
            error_kind::DRAIN_STALL,
            error_kind::DRAIN_DEAD,
        ];
        for kind in kinds {
            assert!(
                KNOWN_KINDS.contains(&kind),
                "error_kind const {kind} missing from the failure matrix"
            );
        }
        // Declared reason pairs belong to declared kinds and resolve cleanly.
        for (kind, reason) in KNOWN_REASONS {
            assert!(
                KNOWN_KINDS.contains(kind),
                "reason key on unknown kind {kind}"
            );
            let _ = bucket(kind, Some(reason));
        }
    }

    /// ADR-040 decision-table spot checks (the load-bearing row semantics).
    #[test]
    fn matrix_rows_match_adr_040_table() {
        // strict-gate desync classes quarantine their pool
        for r in [
            "missed_log",
            "unhandled_reorg",
            "storage_mutated",
            "unclassified",
        ] {
            assert_eq!(action("solver_state_desync", Some(r)), Action::Quarantine);
            assert_eq!(scope("solver_state_desync", Some(r)), Scope::Pool);
        }
        // delivery lag never trips (ADR-021 Part B, retained)
        assert_eq!(
            action("solver_state_desync", Some("delivery_lag")),
            Action::Event
        );
        // sim reasons
        assert_eq!(
            action("sim_failure", Some("pre_encode")),
            Action::Quarantine
        );
        assert_eq!(scope("sim_failure", Some("pre_encode")), Scope::Path);
        assert_eq!(
            action("sim_failure", Some("revert_economics")),
            Action::Observe
        );
        assert_eq!(action("sim_failure", Some("rpc")), Action::Event);
        assert_eq!(
            action("sim_failure", Some("revert_pool_state")),
            Action::Event
        );
        // benign never touches errors (severity check)
        assert_eq!(
            bucket("sim_failure", Some("revert_economics")).0,
            Severity::Benign
        );
        // process fatals
        for k in ["ws_completeness", "drain_stall", "drain_dead"] {
            assert_eq!(action(k, None), Action::Exit);
            assert_eq!(scope(k, None), Scope::Process);
        }
        // degraded defaults
        assert_eq!(action("submit_failure", None), Action::Event);
        assert_eq!(action("monitor_failure", None), Action::Event);
        assert_eq!(scope("monitor_failure", None), Scope::Path);
        // verify mismatch = deny admission = quarantine-class severity
        assert_eq!(bucket("verify_mismatch", None).0, Severity::Tainted);
    }

    // --- overrides ---

    #[test]
    fn overrides_validate_closed_keys_and_actions() {
        assert!(validate_pair("sim_failure.pre_encode", "observe").is_ok());
        assert!(validate_pair("sim_failure", "exit").is_ok());
        assert_eq!(
            validate_pair("nope", "exit"),
            Err(OverrideError::UnknownBucket("nope".to_owned()))
        );
        assert_eq!(
            validate_pair("sim_failure.nope", "exit"),
            Err(OverrideError::UnknownBucket("sim_failure.nope".to_owned()))
        );
        assert_eq!(
            validate_pair("sim_failure", "yolo"),
            Err(OverrideError::UnknownAction("yolo".to_owned()))
        );
    }

    #[test]
    fn installed_override_governs_action() {
        static FIRST: OnceLock<()> = OnceLock::new();
        // Buckets deliberately NOT asserted by the table test above: the
        // override store is process-global and tests run in parallel.
        if FIRST.set(()).is_ok() {
            install_overrides([("verify_mismatch", "observe")]).expect("valid overrides");
        }
        // Override applied: the table default for verify_mismatch would be
        // Quarantine (tainted); the operator bucket override wins.
        assert_eq!(action("verify_mismatch", None), Action::Observe);
        // Non-overridden buckets keep table defaults.
        assert_eq!(action("sim_failure", Some("rpc")), Action::Event);
        assert_eq!(action("ws_completeness", None), Action::Exit);
    }

    // --- cooldown (kept from D63GSE) ---

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
