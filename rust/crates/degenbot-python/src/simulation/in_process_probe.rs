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
                fdict.set_item("reverting_frame", rdict)?;
            }
            None => fdict.set_item("reverting_frame", py.None())?,
        }
        let swaps_list = PyList::empty(py);
        for s in &f.captured_swaps {
            swaps_list.append(captured_swap_to_dict(py, s)?)?;
        }
        fdict.set_item("captured_swaps", swaps_list)?;
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
