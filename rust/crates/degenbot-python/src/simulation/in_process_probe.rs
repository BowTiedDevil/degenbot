//! `simulate_in_process_revert_probe` — the ADR-005 Tier-2 dual-driver
//! parity entrypoint for the revm inspector.
//!
//! The production inspector path (`dispatch_profitable_py`) runs over a real
//! provider (`WrapDatabaseAsync<AlloyDB>`) — the cold-miss fallback RPCs the
//! node. The Tier-2 dual-driver parity test can't depend on a live RPC, so
//! this binding exposes the **in-process** path (`simulate_in_process_with_db`
//! over `CacheDB<EmptyDB>`) — no RPC, no `BotState` tracked pools. The
//! executor is injected with caller-supplied runtime bytecode; the 7-call
//! orchestration runs against the empty DB (all balance reads return zero,
//! `execute()` either reverts or no-ops).
//!
//! This is a **test-only parity probe**, not a production entrypoint: the
//! fixture (addresses, 2-hop V2 path, block env) is hardcoded to mirror the
//! `simulate_in_process_with_db_revert_with_data_attributes_reverting_frame`
//! Rust smoke test. The Python parity test
//! (`tests/standalone_parity/test_inspector_dual_driver.py`) drives the SAME
//! fixture + asserts the SAME recorded output (`reverting_frame`, `captured_swaps`,
//! bucket) the Rust parity test
//! (`rust/crates/degenbot/tests/parity_inspector.rs`) does. The shared fixture
//! is `tests/standalone_parity/fixtures/inspector_cafebabe_revert.json`.
//!
//! Per ADR-013 (the FFI seam is private), this is a thin `PyO3` wrapper: arg
//! extraction → core call → result wrap. No business logic.

use std::sync::Arc;

use alloy::network::Ethereum;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::client::ClientBuilder;
use alloy::transports::mock::{Asserter, MockTransport};
use degenbot_backrun_strategy::{
    simulate_in_process_with_db, FailBuckets, SimulateContext, SimulatePath,
};
use degenbot_executor::composers::{EncodeOptions, HopInfo, PathInfo, V2HopInfo};
use degenbot_executor::{compute_simulation_warmup_slots, WarmupSlots};
use degenbot_rpc::provider::AlloyProvider;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;

use crate::simulation::outcome::captured_swap_to_dict;

// ---- the SELFDESTRUCT-gift success-path fixture (mirrors the Rust
//      parity_evm_sim.rs test — a non-None SimResult over CacheDB<EmptyDB>) ----
// The executor stub CALLs a "gift" contract; the gift SELFDESTRUCTs to the
// executor (CALLER), sending its 1 ETH balance → gross_profit = 1 ETH → the
// only success-path (non-None SimResult) achievable over CacheDB<EmptyDB>
// (no real pool state — the profit comes from the gift's ETH, not a swap).
// Multicall3 bytecode is deployed so `getEthBalance` returns real balances
// (without it, the pre/post balance reads return empty → Decoded as 0 →
// gross_profit = 0 → no-profit).
const SMOKE_GIFT: alloy::primitives::Address =
    alloy::primitives::address!("dddddddddddddddddddddddddddddddddddddddd");
const ONE_ETH: alloy::primitives::U256 =
    alloy::primitives::U256::from_limbs([1_000_000_000_000_000_000u64, 0, 0, 0]);

/// Multicall3.getEthBalance(address) → `address.balance`.
/// `PUSH1 0x04 CALLDATALOAD BALANCE PUSH1 0x00 MSTORE PUSH1 0x20 PUSH1 0x00 RETURN`
const MULTICALL3_BYTECODE: [u8; 12] = [
    0x60, 0x04, 0x35, 0x31, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xF3,
];
/// Gift contract: `CALLER SELFDESTRUCT` → sends gift's ETH to the caller.
const GIFT_BYTECODE: [u8; 2] = [0x33, 0xFF];

/// Build the executor stub bytecode that CALLs the gift (joyless: the
/// the execute(bytes,uint256) calldata is ignored — the stub just CALLs the
/// gift + stops). `PUSH1 0x00 ×5 PUSH20 <gift> GAS CALL POP STOP`.
fn executor_stub_bytecode(gift: alloy::primitives::Address) -> Vec<u8> {
    let mut code = vec![
        0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x73,
    ];
    code.extend_from_slice(gift.as_slice());
    code.extend_from_slice(&[0x5A, 0xF1, 0x50, 0x00]);
    code
}

// ---- the hardcoded smoke fixture (mirrors the Rust smoke test) ----
// Constants are `const` (not `static`) so they're compiled inline + have no
// `Sync` requirement — matches the `simulate_in_process_with_db_*` smoke
// tests in `degenbot-backrun-strategy`.
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

/// Build a mock `AlloyProvider` whose transport queue is empty (never called
/// in the in-process `CacheDB<EmptyDB>` path — all reads hit the cache). If
/// somehow called, the empty queue returns a transport error — the correct
/// behavior for a no-RPC probe.
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
        state_nonces: vec![],
        opts: EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: false,
        },
    }
}

/// Drive the `0xcafebabe`-style REVERT fixture (executor injected with
/// caller-supplied runtime bytecode) through the in-process revm EVM —
/// ADR-005 Tier-2 dual-driver parity entrypoint for the inspector.
///
/// The fixture (addresses, 2-hop V2 path, block env) is hardcoded to mirror
/// the Rust smoke test; the caller supplies only `path_id` (so it round-trips
/// through the FFI) + `runtime_bytecode` (the executor's injected bytecode —
/// e.g. the `0xcafebabe` REVERT stub or `0xfe` INVALID).
///
/// Returns a dict:
///   - `result` (`None`) — the in-process path returns `None` for reverts
///     + non-profitable outcomes (the revert is tallied in `failures`).
///   - `failures` (`list[dict]`) — one entry per failed candidate, each
///     carrying `path_id`, `bucket`, `fail_index`, `revert_data`,
///     `reverting_frame`, `captured_swaps`, `optimal_input`, `hop_outputs`
///     (same shape as `PyDispatchOutcome.failures()`).
///   - `fail_buckets` (`dict[str, int]`) — the bucket tally.
///
/// No RPC. `CacheDB<EmptyDB>` handles all state reads (empty → zero). The
/// provider is a mock with an empty queue (errors if somehow called).
///
/// # Errors
///
/// `RuntimeError` only on an unrecoverable revm `transact` error (a DB
/// cold-miss RPC failure — cannot happen over `CacheDB<EmptyDB>`, but the
/// `ProviderResult` is honored for parity with the production signature).
#[pyfunction]
#[pyo3(signature = (path_id, runtime_bytecode))]
#[allow(clippy::too_many_lines)] // PyO3 FFI dict-building surface; one set_item per field
pub fn simulate_in_process_revert_probe<'py>(
    py: Python<'py>,
    path_id: u64,
    runtime_bytecode: &Bound<'_, PyBytes>,
) -> PyResult<Bound<'py, PyDict>> {
    // 1. Build the ctx (mock provider — never called over CacheDB<EmptyDB>).
    let provider = mock_no_rpc_provider();
    let byte_slice = runtime_bytecode.as_bytes();
    let ctx = SimulateContext {
        provider: &provider,
        executor_owner: SMOKE_OWNER,
        executor_address: SMOKE_EXECUTOR,
        weth_address: SMOKE_WETH,
        pool_manager_address: SMOKE_PM,
        multicall3_address: SMOKE_MULTICALL3,
        inject_code: true,
        injected_address: Some(SMOKE_EXECUTOR),
        runtime_bytecode: alloy::primitives::Bytes::copy_from_slice(byte_slice),
        warmup: smoke_warmup(),
        base_fee_next: SMOKE_BASE_FEE_NEXT,
        current_block: SMOKE_CURRENT_BLOCK,
        block_timestamp: SMOKE_BLOCK_TIMESTAMP,
        block_priority_fees: None,
    };

    // 2. Build the CacheDB + apply the executor-injection overrides.
    let mut cache_db: CacheDB<EmptyDB> = CacheDB::new(EmptyDB::default());
    degenbot_simulation::apply_simulation_overrides(&mut cache_db, &ctx.override_params())
        .map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "simulate_in_process_revert_probe: override apply failed: {e}"
            ))
        })?;

    // 3. Run the 7-call orchestration (GIL-free — release is implicit: no
    //    Python objects held across the call).
    let mut buckets = FailBuckets::new();
    let path = smoke_v2_path(path_id);
    let result = simulate_in_process_with_db(&ctx, cache_db, &path, &mut buckets).map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!(
            "simulate_in_process_revert_probe: sim failed: {e}"
        ))
    })?;

    // 4. Wrap the outcome as a dict (mirror PyDispatchOutcome.failures()).
    let out = PyDict::new(py);
    // `result` is `None` for reverts/non-profitable (the only paths this probe
    // exercises). A success-path `SimResult` is non-trivial to render + isn't
    // needed for the revert-parity fixture.
    out.set_item("result", py.None())?;
    debug_assert!(result.is_none(), "revert-probe fixture expects None result");

    let failures_list = PyList::empty(py);
    for f in buckets.failures() {
        let fdict = PyDict::new(py);
        fdict.set_item("path_id", f.path_id)?;
        fdict.set_item("bucket", &f.bucket)?;
        match f.fail_index {
            Some(idx) => fdict.set_item("fail_index", idx)?,
            None => fdict.set_item("fail_index", py.None())?,
        }
        let hex_str = alloy::primitives::hex::encode(&f.revert_data);
        fdict.set_item("revert_data", format!("0x{hex_str}"))?;
        match &f.reverting_frame {
            Some(rf) => {
                let rdict = PyDict::new(py);
                rdict.set_item("depth", rf.depth)?;
                rdict.set_item("target", format!("{:#x}", rf.target))?;
                rdict.set_item(
                    "selector",
                    format!("0x{}", alloy::primitives::hex::encode(rf.selector)),
                )?;
                rdict.set_item(
                    "revert_data",
                    format!("0x{}", alloy::primitives::hex::encode(&rf.revert_data)),
                )?;
                rdict.set_item("label", &rf.label)?;
                rdict.set_item("outcome_kind", rf.outcome_kind)?;
                rdict.set_item("gas_used", rf.gas_used)?;
                fdict.set_item("reverting_frame", rdict)?;
            }
            None => fdict.set_item("reverting_frame", py.None())?,
        }
        let swaps_list = PyList::empty(py);
        for s in &f.captured_swaps {
            swaps_list.append(captured_swap_to_dict(py, s)?)?;
        }
        fdict.set_item("captured_swaps", swaps_list)?;
        fdict.set_item("log_full_count", f.log_full_count)?;
        let rsw = PyList::empty(py);
        for s in &f.reverted_swaps {
            rsw.append(captured_swap_to_dict(py, s)?)?;
        }
        fdict.set_item("reverted_swaps", rsw)?;
        // Compact per-frame EVM call-trace summary (no-profit diagnostic).
        let ct_list = PyList::empty(py);
        for s in &f.call_trace {
            ct_list.append(s)?;
        }
        fdict.set_item("call_trace", ct_list)?;
        fdict.set_item("weth_before", f.weth_before)?;
        fdict.set_item("weth_after", f.weth_after)?;
        fdict.set_item("eth_before", f.eth_before)?;
        fdict.set_item("eth_after", f.eth_after)?;
        fdict.set_item("erc6909_before", f.erc6909_before)?;
        fdict.set_item("erc6909_after", f.erc6909_after)?;
        // optimal_input + hop_outputs as Python ints (u128 fits in PyLong).
        fdict.set_item("optimal_input", f.optimal_input)?;
        let hop_outputs_list = PyList::empty(py);
        for ho in &f.hop_outputs {
            hop_outputs_list.append(*ho)?;
        }
        fdict.set_item("hop_outputs", hop_outputs_list)?;
        failures_list.append(fdict)?;
    }
    out.set_item("failures", failures_list)?;

    let buckets_dict = PyDict::new(py);
    for (bucket, count) in buckets.buckets() {
        buckets_dict.set_item(bucket, count)?;
    }
    out.set_item("fail_buckets", buckets_dict)?;

    Ok(out)
}

/// Drive the SELFDESTRUCT-gift success-path fixture through the in-process
/// revm EVM — the ADR-005 Tier-2 dual-driver parity entrypoint for the
/// success-path `SimResult` (gross/net/gas).
///
/// The fixture (identical to the Rust `parity_evm_sim.rs` test): the executor
/// stub bytecode CALLs a gift contract; the gift `SELFDESTRUCT`s to the
/// executor (`CALLER`), sending its 1 ETH balance → `gross_profit = 1 ETH` →
/// a non-None `SimResult`. Multicall3 bytecode is deployed so `getEthBalance`
/// returns real ETH balances (the pre/post balance reads).
///
/// The caller supplies only `path_id` (round-trips through the FFI); every
/// other fixture detail is hardcoded to mirror the Rust parity test.
///
/// Returns a dict:
///   - `result` (`dict`) — the `SimResult` as a Python dict (`gross_profit`,
///     `net_profit`, `gas_used`, `priority_fee`, `base_fee_next`, `captured_swaps`,
///     `hop_count`) when the path is profitable. KeyError-free.
///   - `failures` (`list[dict]`) — empty for the success path (same shape as
///     `simulate_in_process_revert_probe` for symmetry).
///   - `fail_buckets` (`dict`) — empty for the success path.
///
/// No RPC. `CacheDB<EmptyDB>` handles all reads. The provider is a mock with
/// an empty queue (errors if somehow called).
///
/// # Errors
///
/// `RuntimeError` only on an unrecoverable revm `transact` error (cannot
/// happen over `CacheDB<EmptyDB>`) or if the fixture regresses to a non-None
/// bucket (a `debug_assert!` fires + is surfaced as `RuntimeError`).
#[pyfunction]
#[pyo3(signature = (path_id))]
pub fn simulate_in_process_success_probe(
    py: Python<'_>,
    path_id: u64,
) -> PyResult<Bound<'_, PyDict>> {
    use crate::conversion::alloy::u256_to_py;

    // 1. Build the ctx (mock provider — never called over CacheDB<EmptyDB>).
    let provider = mock_no_rpc_provider();
    let executor_bc = executor_stub_bytecode(SMOKE_GIFT);
    let ctx = SimulateContext {
        provider: &provider,
        executor_owner: SMOKE_OWNER,
        executor_address: SMOKE_EXECUTOR,
        weth_address: SMOKE_WETH,
        pool_manager_address: SMOKE_PM,
        multicall3_address: SMOKE_MULTICALL3,
        inject_code: true,
        injected_address: Some(SMOKE_EXECUTOR),
        runtime_bytecode: alloy::primitives::Bytes::copy_from_slice(&executor_bc),
        warmup: smoke_warmup(),
        base_fee_next: SMOKE_BASE_FEE_NEXT,
        current_block: SMOKE_CURRENT_BLOCK,
        block_timestamp: SMOKE_BLOCK_TIMESTAMP,
        block_priority_fees: None,
    };

    // 2. Build the CacheDB + apply overrides (owner, executor, WETH warmup).
    let mut cache_db: CacheDB<EmptyDB> = CacheDB::new(EmptyDB::default());
    degenbot_simulation::apply_simulation_overrides(&mut cache_db, &ctx.override_params())
        .map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "simulate_in_process_success_probe: override apply failed: {e}"
            ))
        })?;

    // 3. Deploy the supporting contracts (Multicall3 + gift).
    cache_db.insert_account_info(
        SMOKE_MULTICALL3,
        revm::state::AccountInfo {
            balance: alloy::primitives::U256::ZERO,
            nonce: 1,
            code: Some(revm::bytecode::Bytecode::new_raw(
                alloy::primitives::Bytes::from(MULTICALL3_BYTECODE.to_vec()),
            )),
            ..Default::default()
        },
    );
    cache_db.insert_account_info(
        SMOKE_GIFT,
        revm::state::AccountInfo {
            balance: ONE_ETH,
            nonce: 1,
            code: Some(revm::bytecode::Bytecode::new_raw(
                alloy::primitives::Bytes::from(GIFT_BYTECODE.to_vec()),
            )),
            ..Default::default()
        },
    );

    // 4. Run the 7-call orchestration (GIL-free).
    let mut buckets = FailBuckets::new();
    let path = smoke_v2_path(path_id);
    let result = simulate_in_process_with_db(&ctx, cache_db, &path, &mut buckets).map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!(
            "simulate_in_process_success_probe: sim failed: {e}"
        ))
    })?;

    // 5. Validate: the success-path fixture MUST return a SimResult.
    let sim = result.ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err(
            "simulate_in_process_success_probe: fixture regressed to None (check gift ETH / \
             multicall3 bytecode). A non-None SimResult is the contract.",
        )
    })?;

    // 6. Wrap the SimResult as a dict.
    let result_dict = PyDict::new(py);
    result_dict.set_item("path_id", sim.path_id)?;
    result_dict.set_item("gross_profit", u256_to_py(py, &sim.gross_profit)?)?;
    result_dict.set_item("net_profit", u256_to_py(py, &sim.net_profit)?)?;
    result_dict.set_item("gas_used", sim.gas_used)?;
    result_dict.set_item("priority_fee", sim.priority_fee)?;
    result_dict.set_item("base_fee_next", sim.base_fee_next)?;
    result_dict.set_item("hop_count", sim.hop_count)?;
    let swaps_list = PyList::empty(py);
    for s in &sim.captured_swaps {
        swaps_list.append(captured_swap_to_dict(py, s)?)?;
    }
    result_dict.set_item("captured_swaps", swaps_list)?;

    let out = PyDict::new(py);
    out.set_item("result", result_dict)?;
    // `failures` + `fail_buckets` are empty for the success path (symmetry
    // with `simulate_in_process_revert_probe`).
    out.set_item("failures", PyList::empty(py))?;
    out.set_item("fail_buckets", PyDict::new(py))?;
    debug_assert!(
        buckets.failures().is_empty(),
        "success-probe fixture expects no failures"
    );
    Ok(out)
}
