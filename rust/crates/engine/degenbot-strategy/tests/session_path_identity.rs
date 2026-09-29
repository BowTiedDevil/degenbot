//! The two strategy arms share ONE canonical path identity, seen from the
//! strategy plane.
//!
//! This is the cross-crate half of the seam pinned in-crate by
//! `degenbot-bot`'s `arb_engine::tests::session_paths`: the settlement arm and
//! the txpool-backrun arm are the two real consumers of a route, each with its
//! own policy, and they must reach the SAME canonical path through the
//! session's object registry — the engine's `PathRegistry` stays the only path
//! store, and neither arm's solver, dispatch, or submission policy reaches the
//! canonical object.
//!
//! Seam: `degenbot_bot::arb_engine::EngineDriver` (the production boot that
//! binds the engine's path registry to the session) +
//! `degenbot_substrate::session_registry::SessionObjectRegistry` (the
//! session side) + `Settlement` / `TxpoolBackrun` (the two arms' real policy
//! values). Rationale: `docs/architecture/session-object-registry.md`.

#![expect(clippy::expect_used, reason = "test assertions fail loudly")]

use std::sync::Arc;

use alloy::primitives::aliases::U112;
use alloy::primitives::{Address, U256};
use degenbot_bot::arb_engine::EngineDriver;
use degenbot_bot::bot_core::Bot;
use degenbot_config::BotConfig;
use degenbot_strategy::{Settlement, Strategy, StrategyName, TxpoolBackrun};
use degenbot_substrate::session_registry::{PathObject, PoolIdentity, SessionObjectRegistry};
use degenbot_substrate::state_lock::LockSite;
use degenbot_substrate::RegisterV2PoolParams;

const CHAIN_ID: u64 = 1;

fn pool_a() -> Address {
    Address::from([0x11u8; 20])
}

fn pool_b() -> Address {
    Address::from([0x12u8; 20])
}

fn usdc(amount: u64) -> U112 {
    (U256::from(amount) * U256::from(10u64).pow(U256::from(6))).to::<U112>()
}

fn weth(amount: u64) -> U112 {
    (U256::from(amount) * U256::from(10u64).pow(U256::from(18))).to::<U112>()
}

fn v2_params(
    address: Address,
    token0: Address,
    token1: Address,
    r0: U112,
    r1: U112,
) -> RegisterV2PoolParams {
    RegisterV2PoolParams {
        address,
        token0,
        token1,
        reserve0: r0,
        reserve1: r1,
        fee_token0: (997, 1000),
        fee_token1: (997, 1000),
        factory: Address::ZERO,
        update_block: 0,
        ..Default::default()
    }
}

/// One session as the strategy plane sees it, booted through the driver
/// composition every production boot goes through (`EngineDriver::new`): a
/// `Bot` (the session's live state plus its one object registry) composed with
/// the engine over it. The engine's path registry reaches the session as its
/// path-identity owner because the driver binds it — this test does not install
/// anything, so the arms' shared identity below is the identity a real boot
/// gives them.
struct StrategySession {
    registry: Arc<SessionObjectRegistry>,
    driver: Arc<EngineDriver>,
}

impl StrategySession {
    fn new() -> Self {
        let bot = Arc::new(Bot::new(CHAIN_ID));
        {
            let core = bot.state_arc();
            let mut state = core.write_at(LockSite::Registration);
            let token_a = Address::from([0xaau8; 20]);
            let token_b = Address::from([0xbbu8; 20]);
            state
                .register_v2_pool(&v2_params(
                    pool_a(),
                    token_a,
                    token_b,
                    usdc(1_500_000),
                    weth(800),
                ))
                .expect("test setup: V2 pool A");
            state
                .register_v2_pool(&v2_params(
                    pool_b(),
                    token_b,
                    token_a,
                    weth(1000),
                    usdc(2_000_000),
                ))
                .expect("test setup: V2 pool B");
        }
        let cfg = &degenbot_config::holder::config_arc();
        let driver = Arc::new(EngineDriver::new(bot, cfg));
        let registry = driver.bot().session_registry();
        assert!(
            registry.has_path_owner(),
            "a booted session reaches its paths through the engine it drives"
        );
        for address in [pool_a(), pool_b()] {
            let _ = registry.get_or_create_pool(PoolIdentity::v2(address));
        }
        Self { registry, driver }
    }

    /// The `usdc -> weth -> usdc` route both arms trade.
    fn route() -> [(PoolIdentity, bool); 2] {
        [
            (PoolIdentity::v2(pool_a()), true),
            (PoolIdentity::v2(pool_b()), true),
        ]
    }

    /// What an arm gets when it asks the session for the route it trades: the
    /// canonical object, from which the arm derives its own plan.
    fn canonical_path(&self) -> Arc<PathObject> {
        self.registry
            .get_or_create_path(&Self::route())
            .expect("canonical path")
    }
}

/// One arm's derived plan: the path it trades plus the policy that is the
/// arm's own. Nothing of this reaches the canonical object.
#[derive(Debug, PartialEq, Eq)]
struct ArmPlan {
    path_id: u64,
    solver: &'static str,
    submission: String,
}

/// The settlement arm's plan over the canonical path: the engine's solve
/// cycle on a sealed block, fanning out over its configured endpoints.
fn settlement_plan(path: &PathObject, cfg: &BotConfig) -> ArmPlan {
    let arm = Settlement::from_config(cfg);
    ArmPlan {
        path_id: path.path_id(),
        solver: "engine-cycle",
        submission: arm.config().endpoints.join(","),
    }
}

/// The txpool-backrun arm's plan over the SAME path: its own solver and a
/// budgeted bid through its own relay fan-out.
fn txpool_backrun_plan(path: &PathObject, cfg: &BotConfig) -> ArmPlan {
    let arm = TxpoolBackrun::from_config(cfg, String::from("http://node.invalid"));
    ArmPlan {
        path_id: path.path_id(),
        solver: "frame",
        submission: match &arm.config().submission {
            degenbot_strategy::SubmissionSlot::PublicFanOut { relays } => relays.join(","),
            other => format!("{other:?}"),
        },
    }
}

fn arms_config() -> BotConfig {
    let mut cfg = BotConfig::default();
    cfg.strategy.settlement.active = true;
    cfg.strategy.settlement.endpoints = Some(String::from("https://a.local,https://b.local"));
    cfg.strategy.txpool_backrun.bid_mode = true;
    cfg.strategy.txpool_backrun.budget_wei = 1_000_000_000_000_000_000u128;
    cfg.strategy.txpool_backrun.endpoints = Some(String::from("https://rpc.local"));
    cfg
}

/// The two arms are different strategies that trade the same route, so they
/// share one canonical path: the same object, from both sides, with the
/// engine's id behind it. This is the acceptance claim in its plainest form.
#[test]
fn the_settlement_and_txpool_backrun_arms_share_one_canonical_path() {
    let session = StrategySession::new();

    let from_settlement = session.canonical_path();
    let from_backrun = session.canonical_path();

    assert!(
        Arc::ptr_eq(&from_settlement, &from_backrun),
        "one route, one canonical object"
    );
    assert_eq!(from_settlement.path_id(), from_backrun.path_id());
    // The engine holds exactly one path for it.
    assert_eq!(session.driver.stages().path_count(), 1);
    assert_eq!(session.registry.path_count(), 1);

    // Both arms name the same route in the session's own vocabulary.
    let route = StrategySession::route();
    assert_eq!(from_settlement.identity().hops().len(), 2);
    assert_eq!(from_settlement.hops()[0].pool_identity(), &route[0].0);
}

/// The arms' policy stays out of the canonical object: two arms with
/// different solvers, different submission postures, and different budgets
/// read back the same identity.
#[test]
fn arm_policy_does_not_reach_the_canonical_path() {
    let session = StrategySession::new();
    let cfg = arms_config();

    let path = session.canonical_path();
    let settlement = settlement_plan(&path, &cfg);
    let backrun = txpool_backrun_plan(&path, &cfg);

    // Different arms, different policy — one path id.
    assert_ne!(settlement, backrun);
    assert_eq!(settlement.path_id, backrun.path_id);
    assert_eq!(settlement.path_id, path.path_id());
    assert_ne!(settlement.solver, backrun.solver);
    assert_ne!(settlement.submission, backrun.submission);

    // The canonical object answers identity only, before and after both
    // derivations, and the engine's entry for it is unchanged.
    let reread = session
        .registry
        .resolve_path(&StrategySession::route())
        .expect("canonical path");
    assert_eq!(reread, path);
    assert_eq!(reread.chain_id(), CHAIN_ID);
    assert_eq!(session.driver.stages().path_count(), 1);
}

/// The two arms are named strategies in the plane, and the shared identity is
/// reachable from either one — a path is a session object, not a per-strategy
/// resource.
#[test]
fn the_shared_path_is_reachable_from_either_named_arm() {
    let session = StrategySession::new();
    let cfg = arms_config();
    let path = session.canonical_path();

    assert_eq!(
        Settlement::from_config(&cfg).name(),
        StrategyName::Settlement
    );
    assert_eq!(
        <TxpoolBackrun as Strategy>::NAME,
        StrategyName::TxpoolBackrun
    );
    // Both arms' plans are derived from the one object, and the object carries
    // neither arm's values.
    assert_eq!(settlement_plan(&path, &cfg).path_id, path.path_id());
    assert_eq!(txpool_backrun_plan(&path, &cfg).path_id, path.path_id());
    assert!(!format!("{path:?}").contains("engine-cycle"));
    assert!(!format!("{path:?}").contains("https://a.local"));
}
