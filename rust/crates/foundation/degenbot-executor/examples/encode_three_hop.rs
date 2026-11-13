#![expect(
    clippy::print_stdout,
    reason = "stdout is the encoder's machine-readable interface"
)]

//! Encode one three-hop `PathInfo` through the production command grammar.
//!
//! The Ape execution matrix uses this pure-Rust consumer to ensure that its
//! on-chain checks exercise production bytes rather than a second test encoder.

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
