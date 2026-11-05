#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::default_trait_access,
    reason = "the oracle test drives known-valid fixture values and asserts on the typed product"
)]
//! The session-consumption oracle — the pure-Rust half of the shared
//! event-sequence gate (the fixture is
//! `tests/standalone_parity/fixtures/session_consumption.json`).
//!
//! The Python half (`tests/standalone_parity/test_session_consumption_gate.py`)
//! reads the SAME fixture and feeds the SAME script through the SAME consumer
//! (the `PyO3` seam). Both halves therefore pin, against one source of truth:
//!
//! - **executor work** — the per-batch outcome records a script produces
//!   (path id, block, assembly verdict, simulate verdict), which is the
//!   product of the consumer's row/payload derivation feeding the executor;
//! - **outcome ordering** — one record set per enqueued batch, in arrival
//!   order (the FIFO = nonce order the ordered lane guarantees);
//! - **loud abort** — the first leaf failure surfaces at the consumer with its
//!   batch index and the leaf's detail, and the lane stops;
//! - **stall detection** — the heartbeat the core `stall_watchdog` feeds gets
//!   one beat per ACTUAL stream event, so a silent source feeds none (the trip
//!   itself is a separate named test over the same pairing);
//! - **close on termination** — the typed end verdict (`quiet-end` /
//!   `pump-stopped`) plus the one shared teardown.
//!
//! A seeded mutation of an expected value in an in-memory copy of the oracle
//! must fail the comparator (`seeded_divergence_expected_mutation_fails`): the
//! gate observes the consumer, not itself.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alloy::primitives::{Bytes, U256};
use degenbot_arbitrage::{FeeOnTransferRegistry, PoolDivergence};
use degenbot_batch_executor::session::{
    ConsumerPolicy, SessionClock, SessionConsumer, SessionProgress,
};
use degenbot_batch_executor::{BatchExecutor, ExecutorConfig, ExecutorRuntime, PathResolver};
use degenbot_bot::arb_engine::EngineDriver;
use degenbot_bot::arb_engine::{BlockNotification, Heartbeat, ResultBatch, SessionEvent};
use degenbot_bot::bot_core::Bot;
use degenbot_core::address_utils::parse_address;
use degenbot_executor::composers::{HopInfo, PathInfo, V2HopInfo};
use degenbot_submission::{
    Dispatcher, PathSuppression, ReceiptProbe, SubmissionLedger, SubmissionResult,
    SubmissionTarget, TxSigner,
};
use degenbot_substrate::nonce::NonceAuthority;
use serde_json::Value;

/// The fixture, resolved from this crate's manifest dir.
const ORACLE_REL: &str = "../../../../tests/standalone_parity/fixtures/session_consumption.json";

/// The registered path's id on this side (the fixture's symbolic `path_a`).
const PATH_A: u64 = 7;

/// An id no resolver answers (the fixture's symbolic `path_missing`).
const PATH_MISSING: u64 = 4_242;

fn oracle() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ORACLE_REL);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("session oracle: read {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("session oracle: valid JSON")
}

/// The symbolic name of one substituted path id (the fixture never carries a
/// live registry identity).
fn symbolic(path_id: u64) -> String {
    match path_id {
        PATH_A => "path_a".to_string(),
        PATH_MISSING => "path_missing".to_string(),
        other => format!("path_{other}"),
    }
}

fn live_id(symbolic: &str) -> u64 {
    match symbolic {
        "path_a" => PATH_A,
        "path_missing" => PATH_MISSING,
        other => panic!("the oracle names an unknown path symbol {other:?}"),
    }
}

/// One drained candidate record, in the oracle's symbolic vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordRow {
    path: String,
    block: u64,
    assembly: String,
    simulate: String,
}

/// The consumer's observable product for one script — the exact shape the
/// fixture records (and the exact shape the Python half replays).
#[derive(Debug, Default, PartialEq, Eq)]
struct Trace {
    outcome: String,
    end: String,
    batches: u64,
    blocks: u64,
    stale_batches: u64,
    end_of_stream: u64,
    beats: u64,
    aborted_at: Option<u64>,
    detail: Option<String>,
    records: Vec<Vec<RecordRow>>,
}

/// A two-hop V2 path — the shape the fixture's `path_a` resolves to (the same
/// fixture shape the crate's assembly tests pin).
fn two_hop_path() -> PathInfo {
    let pool = parse_address("0x1111111111111111111111111111111111111111").unwrap();
    let t0 = parse_address("0x2222222222222222222222222222222222222222").unwrap();
    let t1 = parse_address("0x3333333333333333333333333333333333333333").unwrap();
    let v2 = |token0_address, token1_address| {
        HopInfo::V2(V2HopInfo {
            pool_address: pool,
            token0_address,
            token1_address,
            fee: 30,
            zfo: true,
        })
    };
    PathInfo::new(vec![v2(t0, t1), v2(t1, t0)])
}

struct MapResolver(HashMap<u64, PathInfo>);

impl PathResolver for MapResolver {
    fn resolve(&self, path_id: u64) -> Option<PathInfo> {
        self.0.get(&path_id).cloned()
    }
}

struct NoopProbe;

impl ReceiptProbe for NoopProbe {
    fn receipt_found(
        &self,
        _tx_hash: alloy::primitives::B256,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = SubmissionResult<bool>> + Send + '_>>
    {
        Box::pin(async { Ok(false) })
    }
}

/// The session dispatcher the consumer drives (the one coordination owner).
fn dispatcher() -> Arc<Mutex<Dispatcher>> {
    Arc::new(Mutex::new(Dispatcher::for_block(0)))
}

/// A real production executor over a dead-port provider: every row in a script
/// skips pre-sim, so no RPC fires and the run stays offline and deterministic.
async fn offline_executor() -> Arc<BatchExecutor> {
    let provider = Arc::new(
        degenbot_rpc::provider::AlloyProvider::new("http://127.0.0.1:1", 0)
            .await
            .expect("a lazy provider constructs offline"),
    );
    let executor_address = parse_address("0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5").unwrap();
    let weth = parse_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
    let mut verdict = degenbot_config::BotConfig::default();
    verdict.simulation.pipeline_concurrency = 1;
    let runtime = ExecutorRuntime {
        max_candidates: 50,
        use_v4_batch: false,
        dry_run: true,
        resolver: Arc::new(MapResolver(HashMap::from([(PATH_A, two_hop_path())]))),
        suppression: Arc::new(Mutex::new(PathSuppression::new())),
        divergence: Arc::new(Mutex::new(PoolDivergence::new())),
        fot: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
        provider,
        executor_owner: parse_address("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266").unwrap(),
        executor_address,
        weth_address: weth,
        pool_manager_address: alloy::primitives::Address::ZERO,
        multicall3_address: alloy::primitives::Address::ZERO,
        injected_address: None,
        runtime_bytecode: Bytes::new(),
        warmup: degenbot_executor::compute_simulation_warmup_slots(executor_address, weth),
        bot_state: None,
        warm_cache: None,
        dispatcher: Arc::new(Mutex::new(Dispatcher::for_block(0))),
        signer: Arc::new(
            TxSigner::from_key_hex(
                "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
                1,
            )
            .unwrap(),
        ),
        probe: Arc::new(NoopProbe),
        nonce_lane: Arc::new(Arc::new(NonceAuthority::new(0)).mint_lane(
            "oracle",
            Arc::new(SubmissionLedger::new()),
            degenbot_substrate::nonce::LanePolicy::default(),
        )),
        extra_broadcast: Vec::new(),
        target: SubmissionTarget::Public,
    };
    Arc::new(BatchExecutor::new(ExecutorConfig::from_verdict(
        &verdict, runtime,
    )))
}

fn block_notification(oracle: &Value, name: &str) -> BlockNotification {
    let block = &oracle["blocks"][name];
    BlockNotification {
        number: block["number"].as_u64().unwrap(),
        timestamp: block["timestamp"].as_u64().unwrap(),
        base_fee_per_gas: block["base_fee_per_gas"].as_u64(),
        gas_used: block["gas_used"].as_u64().unwrap(),
        gas_limit: block["gas_limit"].as_u64().unwrap(),
    }
}

fn solve_result(row: &Value) -> degenbot_pools::solve::SolvePathResult {
    let u256 = |key: &str| row[key].as_u64().map_or_else(|| U256::ZERO, U256::from);
    let list = |key: &str| -> Vec<U256> {
        row[key]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .map(|v| U256::from(v.as_u64().unwrap()))
                    .collect()
            })
            .unwrap_or_default()
    };
    degenbot_pools::solve::SolvePathResult {
        optimal_input: u256("optimal_input"),
        profit: u256("profit"),
        hop_outputs: list("hop_outputs"),
        consumed_inputs: list("consumed_inputs"),
        state_nonces: row["state_nonces"]
            .as_array()
            .map(|values| values.iter().map(|v| v.as_u64().unwrap()).collect())
            .unwrap_or_default(),
        ..Default::default()
    }
}

fn payload(sim: &Value) -> degenbot_bot::arb_engine::SimulatedPathResult {
    degenbot_bot::arb_engine::SimulatedPathResult {
        path_id: live_id(sim["path"].as_str().unwrap()),
        gross_profit: U256::from(sim["gross_profit"].as_u64().unwrap()),
        net_profit: U256::from(sim["net_profit"].as_u64().unwrap()),
        gas_used: sim["gas_used"].as_u64().unwrap(),
        priority_fee: u128::from(sim["priority_fee"].as_u64().unwrap()),
        base_fee_next: u128::from(sim["base_fee_next"].as_u64().unwrap()),
        execute_calldata: Vec::new(),
        access_list: None,
        captured_swaps: Vec::new(),
        hop_count: 2,
        failure: None,
    }
}

/// Translate one scripted event into the session's own event value.
fn session_event(oracle: &Value, event: &Value) -> SessionEvent {
    match event["kind"].as_str().unwrap() {
        "block" => {
            SessionEvent::Block(block_notification(oracle, event["block"].as_str().unwrap()))
        }
        "batch" => {
            let rows: Vec<(u64, degenbot_pools::solve::SolvePathResult)> = event["rows"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .map(|row| (live_id(row["path"].as_str().unwrap()), solve_result(row)))
                        .collect()
                })
                .unwrap_or_default();
            // The payload map is the engine's own hashbrown map (not a direct
            // dependency here), so it is built through the batch value.
            let mut batch = ResultBatch {
                solve_block: event["solve_block"].as_u64().unwrap(),
                timestamp: 0,
                base_fee_per_gas: None,
                gas_used: 0,
                gas_limit: 0,
                fresh: rows,
                updated: Vec::new(),
                expired: Vec::new(),
                removed: Vec::new(),
                payloads: Default::default(),
            };
            for sim in event["payloads"].as_array().into_iter().flatten() {
                let sim = payload(sim);
                batch.payloads.insert(sim.path_id, sim);
            }
            SessionEvent::Batch(batch)
        }
        other => panic!("the oracle names an unknown event kind {other:?}"),
    }
}

fn simulate_label(verdict: &degenbot_batch_executor::SimulateVerdict) -> &'static str {
    use degenbot_batch_executor::SimulateVerdict as V;
    match verdict {
        V::Profitable(_) => "profitable",
        V::GasUnprofitable(_) => "gas-unprofitable",
        V::Failed(_) => "failed",
        V::Exception => "exception",
    }
}

/// Run one fixture scenario through the consumer and collect its trace.
async fn run_scenario(scenario: &Value) -> Trace {
    let oracle = oracle();
    let executor = offline_executor().await;
    let heartbeat = Heartbeat::new();
    let progress = SessionProgress::new();
    let mut consumer = SessionConsumer::new(
        Arc::clone(&executor),
        SessionClock::new(dispatcher()),
        ConsumerPolicy {
            allow_quiet_end: scenario["allow_quiet_end"].as_bool().unwrap(),
            heartbeat: Some(heartbeat.clone()),
            progress: progress.clone(),
        },
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    for event in scenario["events"].as_array().into_iter().flatten() {
        tx.send(session_event(&oracle, event)).unwrap();
    }
    drop(tx);

    // The loop quiesces the lane itself, so the adapter's remaining duty is
    // only the record drain (exactly the Python half's drain task).
    let outcome = consumer.consume(&mut rx).await;

    let mut records = Vec::new();
    while let Some(set) = executor.next_outcome().await {
        records.push(
            set.records
                .iter()
                .map(|record| RecordRow {
                    path: symbolic(record.path_id),
                    block: record.block,
                    assembly: record.assembly.label().to_string(),
                    simulate: record
                        .simulate
                        .as_ref()
                        .map_or_else(|| "none".to_string(), |v| simulate_label(v).to_string()),
                })
                .collect(),
        );
    }

    let expected = &scenario["expected"];
    assert_eq!(
        progress.batches(),
        expected["batches"].as_u64().unwrap(),
        "the progress view counts the enqueued batches"
    );

    match outcome {
        Ok(report) => Trace {
            outcome: "ok".to_string(),
            end: report
                .end
                .map_or_else(String::new, |end| end.name().to_string()),
            batches: report.batches,
            blocks: report.blocks,
            stale_batches: report.stale_batches,
            end_of_stream: report.end_of_stream,
            beats: heartbeat.seq(),
            aborted_at: None,
            detail: None,
            records,
        },
        Err(error) => Trace {
            outcome: "aborted".to_string(),
            end: error
                .report
                .end
                .map_or_else(String::new, |end| end.name().to_string()),
            batches: error.report.batches,
            blocks: error.report.blocks,
            stale_batches: error.report.stale_batches,
            end_of_stream: error.report.end_of_stream,
            beats: heartbeat.seq(),
            aborted_at: Some(error.batch_index),
            detail: Some(error.detail),
            records,
        },
    }
}

/// The fixture's expected trace for one scenario, in the same shape.
fn expected_trace(expected: &Value) -> Trace {
    Trace {
        outcome: expected["outcome"].as_str().unwrap().to_string(),
        end: expected["end"].as_str().unwrap().to_string(),
        batches: expected["batches"].as_u64().unwrap(),
        blocks: expected["blocks"].as_u64().unwrap(),
        stale_batches: expected["stale_batches"].as_u64().unwrap(),
        end_of_stream: expected["end_of_stream"].as_u64().unwrap(),
        beats: expected["beats"].as_u64().unwrap(),
        aborted_at: expected["aborted_at"].as_u64(),
        detail: None,
        records: expected["records"]
            .as_array()
            .map(|sets| {
                sets.iter()
                    .map(|set| {
                        set.as_array()
                            .unwrap()
                            .iter()
                            .map(|row| RecordRow {
                                path: row["path"].as_str().unwrap().to_string(),
                                block: row["block"].as_u64().unwrap(),
                                assembly: row["assembly"].as_str().unwrap().to_string(),
                                simulate: row["simulate"].as_str().unwrap().to_string(),
                            })
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The comparator: the observable trace against the recorded expectation. The
/// `detail_contains` fragments are the loud-abort detail's own contract.
fn compare(scenario: &Value, actual: &Trace) {
    let expected = &scenario["expected"];
    let mut want = expected_trace(expected);
    if want.outcome == "aborted" {
        // The detail is the leaf's own text; the fixture names fragments of it
        // (a leaf may add context, an oracle fragment may not drift free).
        want.detail.clone_from(&actual.detail);
    }
    assert_eq!(
        actual, &want,
        "scenario {:?} diverged from the shared oracle",
        scenario["name"]
    );
    if let Some(fragments) = expected["detail_contains"].as_array() {
        let detail = actual.detail.as_deref().unwrap_or_default();
        for fragment in fragments {
            let fragment = fragment.as_str().unwrap();
            assert!(
                detail.contains(fragment),
                "scenario {:?}: the abort detail {detail:?} does not carry {fragment:?}",
                scenario["name"]
            );
        }
    }
}

#[tokio::test]
async fn the_shared_event_sequence_oracle_pins_the_consumer_product() {
    let oracle = oracle();
    for scenario in oracle["scenarios"].as_array().unwrap() {
        let trace = run_scenario(scenario).await;
        compare(scenario, &trace);
    }
}

/// Teeth: an in-memory mutation of ONE expected value must fail the comparator
/// (the checked-in oracle is never written).
#[tokio::test]
async fn seeded_divergence_expected_mutation_fails() {
    let oracle = oracle();
    let scenario = oracle["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "multiple-batches-per-block")
        .unwrap();
    let actual = run_scenario(scenario).await;
    compare(scenario, &actual);

    let mut mutated = scenario.clone();
    mutated["expected"]["batches"] = Value::from(99_u64);
    let mutated_want = expected_trace(&mutated["expected"]);
    assert_ne!(
        actual, mutated_want,
        "a mutated expected batch count must fail the comparator"
    );

    let mut mutated_records = scenario.clone();
    mutated_records["expected"]["records"][0][0]["assembly"] = Value::from("assembled");
    let mutated_want = expected_trace(&mutated_records["expected"]);
    assert_ne!(
        actual, mutated_want,
        "a mutated expected assembly verdict must fail the comparator"
    );
}

/// The session half's teardown rule: a consumer whose session move is refused
/// still closes the session, and the failure carries the one shared teardown.
#[tokio::test]
async fn a_refused_resume_still_closes_the_session() {
    let bot = Arc::new(Bot::new(1));
    let driver = Arc::new(EngineDriver::new(
        Arc::clone(&bot),
        degenbot_config::holder::config_arc(),
    ));
    let handoff = driver.claim_consumer().expect("the session's one hand-off");
    let executor = offline_executor().await;
    let heartbeat = Heartbeat::new();
    let mut consumer = SessionConsumer::new(
        executor,
        SessionClock::new(dispatcher()),
        ConsumerPolicy {
            allow_quiet_end: true,
            heartbeat: Some(heartbeat),
            progress: SessionProgress::new(),
        },
    );

    consumer.attach(handoff, Arc::clone(&driver));

    // A session that never connected cannot resume: the refusal is typed, the
    // session is closed anyway, and the teardown is reported.
    let failure = consumer
        .resume()
        .await
        .expect_err("a detached session refuses the resume");
    assert!(
        failure.teardown.is_some(),
        "the refusal path still runs the one teardown"
    );
    assert!(
        driver.is_stopped(),
        "no consumer exit may leave a session running"
    );

    // The closed stream then ends the loop, and its own teardown is the same
    // idempotent value (one shared teardown, every caller).
    let report = tokio::time::timeout(std::time::Duration::from_secs(5), consumer.run())
        .await
        .expect("a closed session stream ends the loop")
        .expect("the closed stream is a quiet end");
    assert_eq!(report.consumer.end_of_stream, 1);
    assert_eq!(report.consumer.batches, 0);
    assert!(driver.is_stopped());
}

/// The session half's report is typed end to end: the consumer's counts, the
/// typed end verdict, and the driver's own teardown facts.
#[tokio::test]
async fn the_session_report_carries_the_typed_end_and_teardown() {
    let bot = Arc::new(Bot::new(1));
    let driver = Arc::new(EngineDriver::new(
        Arc::clone(&bot),
        degenbot_config::holder::config_arc(),
    ));
    let handoff = driver.claim_consumer().expect("the session's one hand-off");
    let executor = offline_executor().await;
    let mut consumer = SessionConsumer::new(
        executor,
        SessionClock::new(dispatcher()),
        ConsumerPolicy::new(false),
    );
    consumer.attach(handoff, Arc::clone(&driver));
    let _ = consumer.resume().await;

    let report = tokio::time::timeout(std::time::Duration::from_secs(5), consumer.run())
        .await
        .expect("a closed session stream ends the loop")
        .expect("a closed stream is not a leaf failure");
    assert_eq!(
        report.consumer.end,
        Some(degenbot_batch_executor::session::ConsumerEnd::PumpStopped),
        "a supervised session reads a body-started stream end as a pump stop"
    );
    assert_eq!(
        report.teardown.delivery,
        degenbot_core::channel_lifecycle::ChannelPhase::Closed
    );
}

/// The oracle's status vocabulary is closed: every recorded status is a
/// declared member, so a new verdict cannot slip into the fixture unwatched.
#[test]
fn the_oracle_vocabulary_is_closed() {
    let oracle = oracle();
    let vocabulary = &oracle["status_vocabulary"];
    let declared = |kind: &str| -> Vec<String> {
        vocabulary[kind]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    let ends = declared("end");
    let outcomes = declared("outcome");
    let assembly = declared("assembly");
    let simulate = declared("simulate");
    for scenario in oracle["scenarios"].as_array().unwrap() {
        let expected = &scenario["expected"];
        assert!(outcomes.contains(&expected["outcome"].as_str().unwrap().to_string()));
        assert!(ends.contains(&expected["end"].as_str().unwrap().to_string()));
        for set in expected["records"].as_array().into_iter().flatten() {
            for row in set.as_array().unwrap() {
                assert!(assembly.contains(&row["assembly"].as_str().unwrap().to_string()));
                assert!(simulate.contains(&row["simulate"].as_str().unwrap().to_string()));
            }
        }
    }
}
