//! The registration-outcome ledger — the Rust twin of
//! `src/degenbot/runner/_registration_ledger.py` (parity-ledger row 9,
//! ergo `XFEJUG`).
//!
//! Owns the four memos the Python pipeline carries:
//!
//! - `registered_paths`: hop signatures already answered by a completed
//!   registration (the dup fast-path in front of verify).
//! - `verified_pools`: pools whose verify lifecycle COMPLETED (a pool fact).
//! - `unregistrable_pools`: STABLE typed build refusals (a pool fact).
//! - `rejected_paths`: deterministic policy/predicate denies.
//!
//! TRANSIENT build/register failures are deliberately never memoized.

use std::collections::{BTreeSet, HashMap};

use degenbot::pathfinding::PoolKind;

/// A path's hop signature: `(engine/BotState pool_id, zero_for_one)` per hop.
pub type HopSignature = Vec<(u64, bool)>;

/// The bounded outcome vocabulary (mirrors `RegistrationOutcome`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegistrationOutcome {
    /// A completed registration (`created == true`).
    Registered,
    /// The engine signature dedup answered an existing path.
    Dup,
    /// The benign registered-path-cap stop.
    PathCap,
    /// Operator-pinned directions disagree with the resolved path length.
    DirectionMismatch,
    /// A hop whose table type is not V2/V3/V4.
    UnknownPoolType,
    /// A V4 hop with no `pool_hash`.
    V4NoHash,
    /// V4 hooked-pool admission refusal.
    V4HookRejected,
    /// V4 dynamic-fee admission refusal.
    V4DynamicFeeRejected,
    /// A deterministic policy/predicate deny.
    PathRejected,
    /// A V2 hop build refusal.
    BuildV2Refused,
    /// A V3 hop build refusal.
    BuildV3Refused,
    /// A V4 hop build refusal.
    BuildV4Refused,
    /// A transient register failure.
    RegisterFailed,
}

impl RegistrationOutcome {
    /// The bounded metric/log tag string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Dup => "dup",
            Self::PathCap => "path-cap",
            Self::DirectionMismatch => "direction-mismatch",
            Self::UnknownPoolType => "unknown-pool-type",
            Self::V4NoHash => "v4-no-hash",
            Self::V4HookRejected => "v4-hook-rejected",
            Self::V4DynamicFeeRejected => "v4-dynamic-fee-rejected",
            Self::PathRejected => "path-rejected-memo",
            Self::BuildV2Refused => "build-v2-refused",
            Self::BuildV3Refused => "build-v3-refused",
            Self::BuildV4Refused => "build-v4-refused",
            Self::RegisterFailed => "register-fail",
        }
    }

    /// The family-specific build-refusal tag (mirrors `_BUILD_REFUSED`;
    /// the Python default fallback is `BUILD_V3_REFUSED`).
    #[must_use]
    pub const fn build_refused_for(kind: PoolKind) -> Self {
        match kind {
            PoolKind::V2 => Self::BuildV2Refused,
            PoolKind::V4 => Self::BuildV4Refused,
            _ => Self::BuildV3Refused,
        }
    }
}

/// Driver-side build failure taxonomy, mapped to the ledger's typed
/// classification (the Rust analogue of the Python exception types).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildFailure {
    /// `RegisterV4PoolError::HookedPool` — reserved admission refusal.
    HookedPool,
    /// `RegisterV4PoolError::DynamicFee`.
    DynamicFee,
    /// `RegisterV4PoolError::FeeExceedsEncoderLimit` (and the V2/V3
    /// high-fee analogue) — a stable pool fact.
    HighFee,
    /// Any other failure: transient, never memoized.
    Transient(String),
}

/// The typed classification of one hop-build exception.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildRefusal {
    /// The bounded outcome tag.
    pub outcome: RegistrationOutcome,
    /// A stable refusal is a pool fact (memoize the pool); a transient
    /// failure stays retryable.
    pub stable: bool,
    /// V4 admission refusals carry their own counters and do not add to
    /// `skip_count`.
    pub counts_as_skip: bool,
    /// The exception text (log-only).
    pub detail: Option<String>,
}

/// A memoized stable refusal of one pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnregistrableRecord {
    /// The bounded tag.
    pub outcome: RegistrationOutcome,
    /// Whether the refusal counts as a skip.
    pub counts_as_skip: bool,
}

/// The four registration memos + the typed build-refusal classification.
#[derive(Debug, Default)]
pub struct RegistrationLedger {
    registered_paths: BTreeSet<HopSignature>,
    verified_pools: BTreeSet<String>,
    unregistrable_pools: HashMap<String, UnregistrableRecord>,
    rejected_paths: BTreeSet<HopSignature>,
}

impl RegistrationLedger {
    /// Hop identity the negative memos key on — known BEFORE any build
    /// (mirrors `pool_memo_key`).
    ///
    /// V2/V3 key off the pool address; V4 off the `pool_hash`. `None` = not
    /// memoizable (no identity on this hop).
    #[must_use]
    pub fn pool_memo_key(
        kind: PoolKind,
        address: Option<&str>,
        pool_hash: Option<&str>,
    ) -> Option<String> {
        match kind {
            PoolKind::V4 => pool_hash.map(|h| format!("v4id:{}", h.to_lowercase())),
            _ => address.map(|a| format!("p:{}", a.to_lowercase())),
        }
    }

    /// Classify a hop-build failure by TYPE (never by class name).
    #[must_use]
    pub fn classify_build_refusal(
        failure: &BuildFailure,
        pool_kind: PoolKind,
        detail: Option<String>,
    ) -> BuildRefusal {
        match failure {
            BuildFailure::HookedPool => BuildRefusal {
                outcome: RegistrationOutcome::V4HookRejected,
                stable: true,
                counts_as_skip: false,
                detail,
            },
            BuildFailure::DynamicFee => BuildRefusal {
                outcome: RegistrationOutcome::V4DynamicFeeRejected,
                stable: true,
                counts_as_skip: false,
                detail,
            },
            BuildFailure::HighFee => BuildRefusal {
                outcome: RegistrationOutcome::build_refused_for(pool_kind),
                stable: true,
                counts_as_skip: true,
                detail,
            },
            BuildFailure::Transient(_) => BuildRefusal {
                outcome: RegistrationOutcome::build_refused_for(pool_kind),
                stable: false,
                counts_as_skip: true,
                detail,
            },
        }
    }

    /// True when this exact hop signature already completed registration.
    #[must_use]
    pub fn path_registered(&self, hop_sig: &[(u64, bool)]) -> bool {
        self.registered_paths.contains(hop_sig)
    }

    /// Record a completed registration (engine-created or engine-dedup'd).
    pub fn memoize_registered_path(&mut self, hop_sig: HopSignature) {
        self.registered_paths.insert(hop_sig);
    }

    /// True when this pool's verify lifecycle already completed.
    #[must_use]
    pub fn pool_verified(&self, key: &str) -> bool {
        self.verified_pools.contains(key)
    }

    /// Record a COMPLETED verify lifecycle (a pool fact).
    pub fn memoize_verified_pool(&mut self, key: String) {
        self.verified_pools.insert(key);
    }

    /// The memoized stable refusal for a pool key, or `None`.
    #[must_use]
    pub fn unregistrable_record(&self, key: Option<&str>) -> Option<&UnregistrableRecord> {
        key.and_then(|k| self.unregistrable_pools.get(k))
    }

    /// Record a STABLE build refusal (`setdefault` — first tag wins).
    pub fn memoize_unregistrable(
        &mut self,
        key: Option<&str>,
        outcome: RegistrationOutcome,
        counts_as_skip: bool,
    ) {
        if let Some(k) = key {
            self.unregistrable_pools
                .entry(k.to_string())
                .or_insert(UnregistrableRecord {
                    outcome,
                    counts_as_skip,
                });
        }
    }

    /// True when this hop signature already hit a deterministic deny.
    #[must_use]
    pub fn path_rejected(&self, hop_sig: &[(u64, bool)]) -> bool {
        self.rejected_paths.contains(hop_sig)
    }

    /// Record a deterministic policy/predicate deny for a hop signature.
    pub fn memoize_rejected_path(&mut self, hop_sig: HopSignature) {
        self.rejected_paths.insert(hop_sig);
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
mod tests {
    use super::*;

    #[test]
    fn pool_memo_key_shapes() {
        assert_eq!(
            RegistrationLedger::pool_memo_key(PoolKind::V3, Some("0xAbC"), None),
            Some("p:0xabc".to_string())
        );
        assert_eq!(
            RegistrationLedger::pool_memo_key(PoolKind::V4, None, Some("0xDEAD")),
            Some("v4id:0xdead".to_string())
        );
        assert_eq!(
            RegistrationLedger::pool_memo_key(PoolKind::V4, None, None),
            None
        );
    }

    #[test]
    fn classify_build_refusal_types() {
        let hooked = RegistrationLedger::classify_build_refusal(
            &BuildFailure::HookedPool,
            PoolKind::V4,
            Some("hooked".to_string()),
        );
        assert_eq!(hooked.outcome, RegistrationOutcome::V4HookRejected);
        assert!(hooked.stable);
        assert!(!hooked.counts_as_skip);

        let dynamic = RegistrationLedger::classify_build_refusal(
            &BuildFailure::DynamicFee,
            PoolKind::V4,
            None,
        );
        assert_eq!(dynamic.outcome, RegistrationOutcome::V4DynamicFeeRejected);
        assert!(dynamic.stable);
        assert!(!dynamic.counts_as_skip);

        let high =
            RegistrationLedger::classify_build_refusal(&BuildFailure::HighFee, PoolKind::V3, None);
        assert_eq!(high.outcome, RegistrationOutcome::BuildV3Refused);
        assert!(high.stable);
        assert!(high.counts_as_skip);

        let transient = RegistrationLedger::classify_build_refusal(
            &BuildFailure::Transient("blip".to_string()),
            PoolKind::V2,
            Some("blip".to_string()),
        );
        assert_eq!(transient.outcome, RegistrationOutcome::BuildV2Refused);
        assert!(!transient.stable);
    }

    #[test]
    fn memos_roundtrip_and_first_refusal_wins() {
        let mut ledger = RegistrationLedger::default();
        assert!(!ledger.path_registered(&[(1, true)]));
        ledger.memoize_registered_path(vec![(1, true)]);
        assert!(ledger.path_registered(&[(1, true)]));

        assert!(!ledger.pool_verified("v3:0x1"));
        ledger.memoize_verified_pool("v3:0x1".to_string());
        assert!(ledger.pool_verified("v3:0x1"));

        ledger.memoize_unregistrable(Some("p:0x1"), RegistrationOutcome::BuildV3Refused, true);
        ledger.memoize_unregistrable(Some("p:0x1"), RegistrationOutcome::V4HookRejected, false);
        let record = ledger.unregistrable_record(Some("p:0x1")).unwrap();
        assert_eq!(record.outcome, RegistrationOutcome::BuildV3Refused);
        assert!(record.counts_as_skip);
        assert!(ledger.unregistrable_record(None).is_none());

        assert!(!ledger.path_rejected(&[(2, false)]));
        ledger.memoize_rejected_path(vec![(2, false)]);
        assert!(ledger.path_rejected(&[(2, false)]));
    }
}
