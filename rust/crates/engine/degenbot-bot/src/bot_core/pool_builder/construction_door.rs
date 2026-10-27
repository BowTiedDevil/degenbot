//! The construction door (ADR-078 D2) — ONE construction request, ONE
//! verdict, family as data.
//!
//! The write door's INSERT verb is `degenbot_substrate::PoolRegistration` on
//! `BotState`; construction is its sibling verb, and it lives here because its
//! interior is engine-side: [`route::construct_pool`] walks the
//! driver-resolved [`ConstructionRoute`], resolves construction identity (the
//! V4 DB two-step, the V3 on-chain `factory()` read), gets-or-registers into
//! the shared `BotState`, folds the `AlreadyRegistered` race, and classifies
//! refusals into [`ConstructionRefusal`]. `degenbot-substrate` is the
//! foundation this crate depends on, so the verb cannot be a `BotState` method
//! without an upward dependency; the handle that owns the shared state on this
//! side is [`Bot`].
//!
//! This module only dispatches: it names the family as data (each family's
//! inputs ride its request variant), translates the request into the
//! interior's `(RequestedPool, V3RouteInputs)` pair, and folds the interior's
//! typed result into one [`ConstructionVerdict`]. The interior is
//! `pub(crate)`: [`route::construct_pool`] is reachable only through this door,
//! so no consumer bypasses the verdict fold.

use std::sync::Arc;

use alloy::primitives::Address;
use degenbot_db::snapshot::TickMapDb;
use degenbot_pools::sim::tick_fetch::TickWordSource;
use degenbot_pools::state::v3_state::ConcentratedLiquiditySlotLayout;

use super::builder::V4PoolBuildOverrides;
use super::route::{
    self, ConstructedPool, ConstructionRefusal, ConstructionRoute, RequestedPool, V3RouteInputs,
};
use crate::bot_core::construction_io::ConstructionIo;
use crate::bot_core::pool_builder::parity;
use crate::bot_core::Bot;
use degenbot_pools::sim::registry::RegisteredPoolFamily;

/// The ONE construction-request vocabulary — every structural family's
/// construction, keyed by family, with that family's inputs riding its
/// variant. A caller builds one value and the door dispatches; a new family
/// adds a variant, never a method.
///
/// The per-family inputs are the ones the construction interior consumes: the
/// V3 tick-map handles are meaningless to the V2/V4 arms, so they ride the V3
/// variant instead of being passed alongside every request.
pub enum PoolConstruction<'a> {
    /// A reserve-pair V2 pool (the `build_v2` DEX-preset resolution).
    V2 {
        /// The pool contract address.
        address: Address,
    },
    /// A concentrated-liquidity V3 pool (the construction route's walk).
    V3 {
        /// The pool contract address.
        address: Address,
        /// The DB tick-map handle: a `TickMapDb` hit seeds `Tracked` (feeding
        /// the quarantine→verify lifecycle); `None` seeds `Sparse` via the
        /// chain arm.
        db: Option<&'a dyn TickMapDb>,
        /// The lazy tick-word fetcher stored on the registered V3 pool for
        /// swap-time boundary detection.
        fetcher: Option<Arc<dyn TickWordSource>>,
        /// An explicit concentrated liquidity slot-layout override (a non-JSON fork deployment
        /// the caller resolved from the DB descriptor); `None` = the
        /// builder's deployment-table resolution wins.
        slot_layout: Option<ConcentratedLiquiditySlotLayout>,
    },
    /// A `(PoolManager, pool_id)`-keyed V4 pool (the DB two-step identity,
    /// with the caller's overrides filling the gaps).
    V4 {
        /// The V4 `PoolManager` contract — also the pool's registered
        /// address.
        pool_manager: Address,
        /// The V4 pool id.
        pool_id: [u8; 32],
        /// The caller-supplied identity overrides (the DB-resolution
        /// fallback).
        overrides: V4PoolBuildOverrides,
        /// The DB tick-map handle for the V4 `Tracked` arm; `None` seeds
        /// `Sparse` via the chain arm.
        db: Option<&'a dyn TickMapDb>,
    },
    /// An Aerodrome V2 (Solidly) pair.
    AerodromeV2 {
        /// The pool contract address.
        address: Address,
    },
    /// A Curve `StableSwap` pool (the discovery-only stableswap family with
    /// no `factory()`).
    Curve {
        /// The pool contract address.
        address: Address,
        /// The registry/factory addresses the LP-token and metapool lookups
        /// consult (driver values, like the V3 route's factory rungs).
        registries: Vec<Address>,
    },
    /// A Balancer V2 weighted pool keyed by its singleton Vault.
    BalancerWeighted {
        /// The Balancer V2 singleton Vault contract.
        vault: Address,
        /// The pool contract address.
        address: Address,
    },
    /// A Balancer V2 stable pool keyed by its singleton Vault.
    BalancerStable {
        /// The Balancer V2 singleton Vault contract.
        vault: Address,
        /// The pool contract address.
        address: Address,
        /// `None` = the shape-derived invariant version wins (BPT among the
        /// Vault tokens → V1, else V2).
        invariant_version: Option<u8>,
    },
}

impl PoolConstruction<'_> {
    /// The family this request names — the tag the verdict carries.
    #[must_use]
    pub const fn family(&self) -> RegisteredPoolFamily {
        match self {
            Self::V2 { .. } => RegisteredPoolFamily::V2,
            Self::V3 { .. } => RegisteredPoolFamily::V3,
            Self::V4 { .. } => RegisteredPoolFamily::V4,
            Self::AerodromeV2 { .. } => RegisteredPoolFamily::AerodromeV2,
            Self::Curve { .. } => RegisteredPoolFamily::Curve,
            Self::BalancerWeighted { .. } => RegisteredPoolFamily::BalancerWeighted,
            Self::BalancerStable { .. } => RegisteredPoolFamily::BalancerStable,
        }
    }
}

impl<'a> PoolConstruction<'a> {
    /// Split the door request into the interior's `(RequestedPool,
    /// V3RouteInputs)` pair — the door's ONLY translation. Each family's
    /// handles ride its own request variant; `V3RouteInputs` carries only the
    /// V3 arm's, so every other arm pairs with its default.
    fn into_route_request(self) -> (RequestedPool<'a>, V3RouteInputs<'a>) {
        match self {
            Self::V2 { address } => (RequestedPool::V2 { address }, V3RouteInputs::default()),
            Self::V3 {
                address,
                db,
                fetcher,
                slot_layout,
            } => (
                RequestedPool::V3 { address },
                V3RouteInputs {
                    db,
                    fetcher,
                    slot_layout,
                },
            ),
            Self::V4 {
                pool_manager,
                pool_id,
                overrides,
                db,
            } => (
                RequestedPool::V4 {
                    pool_manager,
                    pool_id,
                    overrides,
                    db,
                },
                V3RouteInputs::default(),
            ),
            Self::AerodromeV2 { address } => (
                RequestedPool::AerodromeV2 { address },
                V3RouteInputs::default(),
            ),
            Self::Curve {
                address,
                registries,
            } => (
                RequestedPool::Curve {
                    address,
                    registries,
                },
                V3RouteInputs::default(),
            ),
            Self::BalancerWeighted { vault, address } => (
                RequestedPool::BalancerWeighted { vault, address },
                V3RouteInputs::default(),
            ),
            Self::BalancerStable {
                vault,
                address,
                invariant_version,
            } => (
                RequestedPool::BalancerStable {
                    vault,
                    address,
                    invariant_version,
                },
                V3RouteInputs::default(),
            ),
        }
    }
}

impl std::fmt::Debug for PoolConstruction<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The V3 tick-map handles and the V4 overrides are not `Debug` (the
        // former are trait objects); the request's identity is what a log
        // line needs.
        match self {
            Self::V2 { address } => f
                .debug_struct("PoolConstruction::V2")
                .field("address", address)
                .finish(),
            Self::V3 { address, .. } => f
                .debug_struct("PoolConstruction::V3")
                .field("address", address)
                .finish(),
            Self::V4 {
                pool_manager,
                pool_id,
                ..
            } => f
                .debug_struct("PoolConstruction::V4")
                .field("pool_manager", pool_manager)
                .field("pool_id", pool_id)
                .finish(),
            Self::AerodromeV2 { address } => f
                .debug_struct("PoolConstruction::AerodromeV2")
                .field("address", address)
                .finish(),
            Self::Curve {
                address,
                registries,
            } => f
                .debug_struct("PoolConstruction::Curve")
                .field("address", address)
                .field("registries", registries)
                .finish(),
            Self::BalancerWeighted { vault, address } => f
                .debug_struct("PoolConstruction::BalancerWeighted")
                .field("vault", vault)
                .field("address", address)
                .finish(),
            Self::BalancerStable {
                vault,
                address,
                invariant_version,
            } => f
                .debug_struct("PoolConstruction::BalancerStable")
                .field("vault", vault)
                .field("address", address)
                .field("invariant_version", invariant_version)
                .finish(),
        }
    }
}

/// The ONE construction verdict — the family tag plus the constructed,
/// registered pool, or the typed refusal. The family rides on both arms as
/// data, so a consumer that receives a verdict never has to re-derive it from
/// the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConstructionVerdict {
    /// The pool was constructed and inserted into the registry — or the
    /// get-or-register half answered with the already-registered pool.
    Constructed {
        /// The registered family.
        family: RegisteredPoolFamily,
        /// The pool id and the fresh-build identity (`built` is `None` when
        /// the registry GET answered).
        pool: ConstructedPool,
    },
    /// The attempt was refused; the family tag rides beside the typed refusal.
    Refused {
        /// The refused family.
        family: RegisteredPoolFamily,
        /// The typed refusal — the loud-abort `UnsupportedFamily` class or a
        /// classified skip.
        refusal: ConstructionRefusal,
    },
}

impl ConstructionVerdict {
    /// A completed construction.
    #[must_use]
    pub const fn constructed(family: RegisteredPoolFamily, pool: ConstructedPool) -> Self {
        Self::Constructed { family, pool }
    }

    /// A typed refusal.
    #[must_use]
    pub const fn refused(family: RegisteredPoolFamily, refusal: ConstructionRefusal) -> Self {
        Self::Refused { family, refusal }
    }

    /// The family this verdict names.
    #[must_use]
    pub const fn family(&self) -> RegisteredPoolFamily {
        match self {
            Self::Constructed { family, .. } | Self::Refused { family, .. } => *family,
        }
    }

    /// The assigned pool id, or `None` when refused.
    #[must_use]
    pub const fn pool_id(&self) -> Option<u64> {
        match self {
            Self::Constructed { pool, .. } => Some(pool.pool_id),
            Self::Refused { .. } => None,
        }
    }

    /// Whether the verdict is a completed construction (or a registry-GET
    /// answer).
    #[must_use]
    pub const fn is_constructed(&self) -> bool {
        matches!(self, Self::Constructed { .. })
    }

    /// The typed refusal, when the attempt was refused.
    #[must_use]
    pub const fn refusal(&self) -> Option<&ConstructionRefusal> {
        match self {
            Self::Constructed { .. } => None,
            Self::Refused { refusal, .. } => Some(refusal),
        }
    }

    /// Fold the verdict into the construction interior's own result shape,
    /// dropping the family tag the caller already holds.
    ///
    /// # Errors
    ///
    /// Returns the [`ConstructionRefusal`] when the verdict is
    /// [`Self::Refused`].
    pub fn into_result(self) -> Result<ConstructedPool, ConstructionRefusal> {
        match self {
            Self::Constructed { pool, .. } => Ok(pool),
            Self::Refused { refusal, .. } => Err(refusal),
        }
    }
}

impl Bot {
    /// Construct and register one pool through the one construction door
    /// (ADR-078 D2).
    ///
    /// Dispatches into [`route::construct_pool`] and folds its typed result
    /// into the one [`ConstructionVerdict`]: the get-or-register half, the
    /// route walk, identity resolution, the `AlreadyRegistered` race fold, and
    /// the refusal classification all stay in the interior. This method is
    /// additive — no existing entry is rewired onto it.
    #[must_use]
    pub async fn construct_pool(
        &self,
        plan: &ConstructionRoute,
        request: PoolConstruction<'_>,
        io: &ConstructionIo,
        block: Option<u64>,
    ) -> ConstructionVerdict {
        let family = request.family();
        let (requested, v3) = request.into_route_request();
        match route::construct_pool(self, plan, &requested, io, v3, block).await {
            Ok(pool) => {
                if let Some(built) = &pool.built {
                    // Mint-site parity guard: the identity the build minted
                    // must equal the one the registry of record now holds for
                    // `pool_id`. A divergence is a core seam bug — refused
                    // here rather than trusted across any tier boundary.
                    if let Some(registered) =
                        parity::registered_address_identity(self, pool.pool_id)
                    {
                        if let (Some(token0), Some(token1)) =
                            (built.tokens.first(), built.tokens.get(1))
                        {
                            let minted = parity::AddressIdentity {
                                token0: *token0,
                                token1: *token1,
                                address: built.address,
                            };
                            if let Err(divergence) =
                                parity::verify_address_identity(minted, registered)
                            {
                                return ConstructionVerdict::refused(
                                    family,
                                    ConstructionRefusal::IdentityMismatch(Box::new(divergence)),
                                );
                            }
                        }
                    }
                    // Telemetry: one Jaeger node per pool construction +
                    // registration (zero-duration span carrying the full
                    // identity).
                    let (token0, token1) = built.pair().unwrap_or_default();
                    let _reg = tracing::info_span!(
                        "degenbot.pool.register",
                        pool.version = family.tag(),
                        pool.address = %built.address.to_checksum(None),
                        token0 = %token0.to_checksum(None),
                        token1 = %token1.to_checksum(None),
                        dex = %built.variant,
                        pool.id = pool.pool_id,
                    )
                    .entered();
                }
                ConstructionVerdict::constructed(family, pool)
            }
            Err(refusal) => ConstructionVerdict::refused(family, refusal),
        }
    }
}
