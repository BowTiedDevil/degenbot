//! ADR-033 intake contract — `EncodeRequest` / `EncodeContext` unit + routing
//! tests. The hop-alignment checks are the constructor's loud failure mode;
//! the routing test pins that the re-shaped funnel still drives the same
//! production encode (byte-identity itself is pinned by `glopcn_bytepin` +
//! the revm matrix).

use alloy::primitives::{address, Address};
use degenbot_executor::composers::{
    encode_cmd_stream, EncodeContext, EncodeOptions, EncodeRequest, HopInfo, PathInfo, V2HopInfo,
    V3HopInfo,
};

const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
const PM: Address = address!("000000000004444c5dc75cB358380D2e3De08A90");
const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");

fn two_hop() -> PathInfo {
    PathInfo::new(vec![
        HopInfo::V2(V2HopInfo {
            pool_address: address!("000000000000000000000000000000000000DE66"),
            token0_address: WETH,
            token1_address: USDC,
            fee: 30,
            zfo: true,
        }),
        HopInfo::V3(V3HopInfo {
            pool_address: address!("000000000000000000000000000000000000DEA3"),
            token0_address: WETH,
            token1_address: USDC,
            fee: 3000,
            zfo: true,
        }),
    ])
}

#[test]
fn encode_request_new_accepts_hop_aligned_amounts() {
    let req = EncodeRequest::new(
        two_hop(),
        1_000,
        vec![900, 800],
        vec![1_000, 850],
        EncodeOptions::default(),
    );
    assert_eq!(req.path.hops.len(), 2);
    assert_eq!(req.hop_outputs, vec![900, 800]);
    assert_eq!(req.consumed_inputs, vec![1_000, 850]);
    assert_eq!(req.optimal_input, 1_000);
}

#[test]
#[should_panic(expected = "hop_outputs has 3 entries for a 2-hop path")]
fn encode_request_panics_when_hop_outputs_misaligned() {
    let _ = EncodeRequest::new(
        two_hop(),
        1_000,
        vec![900, 800, 700],
        vec![1_000, 850],
        EncodeOptions::default(),
    );
}

#[test]
#[should_panic(expected = "consumed_inputs has 1 entries for a 2-hop path")]
fn encode_request_panics_when_consumed_inputs_misaligned() {
    let _ = EncodeRequest::new(
        two_hop(),
        1_000,
        vec![900, 800],
        vec![1_000],
        EncodeOptions::default(),
    );
}

#[test]
fn encode_cmd_stream_routes_two_hop_v2_v3_via_the_intake_pair() {
    let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
    // A consistent amount chain (the same shape the grammar-parity fixture
    // uses for every family): each hop's executable input ≤ the prior hop's
    // output, so the ledger validator's credit-before-debit holds and the
    // family encodes rather than fatal-Rejecting (ADR-030).
    let req = EncodeRequest::new(
        two_hop(),
        1_000_000_000_000_000_000,
        vec![1_000_000_000_000_000_000, 1_000_000_000_000_000_000],
        vec![999_999_999_999_999_999, 999_999_999_999_999_999],
        EncodeOptions::default(),
    );
    // v2+v3 is a supported 2-hop family — the funnel must encode (Some).
    assert!(encode_cmd_stream(&ctx, &req).is_some());
}
