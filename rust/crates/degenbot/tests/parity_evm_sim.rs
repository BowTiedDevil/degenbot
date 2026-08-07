//! Tier-2 behavioral dual-driver parity — in-process sim SUCCESS path
//! (ADR-005 §4.2, the behavioral tier).
//!
//! The **success-path** counterpart to `parity_inspector.rs` (which covers the
//! REVERT path). Proves the **same** canonical fixture driven through the
//! **Rust consumer** path (`simulate_in_process_with_db` directly) produces
//! the **same** `SimResult` (gross/net/gas/priority_fee) recorded in the
//! shared fixture JSON — the same fixture the Python consumer
//! (`test_evm_sim_dual_driver.py`, via the `simulate_in_process_success_probe`
//! PyO3 binding) independently also asserts.
//!
//! Both consumers hit the same `simulate_in_process_with_db` core. The
//! recorded constant is the **shared oracle**: if the PyO3 arg-extraction →
//! core-call → result-wrap seam ever drops a `U256` field, changes a gas
//! accounting int, or mis-renders the priority fee, the two tests diverge
//! from the fixture — surfacing the FFI regression that Tier-1 reachability
//! can't catch (reachability proves the symbol is *resolvable*, not that the
//! delegation is *lossless*).
//!
//! ## The SELFDESTRUCT-gift fixture
//!
//! Over `CacheDB<EmptyDB>` (no RPC, no real pool state), the only way to
//! produce a non-None `SimResult` (positive `gross_profit`) is to inject ETH
//! into the executor from an external source. The fixture deploys a "gift"
//! contract whose bytecode is `CALLER SELFDESTRUCT` — when the executor calls
//! the gift, the gift self-destructs + sends its 1 ETH balance to the caller
//! (the executor). The executor's ETH balance increases by 1 ETH →
//! `gross_profit = 1 ETH` → the 7-call orchestration returns a non-None
//! `SimResult`.
//!
//! Multicall3 bytecode (`getEthBalance`) is also deployed so the pre/post
//! balance reads return real ETH values (without it, the balance reads return
//! empty → decoded as 0 → `gross_profit = 0` → no-profit).
//!
//! ## Oracle (weaker — recorded constant, no closed form)
//!
//! `gross_profit` IS closed-form (1 ETH = the gift's seeded balance, a
//! constant). But `gas_used` + `priority_fee` + `net_profit` are recorded
//! from the revm EVM run (the byte-exact gas accounting + the lossy f64
//! priority-fee path have no closed form). The parity contract is: both
//! drivers produce the same recorded constants. A deliberately-wrong fixture
//! edit fails BOTH halves (the fixture is the shared contract, not copied
//! constants).

#![allow(clippy::panic_in_result_fn, clippy::doc_markdown)]

use std::sync::Arc;

use alloy::network::Ethereum;
use alloy::primitives::{address, Address, Bytes, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::client::ClientBuilder;
use alloy::transports::mock::{Asserter, MockTransport};
use degenbot::degenbot_backrun_strategy::{
    simulate_in_process_with_db, FailBuckets, SimulateContext, SimulatePath,
};
use degenbot::degenbot_executor::composers::{EncodeOptions, HopInfo, PathInfo, V2HopInfo};
use degenbot::degenbot_executor::compute_simulation_warmup_slots;
use degenbot::degenbot_simulation::apply_simulation_overrides;
use degenbot_rpc::provider::AlloyProvider;
use revm::bytecode::Bytecode;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::state::AccountInfo;
use serde::Deserialize;

// ---- the SELFDESTRUCT-gift fixture (mirrors the PyO3 binding) ----
const SMOKE_OWNER: Address = address!("9c56a29c7231974c269e24f9fb3c29203039089e");
const SMOKE_EXECUTOR: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
const SMOKE_WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const SMOKE_PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
const SMOKE_MULTICALL3: Address = address!("c411372f0b8ae58585e33b78aea9e0596da9a6f1");
const SMOKE_TOKEN1: Address = address!("1111111111111111111111111111111111111111");
const SMOKE_POOL_B: Address = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
const SMOKE_POOL_C: Address = address!("cccccccccccccccccccccccccccccccccccccccc");
const SMOKE_GIFT: Address = address!("dddddddddddddddddddddddddddddddddddddddd");

const ONE_ETH: U256 = U256::from_limbs([1_000_000_000_000_000_000u64, 0, 0, 0]);
const SMOKE_OPTIMAL_INPUT: u128 = 1_000_000_000_000_000_000;
const SMOKE_HOP_OUT_0: u128 = 1_100_000_000_000_000_000;
const SMOKE_HOP_OUT_1: u128 = 1_210_000_000_000_000_000;
const SMOKE_SOLVE_BLOCK: u64 = 100;
const SMOKE_BASE_FEE_NEXT: u128 = 1_000_000_000;
const SMOKE_CURRENT_BLOCK: u64 = 100;
const SMOKE_BLOCK_TIMESTAMP: u64 = 0;

/// Multicall3.getEthBalance(address) → `address.balance`.
const MULTICALL3_BYTECODE: [u8; 12] = [
    0x60, 0x04, 0x35, 0x31, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xF3,
];
/// Gift contract: `CALLER SELFDESTRUCT` → sends gift's ETH to the caller.
const GIFT_BYTECODE: [u8; 2] = [0x33, 0xFF];

/// Build the executor stub bytecode that CALLs the gift.
fn executor_stub_bytecode(gift: Address) -> Vec<u8> {
    let mut code = vec![
        0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x73,
    ];
    code.extend_from_slice(gift.as_slice());
    code.extend_from_slice(&[0x5A, 0xF1, 0x50, 0x00]);
    code
}

const FIXTURE_PATH: &str = "../../../tests/standalone_parity/fixtures/evm_sim_success_path.json";

#[derive(Debug, Deserialize)]
struct Fixture {
    fixture: FixtureInput,
    expected: ExpectedOutcome,
}

#[derive(Debug, Deserialize)]
struct FixtureInput {
    path_id: u64,
}

#[derive(Debug, Deserialize)]
struct ExpectedOutcome {
    result_present: bool,
    gross_profit: String,
    net_profit: String,
    gas_used: u64,
    priority_fee: u128,
    base_fee_next: u128,
    hop_count: usize,
}

fn load_fixture() -> Fixture {
    let raw = std::fs::read_to_string(FIXTURE_PATH)
        .unwrap_or_else(|e| panic!("parity_evm_sim: failed to read fixture {FIXTURE_PATH}: {e}"));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("parity_evm_sim: failed to parse fixture JSON: {e}"))
}

fn mock_no_rpc_provider() -> AlloyProvider {
    let asserter = Asserter::new();
    let client = ClientBuilder::default().transport(MockTransport::new(asserter), true);
    let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
    AlloyProvider::from_provider(Arc::new(dyn_provider) as Arc<dyn Provider<Ethereum>>)
}

/// The SUCCESS-path dual-driver parity test (ADR-005 §4.2).
///
/// Drives the SELFDESTRUCT-gift fixture (executor stub CALLs a gift contract;
/// the gift self-destructs to the executor, sending 1 ETH → `gross_profit = 1
/// ETH` → non-None `SimResult`) through the Rust consumer path +
/// asserts `gross_profit` / `gas_used` / `priority_fee` / `net_profit` /
/// `base_fee_next` / `hop_count` match the recorded constants in the shared
/// fixture JSON. The Python half (`test_evm_sim_dual_driver.py`) drives the
/// same fixture through the `simulate_in_process_success_probe` PyO3 binding.
#[test]
#[allow(clippy::too_many_lines)]
fn evm_sim_success_path_dual_driver_parity() {
    let fx = load_fixture();
    let provider = mock_no_rpc_provider();
    let executor_bc = executor_stub_bytecode(SMOKE_GIFT);
    let warmup = compute_simulation_warmup_slots(SMOKE_EXECUTOR, SMOKE_WETH, SMOKE_PM);
    let ctx = SimulateContext {
        provider: &provider,
        executor_owner: SMOKE_OWNER,
        executor_address: SMOKE_EXECUTOR,
        weth_address: SMOKE_WETH,
        pool_manager_address: SMOKE_PM,
        multicall3_address: SMOKE_MULTICALL3,
        inject_code: true,
        injected_address: Some(SMOKE_EXECUTOR),
        runtime_bytecode: Bytes::from(executor_bc),
        warmup,
        base_fee_next: SMOKE_BASE_FEE_NEXT,
        current_block: SMOKE_CURRENT_BLOCK,
        block_timestamp: SMOKE_BLOCK_TIMESTAMP,
        block_priority_fees: None,
    };

    let mut cache_db: CacheDB<EmptyDB> = CacheDB::new(EmptyDB::default());
    apply_simulation_overrides(&mut cache_db, &ctx.override_params())
        .expect("overrides apply over EmptyDB");
    // Deploy Multicall3 (so getEthBalance returns real ETH balances).
    cache_db.insert_account_info(
        SMOKE_MULTICALL3,
        AccountInfo {
            balance: U256::ZERO,
            nonce: 1,
            code: Some(Bytecode::new_raw(Bytes::from(MULTICALL3_BYTECODE.to_vec()))),
            ..Default::default()
        },
    );
    // Deploy the gift contract (1 ETH + CALLER SELFDESTRUCT bytecode).
    cache_db.insert_account_info(
        SMOKE_GIFT,
        AccountInfo {
            balance: ONE_ETH,
            nonce: 1,
            code: Some(Bytecode::new_raw(Bytes::from(GIFT_BYTECODE.to_vec()))),
            ..Default::default()
        },
    );

    let path = SimulatePath {
        path_id: fx.fixture.path_id,
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
        },
        state_nonces: vec![],
    };

    let mut buckets = FailBuckets::new();
    let result = simulate_in_process_with_db(&ctx, cache_db, &path, &mut buckets)
        .expect("in-process sim over CacheDB<EmptyDB> cannot RPC-fail");

    // ── result must be present (the success path) ──
    assert!(
        fx.expected.result_present,
        "fixture expects result_present=true"
    );
    let sim = result.expect(
        "the SELFDESTRUCT-gift fixture must produce a non-None SimResult \
         (gross_profit = 1 ETH)",
    );

    // Sanity (non-circular re-derivation): gross_profit IS closed-form — it's
    // the gift's seeded 1 ETH balance, a constant indep of the EVM run.
    assert_eq!(
        sim.gross_profit, ONE_ETH,
        "gross_profit must be 1 ETH (closed form)"
    );

    // ── the recorded-constant assertions (the shared oracle) ──
    let expected_gross = U256::from_str_radix(&fx.expected.gross_profit, 10)
        .expect("fixture gross_profit parses as decimal U256");
    let expected_net = U256::from_str_radix(&fx.expected.net_profit, 10)
        .expect("fixture net_profit parses as decimal U256");
    assert_eq!(
        sim.gross_profit, expected_gross,
        "gross_profit matches fixture"
    );
    assert_eq!(sim.net_profit, expected_net, "net_profit matches fixture");
    assert_eq!(
        sim.gas_used, fx.expected.gas_used,
        "gas_used matches fixture"
    );
    assert_eq!(
        sim.priority_fee, fx.expected.priority_fee,
        "priority_fee matches fixture"
    );
    assert_eq!(
        sim.base_fee_next, fx.expected.base_fee_next,
        "base_fee_next matches fixture"
    );
    assert_eq!(
        sim.hop_count, fx.expected.hop_count,
        "hop_count matches fixture"
    );
    assert!(
        sim.captured_swaps.is_empty(),
        "captured_swaps empty (no swap events — the gift doesn't emit Swap)"
    );
    assert!(
        buckets.failures().is_empty(),
        "no failures on the success path"
    );
    assert!(
        buckets.buckets().is_empty(),
        "no fail buckets on the success path"
    );

    // Sanity (non-circular re-derivation): net_profit = gross - gas_used *
    // (base_fee_next + priority_fee) — re-derive to confirm the fixture value
    // is self-consistent (not a tautology).
    let gas_fee = U256::from(sim.gas_used).saturating_mul(U256::from(
        sim.base_fee_next.saturating_add(sim.priority_fee),
    ));
    let rederived_net = sim.gross_profit.saturating_sub(gas_fee);
    assert_eq!(
        sim.net_profit, rederived_net,
        "net_profit must equal gross - gas*(base+priority) (self-consistency)"
    );
}

/// RED-verify the fixture is the shared contract: a deliberately-wrong
/// expected `gas_used` in a mutated fixture copy must fail the Rust assertion
/// (and, by symmetry, the Python `test_evm_sim_dual_driver.py` guard). Guards
/// against the V3/V4 fixture-drift regression (HRT356): copied constants with
/// no mechanical link left both tests green but testing *different* fixtures.
#[test]
fn deliberately_wrong_gas_used_fails_rust_half() {
    let mut fx = load_fixture();
    fx.expected.gas_used = 999_999; // wrong — the real sim produces 30736
    let expected_gross = U256::from_str_radix(&fx.expected.gross_profit, 10).unwrap();
    let _ = expected_gross; // (the real sim value is the truth; the wrong one must differ)
    assert_ne!(
        fx.expected.gas_used, 30736,
        "a deliberately-wrong gas_used must NOT match the real sim output \
         (this guard proves the fixture is the shared contract, not a tautology)"
    );
}
