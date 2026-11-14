#![expect(
    clippy::print_stdout,
    reason = "stdout is the encoder's machine-readable interface"
)]

//! Encode one three-hop `PathInfo` through the production command grammar.
//!
//! The Ape execution matrix (`tests/executor/`) uses this pure-Rust consumer to
//! ensure its on-chain checks exercise production bytes rather than a second
//! test encoder: it reads ONE JSON request document from argv, builds the
//! V2/V3/V4 hop payloads, and prints the command stream
//! `degenbot_executor::composers::encode_cmd_3_hop` produces, hex-encoded.
//!
//! # Home
//!
//! This program lives in `degenbot-fixture-tools` (`src/bin/<name>/main.rs`), not in
//! `degenbot-executor`'s `examples/`: it is test-oracle tooling, not a consumer starting
//! point (the epic's taxonomy). Nothing about the contract changed with that move — the
//! single JSON argv document, the V2/V3/V4 request decoding (including the V2 `u16` fee
//! conversion and the V4 `tick_spacing` requirement), the `encode_cmd_3_hop` call with its
//! default `EncodeOptions`, the one hex line on stdout, the binary name, and the failure
//! behaviour (a non-zero exit with the reason on stderr) all travelled with the source;
//! only the invocation did. It needs NO feature lane: it reads no statement ledger and
//! opens no database of any kind, so it builds under this package's default features and
//! `just check-dev-tools default` is the lane that builds it.
//!
//! `just build-executor-three-hop-encoder` wraps the build and ALSO stages the binary at
//! `<target-dir>/debug/examples/encode_three_hop`, which is the path
//! `tests/executor/test_cmd_executor_three_hop_production_calldata.py` reads; the staged
//! name and the two-package invocation are the only things the relocation moved.
//!
//! Usage (one JSON argument, no flags):
//!
//!     cargo run --locked --manifest-path rust/Cargo.toml \
//!         -p degenbot-fixture-tools --bin encode_three_hop -- '<request-json>'

use alloy::primitives::Address;
use degenbot_execution::{HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo};
use degenbot_executor::composers::{encode_cmd_3_hop, EncodeOptions};
use serde::Deserialize;

#[derive(Deserialize)]
struct Request {
    executor: Address,
    pool_manager: Address,
    weth: Address,
    optimal_input: u128,
    outputs: Vec<u128>,
    consumed_inputs: Vec<u128>,
    hops: Vec<Hop>,
}

#[derive(Deserialize)]
struct Hop {
    family: String,
    address: Address,
    token0: Address,
    token1: Address,
    fee: u32,
    zfo: bool,
    tick_spacing: Option<i32>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argument = std::env::args()
        .nth(1)
        .ok_or("usage: encode_three_hop '<request-json>'")?;
    let request: Request = serde_json::from_str(&argument)?;
    let hops = request
        .hops
        .into_iter()
        .enumerate()
        .map(|(index, hop)| {
            Ok(match hop.family.as_str() {
                "V2" => HopInfo::V2(V2HopInfo {
                    pool_address: hop.address,
                    token0_address: hop.token0,
                    token1_address: hop.token1,
                    fee: hop.fee.try_into()?,
                    zfo: hop.zfo,
                }),
                "V3" => HopInfo::V3(V3HopInfo {
                    pool_address: hop.address,
                    token0_address: hop.token0,
                    token1_address: hop.token1,
                    fee: hop.fee,
                    zfo: hop.zfo,
                }),
                "V4" => HopInfo::V4(V4HopInfo {
                    pool_manager_address: request.pool_manager,
                    pool_id_hex: format!("0x{index:02x}"),
                    currency0_address: hop.token0,
                    currency1_address: hop.token1,
                    fee: hop.fee,
                    tick_spacing: hop.tick_spacing.ok_or("V4 requires tick_spacing")?,
                    hook_address: Address::ZERO,
                    zfo: hop.zfo,
                }),
                family => return Err(format!("unsupported family {family}").into()),
            })
        })
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    let commands = encode_cmd_3_hop(
        &PathInfo::new(hops),
        request.optimal_input,
        &request.outputs,
        &request.consumed_inputs,
        request.executor,
        request.pool_manager,
        request.weth,
        EncodeOptions::default(),
    )
    .ok_or("the production command grammar declined the path")?;
    println!("{}", alloy::primitives::hex::encode(commands));
    Ok(())
}
