//! Target-classification spec — task 34LVLH.
//!
//! Seam: `degenbot_decoders::target_classifier::{classify, RouterRegistry, TargetClass, SwapLeg}`.
//! Vectors hand-encode canonical router calldata layouts; asserts cover the
//! decoded fields the on-demand resolver + post-target solver will consume.

#![allow(clippy::unwrap_used, clippy::panic)]

use alloy::hex::FromHex;
use alloy::primitives::{Address, U256};
use degenbot_decoders::target_classifier::{
    classify, OpaqueReason, PoolProtocol, RouterRegistry, TargetClass,
};

fn addr(s: &str) -> Address {
    Address::from_hex(s.trim_start_matches("0x")).unwrap()
}

fn word_u256(v: U256) -> Vec<u8> {
    v.to_be_bytes::<32>().to_vec()
}

fn u2(v: u64) -> Vec<u8> {
    word_u256(U256::from(v))
}

fn word_addr(a: Address) -> Vec<u8> {
    let mut w = vec![0u8; 32];
    w[12..].copy_from_slice(a.as_slice());
    w
}

const TOKEN_A: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"; // USDC
const TOKEN_B: &str = "0x6B175474E89094C44Da98b954EedeAC495271d0F"; // DAI

fn push_word(mut acc: Vec<u8>, w: &[u8]) -> Vec<u8> {
    acc.extend_from_slice(w);
    acc
}

/// V2 router swapExactTokensForTokens(amountIn, amountOutMin, address[], to, deadline)
fn v2_exact_tokens_calldata(amount_in: u64, min_out: u64, path: &[Address]) -> Vec<u8> {
    let mut args = Vec::new();
    args = push_word(args, &u2(amount_in));
    args = push_word(args, &u2(min_out));
    // dynamic array offset: 5 static words => 0xa0
    args = push_word(args, &u2(0xa0));
    args = push_word(
        args,
        &word_addr(addr("0x1111111111111111111111111111111111111111")),
    );
    args = push_word(args, &u2(9_999_999_999));
    args = push_word(args, &u2(path.len() as u64));
    for t in path {
        args = push_word(args, &word_addr(*t));
    }
    let mut cd = vec![0x38, 0xed, 0x17, 0x39];
    cd.extend_from_slice(&args);
    cd
}

/// V2 pair swap(amount0Out, amount1Out, to, data)
fn v2_pair_swap_calldata(amount0: u64, amount1: u64) -> Vec<u8> {
    let mut args = Vec::new();
    args = push_word(args, &u2(amount0));
    args = push_word(args, &u2(amount1));
    args = push_word(
        args,
        &word_addr(addr("0x2222222222222222222222222222222222222222")),
    );
    args = push_word(args, &u2(0x60));
    args = push_word(args, &u2(0)); // bytes data: empty
    let mut cd = vec![0x02, 0x2c, 0x0d, 0x9f];
    cd.extend_from_slice(&args);
    cd
}

/// V3 router exactInputSingle(ExactInputSingleParams)
fn v3_exact_input_single_calldata(
    token_in: Address,
    token_out: Address,
    amount_in: u64,
    min_out: u64,
) -> Vec<u8> {
    let mut args = Vec::new();
    args = push_word(args, &u2(0x20)); // tuple offset
                                       // tuple: (tokenIn, tokenOut, fee, recipient, amountIn, amountOutMinimum, sqrtPriceLimitX96)
    args = push_word(args, &word_addr(token_in));
    args = push_word(args, &word_addr(token_out));
    args = push_word(args, &u2(500)); // 0.05% pool
    args = push_word(
        args,
        &word_addr(addr("0x3333333333333333333333333333333333333333")),
    );
    args = push_word(args, &u2(amount_in));
    args = push_word(args, &u2(min_out));
    args = push_word(args, &u2(0)); // no price limit
    let mut cd = vec![0x41, 0x4b, 0xf3, 0x89];
    cd.extend_from_slice(&args);
    cd
}

/// V3 router exactInput(ExactInputParams): multihop path.
fn v3_exact_input_calldata(hop_path_bytes: &[u8], amount_in: u64, min_out: u64) -> Vec<u8> {
    let mut args = Vec::new();
    args = push_word(args, &u2(0x20)); // tuple offset
                                       // tuple: (bytes path, address recipient, uint256 amountIn, uint256 amountOutMinimum)
                                       // path offset within tuple: 4 static words => 0x80
    args = push_word(args, &u2(0x80));
    args = push_word(
        args,
        &word_addr(addr("0x3333333333333333333333333333333333333333")),
    );
    args = push_word(args, &u2(amount_in));
    args = push_word(args, &u2(min_out));
    args = push_word(args, &u2(hop_path_bytes.len() as u64));
    args.extend_from_slice(hop_path_bytes);
    while args.len() % 32 != 0 {
        args.push(0);
    }
    let mut cd = vec![0xc0, 0x4b, 0x8d, 0x59];
    cd.extend_from_slice(&args);
    cd
}

fn v3_path_2hop(tokens: &[Address]) -> Vec<u8> {
    // token(20) || fee(3) || token(20) || fee(3) || token(20)
    let mut p = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        p.extend_from_slice(t.as_slice());
        if i + 1 < tokens.len() {
            p.extend_from_slice(&[0x00, 0x07, 0xd0]); // 2000 fee placeholder slice
        }
    }
    p
}

#[test]
fn t01_v2_router_exact_tokens_for_tokens_decodes_path_and_bounds() {
    let a = addr(TOKEN_A);
    let b = addr(TOKEN_B);
    let cd = v2_exact_tokens_calldata(1_000, 999, &[a, b]);
    let reg = RouterRegistry::mainnet();
    let out = classify(
        addr("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D"),
        &cd,
        &reg,
    );
    let TargetClass::Swap(legs) = out else {
        panic!("expected Swap, got {out:?}");
    };
    assert_eq!(legs.len(), 1);
    let l = &legs[0];
    assert_eq!(l.protocol, PoolProtocol::V2);
    assert_eq!(l.pool, None, "router does not name the pool");
    assert_eq!(l.token_in, Some(a));
    assert_eq!(l.token_out, Some(b));
    assert_eq!(l.amount_in, Some(U256::from(1_000)));
    assert_eq!(l.amount_out_min, Some(U256::from(999)));
    assert_eq!(l.hops, 1);
    assert_eq!(l.value, U256::ZERO);
}

#[test]
fn t02_v2_pair_direct_swap_names_the_pool() {
    let pool = addr("0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"); // USDC/WETH pair
    let cd = v2_pair_swap_calldata(1_500, 0);
    let out = classify(pool, &cd, &RouterRegistry::mainnet());
    let TargetClass::Swap(legs) = out else {
        panic!("expected Swap, got {out:?}");
    };
    let l = &legs[0];
    assert_eq!(l.protocol, PoolProtocol::V2);
    assert_eq!(l.pool, Some(pool), "direct pair call names the pool");
    assert_eq!(l.amount_out, Some(U256::from(1_500)));
}

#[test]
fn t03_v3_exact_input_single_decodes_tokens_and_bounds() {
    let a = addr(TOKEN_A);
    let b = addr(TOKEN_B);
    let cd = v3_exact_input_single_calldata(a, b, 5_000, 4_999);
    let out = classify(
        addr("0xE592427A0AEce92De3Edee1F18E0157C05861564"),
        &cd,
        &RouterRegistry::mainnet(),
    );
    let TargetClass::Swap(legs) = out else {
        panic!("expected Swap, got {out:?}");
    };
    let l = &legs[0];
    assert_eq!(l.protocol, PoolProtocol::V3);
    assert_eq!(l.token_in, Some(a));
    assert_eq!(l.token_out, Some(b));
    assert_eq!(l.amount_in, Some(U256::from(5_000)));
    assert_eq!(l.amount_out_min, Some(U256::from(4_999)));
    assert_eq!(l.hops, 0);
}

#[test]
fn t04_v3_exact_input_multihop_decodes_endpoints_and_hop_count() {
    let a = addr(TOKEN_A);
    let mid = addr("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"); // WBTC
    let b = addr(TOKEN_B);
    let cd = v3_exact_input_calldata(&v3_path_2hop(&[a, mid, b]), 7_000, 6_999);
    let out = classify(
        addr("0xE592427A0AEce92De3Edee1F18E0157C05861564"),
        &cd,
        &RouterRegistry::mainnet(),
    );
    let TargetClass::Swap(legs) = out else {
        panic!("expected Swap, got {out:?}");
    };
    let l = &legs[0];
    assert_eq!(l.protocol, PoolProtocol::V3);
    assert_eq!(l.token_in, Some(a));
    assert_eq!(l.token_out, Some(b));
    assert_eq!(l.amount_in, Some(U256::from(7_000)));
    assert_eq!(l.hops, 2);
}

#[test]
fn t05_v3_output_methods_map_bounds_onto_out_fields() {
    // exactOutputSingle(ExactOutputSingleParams): (tokenIn, tokenOut, fee, recipient,
    // amountOut, amountInMaximum, sqrtPriceLimitX96)
    let a = addr(TOKEN_A);
    let b = addr(TOKEN_B);
    let mut args = Vec::new();
    args = push_word(args, &u2(0x20));
    args = push_word(args, &word_addr(a));
    args = push_word(args, &word_addr(b));
    args = push_word(args, &u2(500));
    args = push_word(
        args,
        &word_addr(addr("0x3333333333333333333333333333333333333333")),
    );
    args = push_word(args, &u2(4_000)); // amountOut exact
    args = push_word(args, &u2(4_041)); // amountInMaximum
    args = push_word(args, &u2(0));
    let mut cd = vec![0xdb, 0x3e, 0x21, 0x98];
    cd.extend_from_slice(&args);
    let out = classify(
        addr("0xE592427A0AEce92De3Edee1F18E0157C05861564"),
        &cd,
        &RouterRegistry::mainnet(),
    );
    let TargetClass::Swap(legs) = out else {
        panic!("expected Swap, got {out:?}");
    };
    let l = &legs[0];
    assert_eq!(l.amount_out, Some(U256::from(4_000)));
    assert_eq!(l.amount_in_max, Some(U256::from(4_041)));
    assert_eq!(l.token_in, Some(a));
    assert_eq!(l.token_out, Some(b));
}

#[test]
fn t06_v2_eth_paths_surface_weth_endpoint_and_min() {
    // swapExactETHForTokens(amountOutMin, path, to): value lives in the tx, not args.
    let b = addr(TOKEN_B);
    let mut args = Vec::new();
    args = push_word(args, &u2(88_888));
    args = push_word(args, &u2(0x80));
    args = push_word(
        args,
        &word_addr(addr("0x1111111111111111111111111111111111111111")),
    );
    args = push_word(args, &u2(9_999_999_999));
    args = push_word(args, &u2(2));
    args = push_word(
        args,
        &word_addr(addr("0xC02aaA39b223Fe8D0A0e5C4f27eAD9083C756Cc2")),
    ); // WETH
    args = push_word(args, &word_addr(b));
    while args.len() % 32 != 0 {
        args.push(0);
    }
    let mut cd = vec![0x7f, 0xf3, 0x6a, 0xb5];
    cd.extend_from_slice(&args);
    let out = classify(
        addr("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D"),
        &cd,
        &RouterRegistry::mainnet(),
    );
    let TargetClass::Swap(legs) = out else {
        panic!("expected Swap, got {out:?}");
    };
    let l = &legs[0];
    assert_eq!(
        l.token_in,
        Some(addr("0xC02aaA39b223Fe8D0A0e5C4f27eAD9083C756Cc2"))
    );
    assert_eq!(l.token_out, Some(b));
    assert_eq!(l.amount_out_min, Some(U256::from(88_888)));
}

#[test]
fn t07_npm_collect_is_inert_golden_live_probe() {
    // Golden vector from the live feed probe: collect() on the V3 NPM.
    let cd = vec![
        0xac, 0x96, 0x50, 0xd8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x02, 0x0c, 0x00, 0x00, 0x00, 0x00,
    ];
    let out = classify(
        addr("0xC36442b4a4522E871399CD717aBDD847Ab11FE88"),
        &cd,
        &RouterRegistry::mainnet(),
    );
    assert_eq!(out, TargetClass::Inert);
}

#[test]
fn t08_universal_router_is_opaque_never_guessed() {
    let mut cd = vec![0x24, 0x85, 0x6b, 0xc3];
    cd.extend_from_slice(&[0u8; 200]);
    let out = classify(
        addr("0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD"),
        &cd,
        &RouterRegistry::mainnet(),
    );
    assert_eq!(out, TargetClass::Opaque(OpaqueReason::HubInnerUndecodable));
}

#[test]
fn t09_v4_pool_manager_direct_is_opaque() {
    let mut cd = vec![0x3c, 0xce, 0xcf, 0x5e];
    cd.extend_from_slice(&[0u8; 64]);
    let out = classify(
        addr("0x000000000004444c5dc75cB358380D2e3De08A90"),
        &cd,
        &RouterRegistry::mainnet(),
    );
    assert_eq!(out, TargetClass::Opaque(OpaqueReason::HubInnerUndecodable));
}

#[test]
fn t10_unknown_target_unknown_selector_is_inert() {
    let mut cd = vec![0xde, 0xad, 0xbe, 0xef];
    cd.extend_from_slice(&[0u8; 64]);
    let out = classify(
        addr("0x9999999999999999999999999999999999999999"),
        &cd,
        &RouterRegistry::mainnet(),
    );
    assert_eq!(out, TargetClass::Inert);
}

#[test]
fn t11_short_calldata_is_opaque_too_short() {
    let out = classify(
        addr("0x1111111111111111111111111111111111111111"),
        &[0x38, 0xed],
        &RouterRegistry::mainnet(),
    );
    assert_eq!(out, TargetClass::Opaque(OpaqueReason::CalldataTooShort));
}

#[test]
fn t12_malformed_v2_path_offset_is_opaque_never_guessed() {
    let mut cd = v2_exact_tokens_calldata(1, 1, &[addr(TOKEN_A), addr(TOKEN_B)]);
    // Corrupt the path-offset word (args word 2).
    let off_pos = 4 + 2 * 32;
    cd[off_pos..off_pos + 32].fill(0xff);
    let out = classify(
        addr("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D"),
        &cd,
        &RouterRegistry::mainnet(),
    );
    assert_eq!(out, TargetClass::Opaque(OpaqueReason::MalformedArgs));
}
