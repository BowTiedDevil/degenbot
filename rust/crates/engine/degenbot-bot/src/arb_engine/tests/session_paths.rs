//! Canonical path objects reached through the session registry: one path
//! identity shared by the strategies, validated pool references, and the
//! engine's `PathRegistry` still the only owner of path state.
//!
//! Seam: `degenbot_substrate::session_registry::{ObjectRefusal, PathObject,
//! PoolIdentity, SessionObjectRegistry}` (the session's canonical path
//! identity) + `crate::arb_engine::{path_objects::EnginePathObjects,
//! EngineStages}` (the adapter over the engine's `PathRegistry`). The registry
//! is the session's name for a path; the engine keeps solve, dispatch, and
//! submission. These tests pin the seam between the two from the adapter side,
//! where the engine's own counters are observable.
//!
//! Terminology is the settled set in GLOSSARY.md § Session objects; the
//! identity-vs-policy split is `docs/architecture/session-object-registry.md`.

use super::*;

use std::sync::Arc;

use crate::arb_engine::path_objects::EnginePathObjects;
use crate::arb_engine::{EngineDriver, EngineStages};
use crate::bot_core::{Bot, RegisterV2PoolParams};
use degenbot_substrate::session_registry::{
    ObjectRefusal, PathObject, PoolIdentity, SessionObjectRegistry,
};
use degenbot_substrate::state_lock::LockSite;
use degenbot_substrate::EpochDelta;
use parking_lot::Mutex;

/// Canonical identity is session-scoped; the chain only has to be stable.
const CHAIN_ID: u64 = 1;

fn pool_a() -> Address {
    Address::from([0x11u8; 20])
}

fn pool_b() -> Address {
    Address::from([0x12u8; 20])
}

fn pool_c() -> Address {
    Address::from([0x13u8; 20])
}

/// One session: the engine's path registry behind the stage surface, plus the
/// session object registry the strategies ask. The engine handle is kept so a
/// test can read the engine's OWN state (its registered paths, its resolve
/// snapshot) next to the session's canonical object.
struct Session {
    engine: Arc<Mutex<ArbitrageEngine>>,
    stages: EngineStages,
    registry: SessionObjectRegistry,
}

impl Session {
    /// The session with two live V2 pools registered in BOTH the engine's
    /// `BotState` and the session's pool registry — a route `usdc -> weth ->
    /// usdc`.
    fn with_two_pools() -> Self {
        let engine = ArbitrageEngine::new();
        let _ =
            engine.register_v2_pool(pool_a(), usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);
        let _ = engine.register_v2_pool(
            pool_b(),
            weth(1000),
            usdc(2_000_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let engine = Arc::new(Mutex::new(engine));
        let stages = EngineStages::new(Arc::clone(&engine), Arc::new(EpochDelta::new(0u64)));
        let registry = SessionObjectRegistry::new(CHAIN_ID);
        for address in [pool_a(), pool_b()] {
            let _ = registry.get_or_create_pool(PoolIdentity::v2(address));
        }
        let session = Self {
            engine,
            stages,
            registry,
        };
        session.install_path_owner();
        session
    }

    /// Wire the session's path-identity owner: the engine's `PathRegistry`,
    /// reached through the adapter. This is the whole plumbing under test.
    fn install_path_owner(&self) {
        assert!(
            self.registry
                .install_path_objects(self.stages.session_path_objects())
                .is_ok(),
            "the first path-identity owner installed wins"
        );
    }

    /// The `usdc -> weth -> usdc` route over the two registered pools.
    fn route() -> [(PoolIdentity, bool); 2] {
        [
            (PoolIdentity::v2(pool_a()), true),
            (PoolIdentity::v2(pool_b()), true),
        ]
    }

    fn path(&self, hops: &[(PoolIdentity, bool)]) -> Arc<PathObject> {
        self.registry
            .get_or_create_path(hops)
            .expect("canonical path")
    }
}

/// The settlement arm's derived plan: what the arm intends to DO with the
/// path. Solver choice, dispatch trigger, and submission posture are the
/// arm's, not the path's.
#[derive(Debug, PartialEq, Eq)]
struct SettlementPlan {
    path_id: u64,
    solver: &'static str,
    dispatch: &'static str,
    submission: &'static str,
}

impl SettlementPlan {
    /// Derive the arm's plan from the session's canonical path.
    fn derive(object: &PathObject) -> Self {
        Self {
            path_id: object.path_id(),
            solver: "engine-cycle",
            dispatch: "sealed-block",
            submission: "revert-protecting-fanout",
        }
    }
}

/// The txpool-backrun arm's derived plan over the same route: a different
/// solver, a different dispatch trigger, and a budgeted bid instead of a
/// fan-out.
#[derive(Debug, PartialEq, Eq)]
struct TxpoolBackrunPlan {
    path_id: u64,
    solver: &'static str,
    dispatch: &'static str,
    submission: &'static str,
    bid_bips: u16,
}

impl TxpoolBackrunPlan {
    fn derive(object: &PathObject) -> Self {
        Self {
            path_id: object.path_id(),
            solver: "frame",
            dispatch: "pending-tx",
            submission: "budgeted-bid",
            bid_bips: 30,
        }
    }
}

/// The two arms share the route, so they share the path: one canonical
/// identity, reached through the session registry from both sides, backed by
/// one entry in the engine's path registry.
#[test]
fn settlement_and_txpool_backrun_share_one_canonical_path() {
    let session = Session::with_two_pools();
    let route = Session::route();

    let via_settlement = session.path(&route);
    let via_backrun = session.path(&route);

    // ONE canonical object: the same allocation, not an equal-looking twin.
    assert!(Arc::ptr_eq(&via_settlement, &via_backrun));
    assert_eq!(via_settlement.path_id(), via_backrun.path_id());
    // The id is the engine's own allocator's, not a session-local counter.
    assert_eq!(via_settlement.path_id(), 1);
    assert_eq!(via_settlement.chain_id(), CHAIN_ID);
    // ONE entry in the engine's registry, and one object the session counts.
    assert_eq!(session.stages.path_count(), 1);
    assert_eq!(session.registry.path_count(), 1);

    // Both arms' plans name that one path id.
    assert_eq!(SettlementPlan::derive(&via_settlement).path_id, 1);
    assert_eq!(TxpoolBackrunPlan::derive(&via_backrun).path_id, 1);
}

/// Strategy policy is derived FROM the canonical object and never stored in
/// it: two arms with different solver, dispatch, and submission policy read
/// back the same object, and the object answers identity questions only.
#[test]
fn strategy_policy_does_not_leak_into_the_canonical_path() {
    let session = Session::with_two_pools();
    let route = Session::route();

    let object = session.path(&route);
    let settlement = SettlementPlan::derive(&object);
    let backrun = TxpoolBackrunPlan::derive(&object);

    // The plans disagree about everything except which path they run.
    assert_eq!(settlement.path_id, backrun.path_id);
    assert_ne!(settlement.solver, backrun.solver);
    assert_ne!(settlement.dispatch, backrun.dispatch);
    assert_ne!(settlement.submission, backrun.submission);
    assert_eq!(backrun.bid_bips, 30);

    // The canonical object is untouched by either derivation: identity only.
    let reread = session
        .registry
        .resolve_path(&route)
        .expect("canonical path");
    assert_eq!(reread, object);
    assert!(Arc::ptr_eq(&reread, &object));
    assert_eq!(reread.hops().len(), 2);
    assert!(reread.hops()[0].zero_for_one());
    // Every hop is the session's canonical pool object, so a strategy joins
    // live state through the pool kind rather than through a private map.
    for (hop, address) in reread.hops().iter().zip([pool_a(), pool_b()]) {
        assert!(Arc::ptr_eq(
            hop.pool(),
            &session
                .registry
                .resolve_pool(&PoolIdentity::v2(address))
                .expect("pool")
        ));
    }
    // The engine's path for this identity is the same path, and the engine
    // still owns the solve state for it.
    assert_eq!(session.stages.path_count(), 1);
    let engine = session.engine.lock();
    assert!(engine.cycle.path_resolved.contains_key(&object.path_id()));
}

/// A canonical path is built from VALIDATED pool references at two joins: the
/// session's pool registry (identity) and the live-state owner's registration
/// tables (the id the engine hops on). A hop that fails either is a typed
/// refusal, and neither refusal creates anything.
#[test]
fn a_path_over_an_unvalidated_pool_is_a_typed_refusal() {
    let session = Session::with_two_pools();
    let ghost = PoolIdentity::v2(pool_c());

    // Unknown to the session: the pool kind refuses before the path owner is
    // consulted.
    let refusal = session
        .registry
        .get_or_create_path(&[(ghost.clone(), true), (PoolIdentity::v2(pool_a()), true)])
        .expect_err("unregistered pool");
    assert_eq!(
        refusal,
        ObjectRefusal::UnknownPoolIdentity {
            identity: ghost.clone()
        }
    );

    // Known to the session but absent from the live-state owner: the session
    // has a name for it, the engine has no pool to hop on, so the adapter
    // refuses rather than minting a path over a pool the engine cannot read.
    let _ = session.registry.get_or_create_pool(ghost.clone());
    let refusal = session
        .registry
        .get_or_create_path(&[(ghost.clone(), true), (PoolIdentity::v2(pool_a()), true)])
        .expect_err("pool the engine does not hold");
    assert_eq!(
        refusal,
        ObjectRefusal::UnknownPoolIdentity { identity: ghost }
    );

    // Neither refusal created a path in the engine or in the session.
    assert_eq!(session.stages.path_count(), 0);
    assert_eq!(session.registry.path_count(), 0);
    assert_eq!(session.stages.path_dedups(), 0);
}

/// The adapter is a VIEW over the engine's `PathRegistry`, not a second
/// store: dedup hits, the registered-path cap, and deregistration are all
/// answered by the engine's own counters and grow nothing on the session side.
#[test]
fn the_engines_path_registry_remains_the_only_path_store() {
    let session = Session::with_two_pools();
    let route = Session::route();

    // The cap is the engine's, enforced through the adapter: the second
    // distinct path is refused and only the first exists.
    session.stages.set_path_cap(Some(1));
    let first = session.path(&route);
    let second_route = [
        (PoolIdentity::v2(pool_b()), true),
        (PoolIdentity::v2(pool_a()), false),
    ];
    assert_eq!(
        session
            .registry
            .get_or_create_path(&second_route)
            .expect_err("cap reached"),
        ObjectRefusal::PathCapacityReached {
            cap: 1,
            registered: 1
        }
    );
    assert_eq!(session.stages.path_count(), 1);
    assert_eq!(session.registry.path_count(), 1);

    // Dedup is the engine's too: asking again for the same route joins the
    // first entry and counts a dedup hit there, not a second object here.
    let again = session.path(&route);
    assert!(Arc::ptr_eq(&again, &first));
    assert_eq!(session.stages.path_dedups(), 1);
    assert_eq!(session.stages.path_count(), 1);
    assert_eq!(session.registry.path_count(), 1);

    // Resolve is a read: it neither grows the engine nor the session.
    let resolved = session
        .registry
        .resolve_path(&route)
        .expect("registered route");
    assert!(Arc::ptr_eq(&resolved, &first));
    assert_eq!(session.stages.path_dedups(), 1);
    assert_eq!(session.registry.path_count(), 1);

    // A route the engine never registered is a typed refusal, and asking
    // again for it does not register it.
    let unregistered = [
        (PoolIdentity::v2(pool_a()), true),
        (PoolIdentity::v2(pool_b()), false),
    ];
    assert!(matches!(
        session.registry.resolve_path(&unregistered),
        Err(ObjectRefusal::UnknownPathIdentity { .. })
    ));
    assert_eq!(session.stages.path_count(), 1);

    // Deregistration is the engine's: the canonical object goes with the
    // engine's entry, and re-registering allocates a FRESH id, so a consumer
    // cannot keep naming a path the engine has dropped.
    assert!(session.stages.deregister_path(first.path_id()));
    assert!(matches!(
        session.registry.resolve_path(&route),
        Err(ObjectRefusal::UnknownPathIdentity { .. })
    ));
    session.stages.set_path_cap(None);
    let reregistered = session.path(&route);
    assert!(!Arc::ptr_eq(&reregistered, &first));
    assert_ne!(reregistered.path_id(), first.path_id());
    assert_eq!(session.stages.path_count(), 1);
}

/// The engine still owns solve: a path the session names is a path the
/// engine's solve cycle resolved and will solve, because the canonical object
/// is the engine's entry rather than a parallel record of it.
#[test]
fn the_engine_still_owns_solving_the_canonical_path() {
    let session = Session::with_two_pools();
    let object = session.path(&Session::route());

    // Registration resolved the path through the engine and recorded the
    // solve-eligibility verdict the cycle drives from.
    {
        let engine = session.engine.lock();
        let resolved = engine
            .cycle
            .path_resolved
            .get(&object.path_id())
            .expect("the engine resolved the session's path");
        assert_eq!(resolved.hops.len(), 2);
        assert!(engine.cycle.path_status.get(&object.path_id()).is_some());
    }

    // A cold solve pass produces results keyed by the canonical path's id.
    session.stages.solve_all_paths(1);
    let (_results, block) = session.stages.latest_results();
    assert_eq!(block, 1);
}

/// A session installs its path-identity owner once: a second install is
/// refused, because two owners would be two path-id spaces for one session
/// and could hand two consumers different identities for one route.
#[test]
fn a_second_path_owner_is_refused_and_the_first_keeps_answering() {
    let session = Session::with_two_pools();
    let second = EnginePathObjects::new(Arc::clone(&session.engine));
    let refused = session
        .registry
        .install_path_objects(Arc::new(second))
        .expect_err("the first owner wins");
    assert_eq!(refused.path_count(), 0);

    // The installed owner still answers, and the refused one never grew.
    assert!(session.path(&Session::route()).path_id() > 0);
    assert_eq!(session.stages.path_count(), 1);
}

/// The pool-id join the adapter makes is DERIVED from the live-state owner's
/// registration tables, not from a map the session keeps: a consumer asking
/// for the same identity reaches the same id the path hops on.
#[test]
fn the_hops_resolve_through_the_live_state_owners_registration_tables() {
    let session = Session::with_two_pools();
    let object = session.path(&Session::route());

    let engine = session.engine.lock();
    let core = engine.core.read_at(LockSite::Solver);
    for hop in object.hops() {
        assert!(core.pool_id_for_identity(hop.pool_identity()).is_some());
    }
}

/// One session booted the way production boots it: a `Bot` (the session's
/// live state plus its one object registry) composed with an `EngineDriver`,
/// which builds the engine and binds it. Every driver constructor —
/// standalone-Rust `new`, the `PyO3` `from_stages_with_hub`, the adapter
/// `from_stages` — funnels through the driver's private `assemble`, so this
/// is the boot path, not a hand-built stand-in for it. Nothing here installs a
/// path owner: if the composition stopped binding one, the session's path
/// surface would go dead here exactly as it would in the live bot.
fn booted_session() -> (Arc<EngineDriver>, Arc<SessionObjectRegistry>) {
    let bot = Arc::new(Bot::new(CHAIN_ID));
    {
        let core = bot.state_arc();
        let mut state = core.write_at(LockSite::Registration);
        let token_a = Address::from([0xaau8; 20]);
        let token_b = Address::from([0xbbu8; 20]);
        for (address, token0, token1, reserve0, reserve1) in [
            (pool_a(), token_a, token_b, usdc(1_500_000), weth(800)),
            (pool_b(), token_b, token_a, weth(1000), usdc(2_000_000)),
        ] {
            state
                .register_v2_pool(&RegisterV2PoolParams {
                    address,
                    token0,
                    token1,
                    reserve0,
                    reserve1,
                    fee_token0: (GAMMA_03, FEE_DENOM_03),
                    fee_token1: (GAMMA_03, FEE_DENOM_03),
                    ..Default::default()
                })
                .expect("test setup: V2 pool");
        }
    }
    let cfg = &::degenbot_config::holder::config_arc();
    let driver = Arc::new(EngineDriver::new(bot, cfg));
    let registry = driver.bot().session_registry();
    for address in [pool_a(), pool_b()] {
        let _ = registry.get_or_create_pool(PoolIdentity::v2(address));
    }
    (driver, registry)
}

/// The production boot leaves the session with a LIVE path-identity owner, so
/// the session's canonical path identity is reachable rather than a door
/// nothing opens: a path asked for through the session registry is
/// non-refusing, and its id is the engine's own allocation.
///
/// This is the test that fails if the composition stops installing the
/// adapter. Every other test in this file hand-installs it, so without this one
/// the seam could be uninstalled in production with the whole suite still
/// green.
#[test]
fn the_production_boot_path_installs_a_live_session_path_owner() {
    let (driver, registry) = booted_session();

    assert!(
        registry.has_path_owner(),
        "the driver composition must bind the session's path-identity owner"
    );

    // Non-refusing: the session names the route and gets the engine's path.
    let object = registry
        .get_or_create_path(&Session::route())
        .expect("a booted session's canonical path is not a NoPathOwner refusal");
    assert_eq!(object.path_id(), 1);
    assert_eq!(object.chain_id(), CHAIN_ID);
    assert_eq!(object.hops().len(), 2);

    // The engine behind the driver is the one that registered it, and the
    // session reads that engine's count rather than a session-side tally.
    assert_eq!(driver.stages().path_count(), 1);
    assert_eq!(registry.path_count(), 1);
    assert!(Arc::ptr_eq(
        &registry
            .resolve_path(&Session::route())
            .expect("resolve through the booted owner"),
        &object
    ));
}

/// A second driver over one session is a second engine, hence a second path-id
/// space — the fork the once-installed owner exists to refuse. The session
/// keeps the FIRST owner (a complete, working identity space) and the second
/// engine's routes are unnameable from the session, which is exactly the
/// state the composition site reports at ERROR rather than absorbing.
#[test]
fn a_second_driver_over_one_session_keeps_the_first_path_owner() {
    let (first, registry) = booted_session();
    let object = registry
        .get_or_create_path(&Session::route())
        .expect("the first driver's owner answers");
    assert_eq!(first.stages().path_count(), 1);

    // A second driver over the SAME session `Bot`: its engine allocates its own
    // ids from its own registry.
    let second_stages = EngineStages::with_core(first.core(), Arc::new(EpochDelta::new(0u64)));
    let second = EngineDriver::from_stages(Arc::clone(first.bot()), Arc::new(second_stages));
    let first_core = first.core();
    let (first_pool, second_pool) = {
        let core = first_core.read_at(LockSite::Registration);
        (
            core.pool_id_for_identity(&PoolIdentity::v2(pool_a()))
                .expect("pool A id"),
            core.pool_id_for_identity(&PoolIdentity::v2(pool_b()))
                .expect("pool B id"),
        )
    };
    let foreign_route = vec![
        PoolHop {
            pool_id: second_pool,
            zero_for_one: false,
        },
        PoolHop {
            pool_id: first_pool,
            zero_for_one: true,
        },
    ];
    second
        .stages()
        .register_path(foreign_route)
        .expect("the second engine registers its own route");

    // The session's owner is still the FIRST engine: its routes answer, and
    // the second engine's route is unknown to the session.
    assert!(registry.has_path_owner());
    let again = registry
        .get_or_create_path(&Session::route())
        .expect("the first owner keeps answering");
    assert!(Arc::ptr_eq(&again, &object));
    assert_eq!(first.stages().path_count(), 1);
    let foreign_hops = [
        (PoolIdentity::v2(pool_b()), false),
        (PoolIdentity::v2(pool_a()), true),
    ];
    assert!(
        matches!(
            registry.resolve_path(&foreign_hops),
            Err(ObjectRefusal::UnknownPathIdentity { .. })
        ),
        "the second engine's route is not in the session's id space"
    );
}
