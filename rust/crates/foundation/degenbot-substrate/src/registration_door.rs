//! The family write door (ADR-078 D1) — ONE registration request, ONE
//! verdict, family as data.
//!
//! Mirrors the read door's shape (`degenbot_pools::families::door`): a single
//! [`PoolRegistration`] request enum whose per-family params ride as additive
//! variants, and a single [`RegistrationVerdict`] outcome. The per-family
//! orchestration modules keep their mechanics behind the door — this module
//! only dispatches, so every existing `register_*` entry compiles and behaves
//! identically.
//!
//! # Refusal vocabulary
//!
//! ADR-077's `RefusalKind` is owned by the registration outcome ledger in
//! `degenbot-bot` (an engine crate), which depends on this foundation crate,
//! so the vocabulary cannot live here without an upward dependency, and no
//! shared lower home carries it. The verdict's refusal arm therefore carries a
//! substrate-local [`RegistrationRefusal`] shaped field-for-field like
//! ADR-077's `RegisterRefusal` (`AlreadyRegistered` / `SpecViolation` /
//! `Admission`); [`AdmissionFailure`] carries the three immutable-pool-fact
//! arms of the `BuildFailure` taxonomy a registration insert can raise. The
//! ledger's `RefusalKind::RegisterRefused` maps onto it losslessly — family
//! and phase are fixed (`family` from the request's variant,
//! `RegistrationPhase::Register`), and each arm's fields are carried verbatim.

use alloy::primitives::Address;
use degenbot_pools::aerodrome_v2_state::{
    RegisterAerodromePoolError, RegisterAerodromeV2PoolParams,
};
use degenbot_pools::balancer_stable_state::RegisterBalancerStablePoolParams;
use degenbot_pools::balancer_weighted_state::RegisterBalancerWeightedPoolParams;
use degenbot_pools::curve_state::RegisterCurvePoolParams;
use degenbot_pools::registry::RegisteredPoolFamily;
use degenbot_pools::v2_state::{RegisterV2PoolError, RegisterV2PoolParams};
use degenbot_pools::v3_state::{RegisterV3PoolError, RegisterV3PoolParams};
use degenbot_pools::v4_state::{RegisterV4PoolError, RegisterV4PoolParams};

use super::BotState;

/// The ONE registration-request vocabulary — every structural family's
/// registration, keyed by family. A caller builds one value and the door
/// dispatches; a new family adds a params variant, never a method.
///
/// The params ride boxed so the request stays pointer-sized (the family params
/// structs carry tick maps, balance vectors, and trait objects); the variant
/// still carries the existing `degenbot-pools` params type.
#[derive(Clone, Debug)]
pub enum PoolRegistration {
    /// A reserve-pair V2 pool.
    V2(Box<RegisterV2PoolParams>),
    /// A concentrated-liquidity V3 pool.
    V3(Box<RegisterV3PoolParams>),
    /// A `(PoolManager, pool_id)`-keyed V4 pool.
    V4(Box<RegisterV4PoolParams>),
    /// A Curve `StableSwap` pool.
    Curve(Box<RegisterCurvePoolParams>),
    /// A Balancer V2 weighted pool.
    BalancerWeighted(Box<RegisterBalancerWeightedPoolParams>),
    /// A Balancer V2 stable pool.
    BalancerStable(Box<RegisterBalancerStablePoolParams>),
    /// An Aerodrome V2 (Solidly) pool.
    AerodromeV2(Box<RegisterAerodromeV2PoolParams>),
}

impl PoolRegistration {
    /// The registered family this request names — the family tag the verdict
    /// carries and the ledger's `RefusalKind` maps onto.
    #[must_use]
    pub const fn family(&self) -> RegisteredPoolFamily {
        match self {
            Self::V2(_) => RegisteredPoolFamily::V2,
            Self::V3(_) => RegisteredPoolFamily::V3,
            Self::V4(_) => RegisteredPoolFamily::V4,
            Self::Curve(_) => RegisteredPoolFamily::Curve,
            Self::BalancerWeighted(_) => RegisteredPoolFamily::BalancerWeighted,
            Self::BalancerStable(_) => RegisteredPoolFamily::BalancerStable,
            Self::AerodromeV2(_) => RegisteredPoolFamily::AerodromeV2,
        }
    }
}

/// The immutable-pool-fact classification of an admission refusal — the three
/// `BuildFailure` arms a registry insert can raise. A fee above the encoder
/// limit is [`Self::HighFee`]; the V4 hook and dynamic-fee facts carry their
/// own counters at the ledger, which is why they are distinct arms rather than
/// one generic refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionFailure {
    /// The V4 hooked-pool admission refusal (reserved at the register entry
    /// since ADR-037; carried so the mapping stays total).
    HookedPool,
    /// The V4 dynamic-fee admission refusal.
    DynamicFee,
    /// A static fee above the executor's encoding limit.
    HighFee,
}

/// The registry-insert refusal, shaped as ADR-077's `RegisterRefusal` so the
/// ledger's `RefusalKind::RegisterRefused` maps onto it losslessly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistrationRefusal {
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
    /// A V4 admission refusal (hooked pool / dynamic fee / encoder limit),
    /// with the facts the refusal's message interpolates.
    Admission {
        /// The admission classification.
        failure: AdmissionFailure,
        /// The hook flags, for the hooked-pool class.
        hook_flags: Option<u16>,
        /// The offending fee, for the dynamic-fee / encoder-limit classes.
        fee: Option<u32>,
    },
}

impl RegistrationRefusal {
    /// A duplicate-address refusal for the families whose register entry
    /// panics rather than returns (Curve, Balancer weighted/stable) — the door
    /// pre-checks the registry so the refusal is typed instead of a panic. The
    /// detail mirrors the register entry's own assertion message.
    fn duplicate_address(address: Address) -> Self {
        Self::AlreadyRegistered {
            address: Some(address),
            pool_key: None,
            detail: format!("pool already registered: {address}"),
        }
    }

    /// The V2 register entry's typed refusal.
    fn from_v2(err: &RegisterV2PoolError) -> Self {
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

    /// The V3 register entry's typed refusal.
    fn from_v3(err: &RegisterV3PoolError) -> Self {
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

    /// The V4 register entry's typed refusal; its admission arms ride the
    /// [`AdmissionFailure`] classification, carrying the facts their messages
    /// interpolate.
    fn from_v4(err: &RegisterV4PoolError) -> Self {
        match err {
            RegisterV4PoolError::HookedPool { hook_flags } => Self::Admission {
                failure: AdmissionFailure::HookedPool,
                hook_flags: Some(*hook_flags),
                fee: None,
            },
            RegisterV4PoolError::DynamicFee { fee } => Self::Admission {
                failure: AdmissionFailure::DynamicFee,
                hook_flags: None,
                fee: Some(*fee),
            },
            RegisterV4PoolError::FeeExceedsEncoderLimit { fee } => Self::Admission {
                failure: AdmissionFailure::HighFee,
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

    /// The Aerodrome register entry's typed refusal.
    fn from_aerodrome(err: &RegisterAerodromePoolError) -> Self {
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

/// The ONE registration verdict — the family tag plus the registered pool id,
/// or the typed refusal. The family rides on both arms as data, so a consumer
/// that receives a verdict never has to re-derive it from the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistrationVerdict {
    /// The pool was constructed and inserted into the registry.
    Registered {
        /// The auto-assigned pool id.
        pool_id: u64,
        /// The registered family.
        family: RegisteredPoolFamily,
    },
    /// The registration was refused; the family tag rides beside the typed
    /// refusal.
    Refused {
        /// The refused family.
        family: RegisteredPoolFamily,
        /// The typed refusal, shaped as ADR-077's `RegisterRefusal`.
        refusal: RegistrationRefusal,
    },
}

impl RegistrationVerdict {
    /// A completed registration.
    #[must_use]
    pub const fn registered(pool_id: u64, family: RegisteredPoolFamily) -> Self {
        Self::Registered { pool_id, family }
    }

    /// A typed refusal.
    #[must_use]
    pub const fn refused(family: RegisteredPoolFamily, refusal: RegistrationRefusal) -> Self {
        Self::Refused { family, refusal }
    }

    /// The assigned pool id, or `None` when refused.
    #[must_use]
    pub const fn pool_id(&self) -> Option<u64> {
        match self {
            Self::Registered { pool_id, .. } => Some(*pool_id),
            Self::Refused { .. } => None,
        }
    }

    /// Whether the verdict is a completed registration.
    #[must_use]
    pub const fn is_registered(&self) -> bool {
        matches!(self, Self::Registered { .. })
    }
}

impl BotState {
    /// Register a pool through the one family write door (ADR-078 D1).
    ///
    /// Dispatches to the structural family's existing `register_*` entry and
    /// folds its typed result into the one [`RegistrationVerdict`]. The
    /// per-family mechanics stay behind the door unchanged; this method is
    /// additive and no existing entry is rewired onto it.
    ///
    /// For the families whose register entry panics on a duplicate address
    /// (Curve, Balancer weighted/stable), the door pre-checks the registry and
    /// answers the typed [`RegistrationRefusal::AlreadyRegistered`] instead,
    /// so every family's refusal crosses the door as data.
    #[must_use]
    pub fn register_pool(&mut self, registration: PoolRegistration) -> RegistrationVerdict {
        match registration {
            PoolRegistration::V2(params) => match self.register_v2_pool(&params) {
                Ok(pool_id) => RegistrationVerdict::registered(pool_id, RegisteredPoolFamily::V2),
                Err(err) => RegistrationVerdict::refused(
                    RegisteredPoolFamily::V2,
                    RegistrationRefusal::from_v2(&err),
                ),
            },
            PoolRegistration::V3(params) => match self.register_v3_pool(&params) {
                Ok(pool_id) => RegistrationVerdict::registered(pool_id, RegisteredPoolFamily::V3),
                Err(err) => RegistrationVerdict::refused(
                    RegisteredPoolFamily::V3,
                    RegistrationRefusal::from_v3(&err),
                ),
            },
            PoolRegistration::V4(params) => match self.register_v4_pool(&params) {
                Ok(pool_id) => RegistrationVerdict::registered(pool_id, RegisteredPoolFamily::V4),
                Err(err) => RegistrationVerdict::refused(
                    RegisteredPoolFamily::V4,
                    RegistrationRefusal::from_v4(&err),
                ),
            },
            PoolRegistration::Curve(params) => {
                if self.registry.pool_addresses.contains_key(&params.address) {
                    return RegistrationVerdict::refused(
                        RegisteredPoolFamily::Curve,
                        RegistrationRefusal::duplicate_address(params.address),
                    );
                }
                RegistrationVerdict::registered(
                    self.register_curve_pool(&params),
                    RegisteredPoolFamily::Curve,
                )
            }
            PoolRegistration::BalancerWeighted(params) => {
                if self.registry.pool_addresses.contains_key(&params.address) {
                    return RegistrationVerdict::refused(
                        RegisteredPoolFamily::BalancerWeighted,
                        RegistrationRefusal::duplicate_address(params.address),
                    );
                }
                RegistrationVerdict::registered(
                    self.register_balancer_weighted_pool(&params),
                    RegisteredPoolFamily::BalancerWeighted,
                )
            }
            PoolRegistration::BalancerStable(params) => {
                if self.registry.pool_addresses.contains_key(&params.address) {
                    return RegistrationVerdict::refused(
                        RegisteredPoolFamily::BalancerStable,
                        RegistrationRefusal::duplicate_address(params.address),
                    );
                }
                RegistrationVerdict::registered(
                    self.register_balancer_stable_pool(&params),
                    RegisteredPoolFamily::BalancerStable,
                )
            }
            PoolRegistration::AerodromeV2(params) => match self.register_aerodrome_pool(&params) {
                Ok(pool_id) => {
                    RegistrationVerdict::registered(pool_id, RegisteredPoolFamily::AerodromeV2)
                }
                Err(err) => RegistrationVerdict::refused(
                    RegisteredPoolFamily::AerodromeV2,
                    RegistrationRefusal::from_aerodrome(&err),
                ),
            },
        }
    }
}
