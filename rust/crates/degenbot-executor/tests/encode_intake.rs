//! ADR-033 intake contract — `EncodeRequest` / `EncodeContext` unit + routing
//! tests. The hop-alignment checks are the constructor's loud failure mode;
//! the routing test pins that the re-shaped funnel still drives the same
//! production encode (byte-identity itself is pinned by `glopcn_bytepin` +
//! the revm matrix).

#![expect(
    clippy::panic,
    reason = "test assertions on must-encode paths panic on failure (repo convention: honesty_invariant.rs)"
)]

use alloy::primitives::{address, Address};
use degenbot_executor::composers::{
    encode_cmd_stream, EncodeContext, EncodeOptions, EncodeRequest, HopInfo, PathInfo, V2HopInfo,
    V3HopInfo, V4HopInfo,
};

const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
const TOK2: Address = Address::repeat_byte(0xAB);
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

// ═══════════════════════════════════════════════════════════════════════════
// TGUZCT — the `use_v4_batch` × `erc6909_profit` interplay (was SMOZG3 open
// question 3). On the deployed artifact the combination COMPOSES via the
// `V4_BATCH_OPEN_WETH` (0x43) command: the batch skips its WETH tail-settle,
// so the follow-up `V4_MINT_COMPACT` finds the live delta. (The
// pre-deployment artifact's 0x42 tail-settle starved the mint — D0; the
// intake declined the combination until this flip.) The pairing gate in the
// validator keeps the composed stream checkable.
// ═══════════════════════════════════════════════════════════════════════════

fn v4_hop(c0: Address, c1: Address, zfo: bool) -> HopInfo {
    HopInfo::V4(V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x0".into(),
        currency0_address: c0,
        currency1_address: c1,
        fee: 3000,
        tick_spacing: 60,
        hook_address: Address::ZERO,
        zfo,
    })
}

/// A WETH-terminal 2-hop V4 path (WETH → USDC → WETH), consistent amounts.
fn v4v4_weth_terminal() -> PathInfo {
    PathInfo::new(vec![v4_hop(WETH, USDC, true), v4_hop(USDC, WETH, true)])
}

#[test]
fn batch_and_erc6909_capture_weth_terminal_composes_via_open_batch() {
    // TGUZCT/SW42JA: the deployed artifact's 0x43 open-weth batch leaves the
    // WETH delta open for the trailing mint — the intake encodes the open
    // batch (not the legacy 0x42).
    let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
    let req = EncodeRequest::new(
        v4v4_weth_terminal(),
        1_000_000_000_000_000_000,
        vec![1_000_000_000_000_000_000, 1_200_000_000_000_000_000],
        vec![999_999_999_999_999_999, 999_999_999_999_999_999],
        EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: true,
            ..Default::default()
        },
    );
    let bytes = encode_cmd_stream(&ctx, &req)
        .unwrap_or_else(|| panic!("batch + erc6909 capture must compose (TGUZCT)"));
    assert!(
        bytes.windows(2).any(|w| w[0] == 0x43 && w[1] == 2),
        "stream must carry the open-weth batch command (0x43, 2 entries)"
    );
}

#[test]
fn erc6909_capture_without_batch_still_encodes() {
    // Control: the decline is the COMBINATION, not the capture axis alone —
    // individual swaps + `V4_MINT_COMPACT` is the proven gas-saving capture.
    let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
    let req = EncodeRequest::new(
        v4v4_weth_terminal(),
        1_000_000_000_000_000_000,
        vec![1_000_000_000_000_000_000, 1_200_000_000_000_000_000],
        vec![999_999_999_999_999_999, 999_999_999_999_999_999],
        EncodeOptions {
            erc6909_profit: true,
            ..Default::default()
        },
    );
    assert!(encode_cmd_stream(&ctx, &req).is_some());
}

#[test]
fn v4v4v4_batch_and_erc6909_capture_weth_terminal_composes_via_open_batch() {
    // TGUZCT/SW42JA: the 3-hop pure-V4 family composes too (0x43 open batch).
    // Three tokens so the path is WETH-terminal: WETH -> TOK2 -> USDC -> WETH.
    let path = PathInfo::new(vec![
        v4_hop(WETH, TOK2, true),
        v4_hop(TOK2, USDC, true),
        v4_hop(USDC, WETH, true),
    ]);
    let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
    let req = EncodeRequest::new(
        path,
        1_000_000_000_000_000_000,
        vec![
            1_000_000_000_000_000_000,
            1_100_000_000_000_000_000,
            1_300_000_000_000_000_000,
        ],
        vec![999_999_999_999_999_999; 3],
        EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: true,
            ..Default::default()
        },
    );
    let bytes = encode_cmd_stream(&ctx, &req)
        .unwrap_or_else(|| panic!("3-hop batch + erc6909 capture must compose (TGUZCT)"));
    assert!(
        bytes.windows(2).any(|w| w[0] == 0x43 && w[1] == 3),
        "stream must carry the open-weth batch command (0x43, 3 entries)"
    );
}

#[test]
fn v4v4v4_erc6909_capture_without_batch_still_encodes() {
    // WETH-terminal via three tokens (an odd hop count cannot return to WETH
    // across two currencies).
    let path = PathInfo::new(vec![
        v4_hop(WETH, TOK2, true),
        v4_hop(TOK2, USDC, true),
        v4_hop(USDC, WETH, true),
    ]);
    let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
    let req = EncodeRequest::new(
        path,
        1_000_000_000_000_000_000,
        vec![
            1_000_000_000_000_000_000,
            1_100_000_000_000_000_000,
            1_300_000_000_000_000,
        ],
        vec![999_999_999_999_999_999; 3],
        EncodeOptions {
            erc6909_profit: true,
            ..Default::default()
        },
    );
    assert!(encode_cmd_stream(&ctx, &req).is_some());
}

// ── TGUZCT/TAZXHN: byte-stability guard for the NON-BATCH capture stream ──
// The flip (bdde6759b) must not move the proven non-batch erc6909 capture
// stream — pin its full-byte hash (the "non-batch capture unchanged
// byte-for-byte" acceptance bullet, mechanically enforced).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a 64 offset basis
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(1_099_511_628_211);
    }
    h
}

#[test]
fn nonbatch_capture_stream_bytes_are_stable() {
    // TGUZCT/TAZXHN acceptance: the 0x43 flip may not perturb the proven
    // non-batch erc6909 capture stream — full-byte fnv1a pins, captured
    // pre-flip (they must equal the post-flip bytes exactly).
    let two = {
        let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
        let req = EncodeRequest::new(
            v4v4_weth_terminal(),
            1_000_000_000_000_000_000,
            vec![1_000_000_000_000_000_000, 1_200_000_000_000_000_000],
            vec![999_999_999_999_999_999, 999_999_999_999_999_999],
            EncodeOptions {
                erc6909_profit: true,
                ..Default::default()
            },
        );
        encode_cmd_stream(&ctx, &req).unwrap_or_else(|| panic!("non-batch erc6909 must encode"))
    };
    let three_path = PathInfo::new(vec![
        v4_hop(WETH, TOK2, true),
        v4_hop(TOK2, USDC, true),
        v4_hop(USDC, WETH, true),
    ]);
    let three = {
        let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
        let req = EncodeRequest::new(
            three_path.clone(),
            1_000_000_000_000_000_000,
            vec![
                1_000_000_000_000_000_000,
                1_100_000_000_000_000_000,
                1_300_000_000_000_000_000,
            ],
            vec![999_999_999_999_999_999; 3],
            EncodeOptions {
                erc6909_profit: true,
                ..Default::default()
            },
        );
        encode_cmd_stream(&ctx, &req)
            .unwrap_or_else(|| panic!("non-batch erc6909 3-hop must encode"))
    };
    assert_eq!(
        fnv1a(&two),
        0x5168_eabb_6b08_d9ee,
        "2-hop non-batch erc6909 capture stream moved (flip regression)"
    );
    assert_eq!(
        fnv1a(&three),
        0x0638_197a_66c1_7168,
        "3-hop non-batch erc6909 capture stream moved (flip regression)"
    );
}
