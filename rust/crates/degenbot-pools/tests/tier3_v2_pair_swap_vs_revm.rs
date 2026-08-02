//! Tier-3 V2-pair on-chain accuracy oracle (ergo task `TLBUNW`, epic
//! `UP5NH6` — the V2 family slice of SH6HAK's Tier-3 cutover). Deploys the
//! canonical v2-core `UniswapV2Pair` as real bytecode via the
//! `V2SwapOracleHarness` (solc-0.5.16 compiled), mints reserves + `sync`s so
//! the pair's slot-8 reserves equal the live `balanceOf` (K-check consistency
//! by construction — per ADR-020 D4 the whole-slot-set seeding avoids the
//! production slot-8-vs-balanceOf inconsistency), then drives `pair.swap`
//! with the engine's computed `amountOut` and asserts byte-exactness via the
//! K-invariant boundary:
//!
//!   - `doSwap(amount_in, zfo, engine_out)` SUCCEEDS (the K-check
//!     `balance0Adjusted * balance1Adjusted >= reserve0 * reserve1` passes
//!     with equality at the maximal output) → `engine_out ≤` on-chain
//!     `getAmountOut(amount_in)`.
//!   - `doSwap(amount_in, zfo, engine_out + 1)` REVERTS (`UniswapV2: K`) →
//!     `engine_out + 1 >` maximal → `engine_out ≥` maximal.
//!   - Together: `engine_out ==` on-chain `getAmountOut(amount_in)`
//!     byte-exact — the engine's `IntHopState::swap`
//!     (`gamma_numer * reserve_out * x / (fee_denom * reserve_in +
//!     gamma_numer * x)`, EVM floor DIV) mirrors v2-core's `getAmountOut`
//!     exactly.
//!
//! The canonical V2 fee is hardcoded 0.3%
//! (`gamma_numer = 997, fee_denom = 1000`) in v2-core `UniswapV2Pair`. The
//! engine models fee via `V2PoolIdentity::fee_token0/fee_token1`; the
//! fork-fee case (Pancake/Sushi hardcode different fees in bytecode) is a
//! follow-on sub-slice — the engine's fee-parameterization is already
//! byte-exact to `getAmountOut` for any `(gamma_numer, fee_denom)`, so a
//! fork-bytecode Tier-3 harness would re-prove the same formula at the
//! fork's fee and is deferred as gilding.
//!
//! ## Harness bytecode (committed)
//!
//! Runs in the default `cargo test --workspace` suite. The canonical
//! v2-core bytecode is loaded from the committed `tier3-oracle/artifacts/`
//! tree (no solc/forge needed to RUN). Artifact integrity is enforced two
//! ways: `tier3_harness_artifacts.rs` hashes the tracked sources
//! (toolchain-free), and `tier3-oracle/verify-tier3-artifacts.sh` recompiles
//! every harness and byte-compares it to the committed artifact. After a
//! harness-source edit, regenerate + publish via
//! `tier3-oracle/build-tier3-v2-swap-harness.sh`.

#![allow(clippy::cast_possible_wrap)]

use std::path::PathBuf;

use alloy::primitives::{keccak256, Bytes, U256};
use proptest::prelude::*;
use revm::context::TxEnv;
use revm::context_interface::result::{ExecutionResult, Output};
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::primitives::TxKind;
use revm::{ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};

use degenbot_v2_math::IntHopState;

/// The canonical Uniswap V2 0.3% fee: retained fraction 997/1000.
const V2_FEE_GAMMA_NUMER: u64 = 997;
const V2_FEE_DENOM: u64 = 1000;

/// First 4 bytes of `keccak256(signature)` — the Solidity function selector.
fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes());
    let mut out = [0u8; 4];
    out.copy_from_slice(&h.0[0..4]);
    out
}

/// Repo path to a built harness artifact (foundry `out/<File>.sol/<Contract>.json`).
fn harness_artifact_path(file: &str, contract: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tier3-oracle/artifacts")
        .join(file)
        .join(format!("{contract}.json"))
}

/// Load the creation (`bytecode.object`) hex for a harness.
fn load_creation_bytecode(file: &str, contract: &str) -> Vec<u8> {
    let path = harness_artifact_path(file, contract);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing harness artifact {}: {e}", path.display()));
    let v: serde_json::Value = serde_json::from_str(&raw).expect("valid harness JSON");
    let hex_str = v["bytecode"]["object"]
        .as_str()
        .expect("artifact has bytecode.object (creation)");
    hex::decode(hex_str.trim_start_matches("0x")).expect("hex creation bytecode")
}

/// The engine's V2 `getAmountOut` via `IntHopState::swap`.
fn engine_amount_out(reserve_in: U256, reserve_out: U256, amount_in: U256) -> U256 {
    IntHopState::new(reserve_in, reserve_out, V2_FEE_GAMMA_NUMER, V2_FEE_DENOM)
        .swap(amount_in)
        .expect("engine swap does not overflow under proptest bounds")
}

/// Run a single pristine swap attempt: deploy a fresh harness, `setup(r0,r1)`
/// (mint reserves + sync), then `doSwap(amount_in, zfo, amount_out)`.
/// Returns whether `pair.swap` accepted `amount_out` (the K-invariant check
/// passed). Fully self-contained: each call rebuilds the evm + harness so
/// reserves/balances are pristine (a `doSwap` mutates them, so reuse would
/// compound and break the K-boundary crispness).
fn pristine_swap_accepts(r0: u128, r1: u128, amount_in: U256, zfo: bool, amount_out: U256) -> bool {
    let db = CacheDB::new(EmptyDB::default());
    let mut evm = revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet();
    evm.ctx.cfg.disable_nonce_check = true;

    // 1. Deploy the harness (mock tokens + the real UniswapV2Pair).
    let init_code = load_creation_bytecode("V2SwapOracleHarness.sol", "V2SwapOracleHarness");
    let deploy_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Create)
                .gas_limit(16_700_000)
                .data(Bytes::from(init_code))
                .build()
                .expect("deploy tx"),
        )
        .expect("deploy transact");
    let harness = match &deploy_res.result {
        ExecutionResult::Success {
            output: Output::Create(_, Some(addr)),
            ..
        } => *addr,
        other => panic!("harness deploy did not create a contract: {other:?}"),
    };
    evm.commit(deploy_res.state);

    // 2. setup(r0, r1): mint reserves + sync (slot-8 == balanceOf).
    let mut setup_call = selector("setup(uint112,uint112)").to_vec();
    setup_call.extend_from_slice(&U256::from(r0).to_be_bytes::<32>());
    setup_call.extend_from_slice(&U256::from(r1).to_be_bytes::<32>());
    let setup_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(6_000_000)
                .data(Bytes::from(setup_call))
                .build()
                .expect("setup tx"),
        )
        .expect("setup transact");
    assert!(
        matches!(&setup_res.result, ExecutionResult::Success { .. }),
        "setup failed: {:?}",
        setup_res.result
    );
    evm.commit(setup_res.state);

    // 3. doSwap(amount_in, zfo, amount_out, recipient=harness).
    let recipient = harness;
    let mut swap_call = selector("doSwap(uint256,bool,uint256,address)").to_vec();
    swap_call.extend_from_slice(&amount_in.to_be_bytes::<32>());
    swap_call.extend_from_slice(&U256::from(zfo).to_be_bytes::<32>());
    swap_call.extend_from_slice(&amount_out.to_be_bytes::<32>());
    // address: right-aligned in 32 bytes (12 zero bytes prefix + 20-byte address).
    // A bare `recipient.as_slice()` (20 bytes) short-changes the calldata and
    // Solidity 0.5.16's ABI decoder reverts it (empty revert).
    swap_call.extend_from_slice(&[0u8; 12]);
    swap_call.extend_from_slice(recipient.as_slice());
    let swap_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(6_000_000)
                .data(Bytes::from(swap_call))
                .build()
                .expect("doSwap tx"),
        )
        .expect("doSwap transact");
    let ok = matches!(&swap_res.result, ExecutionResult::Success { .. });
    ok
}

/// Pinned byte-exact oracle: the engine's `swap` output is the on-chain
/// maximal `getAmountOut` — proven by `pair.swap` accepting it (K-check
/// passes with equality) and rejecting `+1` (K-check reverts
/// `UniswapV2: K`). Each assertion runs on a fresh pristine harness (a
/// `doSwap` mutates reserves/balances, so reuse would compound).
#[test]
fn v2_pair_swap_is_byte_exact_to_v2_core_get_amount_out() {
    // 1000 token0 ↔ 2000 token1 (1e21 / 2e21 wei); swap 100 token0 in.
    let r0: u128 = 1_000_000_000_000_000_000_000;
    let r1: u128 = 2_000_000_000_000_000_000_000;
    let amount_in: u128 = 100_000_000_000_000_000_000;
    let zfo = true;

    let engine_out = engine_amount_out(U256::from(r0), U256::from(r1), U256::from(amount_in));
    assert!(
        engine_out + U256::from(1u64) <= U256::from(r1),
        "engine_out {engine_out} +1 must be ≤ r1 {r1} (no token-transfer underflow on +1)"
    );

    // engine_out is accepted (K holds with equality at the maximal output).
    assert!(
        pristine_swap_accepts(r0, r1, U256::from(amount_in), zfo, engine_out),
        "engine_out {engine_out} should be accepted by pair.swap (K-check passes)"
    );
    // engine_out + 1 is REJECTED (K-check reverts — `UniswapV2: K`).
    assert!(
        !pristine_swap_accepts(
            r0,
            r1,
            U256::from(amount_in),
            zfo,
            engine_out + U256::from(1u64)
        ),
        "engine_out + 1 should REVERT (K-check) — byte-exact maximal boundary"
    );
}

proptest! {
    /// Proptest the byte-exact V2 oracle over (reserve0, reserve1, amount_in,
    /// direction). Each of the two assertions (accept engine_out, reject
    /// engine_out+1) runs on a fresh pristine harness. Bounds keep amounts in
    /// the no-token-transfer-underflow domain (`engine_out + 1 ≤ reserve_out`)
    /// so the `+1` revert is the K-check, not a pre-K `balanceOf` underflow
    /// (Solidity 0.5.16 has no overflow checks — an underflow wraps and the
    /// K-check would spuriously pass, masking the byte-exact boundary).
    #[test]
    fn v2_pair_byte_exact_proptest(
        r0 in 1_000_000u128..1_000_000_000_000_000_000u128,
        r1 in 1_000_000u128..1_000_000_000_000_000_000u128,
        // amount_in = reserve_in / frac (frac ≥ 4 keeps engine_out ≤ ~reserve_out/4).
        amount_in_frac in 4u64..1_000u64,
        zfo in any::<bool>(),
    ) {
        let (reserve_in, reserve_out) = if zfo { (r0, r1) } else { (r1, r0) };
        let amount_in = U256::from(reserve_in) / U256::from(amount_in_frac);
        if amount_in.is_zero() {
            return Ok(()); // degenerate (getAmountOut(0) = 0; +1 not byte-exact).
        }
        let engine_out = engine_amount_out(U256::from(reserve_in), U256::from(reserve_out), amount_in);
        // Skip cases where engine_out+1 ≥ reserve_out (would underflow pre-K).
        if engine_out + U256::from(1u64) >= U256::from(reserve_out) {
            return Ok(());
        }

        // engine_out must be accepted (K holds with equality at the maximal output).
        prop_assert!(
            pristine_swap_accepts(r0, r1, amount_in, zfo, engine_out),
            "engine_out {} should pass K-check (r0={}, r1={}, in={}, zfo={})",
            engine_out, r0, r1, amount_in, zfo
        );
        // engine_out + 1 must revert (K-check fails).
        prop_assert!(
            !pristine_swap_accepts(r0, r1, amount_in, zfo, engine_out + U256::from(1u64)),
            "engine_out + 1 should REVERT (K-check) — byte-exact boundary broken (r0={}, r1={}, in={}, zfo={})",
            r0, r1, amount_in, zfo
        );
    }
}
