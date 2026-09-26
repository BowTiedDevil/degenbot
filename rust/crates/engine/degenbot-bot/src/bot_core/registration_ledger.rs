//! The registration outcome ledger — the ONE owner of the registration
//! outcome vocabulary and its negative memos.
//!
//! A registration unit ends in exactly one of a small set of outcomes, and
//! the deployment layer needs that set closed: it feeds bounded metric tags,
//! skip counters, and per-pool negative memoization. Keeping the vocabulary
//! in one owner is what makes it checkable — the tag set is pinned by a test
//! here, and every consumer (the pure-Rust driver, the Python registration
//! pipeline) reads these same values rather than re-declaring its own.
//!
//! # The four memos
//!
//! - **registered paths** — hop signatures already answered by a completed
//!   registration (the dup fast-path in front of verify).
//! - **verified pools** — pools whose verify lifecycle COMPLETED (a pool
//!   fact, for the pipeline's own lifetime).
//! - **unregistrable pools** — STABLE typed build refusals (a pool fact: no
//!   candidate path through that pool can register).
//! - **rejected paths** — deterministic policy/predicate denies, per hop
//!   signature.
//!
//! TRANSIENT build/register failures are deliberately never memoized — a
//! raced build or an RPC blip must stay retryable.
//!
//! # Why the taxonomy is a type, not a string
//!
//! Every arm is a variant with a stable tag, so a consumer branches on the
//! variant and never parses a message. The `V4` admission refusals (hook /
//! dynamic fee) carry their own counters and do not add to the generic skip
//! counter, which is what `counts_as_skip` records.

use std::collections::{BTreeSet, HashMap};

use degenbot_pathfinding::PoolKind;
use degenbot_pools::v4_state::RegisterV4PoolError;

/// A path's hop signature: `(pool_id, zero_for_one)` per hop.
pub type HopSignature = Vec<(u64, bool)>;

/// The bounded registration-outcome vocabulary.
///
/// The tag strings are the cross-language contract: the Python registration
/// pipeline builds its own label enum from this list, so a tag can never
/// drift between the core and a driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    /// A V4 hop with no on-chain pool id.
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

/// Every outcome, in declaration order — the vocabulary a cross-language
/// consumer enumerates.
pub const REGISTRATION_OUTCOMES: [RegistrationOutcome; 13] = [
    RegistrationOutcome::Registered,
    RegistrationOutcome::Dup,
    RegistrationOutcome::PathCap,
    RegistrationOutcome::DirectionMismatch,
    RegistrationOutcome::UnknownPoolType,
    RegistrationOutcome::V4NoHash,
    RegistrationOutcome::V4HookRejected,
    RegistrationOutcome::V4DynamicFeeRejected,
    RegistrationOutcome::PathRejected,
    RegistrationOutcome::BuildV2Refused,
    RegistrationOutcome::BuildV3Refused,
    RegistrationOutcome::BuildV4Refused,
    RegistrationOutcome::RegisterFailed,
];

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

    /// The bounded tag of every outcome, for a cross-language consumer that
    /// builds its label set from this vocabulary.
    #[must_use]
    pub fn tags() -> Vec<&'static str> {
        REGISTRATION_OUTCOMES
            .iter()
            .map(|outcome| outcome.as_str())
            .collect()
    }

    /// The outcome carrying `tag`, if the vocabulary names it.
    ///
    /// The read direction for a consumer that receives a tag as data (a
    /// config value, a recorded metric) rather than naming a variant.
    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        REGISTRATION_OUTCOMES
            .into_iter()
            .find(|outcome| outcome.as_str() == tag)
    }

    /// The family-specific build-refusal tag. The fallback is the V3 tag: a
    /// build refusal for a family outside the modelled three still has to
    /// report something bounded.
    #[must_use]
    pub const fn build_refused_for(kind: PoolKind) -> Self {
        match kind {
            PoolKind::V2 => Self::BuildV2Refused,
            PoolKind::V4 => Self::BuildV4Refused,
            _ => Self::BuildV3Refused,
        }
    }
}

/// The build failure a caller observed, before classification.
///
/// The variants are the *typed* refusals a registration can hit; anything
/// the caller cannot name is [`BuildFailure::Transient`] and stays
/// retryable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildFailure {
    /// The V4 hooked-pool admission refusal.
    HookedPool,
    /// The V4 dynamic-fee admission refusal.
    DynamicFee,
    /// A fee above the encoder's limit — a stable pool fact for every family.
    HighFee,
    /// Any other failure: transient, never memoized.
    Transient(String),
}

impl From<RegisterV4PoolError> for BuildFailure {
    /// Classify a V4 admission refusal by TYPE. The hook and dynamic-fee
    /// categories carry their own counters (`counts_as_skip == false`); a fee
    /// above the encoder limit is a stable pool fact. `AlreadyRegistered` is a
    /// wiring race, not a pool fact, so it stays retryable.
    fn from(err: RegisterV4PoolError) -> Self {
        match err {
            RegisterV4PoolError::DynamicFee { .. } => Self::DynamicFee,
            RegisterV4PoolError::HookedPool { .. } => Self::HookedPool,
            RegisterV4PoolError::FeeExceedsEncoderLimit { .. }
            | RegisterV4PoolError::SpecViolation(_) => Self::HighFee,
            other @ RegisterV4PoolError::AlreadyRegistered { .. } => {
                Self::Transient(format!("{other:?}"))
            }
        }
    }
}

/// The typed classification of one hop-build failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildRefusal {
    /// The bounded outcome tag.
    pub outcome: RegistrationOutcome,
    /// A stable refusal is a pool fact (memoize the pool); a transient
    /// failure stays retryable.
    pub stable: bool,
    /// Whether the refusal adds to the generic skip counter. The V4
    /// admission refusals carry their own counters and do not.
    pub counts_as_skip: bool,
    /// The failure text (log-only, never a label).
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

/// The four registration memos plus the typed build-refusal classification.
#[derive(Debug, Default)]
pub struct RegistrationLedger {
    registered_paths: BTreeSet<HopSignature>,
    verified_pools: BTreeSet<String>,
    unregistrable_pools: HashMap<String, UnregistrableRecord>,
    rejected_paths: BTreeSet<HopSignature>,
}

impl RegistrationLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The hop identity the negative memos key on — known BEFORE any build.
    ///
    /// V2/V3 key off the pool address; V4 off the on-chain pool id, which the
    /// discovery edge carries pre-build, so a refused pool is recognizable
    /// without an RPC. `None` means the hop carries no identity and is
    /// therefore not memoizable.
    #[must_use]
    pub fn pool_memo_key(
        kind: PoolKind,
        address: Option<&str>,
        pool_hash: Option<&str>,
    ) -> Option<String> {
        match kind {
            PoolKind::V4 => pool_hash.map(|hash| format!("v4id:{}", hash.to_lowercase())),
            _ => address.map(|address| format!("p:{}", address.to_lowercase())),
        }
    }

    /// Classify a hop-build failure by TYPE, never by message or class name.
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

    /// Whether this exact hop signature already completed registration.
    #[must_use]
    pub fn path_registered(&self, hop_sig: &[(u64, bool)]) -> bool {
        self.registered_paths.contains(hop_sig)
    }

    /// Record a completed registration (engine-created or engine-dedup'd).
    pub fn memoize_registered_path(&mut self, hop_sig: HopSignature) {
        self.registered_paths.insert(hop_sig);
    }

    /// Whether this pool's verify lifecycle already completed.
    #[must_use]
    pub fn pool_verified(&self, key: &str) -> bool {
        self.verified_pools.contains(key)
    }

    /// Record a COMPLETED verify lifecycle (a pool fact).
    pub fn memoize_verified_pool(&mut self, key: String) {
        self.verified_pools.insert(key);
    }

    /// How many pools have a completed verify-lifecycle fact (test/diagnostic
    /// witness).
    #[must_use]
    pub fn verified_pool_count(&self) -> usize {
        self.verified_pools.len()
    }

    /// The memoized stable refusal for a pool key, or `None`.
    #[must_use]
    pub fn unregistrable_record(&self, key: Option<&str>) -> Option<&UnregistrableRecord> {
        key.and_then(|key| self.unregistrable_pools.get(key))
    }

    /// Record a STABLE build refusal (`setdefault` — the first tag wins, so a
    /// later transient answer cannot overwrite the pool fact).
    pub fn memoize_unregistrable(
        &mut self,
        key: Option<&str>,
        outcome: RegistrationOutcome,
        counts_as_skip: bool,
    ) {
        if let Some(key) = key {
            self.unregistrable_pools
                .entry(key.to_string())
                .or_insert(UnregistrableRecord {
                    outcome,
                    counts_as_skip,
                });
        }
    }

    /// Whether this hop signature already hit a deterministic deny.
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

    /// The vocabulary is a cross-language contract, so its exact tag set is
    /// pinned: a rename must be a deliberate edit here and in the Python
    /// adapter's parity test, never an accident.
    #[test]
    fn outcome_tag_set_is_closed_and_unique() {
        assert_eq!(
            RegistrationOutcome::tags(),
            vec![
                "registered",
                "dup",
                "path-cap",
                "direction-mismatch",
                "unknown-pool-type",
                "v4-no-hash",
                "v4-hook-rejected",
                "v4-dynamic-fee-rejected",
                "path-rejected-memo",
                "build-v2-refused",
                "build-v3-refused",
                "build-v4-refused",
                "register-fail",
            ]
        );
        for outcome in REGISTRATION_OUTCOMES {
            assert_eq!(
                RegistrationOutcome::from_tag(outcome.as_str()),
                Some(outcome),
                "every tag must round-trip back to its outcome"
            );
        }
        assert_eq!(RegistrationOutcome::from_tag("not-an-outcome"), None);
    }

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
        assert_eq!(
            RegistrationLedger::pool_memo_key(PoolKind::V2, None, None),
            None
        );
    }

    #[test]
    fn classify_build_refusal_by_type() {
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

        let high_fee =
            RegistrationLedger::classify_build_refusal(&BuildFailure::HighFee, PoolKind::V3, None);
        assert_eq!(high_fee.outcome, RegistrationOutcome::BuildV3Refused);
        assert!(high_fee.stable);
        assert!(high_fee.counts_as_skip);

        let transient = RegistrationLedger::classify_build_refusal(
            &BuildFailure::Transient("blip".to_string()),
            PoolKind::V2,
            Some("blip".to_string()),
        );
        assert_eq!(transient.outcome, RegistrationOutcome::BuildV2Refused);
        assert!(!transient.stable, "a transient failure is not a pool fact");
        assert!(transient.counts_as_skip);
    }

    /// The V4 admission refusal taxonomy has ONE home: the core maps the
    /// typed `RegisterV4PoolError` into `BuildFailure`, so the Python binding
    /// and the pure-Rust driver cannot classify one construction refusal two
    /// different ways.
    #[test]
    fn v4_admission_refusal_maps_to_one_build_failure_taxonomy() {
        use degenbot_pools::v4_state::RegisterV4PoolError;

        assert_eq!(
            BuildFailure::from(RegisterV4PoolError::HookedPool { hook_flags: 0xCC }),
            BuildFailure::HookedPool
        );
        assert_eq!(
            BuildFailure::from(RegisterV4PoolError::DynamicFee { fee: 0x10_0000 }),
            BuildFailure::DynamicFee
        );
        assert_eq!(
            BuildFailure::from(RegisterV4PoolError::FeeExceedsEncoderLimit { fee: 65_536 }),
            BuildFailure::HighFee
        );

        // The stable-fact classification is what decides memoization, so the
        // fee refusal must classify stable+skip through the shared classifier.
        let refusal = RegistrationLedger::classify_build_refusal(
            &BuildFailure::from(RegisterV4PoolError::FeeExceedsEncoderLimit { fee: 65_536 }),
            PoolKind::V4,
            None,
        );
        assert_eq!(refusal.outcome, RegistrationOutcome::BuildV4Refused);
        assert!(refusal.stable);
        assert!(refusal.counts_as_skip);
    }

    #[test]
    fn memos_roundtrip_and_first_refusal_wins() {
        let mut ledger = RegistrationLedger::new();
        assert!(!ledger.path_registered(&[(1, true)]));
        ledger.memoize_registered_path(vec![(1, true)]);
        assert!(ledger.path_registered(&[(1, true)]));
        assert!(
            !ledger.path_registered(&[(1, false)]),
            "orientation is part of the signature"
        );

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
