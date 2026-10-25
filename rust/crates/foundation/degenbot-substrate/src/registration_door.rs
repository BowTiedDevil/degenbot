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

use alloy::primitives::Address;
use degenbot_pools::sim::registration_refusal::RegisterRefusal;
use degenbot_pools::sim::registry::RegisteredPoolFamily;
use degenbot_pools::state::aerodrome_v2_state::RegisterAerodromeV2PoolParams;
use degenbot_pools::state::balancer_stable_state::RegisterBalancerStablePoolParams;
use degenbot_pools::state::balancer_weighted_state::RegisterBalancerWeightedPoolParams;
use degenbot_pools::state::curve_state::RegisterCurvePoolParams;
use degenbot_pools::state::v2_state::RegisterV2PoolParams;
use degenbot_pools::state::v3_state::RegisterV3PoolParams;
use degenbot_pools::state::v4_state::RegisterV4PoolParams;

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

/// A duplicate-address refusal for the families whose register entry panics
/// rather than returns (Curve, Balancer weighted/stable) — the door pre-checks
/// the registry so the refusal is typed instead of a panic. The detail mirrors
/// the register entry's own assertion message.
fn duplicate_address(address: Address) -> RegisterRefusal {
    RegisterRefusal::AlreadyRegistered {
        address: Some(address),
        pool_key: None,
        detail: format!("pool already registered: {address}"),
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
    /// answers the typed [`RegisterRefusal::AlreadyRegistered`] instead,
    /// so every family's refusal crosses the door as data.
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
            PoolRegistration::Curve(params) => {
                if self.registry.pool_addresses.contains_key(&params.address) {
                    return RegistrationVerdict::refused(
                        RegisteredPoolFamily::Curve,
                        duplicate_address(params.address),
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
                        duplicate_address(params.address),
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
                        duplicate_address(params.address),
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
                    RegisterRefusal::from(&err),
                ),
            },
        }
    }
}
