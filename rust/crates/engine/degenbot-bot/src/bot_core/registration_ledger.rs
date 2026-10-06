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

use std::collections::{BTreeMap, BTreeSet, HashMap};

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

// ── The registration unit contract ──────────────────────────────────────────
//
// The per-path unit outcome, its summary counters, and the fold that relates
// them — the contract the pure-Rust driver and the Python registration
// pipeline both construct and assert against, beside the vocabulary and
// memos above.

/// The reason label one unit outcome records in the report's outcome
/// breakdown.
///
/// A bounded [`RegistrationOutcome`] tag is the normal label and the only
/// kind the V4 admission counters respond to; a driver may also record a
/// free-form label (an exception-derived tag it holds), which the fold
/// records verbatim and nothing else responds to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutcomeLabel {
    /// A bounded vocabulary tag.
    Vocabulary(RegistrationOutcome),
    /// A free-form driver label, recorded verbatim.
    FreeForm(String),
}

impl OutcomeLabel {
    /// The string the breakdown records.
    #[must_use]
    pub fn as_label(&self) -> &str {
        match self {
            Self::Vocabulary(outcome) => outcome.as_str(),
            Self::FreeForm(label) => label,
        }
    }

    /// The label a driver tag names: its vocabulary member, or the tag
    /// itself as a free-form label. An unknown tag is never refused here —
    /// drivers hold exception-derived tags the vocabulary does not name.
    #[must_use]
    pub fn from_driver_tag(tag: &str) -> Self {
        RegistrationOutcome::from_tag(tag)
            .map_or_else(|| Self::FreeForm(tag.to_owned()), Self::Vocabulary)
    }
}

/// The per-path registration unit outcome — the one typed definition both
/// drivers construct and fold.
///
/// One unit = one candidate path = one terminal outcome. The unit-sequence
/// contract both drivers implement:
///
/// 1. a unit's preparation and registration stages answer EXACTLY ONE
///    outcome: a benign skip, a counted rejection, the benign cap stop, a
///    transient register failure, or a completed registration;
/// 2. the driver folds that outcome exactly once, through
///    [`PipelineReport::absorb`], before the next unit starts — the fold is
///    the only counter mutation, and the driver's own folded-unit count must
///    equal [`PipelineReport::units_folded`] when the crawl ends. A repeated
///    fold double-counts the unit; a dropped fold loses it — both surface as
///    a `units_folded` mismatch;
/// 3. the fold is total (every variant lands in a counter) and
///    order-independent (counters commute; the cap latches).
///
/// Units may run concurrently on fleet seats: they never mutate a report —
/// they return one of these, and the single-loop driver folds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistrationUnitOutcome {
    /// A benign build/direction/admission skip.
    Skip {
        /// The reason label the breakdown records — a bounded refusal (the
        /// normal case, and the only kind the V4 admission counters respond
        /// to) or the driver's own free-form tag.
        label: OutcomeLabel,
        /// Whether the skip adds to `skip_count` (V4 admission refusals do
        /// not — they carry their own counters).
        counts_as_skip: bool,
        /// Log-only failure detail.
        detail: Option<String>,
    },
    /// A counted engine rejection, folding the `engine_reject_count` +
    /// `other_exc_count` pair. A rejection is NOT a skip: it records no
    /// breakdown entry (the counts carry it; the detail rides for the
    /// driver's log line).
    Reject {
        /// Log-only detail.
        detail: Option<String>,
    },
    /// The benign registered-path-cap stop.
    Cap,
    /// A transient register failure — deliberately never memoized (a raced
    /// build or an RPC blip must stay retryable).
    RegisterFailed {
        /// Log-only failure detail.
        detail: Option<String>,
    },
    /// A completed registration (`created == false` = the engine's signature
    /// dedup answered an existing path).
    Registered {
        /// Whether a NEW path id was created.
        created: bool,
        /// V4 hops that reached the registration stage — the
        /// `v4_pool_count` witness, folded on this variant only (counted on
        /// created AND duplicate outcomes).
        v4_hops: usize,
    },
}

impl RegistrationUnitOutcome {
    /// The closed unit-kind set, in declaration order — the vocabulary a
    /// cross-language consumer builds its kind labels from.
    pub const KINDS: [&'static str; 5] = ["skip", "reject", "cap", "register-fail", "registered"];
}

/// The registration summary counters — the one definition of how unit
/// outcomes accumulate.
///
/// The numeric fields, `capped`, the breakdown, and the fold witnesses are
/// mutated ONLY by [`PipelineReport::absorb`]. `candidates`,
/// `direction_errors`, `policy_rejected`, and `token_filter_count` are
/// driver-loop witnesses (a driver's own coordination stages maintain them;
/// the fold never touches them).
///
/// The fold identities, pinned by this module's tests and assertable via
/// [`PipelineReport::fold_identities_hold`]:
///
/// - every folded unit lands in exactly one terminal bucket:
///   `units_folded == path_count + dup_count + engine_reject_count + register_fail_count + skip_count + uncounted_skip_count`;
/// - the reject pair folds together: `other_exc_count == engine_reject_count`;
/// - the cap stop is a skip: `cap_skip_count <= skip_count`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PipelineReport {
    /// New paths registered.
    pub path_count: usize,
    /// Total skips, including the cap stop.
    pub skip_count: usize,
    /// Benign post-cap skips (a subset of `skip_count`).
    pub cap_skip_count: usize,
    /// Token-filtered paths (driver policy; the fold never touches it).
    pub token_filter_count: usize,
    /// Counted engine rejections.
    pub engine_reject_count: usize,
    /// Engine-dedup duplicates.
    pub dup_count: usize,
    /// Transient register failures.
    pub register_fail_count: usize,
    /// V4 hops that reached the registration stage (folded on `Registered`
    /// only — counted on created AND duplicate outcomes).
    pub v4_pool_count: usize,
    /// V4 hook rejections.
    pub v4_hook_rejected: usize,
    /// V4 dynamic-fee rejections.
    pub v4_dynamic_fee_rejected: usize,
    /// The reject pair's second counter (folds with `engine_reject_count`).
    pub other_exc_count: usize,
    /// Units folded so far — the one-fold-per-unit witness.
    pub units_folded: usize,
    /// Skips folded with `counts_as_skip == false` (they carry their own
    /// family counters, not `skip_count`).
    pub uncounted_skip_count: usize,
    /// Paths that passed the policy gate (driver-loop maintained).
    pub candidates: usize,
    /// Paths that failed direction resolution (driver-loop maintained).
    pub direction_errors: usize,
    /// Paths rejected by the driver policy (driver-loop maintained).
    pub policy_rejected: usize,
    /// Whether the benign registered-path cap was hit (latched).
    pub capped: bool,
    /// Reason-tagged outcome breakdown.
    pub skip_reasons: BTreeMap<String, usize>,
}

impl PipelineReport {
    /// Fold one unit outcome into the report — the one outcome-counter fold.
    pub fn absorb(&mut self, outcome: &RegistrationUnitOutcome) {
        self.units_folded += 1;
        match outcome {
            RegistrationUnitOutcome::Skip {
                label,
                counts_as_skip,
                ..
            } => {
                if *counts_as_skip {
                    self.skip_count += 1;
                } else {
                    self.uncounted_skip_count += 1;
                }
                match label {
                    OutcomeLabel::Vocabulary(RegistrationOutcome::V4HookRejected) => {
                        self.v4_hook_rejected += 1;
                    }
                    OutcomeLabel::Vocabulary(RegistrationOutcome::V4DynamicFeeRejected) => {
                        self.v4_dynamic_fee_rejected += 1;
                    }
                    _ => {}
                }
                *self
                    .skip_reasons
                    .entry(label.as_label().to_owned())
                    .or_insert(0) += 1;
            }
            RegistrationUnitOutcome::Reject { .. } => {
                self.engine_reject_count += 1;
                self.other_exc_count += 1;
            }
            RegistrationUnitOutcome::Cap => {
                self.skip_count += 1;
                self.cap_skip_count += 1;
                self.capped = true;
                *self
                    .skip_reasons
                    .entry(RegistrationOutcome::PathCap.as_str().to_owned())
                    .or_insert(0) += 1;
            }
            RegistrationUnitOutcome::RegisterFailed { .. } => {
                self.register_fail_count += 1;
                *self
                    .skip_reasons
                    .entry(RegistrationOutcome::RegisterFailed.as_str().to_owned())
                    .or_insert(0) += 1;
            }
            RegistrationUnitOutcome::Registered { created, v4_hops } => {
                self.v4_pool_count += *v4_hops;
                if *created {
                    self.path_count += 1;
                } else {
                    self.dup_count += 1;
                    *self
                        .skip_reasons
                        .entry(RegistrationOutcome::Dup.as_str().to_owned())
                        .or_insert(0) += 1;
                }
            }
        }
    }

    /// Whether the fold identities hold. They hold by construction after any
    /// sequence of folds; a violation means counters were mutated outside
    /// the fold.
    #[must_use]
    pub fn fold_identities_hold(&self) -> bool {
        self.other_exc_count == self.engine_reject_count
            && self.cap_skip_count <= self.skip_count
            && self.units_folded
                == self.path_count
                    + self.dup_count
                    + self.engine_reject_count
                    + self.register_fail_count
                    + self.skip_count
                    + self.uncounted_skip_count
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

    /// The unit-kind set is a cross-language contract, so it is pinned like
    /// the outcome tag set: a rename must be a deliberate edit here and in
    /// the Python adapter, never an accident.
    #[test]
    fn unit_kind_set_is_closed_and_unique() {
        assert_eq!(
            RegistrationUnitOutcome::KINDS,
            ["skip", "reject", "cap", "register-fail", "registered"]
        );
    }

    /// The fold arithmetic per outcome shape — the counter-parity bar both
    /// drivers inherit. Each arm lands in exactly one terminal bucket (or an
    /// uncounted skip), the V4 admission refusals carry their own counters
    /// and no `skip_count`, and the V4-hop witness folds on registered
    /// outcomes only (created AND duplicate).
    #[test]
    fn fold_lands_every_outcome_in_its_counter_bucket() {
        let mut report = PipelineReport::default();

        report.absorb(&RegistrationUnitOutcome::Skip {
            label: OutcomeLabel::Vocabulary(RegistrationOutcome::BuildV3Refused),
            counts_as_skip: true,
            detail: Some("boom".to_owned()),
        });
        assert_eq!(report.skip_count, 1);
        assert_eq!(report.skip_reasons["build-v3-refused"], 1);

        report.absorb(&RegistrationUnitOutcome::Skip {
            label: OutcomeLabel::Vocabulary(RegistrationOutcome::V4HookRejected),
            counts_as_skip: false,
            detail: None,
        });
        assert_eq!(report.v4_hook_rejected, 1);
        assert_eq!(report.skip_count, 1, "V4 admission refusals are not skips");
        assert_eq!(report.uncounted_skip_count, 1);
        assert_eq!(report.skip_reasons["v4-hook-rejected"], 1);

        report.absorb(&RegistrationUnitOutcome::Reject { detail: None });
        assert_eq!(report.engine_reject_count, 1);
        assert_eq!(report.other_exc_count, 1);
        assert!(
            !report.skip_reasons.contains_key("path-rejected-memo"),
            "a rejection is not a skip: no breakdown entry"
        );

        report.absorb(&RegistrationUnitOutcome::RegisterFailed { detail: None });
        assert_eq!(report.register_fail_count, 1);
        assert_eq!(report.skip_reasons["register-fail"], 1);

        report.absorb(&RegistrationUnitOutcome::Registered {
            created: true,
            v4_hops: 2,
        });
        assert_eq!(report.path_count, 1);
        assert_eq!(report.v4_pool_count, 2);

        report.absorb(&RegistrationUnitOutcome::Registered {
            created: false,
            v4_hops: 3,
        });
        assert_eq!(report.dup_count, 1);
        assert_eq!(report.v4_pool_count, 5, "the witness counts dups too");
        assert_eq!(report.skip_reasons["dup"], 1);

        report.absorb(&RegistrationUnitOutcome::Cap);
        assert!(report.capped);
        assert_eq!(report.skip_count, 2);
        assert_eq!(report.cap_skip_count, 1);
        assert_eq!(report.skip_reasons["path-cap"], 1);

        assert_eq!(report.units_folded, 7);
        assert!(
            report.fold_identities_hold(),
            "every fold landed in exactly one bucket"
        );

        // A free-form driver label is recorded verbatim; no family counter
        // responds to it.
        report.absorb(&RegistrationUnitOutcome::Skip {
            label: OutcomeLabel::FreeForm("build-v3:ConnectionError".to_owned()),
            counts_as_skip: true,
            detail: None,
        });
        assert_eq!(report.skip_reasons["build-v3:ConnectionError"], 1);
        assert_eq!(report.v4_hook_rejected, 1);
        assert!(report.fold_identities_hold());
    }

    /// The fold is order-independent: the same outcome multiset folds to the
    /// same report whatever order the units resolve in (the cap latches
    /// either way).
    #[test]
    fn fold_is_order_independent() {
        let outcomes = [
            RegistrationUnitOutcome::Skip {
                label: OutcomeLabel::Vocabulary(RegistrationOutcome::V4NoHash),
                counts_as_skip: true,
                detail: None,
            },
            RegistrationUnitOutcome::Reject { detail: None },
            RegistrationUnitOutcome::Registered {
                created: true,
                v4_hops: 1,
            },
            RegistrationUnitOutcome::Registered {
                created: false,
                v4_hops: 0,
            },
            RegistrationUnitOutcome::RegisterFailed { detail: None },
            RegistrationUnitOutcome::Cap,
        ];

        let mut forward = PipelineReport::default();
        for outcome in &outcomes {
            forward.absorb(outcome);
        }
        let mut reverse = PipelineReport::default();
        for outcome in outcomes.iter().rev() {
            reverse.absorb(outcome);
        }
        assert_eq!(forward, reverse, "counters commute; the cap latches");
        assert!(forward.fold_identities_hold());
        assert!(reverse.fold_identities_hold());
    }

    /// What a repeated fold means: the unit double-counts, and the
    /// one-fold-per-unit witness shows it (a driver that folds one outcome
    /// per unit ends with `units_folded` equal to its own unit count).
    #[test]
    fn a_repeated_fold_double_counts_and_shows_in_units_folded() {
        let outcome = RegistrationUnitOutcome::Registered {
            created: true,
            v4_hops: 0,
        };
        let mut report = PipelineReport::default();
        report.absorb(&outcome);
        report.absorb(&outcome);
        assert_eq!(report.units_folded, 2);
        assert_eq!(report.path_count, 2, "the same unit counted twice");
        assert!(
            report.fold_identities_hold(),
            "the identities stay consistent even across a driver's double fold"
        );
    }
}
