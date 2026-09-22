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

use alloy::primitives::{address, aliases::U112, Address, B256, U128, U256};

use degenbot_bot::connector_index::{V2ConnectorIndex, V2Edge, V4Edge};
use degenbot_db::connection::DegenbotDb;
use degenbot_pools::slot_layout::V2ReservesParts;
use degenbot_pools::v3_state::ClSlotLayout;
use degenbot_pools::TickInfo;
use degenbot_simulation::sim::evm::journal_pools::{
    PoolFamily, PoolPostKind, PoolPostState, TouchedTickWord, TypedPoolPost, V4PoolSet,
};
use degenbot_strategy::backrun_engine::{BackrunHopRef, BackrunSolver, LaneFamily};
use degenbot_strategy::backrun_strategy::{admit_extracted, WETH};
use degenbot_strategy::frame_pipeline::MarketContext;
use degenbot_strategy::pending_tx::V3TickWindow;
use hashbrown::HashMap as HbMap;

const TOK: Address = address!("0000000000000000000000000000000000000aa1");
/// P: the V2 connector the V4 anchor settles its drift through.
const P: Address = address!("000000000000000000000000000000000000b001");
const V4_MANAGER: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
const V4_DB_POOL_ID: u64 = 902;

fn v4_pool_hash() -> B256 {
    B256::new([0xab; 32])
}

fn runtime_fixture(v4_fee: u32) -> MarketContext {
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
    });
    index.push_v4_edge(V4Edge {
        pool_hash: v4_pool_hash(),
        manager: V4_MANAGER,
        token0: TOK,
        token1: WETH,
        fee: v4_fee,
        fee_currency1: v4_fee,
        tick_spacing: 10,
        hooks: Address::ZERO,
        db_pool_id: V4_DB_POOL_ID,
    });
    MarketContext::new(
        1,
        Some(std::sync::Arc::new(
            degenbot_bot::bot_core::RouteRegistry::new(index),
        )),
        Some(db),
        8,
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

/// The chain view's V4 in-range window: two initialized ticks straddling
/// tick 0 (the slack a shallow target swap leaves untracked).
struct V4TwoTickWindow;

impl V3TickWindow for V4TwoTickWindow {
    fn tick_window(
        &self,
        _pool: Address,
        _layout: ClSlotLayout,
        _tick_spacing: i32,
        _current_tick: i32,
        _head: u64,
    ) -> HbMap<i32, TickInfo> {
        HbMap::default()
    }

    fn v4_tick_window(
        &self,
        _manager: Address,
        _pool_id: B256,
        _tick_spacing: i32,
        _current_tick: i32,
        head: u64,
    ) -> HbMap<i32, TickInfo> {
        let mut m = HbMap::default();
        m.insert(
            120,
            TickInfo {
                liquidity_gross: U128::from(10_000),
                liquidity_net: 5_000,
                block: head,
            },
        );
        m.insert(
            -120,
            TickInfo {
                liquidity_gross: U128::from(8_000),
                liquidity_net: -4_000,
                block: head,
            },
        );
        m
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
fn v4_anchor_merges_the_chain_view_tick_window() {
    // The target crossed no initialized tick: the replayed V4 post carries no
    // touched ticks, so the cleared chain view must supply the in-range
    // window or the projection cannot build a range sequence.
    let rt = runtime_fixture(500);
    let mut solver = BackrunSolver::new();
    let mut v4 = v4_post();
    let PoolPostKind::Typed(TypedPoolPost::V4 { touched_ticks, .. }) = &mut v4.kind else {
        panic!("fixture is a typed V4 post");
    };
    touched_ticks.clear();
    let affected = admit_extracted(
        &rt,
        &mut solver,
        &[v4, v2_post()],
        1,
        "0xtest",
        Some(&V4TwoTickWindow),
    );
    let v4 = affected
        .iter()
        .find(|a| a.address == V4_MANAGER)
        .expect("the V4 post admits with the window");
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
        .expect("the merged window makes the V4 anchor solvable");
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
