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
use degenbot_pools::tick_fetch::TickWordFetcher;
use degenbot_pools::v3_state::ClSlotLayout;

use super::builder::{self, PoolBuilderError, V4BuildResult, V4PoolBuildOverrides};
use super::choreography;
use crate::bot_core::construction_io::ConstructionIo;
use crate::bot_core::registration_ledger::BuildFailure;
use crate::bot_core::state_lock::LockSite;
use crate::bot_core::{
    Bot, RegisterV2PoolError, RegisterV3PoolError, RegisterV4PoolError, RegisteredPoolFamily,
};

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

/// One requested pool — the family plus the identity that names it.
#[derive(Debug, Clone, Copy)]
pub enum RequestedPool {
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
    },
}

/// The V3-only construction inputs the other arms ignore (the Python driver
/// threads a lazy tick-word fetcher and a fork slot-layout hint; the pure
/// Rust consumer passes `None`).
#[derive(Default)]
pub struct V3RouteInputs<'a> {
    /// The DB tick-map handle (Tracked arm); `None` seeds Sparse via the
    /// chain arm.
    pub db: Option<&'a dyn TickMapDb>,
    /// The lazy tick-word fetcher stored on the registered V3 pool for
    /// swap-time boundary detection.
    pub fetcher: Option<Arc<dyn TickWordFetcher>>,
    /// Explicit CL slot-layout override (a non-JSON fork deployment the
    /// caller resolved from the DB descriptor); `None` = the builder's
    /// deployment-table resolution wins.
    pub slot_layout: Option<ClSlotLayout>,
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
    /// Skip semantics — the refusal classifies on the build-refusal taxonomy
    /// (a stable pool fact memoizes; a transient failure stays retryable and
    /// is never memoized). The use site folds `BuildFailure` into its ledger
    /// and metric tags unchanged.
    #[error("construction refused (skip): {0:?}")]
    Skipped(BuildFailure),
}

/// The identity of a FRESH build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltIdentity {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    /// The factory the build resolved (`None` for V4 — a manager-hosted pool
    /// has no factory).
    pub factory: Option<Address>,
    /// The V4 on-chain pool id (`None` for the address-keyed families).
    pub v4_pool_id: Option<[u8; 32]>,
}

/// The constructed, registered pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstructedPool {
    /// The pool's engine `pool_id` in the shared `BotState`.
    pub pool_id: u64,
    /// The fresh-build identity; `None` when the registry GET answered — the
    /// already-registered pool's identity lives on the shared state, and the
    /// get half never re-derives it.
    pub built: Option<BuiltIdentity>,
}

/// Construct and register one requested pool — the core's ONE entry.
///
/// # Errors
///
/// Returns [`ConstructionRefusal::UnsupportedFamily`] for the loud-abort
/// class and [`ConstructionRefusal::Skipped`] for everything the taxonomy
/// classifies as a skip.
pub async fn construct_pool(
    bot: &Bot,
    route: &ConstructionRoute,
    request: &RequestedPool,
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
        } => construct_v4(bot, *pool_manager, *pool_id, overrides, v3.db, io, block).await,
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
        .try_registered_v4(pool_manager, pool_id)
        .map(|registered| registered.pool_id)
}

/// Fold an admission result into a registry reuse when the state reports a
/// concurrent `AlreadyRegistered` — a benign race artifact (the pre-check
/// raced a concurrent writer), never a pool fact. A re-read miss stays a
/// transient refusal.
fn fold_already_registered<E: std::fmt::Debug>(
    result: Result<u64, E>,
    is_already_registered: impl FnOnce(&E) -> bool,
    reuse: impl FnOnce() -> Option<u64>,
    map_refusal: impl FnOnce(E) -> ConstructionRefusal,
) -> Result<u64, ConstructionRefusal> {
    match result {
        Ok(pool_id) => Ok(pool_id),
        Err(err) if is_already_registered(&err) => reuse().ok_or_else(|| {
            ConstructionRefusal::Skipped(BuildFailure::Transient(format!(
                "already-registered admission race with no registry entry to reuse ({err:?})"
            )))
        }),
        Err(err) => Err(map_refusal(err)),
    }
}

/// Classify a builder failure. The factory-identity family (unknown preset,
/// unknown identity, CREATE2 contradiction) is the loud-abort class; every
/// other builder failure is I/O-derived and stays a retriable skip.
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
        PoolBuilderError::Create2 => ConstructionRefusal::UnsupportedFamily {
            address,
            detail: "CREATE2 address verification failed".to_owned(),
        },
        other => ConstructionRefusal::Skipped(BuildFailure::Transient(other.to_string())),
    }
}

/// Classify a V3 registration refusal. A spec violation deliberately maps to
/// a TRANSIENT skip here: the V3 use site's existing taxonomy application
/// treats an out-of-spec field as a retriable skip (the registration stage
/// validates live scalars whose reads can straddle a boundary), distinct
/// from the V4 admission taxonomy's stable arms.
fn classify_register_v3(err: RegisterV3PoolError) -> ConstructionRefusal {
    match err {
        RegisterV3PoolError::SpecViolation(v) => ConstructionRefusal::Skipped(
            BuildFailure::Transient(format!("V3 pool registration failed: {v}")),
        ),
        other @ RegisterV3PoolError::AlreadyRegistered { .. } => {
            ConstructionRefusal::Skipped(BuildFailure::Transient(format!("{other:?}")))
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
    let result = bot
        .state_arc()
        .write_at(LockSite::Core)
        .register_v2_pool(&params);
    let pool_id = fold_already_registered(
        result,
        |err| matches!(err, RegisterV2PoolError::AlreadyRegistered { .. }),
        || registered_address_pool(bot, address, RegisteredPoolFamily::V2),
        |err| ConstructionRefusal::Skipped(BuildFailure::Transient(format!("{err:?}"))),
    )?;
    Ok(ConstructedPool {
        pool_id,
        built: Some(BuiltIdentity {
            address,
            token0: params.token0,
            token1: params.token1,
            factory: Some(params.factory),
            v4_pool_id: None,
        }),
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
    if let Some(fetcher) = &v3.fetcher {
        params.fetcher = Some(Arc::clone(fetcher));
    }
    if let Some(slot_layout) = v3.slot_layout {
        params.slot_layout = slot_layout;
    }
    let result = bot
        .state_arc()
        .write_at(LockSite::Core)
        .register_v3_pool(&params);
    let pool_id = fold_already_registered(
        result,
        |err| matches!(err, RegisterV3PoolError::AlreadyRegistered { .. }),
        || registered_address_pool(bot, address, RegisteredPoolFamily::V3),
        classify_register_v3,
    )?;
    Ok(ConstructedPool {
        pool_id,
        built: Some(BuiltIdentity {
            address,
            token0: params.token0,
            token1: params.token1,
            factory: Some(params.factory),
            v4_pool_id: None,
        }),
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
    let identity =
        builder::resolve_v4_identity(bot.chain_id(), pool_manager, pool_id, overrides, io)
            .await
            .map_err(|err| match err {
                // An incomplete identity is a data problem (an in-flight DB
                // row or a caller that under-specified the overrides) —
                // retriable, never a family verdict.
                builder::PoolBuilderError::MissingIdentity { message } => {
                    ConstructionRefusal::Skipped(BuildFailure::Transient(message))
                }
                other => classify_builder_error(other, pool_manager),
            })?;
    let built: V4BuildResult = builder::build_v4(identity, db, io, block)
        .await
        .map_err(|err| classify_builder_error(err, pool_manager))?;
    let result = bot
        .state_arc()
        .write_at(LockSite::Core)
        .register_v4_pool(&built.params);
    let registered = fold_already_registered(
        result,
        |err| matches!(err, RegisterV4PoolError::AlreadyRegistered { .. }),
        || registered_v4_pool(bot, pool_manager, &pool_id),
        |err| ConstructionRefusal::Skipped(BuildFailure::from(err)),
    )?;
    Ok(ConstructedPool {
        pool_id: registered,
        built: Some(BuiltIdentity {
            address: pool_manager,
            token0: identity.currency0,
            token1: identity.currency1,
            factory: None,
            v4_pool_id: Some(pool_id),
        }),
    })
}

#[cfg(test)]
mod route_tests {
    #![expect(clippy::expect_used)] // test assertions read the refusal arms directly
    use super::*;

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
            PoolBuilderError::Create2,
        ] {
            assert!(matches!(
                classify_builder_error(err, address),
                ConstructionRefusal::UnsupportedFamily { .. }
            ));
        }
        for err in [
            PoolBuilderError::Spec,
            PoolBuilderError::Decoding {
                message: "x".to_owned(),
            },
        ] {
            assert!(matches!(
                classify_builder_error(err, address),
                ConstructionRefusal::Skipped(BuildFailure::Transient(_))
            ));
        }
    }

    #[test]
    fn classify_maps_v4_admission_through_the_taxonomy() {
        // The V4 arm reuses the taxonomy's own From<RegisterV4PoolError> —
        // stable admission facts (hook/dynamic fee) classify exactly as the
        // ledger's V4 arms do, never as loud aborts.
        let refusal =
            ConstructionRefusal::Skipped(BuildFailure::from(RegisterV4PoolError::DynamicFee {
                fee: 0x100_000,
            }));
        assert!(matches!(
            refusal,
            ConstructionRefusal::Skipped(BuildFailure::DynamicFee)
        ));
        let high = ConstructionRefusal::Skipped(BuildFailure::from(
            RegisterV4PoolError::FeeExceedsEncoderLimit { fee: 65_536 },
        ));
        assert!(matches!(
            high,
            ConstructionRefusal::Skipped(BuildFailure::HighFee)
        ));
    }

    #[test]
    fn fold_folds_an_admission_race_and_keeps_other_refusals() {
        let folded = fold_already_registered::<RegisterV3PoolError>(
            Err(RegisterV3PoolError::AlreadyRegistered {
                address: Address::new([0x55u8; 20]),
            }),
            |err| matches!(err, RegisterV3PoolError::AlreadyRegistered { .. }),
            || Some(42),
            classify_register_v3,
        )
        .expect("race folds to the winner's pool id");
        assert_eq!(folded, 42);
        let missed = fold_already_registered::<RegisterV3PoolError>(
            Err(RegisterV3PoolError::AlreadyRegistered {
                address: Address::new([0x55u8; 20]),
            }),
            |err| matches!(err, RegisterV3PoolError::AlreadyRegistered { .. }),
            || None,
            classify_register_v3,
        )
        .expect_err("a race fold with no registry entry stays a skip");
        assert!(matches!(missed, ConstructionRefusal::Skipped(_)));
        let refused = fold_already_registered::<RegisterV3PoolError>(
            Err(RegisterV3PoolError::SpecViolation(
                degenbot_pools::spec_bounds::SpecViolation {
                    field: "fee",
                    value: degenbot_pools::spec_bounds::SpecValue::U32(3_000),
                    bound: "uint24",
                },
            )),
            |err| matches!(err, RegisterV3PoolError::AlreadyRegistered { .. }),
            || Some(7),
            classify_register_v3,
        )
        .expect_err("a non-race refusal is not folded");
        assert!(matches!(refused, ConstructionRefusal::Skipped(_)));
    }
}
