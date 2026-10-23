//! The ONE registration-refusal vocabulary — the payload a refused
//! registration carries, shared by the foundation write door and the engine's
//! outcome ledger.
//!
//! A registration answers one of three ways: the pool is registered (an id),
//! the build was admitted but not built (the build taxonomy), or the insert
//! was refused (the register payload). Those payload shapes are spoken by two
//! layers that cannot see each other:
//!
//! - the family write door in `degenbot-substrate`, which folds a
//!   registration request into a registration verdict without an engine, and
//! - the registration outcome ledger in `degenbot-bot`, which memoizes the
//!   refusal against the pool and renders its bounded tag
//!   (`RefusalKind::RegisterRefused` / `RefusalKind::BuildRefused`).
//!
//! Foundation ← engine is the dependency direction, so a vocabulary both
//! layers speak has to live at or below the foundation crate. This crate is
//! that home: it owns the family tag
//! ([`RegisteredPoolFamily`](crate::registry::RegisteredPoolFamily)), the
//! per-family `Register*PoolError` types the refusals are derived from, and
//! both consumers already depend on it. One owner makes the ledger's fold
//! structural — the door hands the ledger the exact type it stores — instead
//! of a hand-maintained field-for-field twin.
//!
//! What stays engine-side is the *phase* vocabulary: `RegistrationPhase` and
//! `RefusalKind` describe WHEN in the pipeline a refusal arose and which
//! lifecycle arm owns it, which is engine pipeline knowledge. The payload
//! below is phase-free, so no engine type reaches this foundation.

use alloy::primitives::Address;

use crate::aerodrome_v2_state::RegisterAerodromePoolError;
use crate::v2_state::RegisterV2PoolError;
use crate::v3_state::RegisterV3PoolError;
use crate::v4_state::RegisterV4PoolError;

/// Why a registry insert refused.
///
/// Family and phase ride beside this payload (the ledger's `RefusalKind`
/// fixes both when it folds a verdict), so nothing here names a family: the
/// same three shapes serve every family's register entry.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RegisterRefusal {
    /// A pool at this address (or `(pool_manager, pool_id)` key) is already
    /// registered. The address rides when the family keys by address; the V4
    /// `(pool_manager, pool_id)` key rides when the family keys by pair.
    AlreadyRegistered {
        /// The refused pool address, when the family keys by address.
        address: Option<Address>,
        /// The refused V4 `(pool_manager, pool_id)` key, when the family keys
        /// by pair.
        pool_key: Option<(Address, [u8; 32])>,
        /// The source rendering (log-only).
        detail: String,
    },
    /// An out-of-spec field, carrying the `SpecViolation`'s own rendering.
    SpecViolation {
        /// The source rendering (log-only).
        detail: String,
    },
    /// A V4 admission refusal (hooked pool / dynamic fee / encoder limit):
    /// the [`BuildFailure`] taxonomy is its one home, and the facts the
    /// refusal's message interpolates ride beside it.
    Admission {
        /// The admission classification.
        failure: BuildFailure,
        /// The hook flags, for the hooked-pool class.
        hook_flags: Option<u16>,
        /// The offending fee, for the dynamic-fee / encoder-limit classes.
        fee: Option<u32>,
    },
}

impl From<&RegisterV2PoolError> for RegisterRefusal {
    /// The V2 register entry's typed refusal. The `AlreadyRegistered`
    /// rendering is the source error's `Debug` shape, which is the historical
    /// log text.
    fn from(err: &RegisterV2PoolError) -> Self {
        match err {
            RegisterV2PoolError::AlreadyRegistered { address } => Self::AlreadyRegistered {
                address: Some(*address),
                pool_key: None,
                detail: format!("{err:?}"),
            },
            RegisterV2PoolError::SpecViolation(v) => Self::SpecViolation {
                detail: v.to_string(),
            },
        }
    }
}

impl From<&RegisterV3PoolError> for RegisterRefusal {
    /// The V3 register entry's typed refusal.
    fn from(err: &RegisterV3PoolError) -> Self {
        match err {
            RegisterV3PoolError::AlreadyRegistered { address } => Self::AlreadyRegistered {
                address: Some(*address),
                pool_key: None,
                detail: format!("{err:?}"),
            },
            RegisterV3PoolError::SpecViolation(v) => Self::SpecViolation {
                detail: v.to_string(),
            },
        }
    }
}

impl From<&RegisterV4PoolError> for RegisterRefusal {
    /// The V4 register entry's typed refusal; its admission arms ride the
    /// [`BuildFailure`] classification, carrying the facts their messages
    /// interpolate.
    fn from(err: &RegisterV4PoolError) -> Self {
        match err {
            RegisterV4PoolError::HookedPool { hook_flags } => Self::Admission {
                failure: BuildFailure::HookedPool,
                hook_flags: Some(*hook_flags),
                fee: None,
            },
            RegisterV4PoolError::DynamicFee { fee } => Self::Admission {
                failure: BuildFailure::DynamicFee,
                hook_flags: None,
                fee: Some(*fee),
            },
            RegisterV4PoolError::FeeExceedsEncoderLimit { fee } => Self::Admission {
                failure: BuildFailure::HighFee,
                hook_flags: None,
                fee: Some(*fee),
            },
            RegisterV4PoolError::AlreadyRegistered {
                pool_manager,
                pool_id,
            } => Self::AlreadyRegistered {
                address: None,
                pool_key: Some((*pool_manager, *pool_id)),
                detail: format!("{err:?}"),
            },
            RegisterV4PoolError::SpecViolation(v) => Self::SpecViolation {
                detail: v.to_string(),
            },
        }
    }
}

impl From<&RegisterAerodromePoolError> for RegisterRefusal {
    /// The Aerodrome register entry's typed refusal.
    fn from(err: &RegisterAerodromePoolError) -> Self {
        match err {
            RegisterAerodromePoolError::AlreadyRegistered { address } => Self::AlreadyRegistered {
                address: Some(*address),
                pool_key: None,
                detail: format!("{err:?}"),
            },
            RegisterAerodromePoolError::SpecViolation(v) => Self::SpecViolation {
                detail: v.to_string(),
            },
        }
    }
}

/// Why a build refused a pool as out of spec.
///
/// One variant per refusal mechanism the builders raise, so a refused build
/// names its reason ("out of spec (curve broken pool)") instead of reporting
/// one undifferentiated "out of spec": a Curve pool broken beyond repair, a
/// Balancer pool whose swaps are disabled at the contract, a pool exposing
/// fewer than two coins, and a reserve beyond `uint112` are four different
/// facts about four different pools.
///
/// The type lives at this layer because [`BuildFailure`] carries it — the
/// builders that raise it, the ledger that memoizes the refusal, and the FFI
/// that renders the exception all speak this one taxonomy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SpecRefusal {
    /// A reserve above the on-chain `uint112` bound (V2 / Aerodrome V2).
    V2Reserve,
    /// A pool in the known-broken Balancer V2 set — swaps disabled at the
    /// contract, so the pool could never simulate.
    BalancerBrokenPool,
    /// A composable weighted pool: the pool contract is its own BPT and a
    /// Vault token, while `getNormalizedWeights()` covers only the underlying
    /// tokens. The weighted arm holds one weight per Vault token, so a weight
    /// vector shorter than the token vector is a shape it cannot represent.
    BalancerComposableWeighted,
    /// A Balancer pool whose rate provider cannot be read at the pinned block:
    /// the nested pool it points at reverts, or answers unparseable data. A
    /// pool that cannot price its own tokens cannot simulate.
    BalancerRateProviderUnreadable,
    /// A pool in the known-broken Curve V1 set.
    CurveBrokenPool,
    /// A Curve pool whose required getter (`A()` / `fee()` / `admin_fee()`)
    /// reverted: the contract does not implement the interface it claims.
    CurveRequiredReadReverted,
    /// A Curve pool exposing fewer than two coins.
    CurveTooFewCoins,
    /// A Curve pool whose `coins()` and `balances()` lengths disagree, so the
    /// N-consistent register params cannot be assembled.
    CurveCoinsBalancesMismatch,
    /// A Curve pool coin whose `decimals()` reverted.
    CurveCoinDecimalsReverted,
    /// A coin that cannot serve an ERC-20 read at all (no runtime code — an
    /// old vyper pool's ETH sentinel, or a junk `coins()` return).
    NoCodeCoin,
}

impl SpecRefusal {
    /// The bounded reason label: the message suffix a refusal renders and the
    /// tag a metric or log line carries.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::V2Reserve => "v2 reserve out of bounds",
            Self::BalancerBrokenPool => "balancer broken pool",
            Self::BalancerComposableWeighted => "balancer composable weighted pool",
            Self::BalancerRateProviderUnreadable => "balancer rate provider unreadable",
            Self::CurveBrokenPool => "curve broken pool",
            Self::CurveRequiredReadReverted => "curve required read reverted",
            Self::CurveTooFewCoins => "curve too few coins",
            Self::CurveCoinsBalancesMismatch => "curve coins/balances mismatch",
            Self::CurveCoinDecimalsReverted => "curve coin decimals reverted",
            Self::NoCodeCoin => "no-code coin",
        }
    }
}

impl std::fmt::Display for SpecRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The build failure a caller observed, before classification.
///
/// The variants are the *typed* refusals a registration can hit; anything
/// the caller cannot name is [`BuildFailure::Transient`] and stays
/// retryable. The builder's own error type is not `Clone` (this vocabulary
/// is), so its arms ride as the facts the shell's builder-error rendering
/// interpolates.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BuildFailure {
    /// The V4 hooked-pool admission refusal.
    HookedPool,
    /// The V4 dynamic-fee admission refusal.
    DynamicFee,
    /// A fee above the encoder's limit — a stable pool fact for every family.
    HighFee,
    /// A builder RPC / transport failure, with the provider error's rendering.
    Rpc(String),
    /// The builder refused the pool as out of spec, with the reason.
    Spec(SpecRefusal),
    /// A builder decode failure.
    Decoding(String),
    /// A builder DB read failure, with the DB error's rendering.
    Db(String),
    /// An incomplete V4 identity — an intake rejection.
    MissingIdentity(String),
    /// A tracked tick map the intake reconciliation rejected.
    TickAssembly(String),
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
