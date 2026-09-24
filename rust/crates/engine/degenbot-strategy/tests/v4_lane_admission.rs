//! The V4 lane's frame admission: a typed V4 post-state with a connector
//! roster edge admits into the scope (workspace id + `LaneFamily::V4`),
//! declares, and solves a mixed V4 + V2 cycle. A fee past the executor's
//! 2-byte encoding limit is refused loudly at admission.
//!
//! Kept in its own test binary so the process-global typed
//! `logging.trace_jsonl` override cannot race another test file's trace
//! assertions.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::similar_names
)]

use alloy::primitives::{address, aliases::U112, Address, B256, I256, U256};

use degenbot_bot::bot_core::executor_hop::V2FeePair;
use degenbot_bot::bot_core::pool_ingress::{
    TickMapSampleTarget, TickMapSampleVerifier, VerifyLevel,
};
use degenbot_bot::connector_index::{V2ConnectorIndex, V2Edge, V4Edge};
use degenbot_db::connection::DegenbotDb;
use degenbot_db::{ApplyBitmapAtWord, ApplyLiquidityAtTick};
use degenbot_pools::slot_layout::V2ReservesParts;
use degenbot_rpc::liquidity_verifier::LiquidityMap as RpcLiquidityMap;
use degenbot_simulation::sim::evm::journal_pools::{
    PoolFamily, PoolPostKind, PoolPostState, TouchedTickWord, TypedPoolPost, V4PoolSet,
};
use degenbot_strategy::backrun_engine::{BackrunHopRef, BackrunSolver, LaneFamily};
use degenbot_strategy::backrun_strategy::admit_extracted;
use degenbot_strategy::frame_pipeline::MarketContext;
use degenbot_strategy::ETHEREUM_WETH as WETH;
use hashbrown::HashMap as HbMap;

fn v2_fee_pair() -> V2FeePair {
    V2FeePair::from_discovered(Some(3), Some(3), Some(1_000))
}

/// Test stand-in for the Db→head backfill transport. The fixtures stamp no
/// `liquidity_update_block`, so no window is ever backfilled; an unexpected
/// fetch declines loudly rather than staging stale state.
struct NoBackfill;

impl degenbot_bot::bot_core::pool_ingress::LiquidityLogSource for NoBackfill {
    fn fetch_v3_liquidity_events(
        &self,
        _pool: alloy::primitives::Address,
        _from: u64,
        _to: u64,
    ) -> Result<Vec<degenbot_db::LiquidityUpdateEvent>, String> {
        Err("this fixture wires no backfill transport".into())
    }

    fn fetch_v4_liquidity_events(
        &self,
        _manager: alloy::primitives::Address,
        _pool_id: alloy::primitives::B256,
        _from: u64,
        _to: u64,
    ) -> Result<Vec<degenbot_db::LiquidityUpdateEvent>, String> {
        Err("this fixture wires no backfill transport".into())
    }
}

fn market_context(
    registry: Option<std::sync::Arc<degenbot_bot::bot_core::RouteRegistry>>,
    db: Option<std::sync::Arc<degenbot_db::connection::DegenbotDb>>,
    verify: VerifyLevel,
    verifier: Option<std::sync::Arc<dyn TickMapSampleVerifier>>,
) -> MarketContext {
    let db_arm = db.clone().map(|db| {
        degenbot_bot::bot_core::pool_ingress::DbArm::new(db, std::sync::Arc::new(NoBackfill))
    });
    let kit = degenbot_strategy::strategy_kit::StrategyKit::resolve(
        registry, db_arm, None, verify, verifier,
    );
    MarketContext::new(1, db, kit, 8, 4)
}

const TOK: Address = address!("0000000000000000000000000000000000000aa1");
/// P: the V2 connector the V4 anchor settles its drift through.
const P: Address = address!("000000000000000000000000000000000000b001");
const V4_MANAGER: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
const V4_DB_POOL_ID: u64 = 902;

fn v4_pool_hash() -> B256 {
    B256::new([0xab; 32])
}

fn runtime_fixture(v4_fee: u32) -> MarketContext {
    runtime_fixture_with(v4_fee, VerifyLevel::default(), None)
}

fn runtime_fixture_with(
    v4_fee: u32,
    verify: VerifyLevel,
    verifier: Option<std::sync::Arc<dyn TickMapSampleVerifier>>,
) -> MarketContext {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let tok_id = db
        .get_or_create_erc20_token(1, &TOK.to_checksum(None), None, None, None)
        .unwrap();
    let weth_id = db
        .get_or_create_erc20_token(1, &WETH.to_checksum(None), None, None, None)
        .unwrap();
    let mut index = V2ConnectorIndex::default();
    index.push_edge(V2Edge {
        pool_id: 101,
        token0_id: u64::try_from(tok_id).unwrap(),
        token1_id: u64::try_from(weth_id).unwrap(),
        address: P,
        fees: v2_fee_pair(),
    });
    index.push_v4_edge(V4Edge {
        pool_hash: v4_pool_hash(),
        manager: V4_MANAGER,
        state_view: None,
        token0: TOK,
        token1: WETH,
        fee: v4_fee,
        fee_currency1: v4_fee,
        tick_spacing: 10,
        hooks: Address::ZERO,
        db_pool_id: V4_DB_POOL_ID,
    });
    {
        let conn = db.lock();
        conn.execute_batch(&format!(
            "PRAGMA foreign_keys=OFF;
             INSERT INTO exchanges (id, chain_id, name, active, factory) VALUES
                (1, 1, 'uniswap_v4', 1, '{V4_MANAGER}');
             INSERT INTO pool_managers (id, address, chain, kind, state_view, exchange_id) VALUES
                (1, '{}', 1, 'uniswap_v4', NULL, 1);
             INSERT INTO managed_pools (id, kind, manager_id) VALUES
                ({V4_DB_POOL_ID}, 'uniswap_v4', 1);
             INSERT INTO uniswap_v4_pools
                (managed_pool_id, pool_hash, hooks, currency0_id, currency1_id,
                 fee_currency0, fee_currency1, fee_denominator, tick_spacing)
             VALUES ({V4_DB_POOL_ID}, '{}', '{}', {}, {}, {}, {}, 1000000, 10);",
            V4_MANAGER.to_checksum(None),
            v4_pool_hash(),
            Address::ZERO.to_checksum(None),
            tok_id,
            weth_id,
            v4_fee,
            v4_fee,
        ))
        .unwrap();
    }
    let mut ticks = HbMap::new();
    ticks.insert(
        -120,
        ApplyLiquidityAtTick {
            liquidity_net: I256::try_from(-4_000_i64).unwrap(),
            liquidity_gross: alloy::primitives::U128::from(8_000_u64),
            block: 0,
        },
    );
    ticks.insert(
        120,
        ApplyLiquidityAtTick {
            liquidity_net: I256::try_from(5_000_i64).unwrap(),
            liquidity_gross: alloy::primitives::U128::from(10_000_u64),
            block: 0,
        },
    );
    let mut bitmaps = HbMap::new();
    bitmaps.insert(
        -1,
        ApplyBitmapAtWord {
            bitmap: U256::from(1_u8) << 244,
            block: 0,
        },
    );
    bitmaps.insert(
        0,
        ApplyBitmapAtWord {
            bitmap: U256::from(1_u8) << 12,
            block: 0,
        },
    );
    db.upsert_v4_liquidity_positions(i64::try_from(V4_DB_POOL_ID).unwrap(), &ticks)
        .unwrap();
    db.upsert_v4_initialization_maps(i64::try_from(V4_DB_POOL_ID).unwrap(), &bitmaps)
        .unwrap();
    market_context(
        Some(std::sync::Arc::new(
            degenbot_bot::bot_core::RouteRegistry::new(index),
        )),
        Some(std::sync::Arc::new(db)),
        verify,
        verifier,
    )
}

/// The golden V4 post (the planning sandbox fixture): tick 0 at price 1.0,
/// two initialized ticks straddling the active range so the solve can cross.
fn v4_post() -> PoolPostState {
    PoolPostState {
        address: V4_MANAGER,
        family: PoolFamily::V4PoolManager {
            pools: V4PoolSet::default(),
        },
        kind: PoolPostKind::Typed(TypedPoolPost::V4 {
            pool_id: v4_pool_hash(),
            sqrt_price_x96: Some(U256::from(1u128) << 96),
            tick: Some(0),
            liquidity: Some(1_000_000_000),
            touched_ticks: vec![
                TouchedTickWord {
                    tick: 120,
                    liquidity_gross: 10_000,
                    liquidity_net: 5_000,
                },
                TouchedTickWord {
                    tick: -120,
                    liquidity_gross: 8_000,
                    liquidity_net: -4_000,
                },
            ],
        }),
    }
}

/// The golden V2 connector: `TOK`/`WETH` reserves `(500_000, 1_000)`.
fn v2_post() -> PoolPostState {
    PoolPostState {
        address: P,
        family: PoolFamily::V2Pair,
        kind: PoolPostKind::Typed(TypedPoolPost::V2 {
            reserves: V2ReservesParts {
                reserve0: U112::from(500_000u64),
                reserve1: U112::from(1_000u64),
                block_timestamp_last: 0,
            },
        }),
    }
}

struct CountingVerifier {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl TickMapSampleVerifier for CountingVerifier {
    async fn verify(
        &self,
        _target: TickMapSampleTarget,
        _map: &RpcLiquidityMap,
        _block: u64,
    ) -> Result<(), String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn typed_v4_post_admits_and_declares_a_v4_v2_solve() {
    let rt = runtime_fixture(500);
    let mut solver = BackrunSolver::new();
    let affected = admit_extracted(&rt, &mut solver, &[v4_post(), v2_post()], 1, "0xtest", None);
    assert_eq!(affected.len(), 2, "both typed posts admit");

    let v4 = affected
        .iter()
        .find(|a| a.address == V4_MANAGER)
        .expect("the V4 post admits");
    match v4.family {
        LaneFamily::V4 {
            fee,
            pool_id,
            tick_spacing,
            hooks,
        } => {
            assert_eq!(fee, 500);
            assert_eq!(pool_id, v4_pool_hash());
            assert_eq!(tick_spacing, 10);
            assert_eq!(hooks, Address::ZERO);
        }
        other => panic!("expected LaneFamily::V4, got {other:?}"),
    }
    assert_eq!(
        v4.index_pool_id, V4_DB_POOL_ID,
        "the connector DB id rides the admission"
    );
    let p = affected
        .iter()
        .find(|a| a.address == P)
        .expect("the V2 connector admits");

    // The V4 lane declares as a `HopType::V4` hop and the solver's CL
    // dispatch solves it: a projection miss would surface as a typed reject.
    let chain = vec![
        BackrunHopRef {
            pool_id: v4.workspace_pool_id,
            pool: V4_MANAGER,
            token0: TOK,
            token1: WETH,
            zfo: true,
            family: v4.family,
        },
        BackrunHopRef {
            pool_id: p.workspace_pool_id,
            pool: P,
            token0: TOK,
            token1: WETH,
            zfo: false,
            family: p.family,
        },
    ];
    let idx = solver.declare_hops(&chain);
    let solved = solver
        .evaluate_verdict(idx, U256::ZERO)
        .expect("V4 admission + projection reach the solver");
    assert_eq!(solved.optimal_input, U256::from(21_394u64));
    assert_eq!(solved.profit, U256::from(456_202u64));
    assert_eq!(
        solved.hop_outputs,
        vec![U256::from(21_382u64), U256::from(477_596u64)]
    );
}

#[test]
fn v4_anchor_with_no_crossed_ticks_uses_and_samples_the_real_pool_ingress() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let rt = runtime_fixture_with(
        500,
        VerifyLevel::Strict,
        Some(std::sync::Arc::new(CountingVerifier {
            calls: std::sync::Arc::clone(&calls),
        })),
    );
    let mut solver = BackrunSolver::new();
    let mut v4 = v4_post();
    let PoolPostKind::Typed(TypedPoolPost::V4 { touched_ticks, .. }) = &mut v4.kind else {
        panic!("fixture is a typed V4 post");
    };
    touched_ticks.clear();
    let affected = admit_extracted(&rt, &mut solver, &[v4, v2_post()], 1, "0xtest", None);
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the anchor must cross PoolIngress's tracked-map sample"
    );
    let v4 = affected
        .iter()
        .find(|a| a.address == V4_MANAGER)
        .expect("the V4 Db map makes the anchor solvable");
    let p = affected
        .iter()
        .find(|a| a.address == P)
        .expect("the V2 connector admits");

    let chain = vec![
        BackrunHopRef {
            pool_id: v4.workspace_pool_id,
            pool: V4_MANAGER,
            token0: TOK,
            token1: WETH,
            zfo: true,
            family: v4.family,
        },
        BackrunHopRef {
            pool_id: p.workspace_pool_id,
            pool: P,
            token0: TOK,
            token1: WETH,
            zfo: false,
            family: p.family,
        },
    ];
    let idx = solver.declare_hops(&chain);
    let solved = solver
        .evaluate_verdict(idx, U256::ZERO)
        .expect("the Db-backed V4 anchor reaches the solver");
    assert_eq!(solved.optimal_input, U256::from(21_394u64));
    assert_eq!(solved.profit, U256::from(456_202u64));
}

#[test]
fn v4_fee_past_encoder_bound_skips_loudly() {
    let dir = std::env::temp_dir().join(format!("degenbot-v4fee-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let trace = dir.join("trace.jsonl");
    let mut boot = degenbot_config::BotConfig::default();
    boot.logging.trace_jsonl = Some(trace.clone());
    let _ = degenbot_config::holder::install(std::sync::Arc::new(boot));

    let rt = runtime_fixture(degenbot_executor::encoders::V4_FEE_ENCODER_MAX);
    let mut solver = BackrunSolver::new();
    let affected = admit_extracted(&rt, &mut solver, &[v4_post()], 1, "0xtest", None);
    assert!(affected.is_empty(), "an un-encodable fee never admits");

    let text = std::fs::read_to_string(&trace).expect("read trace");
    assert!(
        text.contains("\"stage\":\"v4-fee-encoder-overflow\""),
        "the fee-bound skip must be witnessed in the trace: {text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
