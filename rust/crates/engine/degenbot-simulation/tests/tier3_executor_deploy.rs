#![expect(clippy::expect_used, clippy::panic, clippy::print_stdout)]
//! Tier-3b executor deploy-contract oracle (BHL2R2 / S2 foundation).
//!
//! Verifies that the committed Vyper executor artifact deploys correctly in
//! the evm2 fixture EVM with the REAL constructor-arg contract — resolving the
//! open "verify once by deploying" item from S1 (2ISTMX) before S2's
//! reproduction harness depends on it.
//!
//! ## Finding that this test locks in (correct deployment contract)
//! `cmd_executor.vy`'s constructor takes four role addresses. The `immutables.json`
//! `code_layout` lists seven entries, but only WETH, `PoolManager`, USDC, and USDT
//! come from the appended calldata:
//! `OWNER_ADDR = msg.sender` (endogenous) and `WETH_DELTA_SLOT` /
//! `NATIVE_DELTA_SLOT` are **computed** inside `__init__`
//! (`keccak256(abi.encodePacked(self, currency))`). So the deploy calldata is
//! `creation.hex ++ abi.encode(weth, pool_manager, usdc, usdt)` (128 bytes), not
//! the 7*32 = 224 bytes the `code_layout` alone would suggest. (Earlier guidance to
//! "read slot 0" was also wrong — vyper embeds immutables in the runtime CODE,
//! not storage.)
//!
//! ## What is asserted
//! 1. Deployment with the 4-arg append succeeds and yields a created address.
//! 2. The computed `WETH_DELTA_SLOT` / `NATIVE_DELTA_SLOT` immutables actually
//!    land: after deployment we recompute them from the deployed address and
//!    require the 32-byte values to appear in the deployed runtime code
//!    (pseudo-random keccak outputs — a false positive is cryptographically
//!    implausible), and `WETH_ADDR`/`POOL_MANAGER_ADDR` to appear as well.

use alloy::primitives::{utils::keccak256, Address, Bytes, B256};

use degenbot_simulation::oracle::{self, Output};

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .expect("canonicalize repo root")
}

/// Minimal hex decoder (the `hex` crate is not a dep of this crate).
fn load_hex(rel: &str) -> Vec<u8> {
    let raw = std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("missing {rel}: {e}"));
    let s: String = raw.trim().chars().filter(|c| *c != '\n').collect();
    assert!(s.len().is_multiple_of(2), "{rel}: odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex byte"))
        .collect()
}

/// ABI-encode the constructor's four role addresses.
fn deploy_args(
    weth: Address,
    pool_manager: Address,
    stablecoin0: Address,
    stablecoin1: Address,
) -> Vec<u8> {
    let mut args = vec![0u8; 128];
    args[12..32].copy_from_slice(weth.as_slice());
    args[44..64].copy_from_slice(pool_manager.as_slice());
    args[76..96].copy_from_slice(stablecoin0.as_slice());
    args[108..128].copy_from_slice(stablecoin1.as_slice());
    args
}

#[test]
fn executor_deploys_with_four_constructor_args_and_embeds_computed_immutables() {
    let weth = Address::repeat_byte(0x11);
    let pool_manager = Address::repeat_byte(0x22);
    let stablecoin0 = Address::repeat_byte(0x33);
    let stablecoin1 = Address::repeat_byte(0x44);

    let mut init = load_hex("tier3-oracle/artifacts/executor/cmd_executor.creation.hex");
    init.extend_from_slice(&deploy_args(weth, pool_manager, stablecoin0, stablecoin1));

    let mut evm = oracle::new_fixture_evm();
    oracle::set_disable_nonce_check(&mut evm, true);

    let (executor, code) = match oracle::transact(
        &mut evm,
        oracle::TxSpec::Deploy {
            init_code: Bytes::from(init),
            gas: 16_000_000,
        },
    ) {
        oracle::Verdict::Accepted {
            output: Output::Create(code, Some(addr)),
            ..
        } => (addr, code),
        other => panic!("executor deploy failed: {other:?}"),
    };

    // Recompute the two DELTA_SLOT immutables exactly as __init__ computes them:
    // keccak256(abi.encodePacked(self, currency)) with left-padded 32-byte addresses.
    let self_bytes = B256::left_padding_from(executor.as_slice());
    let weth_bytes = B256::left_padding_from(weth.as_slice());
    let expected_weth_delta = keccak256([self_bytes.as_slice(), weth_bytes.as_slice()].concat());
    let expected_native_delta = keccak256([self_bytes.as_slice(), B256::ZERO.as_slice()].concat());

    // The creation's `Create` output IS the deployed runtime code (the
    // immutables are embedded in it).
    assert!(!code.is_empty(), "deployed executor has empty runtime code");
    println!(
        "executor deployed at {executor}; runtime {} bytes",
        code.len()
    );

    let has = |needle: &[u8]| code.windows(needle.len()).any(|w| w == needle);
    assert!(
        has(expected_weth_delta.as_slice()),
        "WETH_DELTA_SLOT immutable not found in deployed executor code"
    );
    assert!(
        has(expected_native_delta.as_slice()),
        "NATIVE_DELTA_SLOT immutable not found in deployed executor code"
    );
    assert!(
        has(weth.as_slice()),
        "WETH_ADDR immutable not found in deployed executor code"
    );
    assert!(
        has(pool_manager.as_slice()),
        "POOL_MANAGER_ADDR immutable not found in deployed executor code"
    );
    assert!(has(stablecoin0.as_slice()), "USDC_ADDR immutable not found");
    assert!(has(stablecoin1.as_slice()), "USDT_ADDR immutable not found");
}
