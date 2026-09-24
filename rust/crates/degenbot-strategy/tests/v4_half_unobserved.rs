//! Mixed V4+typed frames: the V4 half is observed (a per-state skip trace)
//! while the typed half still admits. Locks the loud posture for offender
//! `admit_extracted`'s formerly-empty V4 arms.
//!
//! Kept in its own test binary so the process-global typed
//! `logging.trace_jsonl` override cannot race another test file's trace
//! assertions.

#![expect(clippy::unwrap_used, clippy::expect_used)]

use alloy::primitives::{address, aliases::U112, Address};

use degenbot_bot::connector_index::{V2ConnectorIndex, V2Edge};
use degenbot_db::connection::DegenbotDb;
use degenbot_pools::slot_layout::V2ReservesParts;
use degenbot_simulation::sim::evm::journal_pools::{
    PoolFamily, PoolPostKind, PoolPostState, TypedPoolPost, V4PoolSet,
};
use degenbot_strategy::backrun_engine::BackrunSolver;
use degenbot_strategy::backrun_strategy::{admit_extracted, WETH};
use degenbot_strategy::frame_pipeline::MarketContext;

fn market_context(
    registry: Option<std::sync::Arc<degenbot_bot::bot_core::RouteRegistry>>,
    db: Option<std::sync::Arc<degenbot_db::connection::DegenbotDb>>,
) -> MarketContext {
    let kit = degenbot_strategy::strategy_kit::StrategyKit::resolve(
        registry,
        db.clone(),
        None,
        degenbot_bot::bot_core::pool_ingress::VerifyLevel::default(),
        None,
    );
    MarketContext::new(1, db, kit, 8, 4)
}

const TOK: Address = address!("0000000000000000000000000000000000000aa1");
const P: Address = address!("000000000000000000000000000000000000b001");
const V4_MANAGER: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");

fn runtime_fixture() -> MarketContext {
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
    market_context(
        Some(std::sync::Arc::new(
            degenbot_bot::bot_core::RouteRegistry::new(index),
        )),
        Some(std::sync::Arc::new(db)),
    )
}

#[test]
fn mixed_frame_traces_the_v4_half_instead_of_dropping_it_silently() {
    let dir = std::env::temp_dir().join(format!("degenbot-v4half-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let trace = dir.join("trace.jsonl");
    let mut boot = degenbot_config::BotConfig::default();
    boot.logging.trace_jsonl = Some(trace.clone());
    let _ = degenbot_config::holder::install(std::sync::Arc::new(boot));

    let rt = runtime_fixture();
    let mut solver = BackrunSolver::new();
    let states = vec![
        PoolPostState {
            address: P,
            family: PoolFamily::V2Pair,
            kind: PoolPostKind::Typed(TypedPoolPost::V2 {
                reserves: V2ReservesParts {
                    reserve0: U112::from(476_259u64),
                    reserve1: U112::from(1_050u64),
                    block_timestamp_last: 0,
                },
            }),
        },
        PoolPostState {
            address: V4_MANAGER,
            family: PoolFamily::V4PoolManager {
                pools: V4PoolSet::default(),
            },
            kind: PoolPostKind::Unsupported,
        },
    ];

    let affected = admit_extracted(&rt, &mut solver, &states, 7, "0xtest", None);
    assert_eq!(affected.len(), 1, "the typed V2 half still admits");
    assert_eq!(affected[0].address, P);

    let text = std::fs::read_to_string(&trace).expect("read trace");
    assert!(
        text.contains("\"stage\":\"v4-half-unobserved\""),
        "the V4 half must be witnessed in the trace: {text}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
