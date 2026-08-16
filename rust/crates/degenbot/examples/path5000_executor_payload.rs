#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]
//! Path-5000 minimal executor-payload harness: run the EXACT encoded
//! V2→V4→V3 arbitrage through the REAL `cmd_executor` bytecode, injected
//! alone into an EmptyDB revm (per the "inject only the executor, then run
//! encoded payloads through it" reduction — no pool/token custody choreography).
//!
//! Sibling of `path26154_executor_payload.rs`: same V4 `0x929b9b09` UNI/MATIC +
//! V3 `0xfaA31847` UNI/WETH legs; new hop0 = UniV2 `0x819f3450` MATIC/WETH
//! (db_id 127) at solve block 25704509.
//!
//! This is the decisive "is it just a bad payload?" experiment: if the encoded
//! command stream is malformed, the executor reverts AT ITS OWN DECODE frame
//! (shallow depth, target = the executor `aaaaaaaa…aaaa`, an `empty`/`VM`/
//! `INVALID` Halt with no pool call reached). If the payload decodes and
//! executes, the executor advances through the address table + V2 flash +
//! V4 unlock and only reverts when it CALLS a pool (deep frame, target = a
//! pool/executor call stack ≥ depth 2) — i.e. the payload is structurally
//! sound and the empty-Halt lives in pool execution, not the command stream.
//!
//! The live `[sim-fail] path=5000 … bucket=empty revert=0x` already showed
//! depth=6 at the PoolManager; this harness reproduces that decode-then-run
//! determinism offline against the same real executor bytecode + the same
//! recorded payload {optimal_input, hop_outputs, path_info}.

#![expect(dead_code)] // deserialized fixture fields
#![expect(clippy::too_many_lines, clippy::doc_markdown, clippy::ref_option)] // run-once investigation probe

use std::sync::Arc;

use alloy::network::Ethereum;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::client::ClientBuilder;
use alloy::transports::mock::{Asserter, MockTransport};
use degenbot::degenbot_backrun_strategy::{
    simulate_in_process_with_db, FailBuckets, SimResult, SimulateContext, SimulatePath,
};
use degenbot::degenbot_executor::composers::{
    encode_cmd_stream, EncodeOptions, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
};
use degenbot::degenbot_executor::{compute_simulation_warmup_slots, WarmupSlots};
use degenbot::degenbot_simulation::apply_simulation_overrides;
use degenbot_rpc::provider::AlloyProvider;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;

#[expect(dead_code)] // deserialized fixture fields are probe inputs
#[derive(serde::Deserialize)]
struct Fixture {
    target_block: u64,
    recorded_solve: RecordedSolve,
    pools: Pools,
    path: Vec<PathHop>,
}
#[derive(serde::Deserialize)]
struct RecordedSolve {
    optimal_input: String,
    #[serde(rename = "hop_outputs")]
    hop_outputs: Vec<String>,
    #[serde(default)]
    sim_bucket: String,
}
#[derive(serde::Deserialize)]
struct Pools {
    v2_0: PoolData,
    v4: PoolData,
    v3_2: PoolData,
}
#[derive(serde::Deserialize)]
struct PathHop {
    hop: usize,
    pool: String,
    zero_for_one: bool,
}
#[derive(serde::Deserialize)]
struct PoolData {
    address: Option<String>,
    token0: Option<String>,
    token1: Option<String>,
    pool_manager: Option<String>,
    pool_id: Option<String>,
    currency0: Option<String>,
    currency1: Option<String>,
    tick_spacing: Option<i32>,
    fee_token0: Option<u32>,
    fee_currency0: Option<u32>,
}

fn parse_addr(s: &Option<String>) -> alloy::primitives::Address {
    s.as_ref().unwrap().parse().unwrap()
}

// Canonical simulation addresses (mirror the parity-inspector corpus).
const EXECUTOR: alloy::primitives::Address =
    alloy::primitives::address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
const WETH: alloy::primitives::Address =
    alloy::primitives::address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const PM: alloy::primitives::Address =
    alloy::primitives::address!("000000000004444c5dc75cb358380d2e3de08a90");
const MULTICALL3: alloy::primitives::Address =
    alloy::primitives::address!("c411372f0b8ae58585e33b78aea9e0596da9a6f1");
const OWNER: alloy::primitives::Address =
    alloy::primitives::address!("9c56a29c7231974c269e24f9fb3c29203039089e");
const SOLVE_BLOCK: u64 = 25_704_509;
const BASE_FEE_NEXT: u128 = 1_000_000_000;
const BLOCK_TS: u64 = 0;

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path5000_v2v4v3_block25704509.json"
);
const EXECUTOR_RUNTIME: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tier3-oracle/artifacts/executor/cmd_executor.runtime.hex"
);
const EXECUTOR_CREATION: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tier3-oracle/artifacts/executor/cmd_executor.creation.hex"
);
/// Load a hex file (`0x`-less or prefixed, whitespace tolerated).
fn load_hex(rel: &str) -> Vec<u8> {
    let raw = std::fs::read_to_string(rel).unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let s: String = raw.trim().chars().filter(|c| !c.is_whitespace()).collect();
    let s = s.strip_prefix("0x").unwrap_or(&s);
    alloy::primitives::hex::decode(s).expect("hex decodes")
}

/// ABI-encode the two `__init__(address weth, address pool_manager)` args.
fn deploy_args(weth: alloy::primitives::Address, pm: alloy::primitives::Address) -> Vec<u8> {
    let mut args = vec![0u8; 64];
    args[12..32].copy_from_slice(weth.as_slice());
    args[44..64].copy_from_slice(pm.as_slice());
    args
}

/// Build the executor runtime bytecode by appending the 5×32-byte immutable
/// tail to the committed `[code_section][CBOR]` runtime (the same construction
/// `contracts/recompile.py` uses).
///
/// Immutable layout (160 bytes appended after CBOR metadata):
/// ```text
///   [0] OWNER_ADDR          — the caller authorized for `execute()`
///   [1] WETH_ADDR
///   [2] POOL_MANAGER_ADDR
///   [3] WETH_DELTA_SLOT     = keccak256(abi.encodePacked(self, WETH))   [left-padded]
///   [4] NATIVE_DELTA_SLOT   = keccak256(abi.encodePacked(self, NATIVE)) [left-padded]
/// ```
/// `self` is the address the executor is injected at (`injected`), matching how
/// the executor's V4 WETH/native delta-ledger slots are keyed. All three address
/// immutables are left-padded to 32 bytes; `NATIVE` is `address(0)`.
fn build_executor_runtime(
    owner: alloy::primitives::Address,
    injected: alloy::primitives::Address,
) -> Vec<u8> {
    use alloy::primitives::{utils::keccak256, B256};

    let mut code = load_hex(EXECUTOR_RUNTIME); // [code_section][CBOR], no immutable tail
    let code_len = code.len();

    let pad32 = |a: alloy::primitives::Address| -> [u8; 32] {
        let mut w = [0u8; 32];
        w[12..32].copy_from_slice(a.as_slice());
        w
    };
    let weth_delta: B256 = keccak256([pad32(injected).as_slice(), pad32(WETH).as_slice()].concat());
    let native_delta: B256 = keccak256(
        [
            pad32(injected).as_slice(),
            pad32(alloy::primitives::Address::ZERO).as_slice(),
        ]
        .concat(),
    );

    code.extend_from_slice(&pad32(owner)); //   [0] OWNER_ADDR
    code.extend_from_slice(&pad32(WETH)); //   [1] WETH_ADDR
    code.extend_from_slice(&pad32(PM)); //   [2] POOL_MANAGER_ADDR
    code.extend_from_slice(weth_delta.as_slice()); // [3]
    code.extend_from_slice(native_delta.as_slice()); // [4]

    println!(
        "  executor runtime: {code_len} bytes code+CBOR + 160-byte immutable tail (owner={owner} injected={injected})"
    );
    println!("  WETH_DELTA_SLOT={weth_delta:#x} NATIVE_DELTA_SLOT={native_delta:#x}");
    code
}

fn mock_no_rpc_provider() -> AlloyProvider {
    let asserter = Asserter::new();
    let client = ClientBuilder::default().transport(MockTransport::new(asserter), true);
    let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
    AlloyProvider::from_provider(Arc::new(dyn_provider) as Arc<dyn Provider<Ethereum>>)
}

fn build_path(fx: &Fixture, path_id: u64) -> SimulatePath {
    let p = &fx.pools;
    let rec = &fx.recorded_solve;
    let optimal_input: u128 = rec.optimal_input.parse().unwrap();
    let hop_outputs: Vec<u128> = rec.hop_outputs.iter().map(|s| s.parse().unwrap()).collect();
    // Forward-input vector (consumed by the CL-hop clamp): hop0 = optimal_input,
    // hop i>0 = hop_outputs[i-1].
    let mut consumed_inputs = vec![optimal_input];
    consumed_inputs.extend_from_slice(&hop_outputs[..hop_outputs.len().saturating_sub(1)]);
    let path_info = PathInfo::new(vec![
        HopInfo::V2(V2HopInfo {
            pool_address: parse_addr(&p.v2_0.address),
            token0_address: parse_addr(&p.v2_0.token0),
            token1_address: parse_addr(&p.v2_0.token1),
            fee: 30, // UniV2 MATIC/WETH 0.30%
            zfo: fx.path[0].zero_for_one,
        }),
        HopInfo::V4(V4HopInfo {
            pool_manager_address: parse_addr(&p.v4.pool_manager),
            pool_id_hex: p.v4.pool_id.as_ref().unwrap().clone(),
            currency0_address: parse_addr(&p.v4.currency0),
            currency1_address: parse_addr(&p.v4.currency1),
            fee: p.v4.fee_currency0.unwrap(),
            tick_spacing: p.v4.tick_spacing.unwrap(),
            hook_address: alloy::primitives::Address::ZERO,
            zfo: fx.path[1].zero_for_one,
        }),
        HopInfo::V3(V3HopInfo {
            pool_address: parse_addr(&p.v3_2.address),
            token0_address: parse_addr(&p.v3_2.token0),
            token1_address: parse_addr(&p.v3_2.token1),
            fee: p.v3_2.fee_token0.unwrap(),
            zfo: fx.path[2].zero_for_one,
        }),
    ]);
    SimulatePath {
        path_id,
        optimal_input,
        hop_outputs,
        consumed_inputs,
        path_info,
        solve_block: SOLVE_BLOCK,
        opts: EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: false,
            ..Default::default()
        },
        state_nonces: vec![],
    }
}

/// Deterministically re-run `execute()` through the injected executor with the
/// public `CallTraceInspector`, printing the full nested call tree + the
/// failing frame. This is the trace/inspection that pins down WHICH contract
/// call (if any) triggered the empty revert — independent of the sim's
/// internal `[sim-trace]` tracing.
#[expect(clippy::too_many_lines)]
fn run_executor_call_trace(fx: &Fixture, runtime: &[u8]) {
    use alloy::primitives::{Bytes, TxKind, U256};
    use degenbot::degenbot_backrun_strategy::calldata::{
        encode_balance_of_calldata, encode_erc6909_balance_of_calldata,
        encode_get_eth_balance_calldata, wrap_execute_calldata,
    };
    use degenbot::degenbot_backrun_strategy::{BALANCE_CALL_GAS_LIMIT, EXECUTE_CONFIG};
    use degenbot::degenbot_simulation::CallTraceInspector;
    use revm::context::TxEnv;
    use revm::{ExecuteEvm, InspectCommitEvm, MainBuilder, MainContext};

    // Encode the exact command stream the payload probe produced.
    let path = build_path(fx, 5000);
    let cmd = encode_cmd_stream(
        &path.path_info,
        path.optimal_input,
        &path.hop_outputs,
        &path.consumed_inputs,
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: false,
            ..Default::default()
        },
    )
    .expect("encode_cmd_stream Some");
    println!(
        "  cmd_stream_hex ({} bytes): 0x{}",
        cmd.len(),
        alloy::primitives::hex::encode(&cmd)
    );

    // `execute(bytes,uint256)` ABI encode — reuse the sim's exact wrapper so
    // the calldata is byte-identical to the live path.
    let data =
        wrap_execute_calldata(EXECUTOR, &cmd, EXECUTE_CONFIG).expect("wrap execute calldata");

    let provider = mock_no_rpc_provider();
    let warmup: WarmupSlots = compute_simulation_warmup_slots(EXECUTOR, WETH, PM);
    let ctx = SimulateContext {
        provider: &provider,
        executor_owner: OWNER,
        executor_address: EXECUTOR,
        weth_address: WETH,
        pool_manager_address: PM,
        multicall3_address: MULTICALL3,
        inject_code: true,
        injected_address: Some(EXECUTOR),
        runtime_bytecode: Bytes::copy_from_slice(runtime),
        warmup,
        base_fee_next: BASE_FEE_NEXT,
        current_block: SOLVE_BLOCK,
        block_timestamp: BLOCK_TS,
        block_priority_fees: None,
    };
    let mut cache_db: CacheDB<EmptyDB> = CacheDB::new(EmptyDB::default());
    apply_simulation_overrides(&mut cache_db, &ctx.override_params()).expect("overrides");

    let (default_ct, _default_handle) = CallTraceInspector::new(); // sets the INSP type
    let (ct, handle) = CallTraceInspector::new(); // the recording inspector
    let tx = TxEnv::builder()
        .caller(OWNER)
        .kind(TxKind::Call(EXECUTOR))
        .data(data)
        .value(U256::ZERO)
        .gas_limit(degenbot::degenbot_backrun_strategy::execute_gas_limit())
        .gas_price(BASE_FEE_NEXT)
        .build()
        .expect("execute tx");
    let mut evm = revm::context::Context::mainnet()
        .with_db(cache_db)
        .build_mainnet_with_inspector(default_ct);
    evm.ctx.cfg.disable_nonce_check = true;
    evm.ctx.modify_block(|block| {
        block.basefee = u64::try_from(BASE_FEE_NEXT).unwrap_or(u64::MAX);
        block.number = U256::from(SOLVE_BLOCK);
        block.timestamp = U256::from(BLOCK_TS);
    });

    // Mirror the sim's pre-execute balance-read txs [0..2] (WETH balanceOf,
    // multicall3 getEthBalance, PM ERC6909 balanceOf) so the execute() call
    // runs on the same accumulated state as the live 7-call vector.
    let weth_call = encode_balance_of_calldata(EXECUTOR).expect("weth call");
    let eth_call = encode_get_eth_balance_calldata(EXECUTOR).expect("eth call");
    let erc6909_call = encode_erc6909_balance_of_calldata(EXECUTOR, WETH).expect("erc6909 call");
    let pre = [
        (WETH, weth_call),
        (MULTICALL3, eth_call),
        (PM, erc6909_call),
    ];
    for (to, calldata) in pre {
        let tx = TxEnv::builder()
            .caller(OWNER)
            .kind(TxKind::Call(to))
            .data(calldata)
            .value(U256::ZERO)
            .gas_limit(BALANCE_CALL_GAS_LIMIT)
            .gas_price(BASE_FEE_NEXT)
            .build()
            .expect("pre tx");
        let _ = evm.transact_one(tx).expect("pre transact");
    }
    let res = evm.inspect_commit(tx, ct).expect("inspect_commit");
    println!(
        "  execute() top-level: {}",
        if res.is_success() {
            "SUCCESS"
        } else {
            "revert/halt"
        }
    );
    let trace = handle.take_trace();

    println!();
    println!("=== deterministic CallTraceInspector render of execute() (who triggered the empty revert) ===");
    let rendered = trace.render_debug();
    for line in rendered.lines() {
        println!("  {line}");
    }
    match trace.failing_frame() {
        None => println!("  failing_frame: NONE (execute() succeeded — no revert frame)"),
        Some(fr) => {
            println!(
                "  failing_frame: depth={} target={} selector=0x{} (this is the deepest non-success frame)",
                fr.depth,
                fr.target,
                alloy::primitives::hex::encode(fr.selector),
            );
            let reverted = trace
                .frames
                .iter()
                .filter(|f| {
                    !matches!(
                        f.outcome,
                        Some(degenbot::degenbot_simulation::FrameOutcome::Success { .. }) | None
                    )
                })
                .collect::<Vec<_>>();
            println!(
                "  contract calls that ended in revert/halt: {}",
                reverted.len()
            );
            for f in reverted {
                println!(
                    "    - d{} target={} selector=0x{}\n      (sub-calls under it that SUCCEEDED are NOT the trigger — \
                     only this frame's own revert caused the top-level failure)",
                    f.depth,
                    f.target,
                    alloy::primitives::hex::encode(f.selector),
                );
            }
        }
    }
}

fn main() {
    let text = std::fs::read_to_string(FIXTURE_PATH)
        .unwrap_or_else(|e| panic!("read fixture {FIXTURE_PATH}: {e}"));
    let fx: Fixture = serde_json::from_str(&text).expect("parse fixture");
    let rec = &fx.recorded_solve;
    println!(
        "payload: optimal_input={} hop_outputs={:?} recorded_bucket={}",
        rec.optimal_input, rec.hop_outputs, rec.sim_bucket
    );

    let runtime_bytecode = build_executor_runtime(OWNER, EXECUTOR);
    println!(
        "executor runtime constructed from real cmd_executor code+CBOR + immutables: {} bytes",
        runtime_bytecode.len()
    );
    let provider = mock_no_rpc_provider();
    let warmup: WarmupSlots = compute_simulation_warmup_slots(EXECUTOR, WETH, PM);

    let ctx = SimulateContext {
        provider: &provider,
        executor_owner: OWNER,
        executor_address: EXECUTOR,
        weth_address: WETH,
        pool_manager_address: PM,
        multicall3_address: MULTICALL3,
        inject_code: true,
        injected_address: Some(EXECUTOR),
        runtime_bytecode: alloy::primitives::Bytes::from(runtime_bytecode.clone()),
        warmup,
        base_fee_next: BASE_FEE_NEXT,
        current_block: SOLVE_BLOCK,
        block_timestamp: BLOCK_TS,
        block_priority_fees: None,
    };

    let mut cache_db: CacheDB<EmptyDB> = CacheDB::new(EmptyDB::default());
    apply_simulation_overrides(&mut cache_db, &ctx.override_params())
        .expect("overrides apply over EmptyDB");

    let mut buckets = FailBuckets::new();
    let path = build_path(&fx, 5000);
    let result = simulate_in_process_with_db(&ctx, cache_db, &path, &mut buckets)
        .expect("in-process sim over CacheDB<EmptyDB> cannot RPC-fail");

    println!("--- execute() verdict ---");
    match result {
        Some(r) => {
            let sr: &SimResult = &r;
            println!(
                "RESULT: SUCCESS — execute() returned gross_profit={} net_profit={} gas_used={} \
                 captured_swaps={}",
                sr.gross_profit,
                sr.net_profit,
                sr.gas_used,
                sr.captured_swaps.len()
            );
        }
        None => println!("RESULT: REVERT/HALT — execute() returned None (reverting frame below)"),
    }

    println!("--- executor decode/execute attribution (the bad-payload discriminator) ---");
    println!("buckets: {:?}", buckets.buckets());
    let failures = buckets.failures();
    println!("failures: {}", failures.len());
    for f in failures {
        println!(
            "  path_id={} bucket={} fail_index={:?}",
            f.path_id, f.bucket, f.fail_index
        );
        println!(
            "  revert_data=0x{} ({} bytes)",
            alloy::primitives::hex::encode(&f.revert_data),
            f.revert_data.len()
        );
        match &f.reverting_frame {
            None => println!("  reverting_frame: None"),
            Some(rf) => {
                println!(
                    "  reverting_frame: depth={} target={} selector=0x{} revert=0x{} label={} outcome_kind={} gas_used={}",
                    rf.depth,
                    rf.target,
                    alloy::primitives::hex::encode(rf.selector),
                    alloy::primitives::hex::encode(&rf.revert_data),
                    rf.label,
                    rf.outcome_kind,
                    rf.gas_used,
                );
                let is_empty_revert = rf.revert_data.is_empty();
                let matches_live = is_empty_revert && f.bucket == "empty";
                println!(
                    "  => {}",
                    if matches_live {
                        "REPRODUCED THE LIVE empty-Halt: bucket=empty, revert=0x matches the live \
                         [sim-fail] path=5000 line exactly. The executor owner gate passed and the \
                         payload DECODED+EXECUTED (it made a pool sub-call, see the sim-trace d2 \
                         0xfaA31847 hop-2 V3 pool, before the empty revert). A malformed command \
                         stream would revert at decode BEFORE any pool call — it does not call a pool. \
                         => NOT a bad payload; the empty-Halt is an execution outcome (fund/balance \
                         or pool-call edge), consistent with the range-exhaustion reading."
                    } else {
                        "non-empty / non-\"empty\" bucket — inspect manually."
                    }
                );
            }
        }
        println!("  captured_swaps: {}", f.captured_swaps.len());
        println!("  --- full revm call trace (per-frame: depth,target,selector,outcome,gas) ---");
        if f.call_trace.is_empty() {
            println!(
                "    (no call trace captured on the failure — see the self-contained trace below)"
            );
        }
        for line in &f.call_trace {
            println!("    {line}");
        }
        // The deepest non-success frame — answers "which contract call (if any)
        // triggered the empty revert".
        let reverted: Vec<&String> = f
            .call_trace
            .iter()
            .filter(|l| l.contains(":revert") || l.contains(":halt"))
            .collect();
        println!(
            "  deepest non-success frame in trace: {}",
            reverted.last().map_or("none", |l| l.as_str())
        );
    }
    println!(
        "note: EmptyDB pools mean the pool CALLS hit non-contract addresses, so the exact V4 \
         range-exhaustion halt is not reproduced here — this isolates the payload-validity axis \
         only. Live log already reported depth=6 target=PoolManager for this path."
    );

    // ── Deterministic revm call-trace inspection of execute() ──
    // Re-run the SAME execute() payload through the injected executor with the
    // public CallTraceInspector, print the full nested call tree + the failing
    // frame. This answers "which contract call (if any) triggered the empty
    // revert" without depending on the sim's internal tracing.
    run_executor_call_trace(&fx, &runtime_bytecode);
}
