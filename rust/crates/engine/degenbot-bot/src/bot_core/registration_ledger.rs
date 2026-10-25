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

use std::borrow::Borrow;
use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::BTreeSet;

use alloy::primitives::Address;
use degenbot_pathfinding::PoolKind;
use degenbot_pools::sim::registration_refusal::SpecRefusal;
use degenbot_pools::state::aerodrome_v2_state::RegisterAerodromePoolError;
use degenbot_pools::state::v2_state::RegisterV2PoolError;
use degenbot_pools::state::v3_state::RegisterV3PoolError;
use degenbot_substrate::RegistrationVerdict;

pub use degenbot_pools::sim::registration_refusal::{BuildFailure, RegisterRefusal};
pub use degenbot_pools::sim::registry::RegisteredPoolFamily;

use super::pool_builder::builder::PoolBuilderError;
use super::pool_builder::parity::IdentityDivergence;
use super::pool_builder::route::ConstructionRefusal;

/// A path's hop signature: `(pool_id, zero_for_one)` per hop.
pub type HopSignature = Vec<(u64, bool)>;

/// The bounded registration-outcome vocabulary.
///
/// The tag strings are the cross-language contract: the Python registration
/// pipeline builds its own label enum from this list, so a tag can never
/// drift between the core and a driver.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    /// A refusal classified through the one refusal vocabulary (ADR-077).
    ///
    /// The carried [`RefusalKind`] holds the family/phase tags and the
    /// load-bearing classification. Its tag is a refusal-kind tag
    /// ([`REFUSAL_KIND_TAGS`]), not one of the bounded
    /// [`REGISTRATION_OUTCOMES`] tags, so it does not round-trip through
    /// [`RegistrationOutcome::from_tag`].
    Refused(RefusalKind),
}

/// Every BOUNDED outcome, in declaration order — the vocabulary a
/// cross-language consumer enumerates. The refusal arm's kind tags are the
/// separate [`REFUSAL_KIND_TAGS`] set.
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
    pub const fn as_str(&self) -> &'static str {
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
            Self::Refused(kind) => kind.tag(),
        }
    }

    /// The bounded tag of every outcome, for a cross-language consumer that
    /// builds its label set from this vocabulary.
    #[must_use]
    pub fn tags() -> Vec<&'static str> {
        REGISTRATION_OUTCOMES
            .iter()
            .map(RegistrationOutcome::as_str)
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

// ── The refusal vocabulary ──────────────────────────────────────────────────
//
// One closed vocabulary for every registration-phase refusal. Family and
// phase ride as DATA on a variant, never as forked variants per family, and
// the load-bearing classifications ride there too: the stable-vs-transient
// build-refusal taxonomy ([`BuildFailure`]) and the fatal-vs-transient verify
// split. The per-family register / build / lifecycle / verify enums are gone
// (ADR-077 D2): every throw site constructs a `RefusalKind`, and the FFI's
// one mapper renders it.

/// The phase of the registration pipeline a refusal arose in.
///
/// Phase rides as data on a [`RefusalKind`] variant, so one family's register
/// and build refusals are one variant carrying different tags rather than
/// forked variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RegistrationPhase {
    /// Intake: an operator-supplied address or list entry that would not
    /// parse.
    Intake,
    /// Identity resolution: the `CREATE2` / EIP-1167 recomputation and the
    /// minted-vs-registered parity guard.
    Identity,
    /// Pool construction: the builder run and the construction-route rung
    /// walk.
    Build,
    /// The registry insert (`BotState::register_*_pool`).
    Register,
    /// The snapshot store: loading or decoding a persisted snapshot.
    Snapshot,
    /// The liquidity-map verify tripwire.
    Verify,
    /// The verify-lifecycle orchestration around the verify steps.
    Lifecycle,
}

// The family tag a refusal carries is `RegisteredPoolFamily` — the one family
// tag, owned by `degenbot-pools` beside the pool registry, so the write door's
// verdict and this ledger's refusals cannot name a family differently. `None`
// on a [`RefusalKind`] means the refusal is family-agnostic (a malformed input
// line, a missing construction I/O).

/// The identity-parity refusal shape — the ONE home of the historical
/// identity-mismatch vocabularies.
///
/// The construction side owns identity parity: the register path's `CREATE2`
/// recomputation, the builder's `CREATE2` check, and the minted-vs-registered
/// parity guard all describe the same refusal — a recomputed or minted
/// identity that disagrees with the declared or registered one — and all
/// arrive here through the one [`IdentityDivergence`] verdict.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IdentityMismatchShape {
    /// A `CREATE2` / EIP-1167 recomputation disagreed with the declared
    /// address: the declared and recomputed addresses ride verbatim, and the
    /// full source rendering (chain, factory, deployer, init hash) rides in
    /// `detail`.
    Create2 {
        /// The declared (expected) pool address.
        declared: Address,
        /// The recomputed address.
        computed: Address,
        /// The source rendering (log-only, never a label).
        detail: String,
    },
    /// A freshly minted construction identity disagreed with the identity the
    /// registry of record holds — the field-by-field parity guard's
    /// divergence.
    MintedVsRegistered {
        /// The address the divergence concerns.
        address: Address,
        /// The field-by-field divergence (log-only).
        detail: String,
    },
}

/// Why a persisted snapshot could not be loaded.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SnapshotLoadReason {
    /// A DB read failed.
    Db {
        /// The source rendering (log-only).
        detail: String,
    },
    /// A hex / tick decode failed.
    Decode {
        /// The source rendering (log-only).
        detail: String,
    },
    /// A value exceeded the range the snapshot store can hold.
    OutOfRange {
        /// The source rendering (log-only).
        detail: String,
    },
}

/// The provenance of a verify failure.
///
/// The fatal-vs-transient classification rides beside it on the
/// [`RefusalKind::VerifyFailed`] variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum VerifyOrigin {
    /// A typed map divergence — the tripwire.
    Mismatch,
    /// A per-call RPC read / transport failure.
    Transport,
    /// Verify-provider construction failed.
    ProviderConstruction,
    /// The verify protocol was violated (`insert()` with no snapshot stream
    /// in progress).
    ProtocolViolation,
    /// A failure outside the named categories.
    Other,
}

/// Why a verify-lifecycle precondition was unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LifecyclePrecondition {
    /// A tracked pool reached a verify step with no provider configured.
    MissingVerifyProvider,
    /// The registered pool's immutable tick spacing was unavailable.
    MissingTickSpacing,
}

/// What a family-level refusal could not serve.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UnsupportedSubject {
    /// No construction-route rung serves this pool address.
    Pool {
        /// The pool address no rung served.
        address: Address,
    },
    /// The factory has no built-in DEX variant preset.
    Factory {
        /// The factory with no preset.
        factory: Address,
    },
    /// No identity selector answered at this address.
    UnknownIdentity {
        /// The address no selector answered for.
        address: Address,
    },
}

/// The ONE closed refusal vocabulary the registration outcome ledger owns.
///
/// Every registration-phase refusal is a variant here. Family and phase ride
/// as data on the variant; so do the load-bearing classifications — a build
/// refusal carries the [`BuildFailure`] taxonomy (stable pool fact vs
/// retryable failure), and a verify refusal carries `fatal` (the mismatch
/// tripwire vs a transient read).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RefusalKind {
    /// Intake: an operator-supplied address or list entry would not parse.
    MalformedInput {
        /// The pipeline phase.
        phase: RegistrationPhase,
        /// The rejected input, verbatim.
        input: String,
        /// The parser's rendering (log-only).
        detail: String,
    },
    /// Identity parity: a recomputed or minted identity disagreed with the
    /// declared or registered one.
    IdentityMismatch {
        /// The family the refusal concerns.
        family: Option<RegisteredPoolFamily>,
        /// The pipeline phase (always [`RegistrationPhase::Identity`]: this
        /// refusal IS identity resolution, whichever entry raised it).
        phase: RegistrationPhase,
        /// The divergence shape.
        shape: IdentityMismatchShape,
    },
    /// No `ConstructionIo` was attached, so no build could run.
    MissingConstructionIo {
        /// The family the refusal concerns.
        family: Option<RegisteredPoolFamily>,
        /// The pipeline phase.
        phase: RegistrationPhase,
    },
    /// A family-level stable refusal: no route rung serves the family, the
    /// factory has no preset, or no identity selector answered. Loud, never a
    /// benign skip.
    UnsupportedFamily {
        /// The family the refusal concerns.
        family: Option<RegisteredPoolFamily>,
        /// The pipeline phase.
        phase: RegistrationPhase,
        /// What could not be served.
        subject: UnsupportedSubject,
        /// The source rendering (log-only).
        detail: String,
    },
    /// A construction refusal that keeps skip semantics; the
    /// stable-vs-transient classification rides on the carried
    /// [`BuildFailure`].
    BuildRefused {
        /// The family the refusal concerns.
        family: Option<RegisteredPoolFamily>,
        /// The pipeline phase.
        phase: RegistrationPhase,
        /// The typed build failure.
        failure: BuildFailure,
    },
    /// The registry insert refused (already registered / out-of-spec / V4
    /// admission).
    RegisterRefused {
        /// The family the refusal concerns.
        family: Option<RegisteredPoolFamily>,
        /// The pipeline phase.
        phase: RegistrationPhase,
        /// The insert refusal.
        reason: RegisterRefusal,
    },
    /// The registry GET answered an address with no readable identity of the
    /// expected family.
    UnreadableIdentity {
        /// The family the refusal concerns.
        family: Option<RegisteredPoolFamily>,
        /// The pipeline phase.
        phase: RegistrationPhase,
        /// The address that answered without a readable identity.
        address: Address,
        /// The source rendering (log-only).
        detail: String,
    },
    /// The persisted snapshot could not be loaded or decoded.
    SnapshotLoadFailed {
        /// The pipeline phase.
        phase: RegistrationPhase,
        /// The load failure.
        reason: SnapshotLoadReason,
    },
    /// A verification failed. `fatal` is the load-bearing classification: a
    /// mismatch or a protocol violation is the tripwire (never auto-repair);
    /// a read / transport / provider-construction failure is transient.
    VerifyFailed {
        /// The family the refusal concerns.
        family: Option<RegisteredPoolFamily>,
        /// The pipeline phase.
        phase: RegistrationPhase,
        /// The fatal-vs-transient classification.
        fatal: bool,
        /// Where the failure came from.
        origin: VerifyOrigin,
        /// The source rendering (log-only).
        detail: String,
    },
    /// Verification was requested but its configuration was absent — a fatal
    /// misconfiguration: the bot must never operate on an unverified map.
    VerifyNotConfigured {
        /// The pipeline phase.
        phase: RegistrationPhase,
        /// The missing-configuration rendering (log-only).
        detail: String,
    },
    /// A verify-lifecycle precondition was unavailable.
    LifecyclePrecondition {
        /// The pipeline phase.
        phase: RegistrationPhase,
        /// The missing precondition.
        reason: LifecyclePrecondition,
    },
}

/// Every refusal-kind tag, in declaration order — the closed set a
/// cross-language consumer builds its refusal labels from.
///
/// The bounded [`REGISTRATION_OUTCOMES`] tag list stays the pre-cutover
/// outcome vocabulary; this list is the refusal arm's own.
pub const REFUSAL_KIND_TAGS: [&str; 11] = [
    "malformed-input",
    "identity-mismatch",
    "missing-construction-io",
    "unsupported-family",
    "build-refused",
    "register-refused",
    "unreadable-identity",
    "snapshot-load-failed",
    "verify-failed",
    "verify-not-configured",
    "lifecycle-precondition",
];

impl RefusalKind {
    /// The bounded tag of this refusal kind.
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::MalformedInput { .. } => "malformed-input",
            Self::IdentityMismatch { .. } => "identity-mismatch",
            Self::MissingConstructionIo { .. } => "missing-construction-io",
            Self::UnsupportedFamily { .. } => "unsupported-family",
            Self::BuildRefused { .. } => "build-refused",
            Self::RegisterRefused { .. } => "register-refused",
            Self::UnreadableIdentity { .. } => "unreadable-identity",
            Self::SnapshotLoadFailed { .. } => "snapshot-load-failed",
            Self::VerifyFailed { .. } => "verify-failed",
            Self::VerifyNotConfigured { .. } => "verify-not-configured",
            Self::LifecyclePrecondition { .. } => "lifecycle-precondition",
        }
    }

    /// Identity parity: the construction side's ONE divergence verdict — a
    /// `CREATE2` / EIP-1167 recomputation or the minted-vs-registered guard.
    /// The register path and the build path both carry the same verdict, so
    /// they land on the same shape for the same fact.
    #[must_use]
    pub fn from_identity_divergence(
        family: Option<RegisteredPoolFamily>,
        divergence: &IdentityDivergence,
    ) -> Self {
        let shape = match divergence {
            IdentityDivergence::Create2(mismatch) => IdentityMismatchShape::Create2 {
                declared: mismatch.expected,
                computed: mismatch.computed,
                detail: mismatch.to_string(),
            },
            IdentityDivergence::MintedVsRegistered { address, detail } => {
                IdentityMismatchShape::MintedVsRegistered {
                    address: *address,
                    detail: detail.clone(),
                }
            }
        };
        Self::IdentityMismatch {
            family,
            phase: RegistrationPhase::Identity,
            shape,
        }
    }

    /// Intake: an address (or list entry) that would not parse. `detail` is
    /// the parser's rendering (`Invalid address '<input>': <source>`).
    #[must_use]
    pub fn malformed_input(input: String, detail: String) -> Self {
        Self::MalformedInput {
            phase: RegistrationPhase::Intake,
            input,
            detail,
        }
    }

    /// The construction route's typed refusal.
    #[must_use]
    pub fn from_construction_refusal(
        refusal: &ConstructionRefusal,
        family: Option<RegisteredPoolFamily>,
    ) -> Self {
        match refusal {
            ConstructionRefusal::UnsupportedFamily { address, detail } => Self::UnsupportedFamily {
                family,
                phase: RegistrationPhase::Build,
                subject: UnsupportedSubject::Pool { address: *address },
                detail: detail.clone(),
            },
            ConstructionRefusal::Skipped(failure) => Self::BuildRefused {
                family,
                phase: RegistrationPhase::Build,
                failure: failure.clone(),
            },
        }
    }

    /// The builder's typed failure. The factory-identity class is the loud
    /// abort; every other builder failure is I/O-derived and stays a
    /// retriable skip on the one build-refusal taxonomy, carrying the facts
    /// the shell's builder-error vocabulary interpolates.
    #[must_use]
    pub fn from_builder_error(
        err: &PoolBuilderError,
        family: Option<RegisteredPoolFamily>,
        phase: RegistrationPhase,
    ) -> Self {
        match err {
            PoolBuilderError::Identity(divergence) => {
                Self::from_identity_divergence(family, divergence)
            }
            PoolBuilderError::UnknownVariant { factory } => Self::UnsupportedFamily {
                family,
                phase,
                subject: UnsupportedSubject::Factory { factory: *factory },
                detail: err.to_string(),
            },
            PoolBuilderError::UnknownPoolIdentity { address } => Self::UnsupportedFamily {
                family,
                phase,
                subject: UnsupportedSubject::UnknownIdentity { address: *address },
                detail: err.to_string(),
            },
            PoolBuilderError::Rpc(inner) => Self::BuildRefused {
                family,
                phase,
                failure: BuildFailure::Rpc(inner.to_string()),
            },
            PoolBuilderError::Spec(reason) => Self::BuildRefused {
                family,
                phase,
                failure: BuildFailure::Spec(*reason),
            },
            PoolBuilderError::Decoding { message } => Self::BuildRefused {
                family,
                phase,
                failure: BuildFailure::Decoding(message.clone()),
            },
            PoolBuilderError::Db(inner) => Self::BuildRefused {
                family,
                phase,
                failure: BuildFailure::Db(inner.to_string()),
            },
            PoolBuilderError::MissingIdentity { message } => Self::BuildRefused {
                family,
                phase,
                failure: BuildFailure::MissingIdentity(message.clone()),
            },
            PoolBuilderError::TickAssembly(inner) => Self::BuildRefused {
                family,
                phase,
                failure: BuildFailure::TickAssembly(inner.to_string()),
            },
        }
    }

    /// The V2 registry insert's refusal. The payload comes from the shared
    /// register-refusal vocabulary, so every construction site — this mapper
    /// and the write door's verdict — derives the same refusal from the same
    /// typed error.
    #[must_use]
    pub fn from_register_v2_error(err: &RegisterV2PoolError) -> Self {
        Self::RegisterRefused {
            family: Some(RegisteredPoolFamily::V2),
            phase: RegistrationPhase::Register,
            reason: RegisterRefusal::from(err),
        }
    }

    /// The V3 registry insert's refusal.
    #[must_use]
    pub fn from_register_v3_error(err: &RegisterV3PoolError) -> Self {
        Self::RegisterRefused {
            family: Some(RegisteredPoolFamily::V3),
            phase: RegistrationPhase::Register,
            reason: RegisterRefusal::from(err),
        }
    }

    /// The Aerodrome registry insert's refusal.
    #[must_use]
    pub fn from_register_aerodrome_error(err: &RegisterAerodromePoolError) -> Self {
        Self::RegisterRefused {
            family: Some(RegisteredPoolFamily::AerodromeV2),
            phase: RegistrationPhase::Register,
            reason: RegisterRefusal::from(err),
        }
    }

    /// The family write door's verdict (ADR-078 D3 step 2): the one
    /// `BotState::register_pool` outcome folds onto the ledger's
    /// [`RefusalKind::RegisterRefused`]. The door hands over the shared
    /// register-refusal payload, so the fold only fixes family and phase (the
    /// family rides the verdict, [`RegistrationPhase::Register`]) and each
    /// arm's fields carry verbatim; the fold tests pin the result equal to
    /// the direct-entry `from_register_*_error` mappers' output.
    ///
    /// # Errors
    /// The mapped [`RefusalKind::RegisterRefused`] when the verdict refused.
    pub fn from_registration_verdict(verdict: RegistrationVerdict) -> Result<u64, Self> {
        match verdict {
            RegistrationVerdict::Registered { pool_id, .. } => Ok(pool_id),
            RegistrationVerdict::Refused { family, refusal } => Err(Self::RegisterRefused {
                family: Some(family),
                phase: RegistrationPhase::Register,
                reason: refusal,
            }),
        }
    }

    /// The no-`ConstructionIo` refusal at a named family and phase.
    #[must_use]
    pub fn missing_construction_io(
        family: Option<RegisteredPoolFamily>,
        phase: RegistrationPhase,
    ) -> Self {
        Self::MissingConstructionIo { family, phase }
    }

    /// A build refusal the taxonomy names for a shape no builder ever saw.
    ///
    /// The driver positively resolved the identity — for the one live arm, a
    /// Balancer Vault pool: `getPoolId()` answered while neither family probe
    /// did — so there is no arm whose builder could refuse it, and no
    /// transient read behind the verdict. The refusal is the taxonomy's own
    /// [`SpecRefusal`], which keeps the stable-pool-fact classification a
    /// builder's spec refusal carries.
    #[must_use]
    pub fn unbuilt_spec_refusal(reason: SpecRefusal) -> Self {
        Self::BuildRefused {
            family: None,
            phase: RegistrationPhase::Build,
            failure: BuildFailure::Spec(reason),
        }
    }

    /// A registry GET that answered an address with no readable identity of
    /// the expected family.
    #[must_use]
    pub fn unreadable_identity(
        family: Option<RegisteredPoolFamily>,
        phase: RegistrationPhase,
        address: Address,
        detail: String,
    ) -> Self {
        Self::UnreadableIdentity {
            family,
            phase,
            address,
            detail,
        }
    }

    /// A verify-lifecycle precondition refusal.
    #[must_use]
    pub fn lifecycle_precondition(reason: LifecyclePrecondition) -> Self {
        Self::LifecyclePrecondition {
            phase: RegistrationPhase::Lifecycle,
            reason,
        }
    }

    /// A verify failure at a named phase. `fatal` is the load-bearing
    /// classification: a mismatch or a protocol violation is the tripwire; a
    /// read / transport / provider-construction failure is transient.
    #[must_use]
    pub fn verify_failed(
        phase: RegistrationPhase,
        fatal: bool,
        origin: VerifyOrigin,
        detail: String,
    ) -> Self {
        Self::VerifyFailed {
            family: None,
            phase,
            fatal,
            origin,
            detail,
        }
    }

    /// Verification was requested but its configuration was absent.
    #[must_use]
    pub fn verify_not_configured(phase: RegistrationPhase, detail: String) -> Self {
        Self::VerifyNotConfigured { phase, detail }
    }

    /// The snapshot-store load refusal.
    #[must_use]
    pub fn snapshot_load_failed(reason: SnapshotLoadReason) -> Self {
        Self::SnapshotLoadFailed {
            phase: RegistrationPhase::Snapshot,
            reason,
        }
    }

    /// The source rendering (log-only) this refusal carries, when it carries
    /// one. Variants whose message is interpolated from typed facts
    /// (`RegisterRefusal::Admission`, the V4 admission classes) render from
    /// those facts instead.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::MalformedInput { detail, .. }
            | Self::UnreadableIdentity { detail, .. }
            | Self::UnsupportedFamily { detail, .. }
            | Self::VerifyFailed { detail, .. }
            | Self::VerifyNotConfigured { detail, .. } => Some(detail),
            Self::IdentityMismatch { shape, .. } => Some(match shape {
                IdentityMismatchShape::Create2 { detail, .. }
                | IdentityMismatchShape::MintedVsRegistered { detail, .. } => detail,
            }),
            Self::RegisterRefused { reason, .. } => match reason {
                RegisterRefusal::AlreadyRegistered { detail, .. }
                | RegisterRefusal::SpecViolation { detail } => Some(detail),
                RegisterRefusal::Admission { .. } => None,
            },
            Self::BuildRefused { failure, .. } => match failure {
                BuildFailure::Rpc(detail)
                | BuildFailure::Decoding(detail)
                | BuildFailure::Db(detail)
                | BuildFailure::MissingIdentity(detail)
                | BuildFailure::TickAssembly(detail)
                | BuildFailure::Transient(detail) => Some(detail),
                BuildFailure::HookedPool | BuildFailure::DynamicFee | BuildFailure::HighFee => None,
                // A spec refusal is the one build arm whose facts are not text:
                // its rendering is the reason label it carries.
                BuildFailure::Spec(reason) => Some(reason.as_str()),
            },
            Self::SnapshotLoadFailed { reason, .. } => Some(match reason {
                SnapshotLoadReason::Db { detail }
                | SnapshotLoadReason::Decode { detail }
                | SnapshotLoadReason::OutOfRange { detail } => detail,
            }),
            Self::MissingConstructionIo { .. } | Self::LifecyclePrecondition { .. } => None,
        }
    }

    /// The same refusal at a different pipeline phase. Phase rides as data,
    /// so a wrapper that owns its own phase (the verify-lifecycle
    /// orchestration around a verify step) re-stamps it without forking the
    /// kind.
    #[must_use]
    pub fn at_phase(self, phase: RegistrationPhase) -> Self {
        match self {
            Self::MalformedInput { input, detail, .. } => Self::MalformedInput {
                phase,
                input,
                detail,
            },
            Self::IdentityMismatch { family, shape, .. } => Self::IdentityMismatch {
                family,
                phase,
                shape,
            },
            Self::MissingConstructionIo { family, .. } => {
                Self::MissingConstructionIo { family, phase }
            }
            Self::UnsupportedFamily {
                family,
                subject,
                detail,
                ..
            } => Self::UnsupportedFamily {
                family,
                phase,
                subject,
                detail,
            },
            Self::BuildRefused {
                family, failure, ..
            } => Self::BuildRefused {
                family,
                phase,
                failure,
            },
            Self::RegisterRefused { family, reason, .. } => Self::RegisterRefused {
                family,
                phase,
                reason,
            },
            Self::UnreadableIdentity {
                family,
                address,
                detail,
                ..
            } => Self::UnreadableIdentity {
                family,
                phase,
                address,
                detail,
            },
            Self::SnapshotLoadFailed { reason, .. } => Self::SnapshotLoadFailed { phase, reason },
            Self::VerifyFailed {
                family,
                fatal,
                origin,
                detail,
                ..
            } => Self::VerifyFailed {
                family,
                phase,
                fatal,
                origin,
                detail,
            },
            Self::VerifyNotConfigured { detail, .. } => Self::VerifyNotConfigured { phase, detail },
            Self::LifecyclePrecondition { reason, .. } => {
                Self::LifecyclePrecondition { phase, reason }
            }
        }
    }
}

impl std::fmt::Display for RefusalKind {
    /// The refusal's bounded tag, plus its source rendering when it carries
    /// one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.tag())?;
        if let Some(detail) = self.detail() {
            write!(f, ": {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for RefusalKind {}

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

/// The strategy pre-filter's verdict on one candidate path, submitted to the
/// eligibility gate as data.
///
/// The pre-filter is policy and stays with the strategy (a Python-side
/// pre-filter), so the ledger is handed the verdict, never a callback. A
/// [`PolicyVerdict::Denied`] is deterministic per hop signature, so the gate
/// records it in the rejected-path memo and answers
/// [`PathAdmission::PolicyDenied`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyVerdict {
    /// The pre-filter accepted the candidate.
    Admitted,
    /// The pre-filter denied the candidate.
    Denied,
}

/// What the eligibility gate answers for one candidate path.
///
/// The gate owns the MECHANICAL eligibility decisions in front of verify and
/// registration — direction agreement, the rejected-path memo, the submitted
/// policy verdict, and the registered-path dup fast-path — in ONE ordered
/// pass ([`RegistrationLedger::admit_path_with_policy`]). The strategy's
/// path-composition pre-filter is policy and stays with the strategy (a
/// Python-side pre-filter): the driver runs it BEFORE the ask and submits the
/// verdict as data, so the policy deny never needs a second ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathAdmission {
    /// Operator-pinned directions disagree with the path's hop count.
    DirectionMismatch,
    /// The hop signature already hit a deterministic deny (the rejected-path
    /// memo).
    RejectedPathMemo,
    /// The submitted policy verdict denied the candidate; the deny is now in
    /// the rejected-path memo.
    PolicyDenied,
    /// The candidate may proceed; `hop_sig` is the signature the memos key on.
    Admitted {
        /// The hop signature `(pool_id, zero_for_one)` per hop, in hop order.
        hop_sig: HopSignature,
    },
    /// The hop signature already completed registration (the dup fast-path).
    AlreadyRegistered,
}

impl PathAdmission {
    /// The closed verdict set, in declaration order — the vocabulary a
    /// cross-language consumer builds its verdict labels from.
    pub const KINDS: [&'static str; 5] = [
        "direction-mismatch",
        "rejected-path-memo",
        "policy-denied",
        "admitted",
        "already-registered",
    ];

    /// The bounded verdict tag.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::DirectionMismatch => "direction-mismatch",
            Self::RejectedPathMemo => "rejected-path-memo",
            Self::PolicyDenied => "policy-denied",
            Self::Admitted { .. } => "admitted",
            Self::AlreadyRegistered => "already-registered",
        }
    }
}

/// One negative-memo container.
///
/// The four registration memos differ only in their key and payload, so the
/// lookup/insert/count rule lives here once instead of four times, and
/// [`Memo::insert_once`] is the one place the *first answer wins* discipline
/// (a stable refusal outranks a later transient one) is expressed.
///
/// Ordered (not hashed) storage is deliberate: the pipeline report folds these
/// in iteration order, and a hash order would make it nondeterministic.
#[derive(Debug)]
struct Memo<K, V = ()>(BTreeMap<K, V>);

impl<K: Ord, V> Default for Memo<K, V> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}

impl<K: Ord, V> Memo<K, V> {
    fn contains<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.0.contains_key(key)
    }

    fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.0.get(key)
    }

    fn insert(&mut self, key: K, value: V) {
        self.0.insert(key, value);
    }

    /// Record only when absent — the first answer wins.
    fn insert_once(&mut self, key: K, value: V) {
        self.0.entry(key).or_insert(value);
    }

    fn len(&self) -> usize {
        self.0.len()
    }
}

/// The identity a pool memo keys on — known BEFORE any build.
///
/// A contract-address family keys on its address; a V4 pool on its on-chain
/// pool id, both of which the discovery edge carries pre-build, so a refused
/// pool is recognizable without an RPC. Distinct from `PoolLookup`, which names
/// a *registered* pool and addresses V4 by `(pool_manager, pool_id)`: the
/// manager is not known until the build resolves it.
///
/// The wire form (`p:0x…` / `v4id:0x…`, lowercase hex) is `Display`/`FromStr`-
/// symmetric, so the Python shell can carry the key as text without owning the
/// format — the shape of the key is decided here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PreBuildIdentity {
    /// A contract-address family (V2, V3, Aerodrome, Curve, Balancer).
    Address(alloy::primitives::Address),
    /// A V4 pool, by its on-chain id.
    V4(degenbot_decoders::v4_swap_decoder::V4PoolId),
}

impl PreBuildIdentity {
    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    /// Decode a hex identity field of an exact byte width.
    fn parse_hex(text: &str, width: usize) -> Option<Vec<u8>> {
        let hex = text.trim();
        let hex = hex.strip_prefix("0x").unwrap_or(hex);
        let bytes = alloy::hex::decode(hex).ok()?;
        (bytes.len() == width).then_some(bytes)
    }
}

impl std::fmt::Display for PreBuildIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Address(address) => write!(f, "p:0x{}", Self::hex(address.as_slice())),
            Self::V4(pool_id) => write!(f, "v4id:0x{}", Self::hex(pool_id)),
        }
    }
}

impl PreBuildIdentity {
    /// The wire form as a memo key, or `None` when `text` is not one.
    ///
    /// The claim-window and Python doors carry keys as text; this is where that
    /// text becomes the typed key, once.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        text.parse().ok()
    }
}

impl std::str::FromStr for PreBuildIdentity {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (kind, hex) = text
            .split_once(':')
            .ok_or_else(|| format!("memo key {text:?} has no kind prefix"))?;
        match kind {
            "p" => Self::parse_hex(hex, 20)
                .and_then(|bytes| <[u8; 20]>::try_from(bytes.as_slice()).ok())
                .map(|bytes| Self::Address(alloy::primitives::Address::from(bytes)))
                .ok_or_else(|| format!("memo key {text:?} is not a 20-byte address")),
            "v4id" => Self::parse_hex(hex, 32)
                .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
                .map(Self::V4)
                .ok_or_else(|| format!("memo key {text:?} is not a 32-byte pool id")),
            other => Err(format!("memo key {text:?} names an unknown kind {other:?}")),
        }
    }
}

/// The four registration memos plus the typed build-refusal classification.
#[derive(Debug, Default)]
pub struct RegistrationLedger {
    registered_paths: Memo<HopSignature>,
    verified_pools: Memo<PreBuildIdentity>,
    unregistrable_pools: Memo<PreBuildIdentity, UnregistrableRecord>,
    rejected_paths: Memo<HopSignature>,
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
    ) -> Option<PreBuildIdentity> {
        match kind {
            PoolKind::V4 => pool_hash.and_then(|hash| {
                let bytes = PreBuildIdentity::parse_hex(hash, 32)?;
                Some(PreBuildIdentity::V4(
                    <[u8; 32]>::try_from(bytes.as_slice()).ok()?,
                ))
            }),
            _ => address.and_then(|address| {
                let bytes = PreBuildIdentity::parse_hex(address, 20)?;
                Some(PreBuildIdentity::Address(alloy::primitives::Address::from(
                    <[u8; 20]>::try_from(bytes.as_slice()).ok()?,
                )))
            }),
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
            BuildFailure::Rpc(_)
            | BuildFailure::Spec(_)
            | BuildFailure::Decoding(_)
            | BuildFailure::Db(_)
            | BuildFailure::MissingIdentity(_)
            | BuildFailure::TickAssembly(_)
            | BuildFailure::Transient(_) => BuildRefusal {
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
        self.registered_paths.insert(hop_sig, ());
    }

    /// Whether this pool's verify lifecycle already completed.
    #[must_use]
    pub fn pool_verified(&self, key: &PreBuildIdentity) -> bool {
        self.verified_pools.contains(key)
    }

    /// Record a COMPLETED verify lifecycle (a pool fact).
    pub fn memoize_verified_pool(&mut self, key: PreBuildIdentity) {
        self.verified_pools.insert(key, ());
    }

    /// How many pools have a completed verify-lifecycle fact (test/diagnostic
    /// witness).
    #[must_use]
    pub fn verified_pool_count(&self) -> usize {
        self.verified_pools.len()
    }

    /// The memoized stable refusal for a pool key, or `None`.
    #[must_use]
    pub fn unregistrable_record(
        &self,
        key: Option<&PreBuildIdentity>,
    ) -> Option<&UnregistrableRecord> {
        key.and_then(|key| self.unregistrable_pools.get(key))
    }

    /// Record a STABLE build refusal (`setdefault` — the first tag wins, so a
    /// later transient answer cannot overwrite the pool fact).
    pub fn memoize_unregistrable(
        &mut self,
        key: Option<PreBuildIdentity>,
        outcome: RegistrationOutcome,
        counts_as_skip: bool,
    ) {
        if let Some(key) = key {
            self.unregistrable_pools.insert_once(
                key,
                UnregistrableRecord {
                    outcome,
                    counts_as_skip,
                },
            );
        }
    }

    /// Whether this hop signature already hit a deterministic deny.
    #[must_use]
    pub fn path_rejected(&self, hop_sig: &[(u64, bool)]) -> bool {
        self.rejected_paths.contains(hop_sig)
    }

    /// Record a deterministic policy/predicate deny for a hop signature.
    pub fn memoize_rejected_path(&mut self, hop_sig: HopSignature) {
        self.rejected_paths.insert(hop_sig, ());
    }

    // ── the eligibility gate ──

    /// The eligibility gate: ONE ordered pass over a candidate path.
    ///
    /// `hop_pool_ids` is one engine pool id per hop, in hop order.
    /// `pinned_directions` is the operator's pinned per-hop direction answer
    /// when the caller pinned one; `resolved_directions` is the direction
    /// answer the caller holds (the pin when it agrees, else the resolver's
    /// answer) — `None` when the caller could not agree one. A candidate
    /// whose directions cannot be agreed is not admissible, whatever the
    /// reason. `policy` is the strategy's path-composition pre-filter
    /// verdict, evaluated by the driver BEFORE this ask and submitted as
    /// data (the pre-filter is policy and stays with the strategy — the
    /// ledger is handed the verdict, never a callback).
    ///
    /// The pass, in order: direction agreement → the rejected-path memo →
    /// the policy verdict → the registered-path dup fast-path. A policy deny
    /// is deterministic per hop signature — the pre-filter is a pure policy
    /// over the path's composition — so it enters the rejected-path memo and
    /// is answered as [`PathAdmission::PolicyDenied`].
    ///
    /// A driver reads the rejected-path memo itself ([`Self::path_rejected`])
    /// before running its pre-filter — a memoized deny must never re-run the
    /// predicate — so a memo hit at THIS ask answers the seat race where
    /// another unit memoized the signature between that read and here.
    pub fn admit_path_with_policy(
        &mut self,
        hop_pool_ids: &[u64],
        pinned_directions: Option<&[bool]>,
        resolved_directions: Option<&[bool]>,
        policy: PolicyVerdict,
    ) -> PathAdmission {
        if pinned_directions.is_some_and(|pinned| pinned.len() != hop_pool_ids.len()) {
            return PathAdmission::DirectionMismatch;
        }
        let directions = match (pinned_directions, resolved_directions) {
            (Some(pinned), _) => pinned,
            (None, Some(resolved)) if resolved.len() == hop_pool_ids.len() => resolved,
            // No direction answer, or one that does not cover the path: the
            // candidate's directions are not agreed.
            (None, _) => return PathAdmission::DirectionMismatch,
        };
        let hop_sig: HopSignature = hop_pool_ids
            .iter()
            .copied()
            .zip(directions.iter().copied())
            .collect();
        if self.path_rejected(&hop_sig) {
            return PathAdmission::RejectedPathMemo;
        }
        if policy == PolicyVerdict::Denied {
            self.memoize_rejected_path(hop_sig);
            return PathAdmission::PolicyDenied;
        }
        if self.path_registered(&hop_sig) {
            return PathAdmission::AlreadyRegistered;
        }
        PathAdmission::Admitted { hop_sig }
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
    use degenbot_pools::state::v4_state::RegisterV4PoolError;
    use degenbot_uniswap::deployments::AddressMismatch;

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
        let address = "0xAbCdEf0000000000000000000000000000000001";
        let hash = format!("0x{}", "aB".repeat(32));
        let from_address =
            RegistrationLedger::pool_memo_key(PoolKind::V3, Some(address), None).unwrap();
        assert_eq!(
            from_address.to_string(),
            "p:0xabcdef0000000000000000000000000000000001"
        );
        assert_eq!(
            PreBuildIdentity::parse(&from_address.to_string()),
            Some(from_address)
        );
        let from_hash = RegistrationLedger::pool_memo_key(PoolKind::V4, None, Some(&hash)).unwrap();
        assert_eq!(from_hash.to_string(), format!("v4id:0x{}", "ab".repeat(32)));
        assert_eq!(
            PreBuildIdentity::parse(&from_hash.to_string()),
            Some(from_hash)
        );
        assert_eq!(
            RegistrationLedger::pool_memo_key(PoolKind::V4, None, None),
            None
        );
        assert_eq!(
            RegistrationLedger::pool_memo_key(PoolKind::V2, None, None),
            None
        );
        // A malformed identity is not memoizable rather than silently keyed by
        // its text.
        assert_eq!(
            RegistrationLedger::pool_memo_key(PoolKind::V3, Some("0xabc"), None),
            None
        );
        assert_eq!(PreBuildIdentity::parse("v3:0x1"), None);
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
        use degenbot_pools::state::v4_state::RegisterV4PoolError;

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

        let key = PreBuildIdentity::Address(alloy::primitives::Address::from([0x01u8; 20]));
        assert!(!ledger.pool_verified(&key));
        ledger.memoize_verified_pool(key);
        assert!(ledger.pool_verified(&key));

        ledger.memoize_unregistrable(Some(key), RegistrationOutcome::BuildV3Refused, true);
        ledger.memoize_unregistrable(Some(key), RegistrationOutcome::V4HookRejected, false);
        let record = ledger.unregistrable_record(Some(&key)).unwrap();
        assert_eq!(record.outcome, RegistrationOutcome::BuildV3Refused);
        assert!(record.counts_as_skip);
        assert!(ledger.unregistrable_record(None).is_none());

        assert!(!ledger.path_rejected(&[(2, false)]));
        ledger.memoize_rejected_path(vec![(2, false)]);
        assert!(ledger.path_rejected(&[(2, false)]));
    }

    /// The eligibility gate owns the mechanical decisions in ONE ordered
    /// pass: direction agreement, the rejected-path memo, the submitted
    /// policy verdict, and the registered-path dup fast-path. The driver
    /// runs the strategy's pre-filter first and submits the verdict as data;
    /// a deny is memoized here and answered as `PolicyDenied`.
    #[test]
    fn eligibility_gate_owns_the_mechanical_decisions() {
        let mut ledger = RegistrationLedger::new();
        let sig = vec![(1, true), (2, false)];

        // Direction agreement: a pin that disagrees with the hop count is not
        // admissible, and it is judged against the hop ids alone.
        assert_eq!(
            ledger.admit_path_with_policy(
                &[1, 2],
                Some(&[true]),
                Some(&[true]),
                PolicyVerdict::Admitted
            ),
            PathAdmission::DirectionMismatch,
            "a pin that does not cover the path is a mismatch"
        );
        // A resolved answer that does not cover the path is not admissible.
        assert_eq!(
            ledger.admit_path_with_policy(&[1, 2], None, Some(&[true]), PolicyVerdict::Admitted),
            PathAdmission::DirectionMismatch
        );
        // No direction answer at all is not admissible — the policy verdict
        // is never consulted for a candidate that fails direction agreement.
        assert_eq!(
            ledger.admit_path_with_policy(&[1, 2], None, None, PolicyVerdict::Denied),
            PathAdmission::DirectionMismatch
        );

        // Agreement builds the signature the memos key on, in hop order.
        assert_eq!(
            ledger.admit_path_with_policy(
                &[1, 2],
                None,
                Some(&[true, false]),
                PolicyVerdict::Admitted
            ),
            PathAdmission::Admitted {
                hop_sig: sig.clone()
            }
        );
        assert_eq!(
            ledger.admit_path_with_policy(
                &[1, 2],
                Some(&[true, false]),
                Some(&[true, false]),
                PolicyVerdict::Admitted
            ),
            PathAdmission::Admitted {
                hop_sig: sig.clone()
            },
            "an agreeing pin is the direction answer"
        );

        // A denied candidate is answered PolicyDenied and its deny enters the
        // rejected-path memo.
        assert_eq!(
            ledger.admit_path_with_policy(&[3], None, Some(&[true]), PolicyVerdict::Denied),
            PathAdmission::PolicyDenied
        );
        assert!(ledger.path_rejected(&[(3, true)]));

        // The memo answers before the policy verdict on every later ask: a
        // memoized deny is never re-decided.
        assert_eq!(
            ledger.admit_path_with_policy(&[3], None, Some(&[true]), PolicyVerdict::Admitted),
            PathAdmission::RejectedPathMemo,
            "the deny is memoized by the ledger, never re-asked"
        );

        // The dup fast-path follows the policy verdict: an admitted candidate
        // on a fresh signature is Admitted; on a registered one it answers
        // AlreadyRegistered.
        assert_eq!(
            ledger.admit_path_with_policy(&[4], None, Some(&[true]), PolicyVerdict::Admitted),
            PathAdmission::Admitted {
                hop_sig: vec![(4, true)]
            }
        );
        ledger.memoize_registered_path(vec![(4, true)]);
        assert_eq!(
            ledger.admit_path_with_policy(&[4], None, Some(&[true]), PolicyVerdict::Admitted),
            PathAdmission::AlreadyRegistered
        );

        // The verdict vocabulary is closed and unique.
        let kinds: Vec<&str> = [
            PathAdmission::DirectionMismatch,
            PathAdmission::RejectedPathMemo,
            PathAdmission::PolicyDenied,
            PathAdmission::Admitted {
                hop_sig: Vec::new(),
            },
            PathAdmission::AlreadyRegistered,
        ]
        .iter()
        .map(PathAdmission::as_str)
        .collect();
        assert_eq!(kinds, PathAdmission::KINDS.to_vec());
        let unique: BTreeSet<&str> = kinds.iter().copied().collect();
        assert_eq!(unique.len(), kinds.len(), "verdict tags are unique");
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

    // ── The refusal vocabulary (ADR-077) ─────────────────────────────────
    //
    // The acceptance bar for the one refusal vocabulary: every variant of
    // every registration-refusal enum in the census maps into a `RefusalKind`
    // and carries the right family, phase, and load-bearing classification.

    use alloy::primitives::{B256, U256};
    use degenbot_core::errors::ProviderError;
    use degenbot_core::hex_utils::HexError;
    use degenbot_db::error::DbError;
    use degenbot_pools::sim::registration_refusal::SpecRefusal;
    use degenbot_pools::sim::tick_fetch::BootstrapTickError;
    use degenbot_pools::state::spec_bounds::{SpecValue, SpecViolation};
    use degenbot_substrate::tick_assembly::TickMapAssemblyError;

    /// The family/phase tags a kind carries — an exhaustive classifier, so a
    /// new variant does not compile until it declares where it sits in the
    /// pipeline.
    fn tags(kind: &RefusalKind) -> (Option<RegisteredPoolFamily>, RegistrationPhase) {
        match kind {
            RefusalKind::MalformedInput { phase, .. }
            | RefusalKind::SnapshotLoadFailed { phase, .. }
            | RefusalKind::VerifyNotConfigured { phase, .. }
            | RefusalKind::LifecyclePrecondition { phase, .. } => (None, *phase),
            RefusalKind::IdentityMismatch { family, phase, .. }
            | RefusalKind::MissingConstructionIo { family, phase }
            | RefusalKind::UnsupportedFamily { family, phase, .. }
            | RefusalKind::BuildRefused { family, phase, .. }
            | RefusalKind::RegisterRefused { family, phase, .. }
            | RefusalKind::UnreadableIdentity { family, phase, .. }
            | RefusalKind::VerifyFailed { family, phase, .. } => (*family, *phase),
        }
    }

    fn assert_tags(
        kind: &RefusalKind,
        family: Option<RegisteredPoolFamily>,
        phase: RegistrationPhase,
    ) {
        assert_eq!(tags(kind), (family, phase), "kind: {kind:?}");
    }

    fn identity_shape(
        kind: &RefusalKind,
    ) -> Option<(
        Option<RegisteredPoolFamily>,
        RegistrationPhase,
        &IdentityMismatchShape,
    )> {
        match kind {
            RefusalKind::IdentityMismatch {
                family,
                phase,
                shape,
            } => Some((*family, *phase, shape)),
            _ => None,
        }
    }

    fn create2_shape(kind: &RefusalKind) -> Option<(Address, Address, String)> {
        match kind {
            RefusalKind::IdentityMismatch {
                shape:
                    IdentityMismatchShape::Create2 {
                        declared,
                        computed,
                        detail,
                    },
                ..
            } => Some((*declared, *computed, detail.clone())),
            _ => None,
        }
    }

    fn minted_shape(kind: &RefusalKind) -> Option<(Address, String)> {
        match kind {
            RefusalKind::IdentityMismatch {
                shape: IdentityMismatchShape::MintedVsRegistered { address, detail },
                ..
            } => Some((*address, detail.clone())),
            _ => None,
        }
    }

    fn build_failure(kind: &RefusalKind) -> Option<&BuildFailure> {
        match kind {
            RefusalKind::BuildRefused { failure, .. } => Some(failure),
            _ => None,
        }
    }

    fn register_reason(kind: &RefusalKind) -> Option<&RegisterRefusal> {
        match kind {
            RefusalKind::RegisterRefused { reason, .. } => Some(reason),
            _ => None,
        }
    }

    fn verify_failure(kind: &RefusalKind) -> Option<(bool, VerifyOrigin)> {
        match kind {
            RefusalKind::VerifyFailed { fatal, origin, .. } => Some((*fatal, *origin)),
            _ => None,
        }
    }

    fn snapshot_reason(kind: &RefusalKind) -> Option<&SnapshotLoadReason> {
        match kind {
            RefusalKind::SnapshotLoadFailed { reason, .. } => Some(reason),
            _ => None,
        }
    }

    fn lifecycle_precondition(kind: &RefusalKind) -> Option<LifecyclePrecondition> {
        match kind {
            RefusalKind::LifecyclePrecondition { reason, .. } => Some(*reason),
            _ => None,
        }
    }

    fn sample_address(byte: u8) -> Address {
        Address::repeat_byte(byte)
    }

    fn address_mismatch() -> AddressMismatch {
        AddressMismatch {
            chain_id: 1,
            factory: sample_address(0x11),
            deployer: sample_address(0x22),
            init_hash: B256::repeat_byte(0x33),
            expected: sample_address(0x44),
            computed: sample_address(0x55),
        }
    }

    fn identity_divergence() -> IdentityDivergence {
        IdentityDivergence::MintedVsRegistered {
            address: sample_address(0x44),
            detail: "minted != registered".to_owned(),
        }
    }

    fn spec_violation() -> SpecViolation {
        SpecViolation {
            field: "reserve0",
            value: SpecValue::U256(U256::MAX),
            bound: "uint112",
        }
    }

    #[test]
    fn refusal_kind_tag_set_is_closed_and_unique() {
        let kinds = [
            RefusalKind::MalformedInput {
                phase: RegistrationPhase::Intake,
                input: String::new(),
                detail: String::new(),
            },
            RefusalKind::IdentityMismatch {
                family: None,
                phase: RegistrationPhase::Identity,
                shape: IdentityMismatchShape::MintedVsRegistered {
                    address: sample_address(0x44),
                    detail: String::new(),
                },
            },
            RefusalKind::MissingConstructionIo {
                family: None,
                phase: RegistrationPhase::Build,
            },
            RefusalKind::UnsupportedFamily {
                family: None,
                phase: RegistrationPhase::Build,
                subject: UnsupportedSubject::Pool {
                    address: sample_address(0x01),
                },
                detail: String::new(),
            },
            RefusalKind::BuildRefused {
                family: None,
                phase: RegistrationPhase::Build,
                failure: BuildFailure::Transient(String::new()),
            },
            RefusalKind::RegisterRefused {
                family: None,
                phase: RegistrationPhase::Register,
                reason: RegisterRefusal::SpecViolation {
                    detail: String::new(),
                },
            },
            RefusalKind::UnreadableIdentity {
                family: None,
                phase: RegistrationPhase::Register,
                address: sample_address(0x01),
                detail: String::new(),
            },
            RefusalKind::SnapshotLoadFailed {
                phase: RegistrationPhase::Snapshot,
                reason: SnapshotLoadReason::Db {
                    detail: String::new(),
                },
            },
            RefusalKind::VerifyFailed {
                family: None,
                phase: RegistrationPhase::Verify,
                fatal: true,
                origin: VerifyOrigin::Mismatch,
                detail: String::new(),
            },
            RefusalKind::VerifyNotConfigured {
                phase: RegistrationPhase::Verify,
                detail: String::new(),
            },
            RefusalKind::LifecyclePrecondition {
                phase: RegistrationPhase::Lifecycle,
                reason: LifecyclePrecondition::MissingVerifyProvider,
            },
        ];

        let kind_tags: Vec<&str> = kinds.iter().map(RefusalKind::tag).collect();
        assert_eq!(kind_tags, REFUSAL_KIND_TAGS.to_vec());
        let unique: std::collections::BTreeSet<&str> = kind_tags.iter().copied().collect();
        assert_eq!(unique.len(), kind_tags.len(), "kind tags are unique");
    }

    /// The historical identity-mismatch vocabularies land on ONE refusal kind
    /// through ONE verdict: the register path's `CREATE2` recomputation, the
    /// builder's `CREATE2` check, and the minted-vs-registered parity guard.
    /// Nothing is lost in the mapping.
    #[test]
    fn identity_parity_vocabularies_land_on_one_refusal_kind() {
        let mismatch = address_mismatch();
        let create2 = IdentityDivergence::Create2(mismatch);

        let v2 = RefusalKind::from_identity_divergence(Some(RegisteredPoolFamily::V2), &create2);
        let v3 = RefusalKind::from_identity_divergence(Some(RegisteredPoolFamily::V3), &create2);
        let aerodrome = RefusalKind::from_identity_divergence(
            Some(RegisteredPoolFamily::AerodromeV2),
            &create2,
        );
        let builder = RefusalKind::from_builder_error(
            &PoolBuilderError::Identity(Box::new(create2.clone())),
            Some(RegisteredPoolFamily::V2),
            RegistrationPhase::Build,
        );
        let guard = RefusalKind::from_identity_divergence(None, &identity_divergence());

        for kind in [&v2, &v3, &aerodrome, &builder, &guard] {
            assert!(
                matches!(kind, RefusalKind::IdentityMismatch { .. }),
                "every identity-mismatch vocabulary lands on the identity-parity kind: {kind:?}"
            );
            assert_eq!(
                tags(kind).1,
                RegistrationPhase::Identity,
                "identity parity is the identity phase wherever it is raised"
            );
        }

        // The builder path carries the SAME verdict as the register path, so
        // the declared / recomputed pair survives there too — the builder no
        // longer discards it.
        assert_eq!(
            builder, v2,
            "one fact, one refusal: the register path and the build path agree"
        );

        // The declared / recomputed pair survives the register-path mapping.
        assert_eq!(
            identity_shape(&v2).unwrap().0,
            Some(RegisteredPoolFamily::V2)
        );
        let (declared, computed, detail) = create2_shape(&v2).unwrap();
        assert_eq!(declared, mismatch.expected);
        assert_eq!(computed, mismatch.computed);
        assert!(detail.contains("does not match CREATE2"));

        // The other two families tag themselves without forking the kind.
        assert_eq!(
            identity_shape(&v3).unwrap().0,
            Some(RegisteredPoolFamily::V3)
        );
        assert_eq!(
            identity_shape(&aerodrome).unwrap().0,
            Some(RegisteredPoolFamily::AerodromeV2)
        );

        // The parity guard's divergence survives verbatim.
        let (family, phase, _) = identity_shape(&guard).unwrap();
        assert_eq!(family, None);
        assert_eq!(phase, RegistrationPhase::Identity);
        let (address, detail) = minted_shape(&guard).unwrap();
        assert_eq!(address, sample_address(0x44));
        assert_eq!(detail, "minted != registered");
    }

    /// The identity-mismatch fact has ONE home (ADR-077 D3): the register
    /// path and the builder path both surface the SAME refusal for the same
    /// recomputed-vs-declared divergence, and the closed vocabulary names
    /// exactly one identity-mismatch kind.
    #[test]
    fn identity_mismatch_has_one_home_and_both_paths_surface_it() {
        assert_eq!(
            REFUSAL_KIND_TAGS
                .iter()
                .filter(|tag| **tag == "identity-mismatch")
                .count(),
            1,
            "exactly one identity-mismatch kind in the closed vocabulary"
        );

        let divergence = IdentityDivergence::Create2(address_mismatch());
        let register =
            RefusalKind::from_identity_divergence(Some(RegisteredPoolFamily::V2), &divergence);
        let build = RefusalKind::from_builder_error(
            &PoolBuilderError::Identity(Box::new(divergence)),
            Some(RegisteredPoolFamily::V2),
            RegistrationPhase::Build,
        );
        assert_eq!(
            register, build,
            "the register path and the build path surface the SAME identity refusal"
        );
        assert!(matches!(register, RefusalKind::IdentityMismatch { .. }));
        assert_eq!(register.tag(), "identity-mismatch");
    }

    #[test]
    fn register_mappers_carry_family_and_phase() {
        let address = sample_address(0x44);

        // The address-keyed families.
        for (kind, family) in [
            (
                RefusalKind::from_register_v2_error(&RegisterV2PoolError::AlreadyRegistered {
                    address,
                }),
                RegisteredPoolFamily::V2,
            ),
            (
                RefusalKind::from_register_v3_error(&RegisterV3PoolError::SpecViolation(
                    spec_violation(),
                )),
                RegisteredPoolFamily::V3,
            ),
            (
                RefusalKind::from_register_aerodrome_error(
                    &RegisterAerodromePoolError::AlreadyRegistered { address },
                ),
                RegisteredPoolFamily::AerodromeV2,
            ),
        ] {
            assert_tags(&kind, Some(family), RegistrationPhase::Register);
        }

        // Identity parity is the identity phase at the register door too.
        assert_tags(
            &RefusalKind::from_identity_divergence(
                Some(RegisteredPoolFamily::V2),
                &IdentityDivergence::Create2(address_mismatch()),
            ),
            Some(RegisteredPoolFamily::V2),
            RegistrationPhase::Identity,
        );

        // A no-io refusal at the identity phase, and a builder family abort.
        assert_tags(
            &RefusalKind::missing_construction_io(
                Some(RegisteredPoolFamily::V4),
                RegistrationPhase::Identity,
            ),
            Some(RegisteredPoolFamily::V4),
            RegistrationPhase::Identity,
        );
        assert_tags(
            &RefusalKind::from_builder_error(
                &PoolBuilderError::UnknownPoolIdentity { address },
                Some(RegisteredPoolFamily::V4),
                RegistrationPhase::Identity,
            ),
            Some(RegisteredPoolFamily::V4),
            RegistrationPhase::Identity,
        );

        let malformed = RefusalKind::malformed_input(
            "0xnope".to_owned(),
            "Invalid address '0xnope': bad hex".to_owned(),
        );
        assert_tags(&malformed, None, RegistrationPhase::Intake);
        assert!(matches!(
            malformed,
            RefusalKind::MalformedInput { ref input, .. } if input.as_str() == "0xnope"
        ));
    }

    #[test]
    fn build_mappers_carry_family_and_phase() {
        let address = sample_address(0x44);
        let build = RegistrationPhase::Build;

        // The family-agnostic build entry.
        assert_tags(
            &RefusalKind::missing_construction_io(None, build),
            None,
            build,
        );
        assert_tags(
            &RefusalKind::from_builder_error(
                &PoolBuilderError::Spec(SpecRefusal::V2Reserve),
                None,
                build,
            ),
            None,
            build,
        );

        // The spec refusal's reason is its log-only detail, so a refused build
        // never renders as a bare tag.
        let spec = RefusalKind::from_builder_error(
            &PoolBuilderError::Spec(SpecRefusal::CurveTooFewCoins),
            None,
            build,
        );
        assert_eq!(spec.detail(), Some("curve too few coins"));
        assert_eq!(spec.to_string(), "build-refused: curve too few coins");

        // Each family's build door names its own family tag.
        for (kind, family) in [
            (
                RefusalKind::from_builder_error(
                    &PoolBuilderError::Spec(SpecRefusal::V2Reserve),
                    Some(RegisteredPoolFamily::V2),
                    build,
                ),
                RegisteredPoolFamily::V2,
            ),
            (
                RefusalKind::from_builder_error(
                    &PoolBuilderError::Spec(SpecRefusal::CurveTooFewCoins),
                    Some(RegisteredPoolFamily::AerodromeV2),
                    build,
                ),
                RegisteredPoolFamily::AerodromeV2,
            ),
            (
                RefusalKind::from_builder_error(
                    &PoolBuilderError::Spec(SpecRefusal::NoCodeCoin),
                    Some(RegisteredPoolFamily::V4),
                    build,
                ),
                RegisteredPoolFamily::V4,
            ),
        ] {
            assert_tags(&kind, Some(family), build);
        }

        // The build door's register arm is the register phase, and its
        // minted-vs-registered parity guard is the identity phase.
        assert_tags(
            &RefusalKind::from_register_v2_error(&RegisterV2PoolError::AlreadyRegistered {
                address,
            }),
            Some(RegisteredPoolFamily::V2),
            RegistrationPhase::Register,
        );
        assert_tags(
            &RefusalKind::from_identity_divergence(
                Some(RegisteredPoolFamily::V4),
                &identity_divergence(),
            ),
            Some(RegisteredPoolFamily::V4),
            RegistrationPhase::Identity,
        );

        // The V3 construction route's refusals.
        assert_tags(
            &RefusalKind::from_construction_refusal(
                &ConstructionRefusal::UnsupportedFamily {
                    address,
                    detail: "no rung serves this pool".to_owned(),
                },
                Some(RegisteredPoolFamily::V3),
            ),
            Some(RegisteredPoolFamily::V3),
            build,
        );
        assert_tags(
            &RefusalKind::unreadable_identity(
                Some(RegisteredPoolFamily::V3),
                RegistrationPhase::Register,
                address,
                "registry GET answered with no readable V3 identity".to_owned(),
            ),
            Some(RegisteredPoolFamily::V3),
            RegistrationPhase::Register,
        );
    }

    #[test]
    fn construction_refusal_maps_into_the_build_taxonomy() {
        let address = sample_address(0x44);

        let unsupported = RefusalKind::from_construction_refusal(
            &ConstructionRefusal::UnsupportedFamily {
                address,
                detail: "no rung".to_owned(),
            },
            Some(RegisteredPoolFamily::V3),
        );
        assert_tags(
            &unsupported,
            Some(RegisteredPoolFamily::V3),
            RegistrationPhase::Build,
        );
        assert!(matches!(
            unsupported,
            RefusalKind::UnsupportedFamily {
                subject: UnsupportedSubject::Pool { .. },
                ..
            }
        ));

        let skipped = RefusalKind::from_construction_refusal(
            &ConstructionRefusal::Skipped(BuildFailure::HighFee),
            Some(RegisteredPoolFamily::V3),
        );
        assert_tags(
            &skipped,
            Some(RegisteredPoolFamily::V3),
            RegistrationPhase::Build,
        );
        assert_eq!(build_failure(&skipped), Some(&BuildFailure::HighFee));
    }

    #[test]
    #[expect(clippy::expect_used)] // the mapper's carried failure payload is asserted present
    fn builder_mapper_carries_the_typed_refusal() {
        type BuilderFailureCase = (PoolBuilderError, fn(&BuildFailure) -> bool);
        let address = sample_address(0x44);
        let build = RegistrationPhase::Build;

        // The loud class: identity and family-identity refusals.
        let create2 = RefusalKind::from_builder_error(
            &PoolBuilderError::Identity(Box::new(IdentityDivergence::Create2(address_mismatch()))),
            Some(RegisteredPoolFamily::V2),
            build,
        );
        assert_tags(
            &create2,
            Some(RegisteredPoolFamily::V2),
            RegistrationPhase::Identity,
        );
        let (declared, computed, _) = create2_shape(&create2).unwrap();
        assert_eq!(declared, address_mismatch().expected);
        assert_eq!(computed, address_mismatch().computed);

        let unknown_variant = RefusalKind::from_builder_error(
            &PoolBuilderError::UnknownVariant { factory: address },
            None,
            build,
        );
        assert_tags(&unknown_variant, None, build);
        assert!(matches!(
            unknown_variant,
            RefusalKind::UnsupportedFamily {
                subject: UnsupportedSubject::Factory { .. },
                ..
            }
        ));

        let unknown_identity = RefusalKind::from_builder_error(
            &PoolBuilderError::UnknownPoolIdentity { address },
            None,
            build,
        );
        assert_tags(&unknown_identity, None, build);
        assert!(matches!(
            unknown_identity,
            RefusalKind::UnsupportedFamily {
                subject: UnsupportedSubject::UnknownIdentity { .. },
                ..
            }
        ));

        // The skip class: every other builder failure keeps its own typed
        // arm (the facts the shell's builder-error rendering interpolates)
        // and stays retryable on the build-refusal taxonomy.
        let cases: [BuilderFailureCase; 8] = [
            (
                PoolBuilderError::Rpc(ProviderError::Timeout {
                    message: "blip".to_owned(),
                }),
                |failure| matches!(failure, BuildFailure::Rpc(_)),
            ),
            (
                PoolBuilderError::Spec(SpecRefusal::CurveBrokenPool),
                |failure| matches!(failure, BuildFailure::Spec(SpecRefusal::CurveBrokenPool)),
            ),
            (
                PoolBuilderError::Decoding {
                    message: "bad bytes".to_owned(),
                },
                |failure| matches!(failure, BuildFailure::Decoding(_)),
            ),
            (
                PoolBuilderError::Db(DbError::Decode("bad row".to_owned())),
                |failure| matches!(failure, BuildFailure::Db(_)),
            ),
            (
                PoolBuilderError::MissingIdentity {
                    message: "incomplete key".to_owned(),
                },
                |failure| matches!(failure, BuildFailure::MissingIdentity(_)),
            ),
            (
                PoolBuilderError::TickAssembly(TickMapAssemblyError::Chain(
                    BootstrapTickError::Rpc,
                )),
                |failure| matches!(failure, BuildFailure::TickAssembly(_)),
            ),
            (
                PoolBuilderError::TickAssembly(TickMapAssemblyError::ChainUnavailable),
                |failure| matches!(failure, BuildFailure::TickAssembly(_)),
            ),
            (
                PoolBuilderError::TickAssembly(TickMapAssemblyError::InconsistentTickMap {
                    word: 0,
                    bit: 1,
                    tick: 60,
                    bitmap_bit: false,
                    row_gross_positive: true,
                }),
                |failure| matches!(failure, BuildFailure::TickAssembly(_)),
            ),
        ];
        for (err, expected) in cases {
            let kind = RefusalKind::from_builder_error(&err, Some(RegisteredPoolFamily::V3), build);
            assert_tags(&kind, Some(RegisteredPoolFamily::V3), build);
            let failure = build_failure(&kind).expect("a builder failure is carried");
            assert!(expected(failure), "typed arm preserved: {err:?}");
            assert!(
                !RegistrationLedger::classify_build_refusal(failure, PoolKind::V3, None).stable,
                "a builder I/O failure stays retryable: {err:?}"
            );
        }
    }

    #[test]
    fn register_mapper_carries_the_insert_refusal() {
        let address = sample_address(0x44);

        // The address-keyed families share the two-variant insert shape.
        let spec_cases = [
            (
                RefusalKind::from_register_v2_error(&RegisterV2PoolError::SpecViolation(
                    spec_violation(),
                )),
                RegisteredPoolFamily::V2,
            ),
            (
                RefusalKind::from_register_v3_error(&RegisterV3PoolError::SpecViolation(
                    spec_violation(),
                )),
                RegisteredPoolFamily::V3,
            ),
            (
                RefusalKind::from_register_aerodrome_error(
                    &RegisterAerodromePoolError::SpecViolation(spec_violation()),
                ),
                RegisteredPoolFamily::AerodromeV2,
            ),
        ];
        for (kind, family) in spec_cases {
            assert_tags(&kind, Some(family), RegistrationPhase::Register);
            assert!(matches!(
                register_reason(&kind),
                Some(RegisterRefusal::SpecViolation { .. })
            ));
        }

        let already =
            RefusalKind::from_register_v2_error(&RegisterV2PoolError::AlreadyRegistered {
                address,
            });
        assert!(matches!(
            register_reason(&already),
            Some(RegisterRefusal::AlreadyRegistered {
                address: Some(found),
                ..
            }) if *found == address
        ));
    }

    #[test]
    fn verify_constructors_carry_the_fatal_classification() {
        let verify = RegistrationPhase::Verify;

        let mismatch = RefusalKind::verify_failed(
            verify,
            true,
            VerifyOrigin::Mismatch,
            "tick 1 disagrees".to_owned(),
        );
        assert_tags(&mismatch, None, verify);
        assert_eq!(
            verify_failure(&mismatch),
            Some((true, VerifyOrigin::Mismatch)),
            "a map divergence is the fatal tripwire"
        );

        let transport = RefusalKind::verify_failed(
            verify,
            false,
            VerifyOrigin::Transport,
            "timeout".to_owned(),
        );
        assert_tags(&transport, None, verify);
        assert_eq!(
            verify_failure(&transport),
            Some((false, VerifyOrigin::Transport)),
            "a read failure stays retryable"
        );

        // The lifecycle wrapper preserves the verify classification and names
        // its own phase.
        let lifecycle = RefusalKind::verify_failed(
            RegistrationPhase::Lifecycle,
            true,
            VerifyOrigin::Mismatch,
            "tick 1 disagrees".to_owned(),
        );
        assert_tags(&lifecycle, None, RegistrationPhase::Lifecycle);
        assert_eq!(
            verify_failure(&lifecycle),
            Some((true, VerifyOrigin::Mismatch))
        );

        for reason in [
            LifecyclePrecondition::MissingVerifyProvider,
            LifecyclePrecondition::MissingTickSpacing,
        ] {
            let kind = RefusalKind::lifecycle_precondition(reason);
            assert_tags(&kind, None, RegistrationPhase::Lifecycle);
            assert_eq!(lifecycle_precondition(&kind), Some(reason));
        }

        // The snapshot verify pipeline's origins, variant by variant.
        for (fatal, origin) in [
            (true, VerifyOrigin::ProtocolViolation),
            (true, VerifyOrigin::Mismatch),
            (false, VerifyOrigin::ProviderConstruction),
            (false, VerifyOrigin::Transport),
            (true, VerifyOrigin::Other),
        ] {
            let kind = RefusalKind::verify_failed(verify, fatal, origin, String::new());
            assert_tags(&kind, None, verify);
            assert_eq!(verify_failure(&kind), Some((fatal, origin)), "{origin:?}");
        }

        let not_configured =
            RefusalKind::verify_not_configured(verify, "set the rpc url".to_owned());
        assert_tags(&not_configured, None, verify);
        assert!(matches!(
            not_configured,
            RefusalKind::VerifyNotConfigured { .. }
        ));
    }

    #[test]
    fn snapshot_load_constructor_carries_the_reason() {
        let db = RefusalKind::snapshot_load_failed(SnapshotLoadReason::Db {
            detail: DbError::Decode("bad row".to_owned()).to_string(),
        });
        assert_tags(&db, None, RegistrationPhase::Snapshot);
        assert!(matches!(
            snapshot_reason(&db),
            Some(SnapshotLoadReason::Db { .. })
        ));

        let hex = RefusalKind::snapshot_load_failed(SnapshotLoadReason::Decode {
            detail: HexError::InvalidHex("zz".to_owned()).to_string(),
        });
        assert_tags(&hex, None, RegistrationPhase::Snapshot);
        assert!(matches!(
            snapshot_reason(&hex),
            Some(SnapshotLoadReason::Decode { .. })
        ));

        let range = RefusalKind::snapshot_load_failed(SnapshotLoadReason::OutOfRange {
            detail: "liquidity out of range".to_owned(),
        });
        assert_tags(&range, None, RegistrationPhase::Snapshot);
        assert!(matches!(
            snapshot_reason(&range),
            Some(SnapshotLoadReason::OutOfRange { .. })
        ));
    }

    #[test]
    fn build_refusals_carry_the_stable_vs_transient_taxonomy() {
        for (failure, stable) in [
            (BuildFailure::HookedPool, true),
            (BuildFailure::DynamicFee, true),
            (BuildFailure::HighFee, true),
            (BuildFailure::Rpc("blip".to_owned()), false),
            (BuildFailure::Spec(SpecRefusal::V2Reserve), false),
            (BuildFailure::Decoding("bad bytes".to_owned()), false),
            (BuildFailure::Db("bad row".to_owned()), false),
            (
                BuildFailure::MissingIdentity("incomplete".to_owned()),
                false,
            ),
            (BuildFailure::TickAssembly("desync".to_owned()), false),
            (BuildFailure::Transient("blip".to_owned()), false),
        ] {
            let kind = RefusalKind::from_construction_refusal(
                &ConstructionRefusal::Skipped(failure.clone()),
                Some(RegisteredPoolFamily::V2),
            );
            assert_eq!(build_failure(&kind), Some(&failure));
            assert_eq!(
                RegistrationLedger::classify_build_refusal(&failure, PoolKind::V2, None).stable,
                stable,
                "the carried failure classifies through the ledger's one taxonomy"
            );
        }
    }

    // ── ADR-078 D3 step 2: the write door's verdict folds losslessly ─────
    //
    // The door hands the ledger the shared register-refusal payload (the same
    // type the direct-entry mappers build), so the fold must land EXACTLY the
    // `RefusalKind` those mappers produce: a converged caller observes the
    // same refusal the direct path returned.

    /// Fold a refused door verdict and unwrap the ledger refusal.
    #[expect(clippy::expect_used)] // the refused fold is asserted to refuse by construction
    fn fold_refusal(family: RegisteredPoolFamily, refusal: RegisterRefusal) -> RefusalKind {
        RefusalKind::from_registration_verdict(RegistrationVerdict::Refused { family, refusal })
            .expect_err("a refused verdict folds to the register refusal")
    }

    #[test]
    fn the_door_verdict_folds_v2_onto_the_direct_entrys_refusal() {
        let address = Address::from([0x11u8; 20]);
        assert_eq!(
            fold_refusal(
                RegisteredPoolFamily::V2,
                RegisterRefusal::AlreadyRegistered {
                    address: Some(address),
                    pool_key: None,
                    detail: format!("{:?}", RegisterV2PoolError::AlreadyRegistered { address }),
                },
            ),
            RefusalKind::from_register_v2_error(&RegisterV2PoolError::AlreadyRegistered {
                address
            }),
        );
        assert_eq!(
            fold_refusal(
                RegisteredPoolFamily::V2,
                RegisterRefusal::SpecViolation {
                    detail: spec_violation().to_string(),
                },
            ),
            RefusalKind::from_register_v2_error(&RegisterV2PoolError::SpecViolation(
                spec_violation()
            )),
        );
    }

    #[test]
    fn the_door_verdict_folds_v3_onto_the_direct_entrys_refusal() {
        let address = Address::from([0x22u8; 20]);
        assert_eq!(
            fold_refusal(
                RegisteredPoolFamily::V3,
                RegisterRefusal::AlreadyRegistered {
                    address: Some(address),
                    pool_key: None,
                    detail: format!("{:?}", RegisterV3PoolError::AlreadyRegistered { address }),
                },
            ),
            RefusalKind::from_register_v3_error(&RegisterV3PoolError::AlreadyRegistered {
                address
            }),
        );
        assert_eq!(
            fold_refusal(
                RegisteredPoolFamily::V3,
                RegisterRefusal::SpecViolation {
                    detail: spec_violation().to_string(),
                },
            ),
            RefusalKind::from_register_v3_error(&RegisterV3PoolError::SpecViolation(
                spec_violation()
            )),
        );
    }

    #[test]
    fn the_door_verdict_folds_aerodrome_onto_the_direct_entrys_refusal() {
        let address = Address::from([0xaeu8; 20]);
        assert_eq!(
            fold_refusal(
                RegisteredPoolFamily::AerodromeV2,
                RegisterRefusal::AlreadyRegistered {
                    address: Some(address),
                    pool_key: None,
                    detail: format!(
                        "{:?}",
                        RegisterAerodromePoolError::AlreadyRegistered { address }
                    ),
                },
            ),
            RefusalKind::from_register_aerodrome_error(
                &RegisterAerodromePoolError::AlreadyRegistered { address }
            ),
        );
        assert_eq!(
            fold_refusal(
                RegisteredPoolFamily::AerodromeV2,
                RegisterRefusal::SpecViolation {
                    detail: spec_violation().to_string(),
                },
            ),
            RefusalKind::from_register_aerodrome_error(&RegisterAerodromePoolError::SpecViolation(
                spec_violation()
            )),
        );
    }

    #[test]
    fn the_door_verdict_folds_v4_onto_the_ledger_refusal() {
        let (manager, pool_id) = (Address::from([0x44u8; 20]), [0xeeu8; 32]);
        let v4_duplicate = RegisterV4PoolError::AlreadyRegistered {
            pool_manager: manager,
            pool_id,
        };
        let cases = [
            (
                RegisterRefusal::Admission {
                    failure: BuildFailure::HookedPool,
                    hook_flags: Some(0x00C0),
                    fee: None,
                },
                RegisterRefusal::Admission {
                    failure: BuildFailure::HookedPool,
                    hook_flags: Some(0x00C0),
                    fee: None,
                },
            ),
            (
                RegisterRefusal::Admission {
                    failure: BuildFailure::DynamicFee,
                    hook_flags: None,
                    fee: Some(0x0080_0000),
                },
                RegisterRefusal::Admission {
                    failure: BuildFailure::DynamicFee,
                    hook_flags: None,
                    fee: Some(0x0080_0000),
                },
            ),
            (
                RegisterRefusal::Admission {
                    failure: BuildFailure::HighFee,
                    hook_flags: None,
                    fee: Some(1_000_001),
                },
                RegisterRefusal::Admission {
                    failure: BuildFailure::HighFee,
                    hook_flags: None,
                    fee: Some(1_000_001),
                },
            ),
            (
                RegisterRefusal::AlreadyRegistered {
                    address: None,
                    pool_key: Some((manager, pool_id)),
                    detail: format!("{v4_duplicate:?}"),
                },
                RegisterRefusal::AlreadyRegistered {
                    address: None,
                    pool_key: Some((manager, pool_id)),
                    detail: format!("{v4_duplicate:?}"),
                },
            ),
            (
                RegisterRefusal::SpecViolation {
                    detail: spec_violation().to_string(),
                },
                RegisterRefusal::SpecViolation {
                    detail: spec_violation().to_string(),
                },
            ),
        ];
        for (door_refusal, ledger_reason) in cases {
            let kind = fold_refusal(RegisteredPoolFamily::V4, door_refusal);
            assert_tags(
                &kind,
                Some(RegisteredPoolFamily::V4),
                RegistrationPhase::Register,
            );
            assert_eq!(
                kind,
                RefusalKind::RegisterRefused {
                    family: Some(RegisteredPoolFamily::V4),
                    phase: RegistrationPhase::Register,
                    reason: ledger_reason,
                },
            );
        }
    }

    #[test]
    fn the_door_folds_the_balance_vector_duplicate_onto_the_typed_refusal() {
        // The state inserts for these families panic on a duplicate; the door
        // pre-checks and refuses AS DATA — the fold carries the door's own
        // rendering (there is no legacy mapper to equal).
        for family in [
            RegisteredPoolFamily::Curve,
            RegisteredPoolFamily::BalancerWeighted,
            RegisteredPoolFamily::BalancerStable,
        ] {
            let address = Address::from([0x71u8; 20]);
            let kind = fold_refusal(
                family,
                RegisterRefusal::AlreadyRegistered {
                    address: Some(address),
                    pool_key: None,
                    detail: format!("pool already registered: {address}"),
                },
            );
            assert_tags(&kind, Some(family), RegistrationPhase::Register);
            assert!(matches!(
                register_reason(&kind),
                Some(RegisterRefusal::AlreadyRegistered {
                    address: Some(hit),
                    pool_key: None,
                    detail,
                }) if *hit == address && *detail == format!("pool already registered: {address}")
            ));
        }
    }

    #[test]
    fn the_door_registered_verdict_folds_to_the_pool_id_and_carries_the_family_tag() {
        for family in [
            RegisteredPoolFamily::V2,
            RegisteredPoolFamily::V3,
            RegisteredPoolFamily::V4,
            RegisteredPoolFamily::Curve,
            RegisteredPoolFamily::BalancerWeighted,
            RegisteredPoolFamily::BalancerStable,
            RegisteredPoolFamily::AerodromeV2,
        ] {
            assert_eq!(
                RefusalKind::from_registration_verdict(RegistrationVerdict::registered(7, family)),
                Ok(7),
            );
            let kind = fold_refusal(
                family,
                RegisterRefusal::SpecViolation {
                    detail: "x".to_owned(),
                },
            );
            assert_tags(&kind, Some(family), RegistrationPhase::Register);
        }
    }
}
