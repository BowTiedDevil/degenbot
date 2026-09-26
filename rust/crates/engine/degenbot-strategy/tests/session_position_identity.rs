//! The two strategy arms agree on ONE canonical position identity, and neither
//! of them holds a position.
//!
//! This is the cross-crate half of the position seam pinned in-crate by
//! `degenbot-bot`'s `bot_core::tests::session_position`: the settlement arm and
//! the txpool-backrun arm are two real consumers of a session, each with its own
//! policy, and they must name the same position through the session's registry —
//! while the VALUE each of them reads is its own fresh read.
//!
//! The observer is installed HERE, by hand, and that is deliberate: this test
//! proves the read MODEL — two arms reading one identity and each getting its
//! own fresh projection — which needs an observer whose block advances per read,
//! and no production boot offers one. Production wiring is covered where it
//! lives, by the binding shell's boot (`docs/architecture/session-object-registry.md`,
//! *Who installs it, and where*), where the cross-strategy claim is also asserted
//! against a real reader.
//!
//! Seam: `degenbot_bot::bot_core::session_registry` (the session side) +
//! `Settlement` / `TxpoolBackrun` (the two arms' real policy values). The
//! identity-vs-projection split and the layer reasoning are in
//! `docs/architecture/session-object-registry.md`.

#![expect(clippy::expect_used, reason = "test assertions fail loudly")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use alloy::primitives::aliases::U112;
use alloy::primitives::{Address, U256};
use degenbot_bot::arb_engine::EngineDriver;
use degenbot_bot::bot_core::session_registry::{
    Freshness, HealthFactor, PositionIdentity, PositionObserver, PositionReading, PositionRefusal,
    SessionObjectRegistry,
};
use degenbot_bot::bot_core::state_lock::LockSite;
use degenbot_bot::bot_core::{Bot, RegisterV2PoolParams};
use degenbot_config::BotConfig;
use degenbot_strategy::{Settlement, Strategy, StrategyName, TxpoolBackrun};

const CHAIN_ID: u64 = 1;

fn pool_a() -> Address {
    Address::from([0x11u8; 20])
}

fn market() -> Address {
    Address::from([0x51u8; 20])
}

fn account() -> Address {
    Address::from([0x52u8; 20])
}

fn v2_params(address: Address, token0: Address, token1: Address) -> RegisterV2PoolParams {
    RegisterV2PoolParams {
        address,
        token0,
        token1,
        reserve0: U256::from(1_500_000u64 * 1_000_000u64).to::<U112>(),
        reserve1: (U256::from(800u64) * U256::from(10u64).pow(U256::from(18u64))).to::<U112>(),
        fee_token0: (997, 1000),
        fee_token1: (997, 1000),
        factory: Address::ZERO,
        update_block: 0,
        ..Default::default()
    }
}

/// A position observer that reports a DIFFERENT block on every read, so a test
/// can tell a fresh read-model projection from a cached value.
#[derive(Debug, Default)]
struct AdvancingObserver {
    reads: AtomicUsize,
}

impl AdvancingObserver {
    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

impl PositionObserver for AdvancingObserver {
    fn read_position(
        &self,
        identity: &PositionIdentity,
        _freshness: &Freshness,
    ) -> Result<PositionReading, PositionRefusal> {
        let read = self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(PositionReading::new(
            *identity,
            21_000_000 + u64::try_from(read).unwrap_or_default(),
            HealthFactor::Ratio(U256::from(1_100_000_000_000_000_000u64)),
        ))
    }
}

/// One session as the strategy plane sees it, booted through the driver
/// composition every production boot goes through (`EngineDriver::new`).
struct StrategySession {
    registry: Arc<SessionObjectRegistry>,
    observer: Arc<AdvancingObserver>,
    driver: Arc<EngineDriver>,
}

impl StrategySession {
    fn new() -> Self {
        let bot = Arc::new(Bot::new(CHAIN_ID));
        {
            let core = bot.state_arc();
            let mut state = core.write_at(LockSite::Registration);
            state
                .register_v2_pool(&v2_params(
                    pool_a(),
                    Address::from([0xaau8; 20]),
                    Address::from([0xbbu8; 20]),
                ))
                .expect("test setup: V2 pool A");
        }
        let cfg = &degenbot_config::holder::config_arc();
        let driver = Arc::new(EngineDriver::new(bot, cfg));
        let registry = driver.bot().session_registry();
        let observer = Arc::new(AdvancingObserver::default());
        assert!(
            registry
                .install_position_observer(Arc::clone(&observer) as Arc<dyn PositionObserver>)
                .is_ok(),
            "the first position observer installed wins"
        );
        Self {
            registry,
            observer,
            driver,
        }
    }

    /// What the settlement arm asks for: this account's position in this market.
    fn settlement_position(&self) -> Result<PositionReading, PositionRefusal> {
        self.registry.read_position(
            &self.registry.position_identity(market(), account()),
            &Freshness::AtOrAfter { block: 21_000_000 },
        )
    }

    /// What the txpool-backrun arm asks for: the same position, read again.
    fn backrun_position(&self) -> Result<PositionReading, PositionRefusal> {
        self.registry.read_position(
            &self.registry.position_identity(market(), account()),
            &Freshness::AtOrAfter { block: 21_000_000 },
        )
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

/// The ENGINE's driver boot installs no position reader, so a session composed
/// only through `EngineDriver::new` refuses every position read rather than
/// inventing one. The reader's source is database state the bot boot owns, not
/// engine state, so the engine has nothing to bind here — and refusal, not a
/// fabricated position, is what a session without a reader must say.
#[test]
fn an_engine_booted_session_refuses_a_position_read_until_a_reader_is_installed() {
    let bot = Arc::new(Bot::new(CHAIN_ID));
    let cfg = &degenbot_config::holder::config_arc();
    let driver = Arc::new(EngineDriver::new(bot, cfg));
    let registry = driver.bot().session_registry();

    assert!(
        !registry.has_position_observer(),
        "a boot installs no position reader on its own"
    );
    assert_eq!(
        registry
            .read_position(
                &registry.position_identity(market(), account()),
                &Freshness::Any,
            )
            .expect_err("no reader, no position"),
        PositionRefusal::NoPositionOwner
    );
}

/// The two arms are different strategies that would act on the same position,
/// so they agree on its identity — and each gets its own fresh reading, because
/// a position is not a session object and holding one across a solve is the
/// consumer's decision, not the registry's.
#[test]
fn the_settlement_and_txpool_backrun_arms_agree_on_one_position_identity() {
    let session = StrategySession::new();
    let cfg = arms_config();
    assert_eq!(
        Settlement::from_config(&cfg).name(),
        StrategyName::Settlement
    );
    assert_eq!(
        <TxpoolBackrun as Strategy>::NAME,
        StrategyName::TxpoolBackrun
    );

    let from_settlement = session
        .settlement_position()
        .expect("the settlement arm reads the position");
    let from_backrun = session
        .backrun_position()
        .expect("the txpool-backrun arm reads the same position");

    assert_eq!(
        from_settlement.identity(),
        from_backrun.identity(),
        "one position, one canonical identity"
    );
    assert_eq!(from_settlement.identity().market(), market());
    assert_eq!(from_settlement.identity().account(), account());
    assert_eq!(from_settlement.identity().chain_id(), CHAIN_ID);

    // Each read is its own projection: the second read observed a later block,
    // and the registry kept no value of its own to hand back instead.
    assert_eq!(from_settlement.observed_block(), 21_000_000);
    assert_eq!(from_backrun.observed_block(), 21_000_001);
    assert_ne!(from_settlement, from_backrun);
    assert_eq!(session.observer.reads(), 2);
    assert!(session
        .driver
        .bot()
        .session_registry()
        .has_position_observer());
}

/// The arms' policy stays out of the position: a risk report and a liquidation
/// candidate read the same identity and reach the same posture, and neither
/// arm's configuration reaches the reading.
#[test]
fn arm_policy_does_not_reach_the_position_reading() {
    let session = StrategySession::new();
    let cfg = arms_config();
    let settlement = Settlement::from_config(&cfg);
    let backrun = TxpoolBackrun::from_config(&cfg, String::from("http://node.invalid"));

    let reading = session.settlement_position().expect("position");
    assert_eq!(
        reading.health_factor(),
        session
            .backrun_position()
            .expect("position")
            .health_factor()
    );
    assert!(!reading.health_factor().is_liquidatable());
    assert!(!format!("{reading:?}").contains("https://a.local"));
    assert!(!format!("{settlement:?}{backrun:?}").contains(&format!("{reading:?}")));
    assert_eq!(session.observer.reads(), 2);
}
