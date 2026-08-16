#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]
//! Path-26154 payload probe: does the production composer encode a VALID
//! `cmd_executor` command stream for the exact recorded V2-V4-V3 arbitrage?
//!
//! Answers the "is it just a bad payload?" question at the ENCODING layer
//! (ergo UO3JM4-adjacent / the path-26154 empty-Halt investigation):
//!   * IF `encode_cmd_stream` returns `None` for this exact path shape +
//!     recorded amounts → the composer REJECTS the payload → a payload/encoding
//!     bug (the sim could not have produced a correct command stream).
//!   * IF it returns `Some(bytes)` → the encoding layer produced a payload;
//!     combined with the live log (the executor ran the preamble + address table
//!     cleanly and reverted at the V4 `PoolManager`, depth=6 — NOT at executor
//!     decode), this indicates the payload is well-formed and the empty-Halt is
//!     downstream in pool execution, not a malformed command stream.
//!
//! Also runs the SAME `encode_cmd_stream` the in-process simulator uses, from
//! the reconstructed fixture pools, and prints the payload size + the executor
//! `execute(bytes,uint256)` calldata wrapper for inspection.

#![expect(dead_code)] // deserialized fixture fields + probe locals
#![expect(clippy::too_many_lines, clippy::ref_option)] // run-once investigation probe

use std::collections::HashMap;

use alloy::primitives::{address, Address, U256};
use degenbot::degenbot_executor::composers::{
    encode_cmd_stream, EncodeOptions, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
};

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path26154_v2v4v3_block25700805.json"
);

const EXECUTOR: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
const POOL_MANAGER: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");

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
    v4_hop_index: usize,
    v4_zero_for_one: bool,
    #[serde(default)]
    v4_onchain: String,
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
    #[serde(default)]
    tick_data: HashMap<String, TickJson>,
}
#[derive(serde::Deserialize)]
struct TickJson {
    liquidity_net: String,
    liquidity_gross: String,
}

fn parse_addr(s: &Option<String>) -> Address {
    s.as_ref().unwrap().parse().unwrap()
}

fn main() {
    let text = std::fs::read_to_string(FIXTURE_PATH)
        .unwrap_or_else(|e| panic!("read fixture {FIXTURE_PATH}: {e}"));
    let fx: Fixture = serde_json::from_str(&text).expect("parse fixture");
    let rec = &fx.recorded_solve;

    let optimal_input: u128 = rec.optimal_input.parse().unwrap();
    let hop_outputs: Vec<u128> = rec.hop_outputs.iter().map(|s| s.parse().unwrap()).collect();
    let p = &fx.pools;

    // V2 hop0 (Sushi MATIC/WETH, zfo=false per fixture path).
    let v2 = HopInfo::V2(V2HopInfo {
        pool_address: parse_addr(&p.v2_0.address),
        token0_address: parse_addr(&p.v2_0.token0),
        token1_address: parse_addr(&p.v2_0.token1),
        fee: 30,
        zfo: fx.path[0].zero_for_one,
    });
    // V4 hop1 (UNI/MATIC pool 0x929b9b09, zfo=false).
    let v4 = HopInfo::V4(V4HopInfo {
        pool_manager_address: parse_addr(&p.v4.pool_manager),
        pool_id_hex: p.v4.pool_id.as_ref().unwrap().clone(),
        currency0_address: parse_addr(&p.v4.currency0),
        currency1_address: parse_addr(&p.v4.currency1),
        fee: p.v4.fee_currency0.unwrap(),
        tick_spacing: p.v4.tick_spacing.unwrap(),
        hook_address: Address::ZERO,
        zfo: fx.path[1].zero_for_one,
    });
    // V3 hop2 (Uniswap UNI/WETH pool 0xfaA31847, zfo=true).
    let v3 = HopInfo::V3(V3HopInfo {
        pool_address: parse_addr(&p.v3_2.address),
        token0_address: parse_addr(&p.v3_2.token0),
        token1_address: parse_addr(&p.v3_2.token1),
        fee: p.v3_2.fee_token0.unwrap(),
        zfo: fx.path[2].zero_for_one,
    });

    let path_info = PathInfo::new(vec![v2, v4, v3]);
    println!(
        "path: V2({}) -> V4({}) -> V3({})  zfo=[{}, {}, {}]",
        p.v2_0.address.as_ref().unwrap(),
        p.v4.pool_id.as_ref().unwrap(),
        p.v3_2.address.as_ref().unwrap(),
        fx.path[0].zero_for_one,
        fx.path[1].zero_for_one,
        fx.path[2].zero_for_one
    );
    println!(
        "recorded solve: optimal_input={optimal_input} hop_outputs={hop_outputs:?} bucket={}",
        rec.sim_bucket
    );

    match encode_cmd_stream(
        &path_info,
        optimal_input,
        &hop_outputs,
        &hop_outputs,
        EXECUTOR,
        POOL_MANAGER,
        WETH,
        EncodeOptions::default(),
    ) {
        None => {
            println!("ENCODE RESULT: None — the composer REJECTED the V2-V4-V3 payload.");
            println!(
                "=> PAYLOAD-LEVEL BUG: no command stream could be produced; the sim could not \
                 have run a correctly-encoded arbitrage for this path."
            );
        }
        Some(cmd) => {
            println!(
                "ENCODE RESULT: Some — command stream is {} bytes ({} hex chars).",
                cmd.len(),
                cmd.len() * 2
            );
            println!(
                "  first 16 bytes (preamble/address-table): {:02x?}",
                &cmd[..16.min(cmd.len())]
            );
            // The execute(bytes,uint256) wrapper the executor is called with:
            // selector 0xab5898e8 ++ offset(32) ++ uint256 config ++ len ++ bytes
            let config = U256::ZERO;
            let mut sig = Vec::new();
            sig.extend_from_slice(&[0xab, 0x58, 0x98, 0xe8]);
            sig.extend_from_slice(&U256::from(0x20).to_be_bytes::<32>());
            sig.extend_from_slice(&config.to_be_bytes::<32>());
            sig.extend_from_slice(&U256::from(cmd.len()).to_be_bytes::<32>());
            sig.extend_from_slice(&cmd);
            println!(
                "  execute() calldata (with config=0) is {} bytes; selector prefix: {:02x?}",
                sig.len(),
                &sig[..4]
            );

            // Sanity: the payload must NOT be empty of pool references — check
            // it references the four addresses in the address table.
            let hay = alloy::primitives::hex::encode(&cmd).to_lowercase();
            let mut found = 0u32;
            for a in [
                p.v2_0.address.as_ref().unwrap().as_str(),
                p.v3_2.address.as_ref().unwrap().as_str(),
                "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2", // WETH
            ] {
                if hay.contains(&a[2..].to_lowercase()) {
                    found += 1;
                }
            }
            println!("  payload hex: {}", &hay[..hay.len().min(160)]);
            println!(
                "  payload contains v2/v3 pool + WETH address bytes: {found}/3 (V4 addr comes via pool_id, not the table)"
            );
            println!(
                "=> ENCODE-LAYER OK: the composer produced a command stream. Combined with the \
                     live log (executor reverted at the V4 PoolManager, depth 6, not at executor \
                     decode), the empty-Halt is NOT a malformed payload — it is in pool execution."
            );
        }
    }
    println!(
        "note: full executor execution needs revm pool-state seeding (V2/V3/V4 storage-slot \
         encoders from the fixture) — deferring that slice."
    );
}
