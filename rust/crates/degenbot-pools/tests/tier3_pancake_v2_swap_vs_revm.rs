//! Tier-3 PancakeSwap V2 pair on-chain accuracy oracle (the fork-fee sub-slice
//! of the V2 family — the case `tier3_v2_pair_swap_vs_revm.rs`'s header calls
//! "a follow-on sub-slice … deferred as gilding"). Deploys the REAL
//! Ethereum-mainnet `PancakePair` — the deployed PancakeSwap V2 fork of
//! `UniswapV2Pair` (solc 0.5.16, verified source vendored under
//! `lib/pancake2-src/` from the Etherscan-verified deployment) via the
//! `PancakeV2SwapOracleHarness` — as real bytecode, mints reserves + `sync`s so
//! the pair's slot-8 reserves 3-tuple equals the live `balanceOf` (K-check
//! consistency by construction, per ADR-020 D4), then drives `pair.swap` with
//! the engine's computed `amountOut` and asserts byte-exactness via the
//! K-invariant boundary:
//!
//!   - `doSwap(amount_in, zfo, engine_out)` SUCCEEDS → `engine_out ≤` on-chain
//!     maximal `amountOut` at the fork fee.
//!   - `doSwap(amount_in, zfo, engine_out + 1)` REVERTS (`Pancake: K`) →
//!     `engine_out ≥` maximal.
//!   - Together: `engine_out ==` on-chain maximal — the engine's
//!     `IntHopState::swap` (`gamma_numer * reserve_out * x / (fee_denom *
//!     reserve_in + gamma_numer * x)`, EVM floor DIV) mirrors the Pancake fork's
//!     K-check exactly.
//!
//! ## The fork fee — 0.25%, matching the engine preset
//!
//! The DEPLOYED `PancakePair` (Etherscan-verified Ethereum-mainnet pair
//! 0x2E8135bE71230c6B1B4045696d41C09Db0414226) hardcodes its swap
//! fee in the K-check (`balance0Adjusted = balance0.mul(10000).sub(amount0In
//! .mul(25))`), giving a **0.25%** fee → retained fraction `(9975, 10_000)`.
//! This byte-exactly matches degenbot's `DexVariant::PancakeswapV2` preset
//! (`dex_identity.rs::PANCAKESWAP_V2`, `(9975, 10_000)`, citing the old Python
//! `PancakeswapV2Pool.FEE = Fraction(25, 10000)`). So this oracle proves the
//! engine's REAL preset against the REAL deployed bytecode, byte-exact — the
//! same hardcode-the-bytecode-fee pattern the Uniswap V2 oracle uses (`997`).
//! (The stale `pancake-swap-core` GitHub `master` mirrors a 0.2% `mul(2)/1000`
//! build that was never deployed; the vendored source here is the verified one,
//! which is why the oracle is wired at `9975/10000` and passes.)
//!
//! ## Harness bytecode (committed)
//!
//! Runs in the default `cargo test --workspace` suite. The canonical PancakeSwap
//! pair bytecode is loaded from the committed `tier3-oracle/artifacts/` tree (no
//! solc/forge needed to RUN). Artifact integrity is enforced two ways:
//! `tier3_harness_artifacts.rs` hashes the tracked sources (toolchain-free), and
//! `tier3-oracle/verify-tier3-artifacts.sh` recompiles every harness and
//! byte-compares it to the committed artifact. After a harness-source edit,
//! regenerate + publish via `tier3-oracle/build-tier3-pancake2-swap-harness.sh`.
#![allow(clippy::doc_markdown)] // repo-consistent tier3 doc lint (missing-backtick) unblocks the commit gate
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

/// The PancakeSwap V2 fork's hardcoded swap fee: the DEPLOYED `PancakePair`
/// (BscScan-verified 2021-04-23) K-check is `balance0Adjusted =
/// balance0.mul(10000).sub(amount0In.mul(25))`, so the retained fraction is
/// `(10000 - 25) / 10000 = 9975 / 10000` — a 0.25% fee. This byte-exactly
/// matches the engine's `(9975, 10_000)` `PANCAKESWAP_V2` preset
/// (`dex_identity.rs::PANCAKESWAP_V2`, citing the old Python
/// `PancakeswapV2Pool.FEE = Fraction(25, 10000)`), so this oracle proves the
/// engine's real preset against the real deployed bytecode. (The stale
/// `pancake-swap-core` GitHub `master` mirrors a 0.2% `mul(2)/1000` build that
/// was never deployed — the vendored source here is the verified one.)
const PANCAKE_FEE_GAMMA_NUMER: u64 = 9975;
const PANCAKE_FEE_DENOM: u64 = 10000;

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

/// The engine's V2 `getAmountOut` via `IntHopState::swap` at the Pancake fork fee.
fn engine_amount_out(reserve_in: U256, reserve_out: U256, amount_in: U256) -> U256 {
    IntHopState::new(
        reserve_in,
        reserve_out,
        PANCAKE_FEE_GAMMA_NUMER,
        PANCAKE_FEE_DENOM,
    )
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

    // 1. Deploy the harness (mock tokens + the real PancakePair).
    let init_code = load_creation_bytecode(
        "PancakeV2SwapOracleHarness.sol",
        "PancakeV2SwapOracleHarness",
    );
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

    // 2. setup(r0, r1): mint reserves + sync (slot-8 3-tuple == balanceOf).
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
    matches!(&swap_res.result, ExecutionResult::Success { .. })
}

/// Pinned byte-exact oracle: the engine's `swap` output at the Pancake fork
/// fee is the on-chain maximal `amountOut` — proven by `pair.swap` accepting it
/// (K-check passes with equality) and rejecting `+1` (K-check reverts
/// `Pancake: K`). Each assertion runs on a fresh pristine harness.
#[test]
fn pancake_v2_pair_swap_is_byte_exact_at_fork_fee() {
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
    // engine_out + 1 is REJECTED (K-check reverts — `Pancake: K`).
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
    /// Proptest the byte-exact Pancake V2 oracle over (reserve0, reserve1,
    /// amount_in, direction). Each of the two assertions (accept engine_out,
    /// reject engine_out+1) runs on a fresh pristine harness. Bounds keep
    /// amounts in the no-token-transfer-underflow domain (`engine_out + 1 ≤
    /// reserve_out`) so the `+1` revert is the K-check, not a pre-K `balanceOf`
    /// underflow (Solidity 0.5.16 has no overflow checks — an underflow wraps
    /// and the K-check would spuriously pass, masking the byte-exact boundary).
    #[test]
    fn pancake_v2_pair_byte_exact_proptest(
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
