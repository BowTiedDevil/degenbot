#![expect(clippy::expect_used, clippy::panic)]
//! Tier-2 behavioral dual-driver parity — revm inspector (ADR-005 standalone
//! claim, the behavioral tier).
//!
//! The Tier-1 `reachability.rs` test proves `simulate_in_process_with_db` +
//! `SimInspector` are *resolvable* from a standalone Rust consumer. This
//! Tier-2 test proves the inspector's revert attribution is *lossless* across
//! the FFI: the **same** canonical fixture (the `0xcafebabe` REVERT stub over
//! a 2-hop V2 path against `CacheDB<EmptyDB>`), driven through the **Rust
//! consumer** path (`simulate_in_process_with_db` directly), produces the
//! **same** reverting-frame + captured-swaps + bucket output recorded in the
//! shared fixture JSON.
//!
//! The matching Python side lives at
//! `tests/standalone_parity/test_inspector_dual_driver.py` — it drives the
//! **same** fixture through the Python consumer path (the
//! `simulate_in_process_revert_probe` PyO3 binding). Both consumers hit the
//! same `simulate_in_process_with_db` + `SimInspector` core; the recorded
//! constant is the shared oracle. If the PyO3 arg-extraction → core-call →
//! result-wrap seam ever drops a field, changes an address format, or
//! mis-renders the revert bytes, the two tests diverge from the fixture.
//!
//! ## Oracle (weaker — recorded constant, no closed form)
//!
//! Unlike the V2/V3/V4 calc parity pairs (which derive `amount_out` from a
//! closed-form `getAmountOut`), the inspector runs a full revm EVM — there is
//! no closed-form derivation of the reverting-frame output. The expected
//! output is a **recorded constant** captured from the
//! `simulate_in_process_with_db_revert_with_data_attributes_reverting_frame`
//! Rust smoke test (the byte-exact EVM run). The parity contract is: both
//! drivers produce that same recorded constant. A non-circular re-derivation
//! (for example, asserting the revert_data equals the bytecode's `0xcafebabe`
//! literal + the label is `classify_revert` on that selector) is noted inline
//! as a sanity check, but the byte-exact fixture comparison is the real gate.
//!
//! V4 slice is deferred (gated on `5RI47E`, the transient V4 pool seeder).

#![expect(clippy::doc_markdown)]

use std::collections::HashMap;
use std::sync::Arc;

use alloy::network::Ethereum;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::client::ClientBuilder;
use alloy::transports::mock::{Asserter, MockTransport};
use degenbot::degenbot_executor::composers::{EncodeOptions, HopInfo, PathInfo, V2HopInfo};
use degenbot::degenbot_executor::{compute_simulation_warmup_slots, WarmupSlots};
use degenbot::degenbot_settlement_strategy::{
    simulate_in_process_with_db, FailBuckets, SimulateContext, SimulatePath,
};
use degenbot::degenbot_simulation::apply_simulation_overrides;
use degenbot_rpc::provider::AlloyProvider;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use serde::Deserialize;

// ---- the hardcoded smoke fixture (mirrors the Rust smoke test + the PyO3
//      `simulate_in_process_revert_probe` binding — the shared contract) ----
const SMOKE_OWNER: alloy::primitives::Address =
    alloy::primitives::address!("9c56a29c7231974c269e24f9fb3c29203039089e");
const SMOKE_EXECUTOR: alloy::primitives::Address =
    alloy::primitives::address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
const SMOKE_WETH: alloy::primitives::Address =
    alloy::primitives::address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const SMOKE_PM: alloy::primitives::Address =
    alloy::primitives::address!("000000000004444c5dc75cb358380d2e3de08a90");
const SMOKE_MULTICALL3: alloy::primitives::Address =
    alloy::primitives::address!("c411372f0b8ae58585e33b78aea9e0596da9a6f1");
const SMOKE_TOKEN1: alloy::primitives::Address =
    alloy::primitives::address!("1111111111111111111111111111111111111111");
const SMOKE_POOL_B: alloy::primitives::Address =
    alloy::primitives::address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
const SMOKE_POOL_C: alloy::primitives::Address =
    alloy::primitives::address!("cccccccccccccccccccccccccccccccccccccccc");

const SMOKE_OPTIMAL_INPUT: u128 = 1_000_000_000_000_000_000;
const SMOKE_HOP_OUT_0: u128 = 1_100_000_000_000_000_000;
const SMOKE_HOP_OUT_1: u128 = 1_210_000_000_000_000_000;
const SMOKE_SOLVE_BLOCK: u64 = 100;
const SMOKE_BASE_FEE_NEXT: u128 = 1_000_000_000;
const SMOKE_CURRENT_BLOCK: u64 = 100;
const SMOKE_BLOCK_TIMESTAMP: u64 = 0;

// The shared fixture JSON path (relative to the crate root).
const FIXTURE_PATH: &str =
    "../../../tests/standalone_parity/fixtures/inspector_cafebabe_revert.json";

#[derive(Debug, Deserialize)]
struct Fixture {
    fixture: FixtureInput,
    expected: ExpectedOutcome,
}

#[derive(Debug, Deserialize)]
struct FixtureInput {
    path_id: u64,
    runtime_bytecode_hex: String,
}

#[derive(Debug, Deserialize)]
struct ExpectedOutcome {
    #[serde(default)]
    #[expect(dead_code)]
    result: Option<serde_json::Value>,
    fail_buckets: HashMap<String, u64>,
    failures: Vec<ExpectedFailure>,
}

#[derive(Debug, Deserialize)]
struct ExpectedFailure {
    path_id: u64,
    bucket: String,
    fail_index: Option<u64>,
    revert_data: String,
    reverting_frame: Option<ExpectedRevertingFrame>,
    captured_swaps: Vec<serde_json::Value>,
    optimal_input: u128,
    hop_outputs: Vec<u128>,
}

#[derive(Debug, Deserialize)]
struct ExpectedRevertingFrame {
    depth: usize,
    target: String,
    selector: String,
    revert_data: String,
    label: String,
}

fn load_fixture() -> Fixture {
    let raw = std::fs::read_to_string(FIXTURE_PATH)
        .unwrap_or_else(|e| panic!("parity_inspector: failed to read fixture {FIXTURE_PATH}: {e}"));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("parity_inspector: failed to parse fixture JSON: {e}"))
}

fn mock_no_rpc_provider() -> AlloyProvider {
    let asserter = Asserter::new();
    let client = ClientBuilder::default().transport(MockTransport::new(asserter), true);
    let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
    AlloyProvider::from_provider(Arc::new(dyn_provider) as Arc<dyn Provider<Ethereum>>)
}

fn smoke_warmup() -> WarmupSlots {
    compute_simulation_warmup_slots(SMOKE_EXECUTOR, SMOKE_WETH, SMOKE_PM)
}

fn smoke_v2_path(path_id: u64) -> SimulatePath {
    SimulatePath {
        path_id,
        optimal_input: SMOKE_OPTIMAL_INPUT,
        hop_outputs: vec![SMOKE_HOP_OUT_0, SMOKE_HOP_OUT_1],
        consumed_inputs: vec![SMOKE_HOP_OUT_0, SMOKE_HOP_OUT_1],
        path_info: PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: SMOKE_POOL_B,
                token0_address: SMOKE_WETH,
                token1_address: SMOKE_TOKEN1,
                fee: 30,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: SMOKE_POOL_C,
                token0_address: SMOKE_TOKEN1,
                token1_address: SMOKE_WETH,
                fee: 30,
                zfo: true,
            }),
        ]),
        solve_block: SMOKE_SOLVE_BLOCK,
        opts: EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: false,
            ..Default::default()
        },
        state_nonces: vec![],
    }
}

/// The smoke-test fixture drives the `0xcafebabe` REVERT executor through the
/// in-process revm EVM + asserts the recorded reverting-frame + bucket +
/// captured-swaps output matches the shared fixture JSON.
///
/// This is the Rust half of the ADR-005 Tier-2 dual-driver parity pair for
/// the inspector. The Python half
/// (`tests/standalone_parity/test_inspector_dual_driver.py`) drives the same
/// fixture through the `simulate_in_process_revert_probe` PyO3 binding.
#[test]
#[expect(clippy::too_many_lines)]
fn inspector_dual_driver_parity_cafebabe_revert() {
    let fx = load_fixture();
    let provider = mock_no_rpc_provider();
    // Decode the executor runtime bytecode from the fixture hex.
    let runtime_bytecode = alloy::primitives::hex::decode(
        fx.fixture
            .runtime_bytecode_hex
            .strip_prefix("0x")
            .unwrap_or(&fx.fixture.runtime_bytecode_hex),
    )
    .expect("fixture runtime_bytecode_hex decodes");
    let ctx = SimulateContext {
        provider: &provider,
        executor_owner: SMOKE_OWNER,
        executor_address: SMOKE_EXECUTOR,
        weth_address: SMOKE_WETH,
        pool_manager_address: SMOKE_PM,
        multicall3_address: SMOKE_MULTICALL3,
        inject_code: true,
        injected_address: Some(SMOKE_EXECUTOR),
        runtime_bytecode: alloy::primitives::Bytes::from(runtime_bytecode),
        warmup: smoke_warmup(),
        base_fee_next: SMOKE_BASE_FEE_NEXT,
        current_block: SMOKE_CURRENT_BLOCK,
        block_timestamp: SMOKE_BLOCK_TIMESTAMP,
        block_priority_fees: None,
    };
    let mut cache_db: CacheDB<EmptyDB> = CacheDB::new(EmptyDB::default());
    apply_simulation_overrides(&mut cache_db, &ctx.override_params())
        .expect("overrides apply over EmptyDB");
    let mut buckets = FailBuckets::new();
    let path = smoke_v2_path(fx.fixture.path_id);
    let result = simulate_in_process_with_db(&ctx, cache_db, &path, &mut buckets)
        .expect("in-process sim over CacheDB<EmptyDB> cannot RPC-fail");
    assert!(
        result.is_none(),
        "reverting execute returns None (matches fixture expected.result=null)"
    );

    // ── fail_buckets ──
    let expected = &fx.expected.fail_buckets;
    let actual: HashMap<String, u64> = buckets
        .buckets()
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    assert_eq!(
        &actual, expected,
        "fail_buckets must match the shared fixture"
    );

    // ── failures ──
    let failures = buckets.failures();
    assert_eq!(
        failures.len(),
        fx.expected.failures.len(),
        "one failure recorded (matches fixture)"
    );
    let f = &failures[0];
    let ef = &fx.expected.failures[0];
    assert_eq!(f.path_id, ef.path_id, "path_id round-trips");
    assert_eq!(f.bucket, ef.bucket, "bucket label matches");
    assert_eq!(
        f.fail_index.map(|i| i as u64),
        ef.fail_index,
        "fail_index matches"
    );
    // revert_data: fixture is "0xcafebabe", f.revert_data is raw bytes.
    let expected_revert_hex = ef.revert_data.strip_prefix("0x").unwrap_or(&ef.revert_data);
    assert_eq!(
        alloy::primitives::hex::encode(&f.revert_data),
        expected_revert_hex,
        "revert_data bytes match the fixture"
    );

    // reverting_frame — the deep attribution.
    match (&f.reverting_frame, &ef.reverting_frame) {
        (Some(rf), Some(erf)) => {
            assert_eq!(rf.depth, erf.depth, "reverting_frame.depth");
            assert_eq!(
                format!("{:#x}", rf.target),
                erf.target,
                "reverting_frame.target (EIP-55 lowercase)"
            );
            assert_eq!(
                format!("0x{}", alloy::primitives::hex::encode(rf.selector)),
                erf.selector,
                "reverting_frame.selector"
            );
            assert_eq!(
                format!("0x{}", alloy::primitives::hex::encode(&rf.revert_data)),
                erf.revert_data,
                "reverting_frame.revert_data"
            );
            assert_eq!(rf.label, erf.label, "reverting_frame.label");
            // Sanity (non-circular re-derivation): the label is
            // `classify_revert` on the revert_data — so it must contain the
            // revert_data hex (the `0xcafebabe` selector) or be "empty" for
            // a Halt with no revert data.
            let revert_hex = alloy::primitives::hex::encode(&rf.revert_data);
            assert!(
                rf.label.contains(&revert_hex) || rf.label == "empty",
                "label `{}` is classify_revert on revert_data `{}`",
                rf.label,
                revert_hex
            );
        }
        (None, None) => {}
        _ => panic!(
            "reverting_frame presence mismatch: actual={:?} expected={:?}",
            f.reverting_frame.is_some(),
            ef.reverting_frame.is_some()
        ),
    }

    // captured_swaps — empty for the cafebabe stub (no swap events before the
    // immediate revert).
    assert!(
        f.captured_swaps.is_empty(),
        "captured_swaps empty (matches fixture)"
    );
    assert_eq!(
        ef.captured_swaps,
        Vec::<serde_json::Value>::new(),
        "fixture captured_swaps is empty"
    );

    // optimal_input + hop_outputs (the solver's EXPECTED amounts — the
    // [sim-diag] classifier's basis).
    assert_eq!(f.optimal_input, ef.optimal_input, "optimal_input matches");
    assert_eq!(f.hop_outputs, ef.hop_outputs, "hop_outputs match");
}
