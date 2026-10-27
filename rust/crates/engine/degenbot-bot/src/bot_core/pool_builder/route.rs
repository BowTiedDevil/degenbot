//! The construction route — the core's ONE entry from a requested pool to a
//! constructed, registered pool.
//!
//! Three things compose here, each core-owned:
//!
//! 1. **Construction-route order** ([`ConstructionRoute`]) — the ordered
//!    policy the driver resolves and supplies (factory rungs, then the
//!    generic builder). A route rung answers with a TYPED negative that
//!    advances the walk (the pool's on-chain factory does not match the
//!    rung); a route FAILURE is a typed [`ConstructionRefusal`] that ends the
//!    attempt — never a bare exception swallowed into the next rung (the
//!    retired driver-side fallback chain's bug).
//! 2. **Construction identity** — the DB two-step (`resolve_v4_identity`) for
//!    a V4 request; the on-chain immutable read (one batch, shared between
//!    the rung decision and the build) for a V3 request.
//! 3. **Get-or-register into session state** — the registry answers an
//!    already-registered pool without any I/O, a fresh build registers once,
//!    and a concurrent `AlreadyRegistered` folds back to the winner's pool
//!    id (a race artifact, never a pool fact).
//!
//! Failure classification runs on the registration ledger's
//! [`BuildFailure`] taxonomy. One arm is deliberately LOUD (ADR-055 D4, the
//! loud-abort rule): a family-level stable refusal — the route serves no
//! rung for this pool's factory, the factory has no built-in DEX preset, no
//! identity selector answered, or the CREATE2 verification failed — aborts
//! the attempt with [`ConstructionRefusal::UnsupportedFamily`] instead of
//! silently skipping. Everything else stays a skip ([`
//! ConstructionRefusal::Skipped`]) with the taxonomy's stable-vs-transient
//! fact riding [`BuildFailure`] so the use site's ledger/metrics fold it
//! unchanged.

use std::sync::Arc;

use alloy::primitives::Address;
use degenbot_db::snapshot::TickMapDb;
use degenbot_pools::ConcentratedLiquiditySlotLayout;
use degenbot_pools::RegisterRefusal;
use degenbot_pools::TickWordSource;

use super::builder::{self, PoolBuilderError, V4BuildResult, V4PoolBuildOverrides};
use super::choreography;
use super::tick_source;
use crate::bot_core::construction_io::ConstructionIo;
use crate::bot_core::registration_ledger::BuildFailure;
use crate::bot_core::Bot;
use degenbot_core::op_warn;
use degenbot_pools::RegisteredPoolFamily;
use degenbot_substrate::state_lock::LockSite;
use degenbot_substrate::{PoolRegistration, RegistrationVerdict};

/// The resolved construction-route policy: the ordered rungs one construction
/// attempt tries, terminating in the generic builder. A driver VALUE — the
/// cockpit resolves which factories are served and whether the generic rung
/// is armed; the core never invents a route.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConstructionRoute {
    /// The factory rungs, in policy order. A pool whose on-chain `factory()`
    /// matches a rung constructs at that rung.
    pub factories: Vec<Address>,
    /// The generic builder rung: constructs a pool at any factory the core
    /// deployment tables resolve (an unmatched factory builds ad-hoc; a known
    /// factory is CREATE2-verified).
    pub generic: bool,
}

impl ConstructionRoute {
    /// The route with only the generic builder rung — every family the
    /// deployment tables resolve is served.
    #[must_use]
    pub fn generic_only() -> Self {
        Self {
            factories: Vec::new(),
            generic: true,
        }
    }

    /// Whether the route serves a pool at `factory` — the typed rung answer.
    /// A negative is the ONLY thing that refuses the route without a failure:
    /// it is a policy answer (no rung serves this family), not an exception
    /// to swallow.
    #[must_use]
    pub fn serves(&self, factory: Address) -> bool {
        self.generic || self.factories.contains(&factory)
    }
}

/// One requested pool — the family, the identity that names it, and the handle
/// that family's construction arm reads (a family's inputs ride its own variant,
/// never another family's).
#[derive(Clone)]
pub enum RequestedPool<'a> {
    /// A single-address V2 pair.
    V2 { address: Address },
    /// A single-address V3 pool (the construction route's walk arm).
    V3 { address: Address },
    /// A V4 pool keyed by the `(PoolManager, pool_id)` pair; the DB two-step
    /// resolves identity first, the caller-supplied overrides fill the gaps.
    V4 {
        pool_manager: Address,
        pool_id: [u8; 32],
        overrides: V4PoolBuildOverrides,
        /// The DB tick-map handle for the `Tracked` arm; `None` seeds
        /// `Sparse` via the chain arm.
        db: Option<&'a dyn TickMapDb>,
    },
    /// An Aerodrome V2 (Solidly) pair.
    AerodromeV2 { address: Address },
    /// A Curve `StableSwap` pool — the registry/factory addresses the
    /// LP-token and metapool lookups consult (driver values, like the V3
    /// route's factory rungs).
    Curve {
        address: Address,
        registries: Vec<Address>,
    },
    /// A Balancer V2 weighted pool keyed by its singleton Vault.
    BalancerWeighted { vault: Address, address: Address },
    /// A Balancer V2 stable pool keyed by its singleton Vault.
    BalancerStable {
        vault: Address,
        address: Address,
        /// `None` = the shape-derived invariant version wins.
        invariant_version: Option<u8>,
    },
}

impl std::fmt::Debug for RequestedPool<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The tick-map handle and the V4 overrides are not `Debug` (the former
        // is a trait object); the request's identity is what a log line needs.
        match self {
            Self::V2 { address } => f
                .debug_struct("RequestedPool::V2")
                .field("address", address)
                .finish(),
            Self::V3 { address } => f
                .debug_struct("RequestedPool::V3")
                .field("address", address)
                .finish(),
            Self::V4 {
                pool_manager,
                pool_id,
                ..
            } => f
                .debug_struct("RequestedPool::V4")
                .field("pool_manager", pool_manager)
                .field("pool_id", pool_id)
                .finish(),
            Self::AerodromeV2 { address } => f
                .debug_struct("RequestedPool::AerodromeV2")
                .field("address", address)
                .finish(),
            Self::Curve {
                address,
                registries,
            } => f
                .debug_struct("RequestedPool::Curve")
                .field("address", address)
                .field("registries", registries)
                .finish(),
            Self::BalancerWeighted { vault, address } => f
                .debug_struct("RequestedPool::BalancerWeighted")
                .field("vault", vault)
                .field("address", address)
                .finish(),
            Self::BalancerStable {
                vault,
                address,
                invariant_version,
            } => f
                .debug_struct("RequestedPool::BalancerStable")
                .field("vault", vault)
                .field("address", address)
                .field("invariant_version", invariant_version)
                .finish(),
        }
    }
}

/// The V3-only construction inputs — read by the V3 arm alone, so every other
/// [`RequestedPool`] variant pairs with [`Default`] (the Python driver threads
/// a lazy tick-word fetcher and a fork slot-layout hint; the pure Rust consumer
/// passes `None`).
#[derive(Default)]
pub struct V3RouteInputs<'a> {
    /// The DB tick-map handle (Tracked arm); `None` seeds Sparse via the
    /// chain arm.
    pub db: Option<&'a dyn TickMapDb>,
    /// The lazy tick-word fetcher stored on the registered V3 pool for
    /// swap-time boundary detection.
    pub fetcher: Option<Arc<dyn TickWordSource>>,
    /// Explicit concentrated liquidity slot-layout override (a non-JSON fork deployment the
    /// caller resolved from the DB descriptor); `None` = the builder's
    /// deployment-table resolution wins.
    pub slot_layout: Option<ConcentratedLiquiditySlotLayout>,
}

/// The typed refusal of one construction attempt.
///
/// Two classes, and the distinction is the ADR-055 D4 posture: a
/// family-level stable refusal is a LOUD typed abort (the infrastructure
/// cannot serve this pool — silently skipping it would hide the gap
/// forever), while everything else keeps skip semantics with the
/// taxonomy's stable-vs-transient fact riding [`BuildFailure`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConstructionRefusal {
    /// Family-level stable refusal — LOUD typed fatal (ADR-055 D4). No route
    /// rung serves this pool's family, the factory has no built-in DEX
    /// variant preset, no identity selector answered, or the factory's
    /// CREATE2 verification failed. Never memoized as a benign skip: the use
    /// site aborts loudly.
    #[error("unsupported pool family at {address}: {detail}")]
    UnsupportedFamily { address: Address, detail: String },
    /// The identity verdict's divergence (the CREATE2 arm) — LOUD like
    /// [`Self::UnsupportedFamily`] (ADR-055 D4), but carrying the divergence
    /// so the use site's identity rendering
    /// (`RefusalKind::IdentityMismatch` -> plain `ValueError`) is unchanged.
    #[error("pool identity mismatch: {0:?}")]
    IdentityMismatch(Box<super::parity::IdentityDivergence>),
    /// Skip semantics — the refusal classifies on the build-refusal taxonomy
    /// (a stable pool fact memoizes; a transient failure stays retryable and
    /// is never memoized). The use site folds `BuildFailure` into its ledger
    /// and metric tags unchanged.
    #[error("construction refused (skip): {0:?}")]
    Skipped(BuildFailure),
    /// The write door's typed refusal crossing unchanged (ADR-078 D1's
    /// [`RegisterRefusal`]): the use site's registration rendering is the one
    /// home for its message, so the construction door never re-classifies
    /// what the write door already classified.
    #[error("pool registration refused: {0:?}")]
    Registration(RegisterRefusal),
}

/// The identity of a FRESH build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltIdentity {
    pub address: Address,
    /// The pool's token set in canonical order: the pair for the pair-keyed
    /// families, the Vault/coin order for the multi-token ones. The verdict
    /// carries it so a consumer builds token companions off the answer
    /// instead of re-reading the registry or asking the handle by family
    /// (ADR-085 D5).
    pub tokens: Vec<Address>,
    /// The identity echo's variant: the DEX name the builder/deployment table
    /// resolved (the pair-keyed families), the family's own spelling (the
    /// multi-token ones), or empty where the family answers a structured
    /// identity instead (V4). Shaped core-side so the verdict answers the
    /// echo without a second derivation (ADR-085 D2).
    pub variant: String,
    /// The factory the build resolved (`None` for V4 — a manager-hosted pool
    /// has no factory).
    pub factory: Option<Address>,
    /// The V4 on-chain pool id (`None` for the address-keyed families).
    pub v4_pool_id: Option<[u8; 32]>,
}

impl BuiltIdentity {
    /// The pair-keyed families' `(token0, token1)` echo.
    #[must_use]
    pub fn pair(&self) -> Option<(Address, Address)> {
        match self.tokens.as_slice() {
            [token0, token1] => Some((*token0, *token1)),
            _ => None,
        }
    }
}

/// The constructed, registered pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstructedPool {
    /// The pool's engine `pool_id` in the shared `BotState`.
    pub pool_id: u64,
    /// The fresh-build identity; `None` when the registry GET answered — the
    /// already-registered pool's identity lives on the shared state, and the
    /// get half never re-derives it.
    pub built: Option<BuiltIdentity>,
}

/// Construct and register one requested pool — the interior the construction
/// door wraps.
///
/// # Errors
///
/// Returns [`ConstructionRefusal::UnsupportedFamily`] for the loud-abort
/// class and [`ConstructionRefusal::Skipped`] for everything the taxonomy
/// classifies as a skip.
pub(crate) async fn construct_pool(
    bot: &Bot,
    route: &ConstructionRoute,
    request: &RequestedPool<'_>,
    io: &ConstructionIo,
    v3: V3RouteInputs<'_>,
    block: Option<u64>,
) -> Result<ConstructedPool, ConstructionRefusal> {
    match request {
        RequestedPool::V2 { address } => construct_v2(bot, *address, io, block).await,
        RequestedPool::V3 { address } => construct_v3(bot, route, *address, io, &v3, block).await,
        RequestedPool::V4 {
            pool_manager,
            pool_id,
            overrides,
            db,
        } => construct_v4(bot, *pool_manager, *pool_id, overrides, *db, io, block).await,
        RequestedPool::AerodromeV2 { address } => {
            construct_aerodrome_v2(bot, *address, io, block).await
        }
        RequestedPool::Curve {
            address,
            registries,
        } => construct_curve(bot, *address, registries, io, block).await,
        RequestedPool::BalancerWeighted { vault, address } => {
            construct_balancer_weighted(bot, *vault, *address, io, block).await
        }
        RequestedPool::BalancerStable {
            vault,
            address,
            invariant_version,
        } => construct_balancer_stable(bot, *vault, *address, *invariant_version, io, block).await,
    }
}

/// The address-keyed GET half of get-or-register, family-guarded: an address
/// registered under a DIFFERENT family is not an answer (the build proceeds
/// and the registration conflict classifies normally).
fn registered_address_pool(
    bot: &Bot,
    address: Address,
    family: RegisteredPoolFamily,
) -> Option<u64> {
    let (pool_id, registered) = bot
        .state_arc()
        .read_at(LockSite::Core)
        .registered_pool_by_address(&address)?;
    (registered == family).then_some(pool_id)
}

/// The `(PoolManager, pool_id)`-keyed GET half.
fn registered_v4_pool(bot: &Bot, pool_manager: Address, pool_id: &[u8; 32]) -> Option<u64> {
    bot.state_arc()
        .read_at(LockSite::Core)
        .try_registered(pool_manager, pool_id)
        .map(|registered| registered.pool_id)
}

/// Fold the write door's ONE verdict into a registry reuse when the state
/// reports a concurrent `AlreadyRegistered` — a benign race artifact (the
/// pre-check raced a concurrent writer), never a pool fact. A re-read miss
/// stays a transient refusal; every other refusal crosses as DATA
/// ([`ConstructionRefusal::Registration`]), never re-classified here.
fn fold_registration_verdict(
    verdict: RegistrationVerdict,
    reuse: impl FnOnce() -> Option<u64>,
) -> Result<u64, ConstructionRefusal> {
    match verdict {
        RegistrationVerdict::Registered { pool_id, .. } => Ok(pool_id),
        RegistrationVerdict::Refused { refusal, .. }
            if matches!(refusal, RegisterRefusal::AlreadyRegistered { .. }) =>
        {
            reuse().ok_or_else(|| {
                ConstructionRefusal::Skipped(BuildFailure::Transient(format!(
                    "already-registered admission race with no registry entry to reuse ({refusal:?})"
                )))
            })
        }
        RegistrationVerdict::Refused { refusal, .. } => {
            Err(ConstructionRefusal::Registration(refusal))
        }
    }
}

/// Classify a builder failure. The factory-identity family (unknown preset,
/// unknown identity, CREATE2 contradiction) is the loud-abort class; every
/// other builder failure stays a skip whose `BuildFailure` class rides the
/// ledger taxonomy's own stable-vs-transient fact.
fn classify_builder_error(err: PoolBuilderError, address: Address) -> ConstructionRefusal {
    match err {
        PoolBuilderError::UnknownVariant { factory } => ConstructionRefusal::UnsupportedFamily {
            address,
            detail: format!("factory {factory} has no built-in DEX variant preset"),
        },
        PoolBuilderError::UnknownPoolIdentity { .. } => ConstructionRefusal::UnsupportedFamily {
            address,
            detail: "no identity selector answered".to_owned(),
        },
        // The identity verdict's divergence keeps the loud-abort class
        // (ADR-055 D4) and crosses as the identity refusal it is, so the use
        // site renders it exactly as the direct build entries do.
        PoolBuilderError::Identity(divergence) => ConstructionRefusal::IdentityMismatch(divergence),
        // The skip arms carry the taxonomy's OWN classes: the stable-vs-
        // transient fact must reach the ledger/metrics fold unchanged
        // (ADR-055 D4) — a spec refusal keeps its pool-fact reason, a
        // transport/DB/decoding failure keeps its retryable class. The FFI
        // renders each class (`SpecViolationError` / `PoolBuildRpcError` /
        // `PoolBuildDecodeError` / ...), so flattening one into a transient
        // string would rename the refusal at the seam.
        PoolBuilderError::Spec(reason) => ConstructionRefusal::Skipped(BuildFailure::Spec(reason)),
        PoolBuilderError::Rpc(inner) => {
            ConstructionRefusal::Skipped(BuildFailure::Rpc(inner.to_string()))
        }
        PoolBuilderError::Decoding { message } => {
            ConstructionRefusal::Skipped(BuildFailure::Decoding(message))
        }
        PoolBuilderError::Db(inner) => {
            ConstructionRefusal::Skipped(BuildFailure::Db(inner.to_string()))
        }
        PoolBuilderError::MissingIdentity { message } => {
            ConstructionRefusal::Skipped(BuildFailure::MissingIdentity(message))
        }
        PoolBuilderError::TickAssembly(inner) => {
            ConstructionRefusal::Skipped(BuildFailure::TickAssembly(inner.to_string()))
        }
    }
}

/// The V2 arm: registry GET, one builder rung (`build_v2` resolves the DEX
/// preset itself — an unknown factory is the loud-abort class), register +
/// race fold.
async fn construct_v2(
    bot: &Bot,
    address: Address,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<ConstructedPool, ConstructionRefusal> {
    if let Some(pool_id) = registered_address_pool(bot, address, RegisteredPoolFamily::V2) {
        return Ok(ConstructedPool {
            pool_id,
            built: None,
        });
    }
    let params = builder::build_v2(bot.chain_id(), address, io, block)
        .await
        .map_err(|err| classify_builder_error(err, address))?;
    // The minted identity rides the fold; capture it before `params` moves
    // into the write door's request.
    let minted = BuiltIdentity {
        address,
        tokens: vec![params.token0, params.token1],
        variant: params.variant.as_str().to_string(),
        factory: Some(params.factory),
        v4_pool_id: None,
    };
    let verdict = bot
        .state_arc()
        .write_at(LockSite::Core)
        .register_pool(PoolRegistration::V2(Box::new(params)));
    let pool_id = fold_registration_verdict(verdict, || {
        registered_address_pool(bot, address, RegisteredPoolFamily::V2)
    })?;
    Ok(ConstructedPool {
        pool_id,
        built: Some(minted),
    })
}

/// The V3 arm: the construction route's walk. ONE immutable batch serves the
/// rung decision AND the build (the retired driver chain re-ran a full build
/// per rung); an unmatched factory on a generic-armed route builds ad-hoc,
/// and on a generic-less route refuses loudly.
async fn construct_v3(
    bot: &Bot,
    route: &ConstructionRoute,
    address: Address,
    io: &ConstructionIo,
    v3: &V3RouteInputs<'_>,
    block: Option<u64>,
) -> Result<ConstructedPool, ConstructionRefusal> {
    if let Some(pool_id) = registered_address_pool(bot, address, RegisteredPoolFamily::V3) {
        return Ok(ConstructedPool {
            pool_id,
            built: None,
        });
    }
    // The rung decision is ONE `factory()` read — a refusal costs one call,
    // never the full immutable batch (and the decision is a TYPED policy
    // answer, not an exception to swallow). A served rung proceeds to the
    // full build, which reads the immutable batch itself.
    let factory = choreography::fetch_address_returning(io, b"factory()", address, block)
        .await
        .map_err(|err| {
            ConstructionRefusal::Skipped(BuildFailure::Transient(format!("v3 factory read: {err}")))
        })?;
    if !route.serves(factory) {
        return Err(ConstructionRefusal::UnsupportedFamily {
            address,
            detail: format!(
                "factory {factory} matches no construction-route rung ({} factory rungs, generic={})",
                route.factories.len(),
                route.generic
            ),
        });
    }
    let mut params = builder::build_v3(bot.chain_id(), address, v3.db, io, block)
        .await
        .map_err(|err| classify_builder_error(err, address))?;
    // The live-miss backfill source: the driver's fetcher when it supplies one
    // (the legacy Python path), else the core's own source bound to the identity
    // the build just resolved. A pure-Rust consumer supplies nothing and still
    // gets sparse-map backfill.
    params.fetcher = match &v3.fetcher {
        Some(source) => Some(Arc::clone(source)),
        None => Some(tick_source::attached(io)),
    };
    if let Some(slot_layout) = v3.slot_layout {
        params.slot_layout = slot_layout;
    }
    // The minted identity rides the fold; capture it before `params` moves
    // into the write door's request. The variant echo is the deployment
    // table's DEX name for the built factory, the generic spelling otherwise.
    let minted = BuiltIdentity {
        address,
        tokens: vec![params.token0, params.token1],
        variant: degenbot_uniswap::deployments::resolve_dex_name(bot.chain_id(), params.factory)
            .map_or_else(|| "uniswap-v3".to_string(), |d| d.as_str().to_string()),
        factory: Some(params.factory),
        v4_pool_id: None,
    };
    // The registration and the reorg genesis anchor share ONE write region: a
    // reader must never observe a registered pool with no genesis anchor. A
    // reorg restore that targets the registration seed has to find a known
    // state boundary, and an untracked (Sparse) pool journals nothing until
    // that anchor lands.
    let state = bot.state_arc();
    let mut core = state.write_at(LockSite::Core);
    let verdict = core.register_pool(PoolRegistration::V3(Box::new(params)));
    // Only the fresh registration seeds: a concurrent-reuse winner already
    // holds the first registrant's anchor, and the get-or-register early
    // return above never built a pool to anchor.
    let anchor = block.filter(|constructed_at| *constructed_at > 0);
    if let (RegistrationVerdict::Registered { pool_id, .. }, Some(anchor)) = (&verdict, anchor) {
        if let Err(err) = core.seed_genesis_by_pool_id(*pool_id, anchor) {
            // Best-effort by design: the pool IS registered, so a refused seed
            // must not fail the construction — but it is reported, never
            // swallowed, because the pool is left without a reorg anchor.
            op_warn!(
                domain = state,
                pool_id = *pool_id,
                anchor,
                error = %err,
                "V3 genesis seed refused after registration: the pool has no reorg anchor"
            );
        }
    }
    // The reuse fold re-reads the registry, so the write guard is released
    // first (a read acquired under a held write guard would deadlock).
    drop(core);
    let pool_id = fold_registration_verdict(verdict, || {
        registered_address_pool(bot, address, RegisteredPoolFamily::V3)
    })?;
    Ok(ConstructedPool {
        pool_id,
        built: Some(minted),
    })
}

/// The V4 arm: registry GET on the `(manager, id)` pair, the DB two-step
/// identity (`resolve_v4_identity` — manager row → V4 row → per-FK token
/// rows, else the caller overrides), build + register + race fold. The
/// admission refusals classify through the V4 taxonomy
/// (`From<RegisterV4PoolError> for BuildFailure`).
async fn construct_v4(
    bot: &Bot,
    pool_manager: Address,
    pool_id: [u8; 32],
    overrides: &V4PoolBuildOverrides,
    db: Option<&dyn TickMapDb>,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<ConstructedPool, ConstructionRefusal> {
    if let Some(id) = registered_v4_pool(bot, pool_manager, &pool_id) {
        return Ok(ConstructedPool {
            pool_id: id,
            built: None,
        });
    }
    // An incomplete identity classifies through the ONE taxonomy arm
    // (`BuildFailure::MissingIdentity` -> the use site's ValueError) — the
    // same class the retired verb's direct callers saw, never a family
    // verdict and never flattened to transient.
    let identity =
        builder::resolve_v4_identity(bot.chain_id(), pool_manager, pool_id, overrides, io)
            .await
            .map_err(|err| classify_builder_error(err, pool_manager))?;
    // The solver-state gate's StateView registry: the OLD FFI verb made
    // this registration driver-side before the build; the door owns it (the
    // resolved identity names the StateView, so no caller value is needed).
    bot.register_state_view(pool_manager, identity.state_view);
    let mut built: V4BuildResult = builder::build_v4(identity, db, io, block)
        .await
        .map_err(|err| classify_builder_error(err, pool_manager))?;
    // Sparse-map parity (the V4 twin of the V3 door arm): the core attaches
    // its own ConstructionIo-backed tick-word source for the live-miss
    // backfill, so no caller callback is needed.
    built.params.fetcher = Some(tick_source::attached(io));
    let verdict = bot
        .state_arc()
        .write_at(LockSite::Core)
        .register_pool(PoolRegistration::V4(Box::new(built.params)));
    let registered =
        fold_registration_verdict(verdict, || registered_v4_pool(bot, pool_manager, &pool_id))?;
    Ok(ConstructedPool {
        pool_id: registered,
        built: Some(BuiltIdentity {
            address: pool_manager,
            tokens: vec![identity.currency0, identity.currency1],
            variant: String::new(),
            factory: None,
            v4_pool_id: Some(pool_id),
        }),
    })
}

/// The Aerodrome V2 arm: registry GET, the Aerodrome builder (the `stable()` +
/// `getFee()` + reserves + CREATE2 choreography), register + race fold.
async fn construct_aerodrome_v2(
    bot: &Bot,
    address: Address,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<ConstructedPool, ConstructionRefusal> {
    if let Some(pool_id) = registered_address_pool(bot, address, RegisteredPoolFamily::AerodromeV2)
    {
        return Ok(ConstructedPool {
            pool_id,
            built: None,
        });
    }
    let params = builder::build_aerodrome_v2(bot.chain_id(), address, io, block)
        .await
        .map_err(|err| classify_builder_error(err, address))?;
    let minted = BuiltIdentity {
        address,
        tokens: vec![params.token0, params.token1],
        variant: "aerodrome".to_string(),
        factory: Some(params.factory),
        v4_pool_id: None,
    };
    let verdict = bot
        .state_arc()
        .write_at(LockSite::Core)
        .register_pool(PoolRegistration::AerodromeV2(Box::new(params)));
    let pool_id = fold_registration_verdict(verdict, || {
        registered_address_pool(bot, address, RegisteredPoolFamily::AerodromeV2)
    })?;
    Ok(ConstructedPool {
        pool_id,
        built: Some(minted),
    })
}

/// The Curve arm: registry GET, the Curve detection choreography (the
/// driver-supplied registry addresses serve the LP-token + metapool lookups),
/// register + race fold.
async fn construct_curve(
    bot: &Bot,
    address: Address,
    registries: &[Address],
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<ConstructedPool, ConstructionRefusal> {
    if let Some(pool_id) = registered_address_pool(bot, address, RegisteredPoolFamily::Curve) {
        return Ok(ConstructedPool {
            pool_id,
            built: None,
        });
    }
    let params = builder::build_curve_pool(address, registries, io, block)
        .await
        .map_err(|err| classify_builder_error(err, address))?;
    let minted = BuiltIdentity {
        address,
        tokens: params.tokens.clone(),
        variant: "stableswap".to_string(),
        factory: None,
        v4_pool_id: None,
    };
    let verdict = bot
        .state_arc()
        .write_at(LockSite::Core)
        .register_pool(PoolRegistration::Curve(Box::new(params)));
    let pool_id = fold_registration_verdict(verdict, || {
        registered_address_pool(bot, address, RegisteredPoolFamily::Curve)
    })?;
    Ok(ConstructedPool {
        pool_id,
        built: Some(minted),
    })
}

/// The Balancer weighted arm: registry GET, the Vault choreography
/// (`getPoolId` + `getPoolTokens` + `getSwapFeePercentage` +
/// `getNormalizedWeights` + decimals scaling), register + race fold.
async fn construct_balancer_weighted(
    bot: &Bot,
    vault: Address,
    address: Address,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<ConstructedPool, ConstructionRefusal> {
    if let Some(pool_id) =
        registered_address_pool(bot, address, RegisteredPoolFamily::BalancerWeighted)
    {
        return Ok(ConstructedPool {
            pool_id,
            built: None,
        });
    }
    let params = builder::build_balancer_weighted(vault, address, io, block)
        .await
        .map_err(|err| classify_builder_error(err, address))?;
    let minted = BuiltIdentity {
        address,
        tokens: params.tokens.clone(),
        variant: "balancer_weighted".to_string(),
        factory: None,
        v4_pool_id: None,
    };
    let verdict = bot
        .state_arc()
        .write_at(LockSite::Core)
        .register_pool(PoolRegistration::BalancerWeighted(Box::new(params)));
    let pool_id = fold_registration_verdict(verdict, || {
        registered_address_pool(bot, address, RegisteredPoolFamily::BalancerWeighted)
    })?;
    Ok(ConstructedPool {
        pool_id,
        built: Some(minted),
    })
}

/// The Balancer stable arm: registry GET, the Vault choreography (the
/// weighted twin plus `getAmplificationParameter` + BPT/rate-provider
/// resolution), register + race fold.
async fn construct_balancer_stable(
    bot: &Bot,
    vault: Address,
    address: Address,
    invariant_version: Option<u8>,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<ConstructedPool, ConstructionRefusal> {
    if let Some(pool_id) =
        registered_address_pool(bot, address, RegisteredPoolFamily::BalancerStable)
    {
        return Ok(ConstructedPool {
            pool_id,
            built: None,
        });
    }
    let params = builder::build_balancer_stable(vault, address, io, block, invariant_version)
        .await
        .map_err(|err| classify_builder_error(err, address))?;
    let minted = BuiltIdentity {
        address,
        tokens: params.tokens.clone(),
        variant: "balancer_stable".to_string(),
        factory: None,
        v4_pool_id: None,
    };
    let verdict = bot
        .state_arc()
        .write_at(LockSite::Core)
        .register_pool(PoolRegistration::BalancerStable(Box::new(params)));
    let pool_id = fold_registration_verdict(verdict, || {
        registered_address_pool(bot, address, RegisteredPoolFamily::BalancerStable)
    })?;
    Ok(ConstructedPool {
        pool_id,
        built: Some(minted),
    })
}

#[cfg(test)]
mod route_tests {
    #![expect(clippy::expect_used)] // test assertions read the refusal arms directly
    use super::*;
    use alloy::primitives::B256;
    use degenbot_core::errors::ProviderError;
    use degenbot_pools::SpecRefusal;
    use degenbot_uniswap::deployments::AddressMismatch;

    use crate::bot_core::pool_builder::parity;
    use degenbot_pools::RegisterV3PoolError;
    use degenbot_pools::RegisterV4PoolError;

    #[test]
    fn route_serves_answers_the_policy_order() {
        let a = Address::new([0x11u8; 20]);
        let b = Address::new([0x22u8; 20]);
        let route = ConstructionRoute {
            factories: vec![a],
            generic: false,
        };
        assert!(route.serves(a));
        assert!(!route.serves(b));
        assert!(ConstructionRoute::generic_only().serves(b));
    }

    #[test]
    fn classify_maps_factory_failures_loud_and_io_failures_to_skips() {
        // ADR-055 D4: the factory-identity family refuses LOUD; I/O failures
        // stay retriable skips.
        let address = Address::new([0x33u8; 20]);
        for err in [
            PoolBuilderError::UnknownVariant {
                factory: Address::new([0x44u8; 20]),
            },
            PoolBuilderError::UnknownPoolIdentity { address },
        ] {
            assert!(matches!(
                classify_builder_error(err, address),
                ConstructionRefusal::UnsupportedFamily { .. }
            ));
        }
        // The CREATE2 divergence is loud like the factory-identity family but
        // crosses as the identity refusal it is: the use site's identity
        // rendering (`RefusalKind::IdentityMismatch` -> plain `ValueError`)
        // must not change to the unsupported-family RuntimeError.
        let divergence = parity::IdentityDivergence::Create2(AddressMismatch {
            chain_id: 1,
            factory: Address::new([0x55u8; 20]),
            deployer: Address::new([0x55u8; 20]),
            init_hash: B256::ZERO,
            expected: address,
            computed: Address::new([0x66u8; 20]),
        });
        assert!(matches!(
            classify_builder_error(PoolBuilderError::Identity(Box::new(divergence)), address),
            ConstructionRefusal::IdentityMismatch(_)
        ));
        // The skip arms carry the taxonomy's OWN classes (ADR-055 D4): a
        // spec refusal keeps its pool-fact reason and a transport/decoding
        // failure keeps its retryable class — the use site's ledger/metrics
        // fold must see the fact, never one flattened transient string (the
        // FFI renders each class: `SpecViolationError` / `PoolBuildRpcError` /
        // `PoolBuildDecodeError`).
        assert!(matches!(
            classify_builder_error(
                PoolBuilderError::Spec(SpecRefusal::CurveTooFewCoins),
                address
            ),
            ConstructionRefusal::Skipped(BuildFailure::Spec(_))
        ));
        assert!(matches!(
            classify_builder_error(
                PoolBuilderError::Rpc(ProviderError::ConnectionFailed {
                    message: "x".to_owned(),
                }),
                address
            ),
            ConstructionRefusal::Skipped(BuildFailure::Rpc(_))
        ));
        assert!(matches!(
            classify_builder_error(
                PoolBuilderError::Decoding {
                    message: "x".to_owned(),
                },
                address
            ),
            ConstructionRefusal::Skipped(BuildFailure::Decoding(_))
        ));
    }

    #[test]
    fn v4_admission_refusals_cross_the_door_as_data() {
        // The write door's typed refusal crosses the construction door
        // unchanged — the use site's registration rendering (`map_admission` /
        // `map_register_refusal`) is the one home for the messages, so the
        // door never re-classifies what the write door already classified.
        let refusal = RegisterRefusal::from(&RegisterV4PoolError::DynamicFee { fee: 0x100_000 });
        let folded = fold_registration_verdict(
            RegistrationVerdict::refused(RegisteredPoolFamily::V4, refusal.clone()),
            || None,
        )
        .expect_err("a non-race refusal is not folded");
        assert_eq!(folded, ConstructionRefusal::Registration(refusal));
        let high =
            RegisterRefusal::from(&RegisterV4PoolError::FeeExceedsEncoderLimit { fee: 65_536 });
        let folded = fold_registration_verdict(
            RegistrationVerdict::refused(RegisteredPoolFamily::V4, high.clone()),
            || None,
        )
        .expect_err("a non-race refusal is not folded");
        assert_eq!(folded, ConstructionRefusal::Registration(high));
    }

    #[test]
    fn fold_folds_an_admission_race_and_keeps_other_refusals() {
        let race = RegisterRefusal::from(&RegisterV3PoolError::AlreadyRegistered {
            address: Address::new([0x55u8; 20]),
        });
        let folded = fold_registration_verdict(
            RegistrationVerdict::refused(RegisteredPoolFamily::V3, race.clone()),
            || Some(42),
        )
        .expect("race folds to the winner's pool id");
        assert_eq!(folded, 42);
        let missed = fold_registration_verdict(
            RegistrationVerdict::refused(RegisteredPoolFamily::V3, race),
            || None,
        )
        .expect_err("a race fold with no registry entry stays a skip");
        assert!(matches!(missed, ConstructionRefusal::Skipped(_)));
        // A non-race refusal is KEPT: the write door's typed refusal crosses
        // the construction door as data, never re-classified into the
        // build-refusal taxonomy — its message has one home, the use site's
        // registration rendering.
        let spec = RegisterRefusal::from(&RegisterV3PoolError::SpecViolation(
            degenbot_pools::SpecViolation {
                field: "fee",
                value: degenbot_pools::SpecValue::U32(3_000),
                bound: "uint24",
            },
        ));
        let refused = fold_registration_verdict(
            RegistrationVerdict::refused(RegisteredPoolFamily::V3, spec.clone()),
            || Some(7),
        )
        .expect_err("a non-race refusal is not folded");
        assert_eq!(refused, ConstructionRefusal::Registration(spec));
    }
}
