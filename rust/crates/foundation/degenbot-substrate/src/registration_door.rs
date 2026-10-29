//! The family write door (ADR-078 D1) — ONE registration request, ONE
//! verdict, family as data.
//!
//! Mirrors the read door's shape (`degenbot_pools::families::door`): a single
//! [`PoolRegistration`] request enum whose per-family params ride as additive
//! variants, and a single [`RegistrationVerdict`] outcome. The per-family
//! orchestration modules keep their mechanics behind the door — construction,
//! spec and admission refusals — while the duplicate decision is the pool
//! table's alone, so this module only dispatches and folds.
//!
//! Updates are deliberately NOT door requests: an update's dispatch axis is
//! `(event kind, key kind, phase, presence)` — routed by the live path's
//! total routing table into `RouteAction`/`ApplyOutcome`, with the
//! unregistered case a documented no-op — not pool family, so a `PoolUpdate`
//! variant enum would split one shared arm and merge two key axes. The
//! update path's own entry is `Bot::refresh_pool`.
//!
//! # Refusal vocabulary
//!
//! The refusal payload is the foundation crate's ONE shared vocabulary:
//! `degenbot_pools::registration_refusal` owns `RegisterRefusal` (the
//! `AlreadyRegistered` / `SpecViolation` / `Admission` shapes) and the
//! `BuildFailure` taxonomy its admission arm classifies through, derived from
//! each family's typed `Register*PoolError` by one `From` impl per family.
//! The engine's registration outcome ledger in `degenbot-bot` stores that
//! exact type, so its `RefusalKind::RegisterRefused` fold is structural —
//! family and phase are fixed at the fold (`family` from the verdict,
//! `RegistrationPhase::Register`) — rather than a field-for-field twin joined
//! by hand-written `From` impls.

use degenbot_pools::RegisterAerodromeV2PoolParams;
use degenbot_pools::RegisterBalancerStablePoolParams;
use degenbot_pools::RegisterBalancerWeightedPoolParams;
use degenbot_pools::RegisterCurvePoolParams;
use degenbot_pools::RegisterRefusal;
use degenbot_pools::RegisterV2PoolParams;
use degenbot_pools::RegisterV3PoolParams;
use degenbot_pools::RegisterV4PoolParams;
use degenbot_pools::RegisteredPoolFamily;

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
        /// The typed refusal — the shared register-refusal vocabulary.
        refusal: RegisterRefusal,
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
    pub const fn refused(family: RegisteredPoolFamily, refusal: RegisterRefusal) -> Self {
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

    /// The assigned pool id, or the refusal — the shape a family arm returns
    /// when it has no per-family error type of its own and speaks the shared
    /// refusal vocabulary directly.
    ///
    /// # Errors
    ///
    /// The verdict's typed refusal.
    pub fn into_result(self) -> Result<u64, RegisterRefusal> {
        match self {
            Self::Registered { pool_id, .. } => Ok(pool_id),
            Self::Refused { refusal, .. } => Err(refusal),
        }
    }
}

impl BotState {
    /// Register a pool through the one family write door (ADR-078 D1).
    ///
    /// Dispatches to the structural family's `register_*` arm and folds its
    /// result into the one [`RegistrationVerdict`]. The arms own construction
    /// and their spec/admission refusals; the duplicate is not theirs to
    /// answer — every arm hands its constructed entry to the pool table, whose
    /// `register` is the one place a key the table already holds is refused.
    /// So every family's refusal crosses the door as data, and a duplicate can
    /// neither panic nor be decided twice.
    #[must_use]
    pub fn register_pool(&mut self, registration: PoolRegistration) -> RegistrationVerdict {
        match registration {
            PoolRegistration::V2(params) => match self.register_v2_pool(&params) {
                Ok(pool_id) => RegistrationVerdict::registered(pool_id, RegisteredPoolFamily::V2),
                Err(err) => RegistrationVerdict::refused(
                    RegisteredPoolFamily::V2,
                    RegisterRefusal::from(&err),
                ),
            },
            PoolRegistration::V3(params) => match self.register_v3_pool(&params) {
                Ok(pool_id) => RegistrationVerdict::registered(pool_id, RegisteredPoolFamily::V3),
                Err(err) => RegistrationVerdict::refused(
                    RegisteredPoolFamily::V3,
                    RegisterRefusal::from(&err),
                ),
            },
            PoolRegistration::V4(params) => match self.register_v4_pool(&params) {
                Ok(pool_id) => RegistrationVerdict::registered(pool_id, RegisteredPoolFamily::V4),
                Err(err) => RegistrationVerdict::refused(
                    RegisteredPoolFamily::V4,
                    RegisterRefusal::from(&err),
                ),
            },
            PoolRegistration::Curve(params) => match self.register_curve_pool(&params) {
                Ok(pool_id) => {
                    RegistrationVerdict::registered(pool_id, RegisteredPoolFamily::Curve)
                }
                Err(refusal) => RegistrationVerdict::refused(RegisteredPoolFamily::Curve, refusal),
            },
            PoolRegistration::BalancerWeighted(params) => {
                match self.register_balancer_weighted_pool(&params) {
                    Ok(pool_id) => RegistrationVerdict::registered(
                        pool_id,
                        RegisteredPoolFamily::BalancerWeighted,
                    ),
                    Err(refusal) => RegistrationVerdict::refused(
                        RegisteredPoolFamily::BalancerWeighted,
                        refusal,
                    ),
                }
            }
            PoolRegistration::BalancerStable(params) => {
                match self.register_balancer_stable_pool(&params) {
                    Ok(pool_id) => RegistrationVerdict::registered(
                        pool_id,
                        RegisteredPoolFamily::BalancerStable,
                    ),
                    Err(refusal) => {
                        RegistrationVerdict::refused(RegisteredPoolFamily::BalancerStable, refusal)
                    }
                }
            }
            PoolRegistration::AerodromeV2(params) => match self.register_aerodrome_pool(&params) {
                Ok(pool_id) => {
                    RegistrationVerdict::registered(pool_id, RegisteredPoolFamily::AerodromeV2)
                }
                Err(err) => RegistrationVerdict::refused(
                    RegisteredPoolFamily::AerodromeV2,
                    RegisterRefusal::from(&err),
                ),
            },
        }
    }
}
