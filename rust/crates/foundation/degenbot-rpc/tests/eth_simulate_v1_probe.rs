//! Live probe pinning the Sidecar exact-sim oracle: `AlloyProvider::eth_simulate_v1`
//! (task PCMMNZ successor - the provider owns the typed method; this pins the
//! gate-2 sweep shape: returnData == the coinbase bid).
//!
//! Ignored by default (network-dependent); run:
//!   `cargo test -p degenbot-rpc --test eth_simulate_v1_probe -- --ignored`

#![expect(clippy::unwrap_used)]

use std::sync::Arc;

use alloy::eips::BlockId;
use alloy::primitives::{address, bytes, U256};
use alloy::rpc::types::eth::simulate::{SimBlock, SimulatePayload};
use degenbot_rpc::provider::AlloyProvider;

/// Gate-2 sweep selector: command `0x15` (`WETH_WITHDRAW_ALL`)
/// Gate-2 sweep calldata (matches `cast calldata execute(bytes,uint256) 0x15 2560003`).
fn sweep_calldata() -> alloy::primitives::Bytes {
    // abi.encode(bytes,uint256) with bytes=[0x15] (WETH_WITHDRAW_ALL) and
    // uint256=2560003 (check_mode 3 = SWEEP, bribe_bips 10000,
    // bribe_recipient_idx 0 = block.coinbase) - matches cast calldata exactly.
    let mut cd = bytes!("ab5898e8").to_vec();
    cd.extend_from_slice(&alloy::primitives::U256::from(0x40).to_be_bytes::<32>());
    cd.extend_from_slice(&alloy::primitives::U256::from(2_560_003_u64).to_be_bytes::<32>());
    cd.extend_from_slice(&alloy::primitives::U256::from(1u64).to_be_bytes::<32>());
    cd.push(0x15);
    cd.extend_from_slice(&[0u8; 31]);
    alloy::primitives::Bytes::from(cd)
}

#[tokio::test]
#[ignore = "live network: requires the provided reth node serving eth_simulateV1"]
async fn live_sweep_sim_returns_coinbase_bid() {
    let rpc_url = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").unwrap();
    let client = alloy::rpc::client::ClientBuilder::default().http(rpc_url.parse().unwrap());
    let inner = alloy::providers::ProviderBuilder::default().connect_client(client);
    let provider = AlloyProvider::from_provider(Arc::new(inner));

    let exec = address!("30b28ed8aa581fbc0191c3b532b0697773070e97");
    let op = address!("5c603b8a137A40426E0dDFA981EC10c245AF080e");

    let payload = SimulatePayload {
        block_state_calls: vec![SimBlock {
            calls: vec![alloy::rpc::types::TransactionRequest {
                from: Some(op),
                to: Some(exec.into()),
                input: alloy::rpc::types::TransactionInput::new(sweep_calldata()),
                gas: Some(300_000),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };

    let blocks = provider
        .eth_simulate_v1(&payload, BlockId::latest())
        .await
        .unwrap();
    assert_eq!(blocks.len(), 1);
    let call = &blocks[0].calls[0];
    assert!(call.status, "sweep must succeed");
    assert_eq!(
        U256::from_be_slice(call.return_data.as_ref()),
        U256::from(600_000_000_000_000u64),
        "coinbase bid = swept executor balance"
    );
}
