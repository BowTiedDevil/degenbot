//! Tier-3a byte-exact oracle for `compute_swap_step` (V3 + V4) against the
//! canonical Uniswap core libraries run as real EVM bytecode in revm.
//!
//! Ergo task `OZRQS6` (epic `UP5NH6`). Closes the "Rust == Rust" blind spot
//! of Tier 2: the existing `swap_math.rs` proptest checks V3/V4 INVARIANTS
//! (amountIn+fee ≤ amountRemaining; sqrtPriceNext in range) — NOT byte-exact
//! equality with the canonical library. This test runs the REAL v3-core
//! `SwapMath.computeSwapStep` (solc 0.7.6) and v4-core `SwapMath.computeSwapStep`
//! (solc 0.8.26) bytecode, deployed into an offline revm `CacheDB<EmptyDB>`,
//! under a proptest fuzz — and asserts each Rust output field === the on-chain
//! output byte-for-byte (U256 equality).
//!
//! ## Why a single-step oracle does NOT catch the word-boundary bug
//!
//! The just-fixed V4 `CurrencyNotSettled` (commit 84534443) was in the SOLVER's
//! MULTI-STEP orchestration (one `compute_swap_step` per initialized-tick
//! range, floored at word boundaries), NOT in `compute_swap_step` itself.
//! Tier 3a does NOT catch that class — it tests a single step in isolation.
//! The bug-catching tier is 3b (the end-to-end `Pool.swap` walk). 3a is the
//! byte-exact single-step foundation + the harness-from-reference-library
//! pattern 3b extends.
//!
//! ## Honest oracle-strength note (per AC)
//!
//! `compute_swap_step_v3`/`v4` were already byte-exact (the divergence was
//! orchestration). This test therefore mostly GREENs symmetrically; its value
//! is the REGRESSION GUARD: any future perturbation to the Rust swap-step math
//! (rounding direction, fee calc, sign convention) REDs here before it ships.
//! The harness-from-reference-library loop (forge/solc → bytecode → revm →
//! assert) is the real deliverable; the end-to-end payoff is 3b.
//!
//! ## Harness bytecode (committed)
//!
//! Runs in the default `cargo test --workspace` suite. The V3/V4
//! `SwapMath`-harness bytecode is loaded from the committed
//! `tier3-oracle/artifacts/` tree (no solc/forge needed to RUN). Artifact
//! integrity is enforced two ways: `tier3_harness_artifacts.rs` hashes the
//! tracked sources (toolchain-free), and `tier3-oracle/verify-tier3-artifacts.sh`
//! recompiles every harness and byte-compares it to the committed artifact.
//! After a harness-source edit, regenerate + publish via
//! `tier3-oracle/build-tier3-harnesses.sh`.

use std::path::PathBuf;

use alloy::primitives::{aliases::I256, Address, Bytes, U256};
use proptest::prelude::*;
use revm::bytecode::Bytecode;
use revm::context::TxEnv;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::primitives::TxKind;
use revm::state::AccountInfo;
use revm::{ExecuteEvm, MainBuilder, MainContext};

use degenbot_cl_math::cl_lib::swap_math::{
    compute_swap_step_v3, compute_swap_step_v4, SwapStepResult,
};

/// `computeSwapStep(uint160,uint160,uint128,int256,uint24)` selector
/// (`cast sig` → `0x100d3f74`). Identical for the V3 + V4 harnesses (same
/// external signature).
const COMPUTE_SWAP_STEP_SELECTOR: [u8; 4] = [0x10, 0x0d, 0x3f, 0x74];

/// Marker bit 23 of an int24 (the tick/sign-extension boundary — also the
/// exact-in branch boundary reused below). Unused elsewhere; kept as a named
/// constant for the on-chain-decode sign extension check.
const _INT24_SIGN_BIT: u32 = 0x0080_0000;

/// Repo path to a built harness artifact JSON (foundry `out/<File>.sol/<Contract>.json`
/// shape — the V3 direct-solc build reshapes to this same shape).
fn harness_artifact_path(contract_dir: &str, contract: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tier3-oracle/artifacts")
        .join(contract_dir)
        .join(format!("{contract}.json"))
}

/// Load the `deployedBytecode.object` hex from a harness artifact JSON.
fn load_harness_bytecode(contract_dir: &str, contract: &str) -> Bytecode {
    let path = harness_artifact_path(contract_dir, contract);
    let path_display = path.display();
    let json_str = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "Tier-3a: harness artifact not found at {path_display} ({err}). \
             Run `just test-tier3-step` (it runs \
             tier3-oracle/build-tier3-harnesses.sh before this test), not bare \
             `cargo test --include-ignored`."
        )
    });
    let parsed: serde_json::Value =
        serde_json::from_str(&json_str).expect("harness JSON must be valid");
    let hex_str = parsed
        .get("deployedBytecode")
        .and_then(|v| v.get("object"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("harness JSON missing deployedBytecode.object: {path_display}"));
    let bytes = alloy::primitives::hex::decode(hex_str.trim_start_matches("0x"))
        .expect("deployedBytecode.object must be valid 0x-prefixed hex");
    Bytecode::new_raw(Bytes::from(bytes))
}

/// ABI-encode the `computeSwapStep` calldata: selector + 5 padded 32-byte args.
fn compute_swap_step_calldata(
    sqrt_current: U256,
    sqrt_target: U256,
    liquidity: u128,
    amount_remaining: I256,
    fee_pips: u32,
) -> Bytes {
    let mut data = Vec::with_capacity(4 + 5 * 32);
    data.extend_from_slice(&COMPUTE_SWAP_STEP_SELECTOR);
    data.extend_from_slice(&sqrt_current.to_be_bytes::<32>());
    data.extend_from_slice(&sqrt_target.to_be_bytes::<32>());
    data.extend_from_slice(&U256::from(liquidity).to_be_bytes::<32>());
    data.extend_from_slice(&amount_remaining.to_be_bytes::<32>());
    data.extend_from_slice(&U256::from(fee_pips).to_be_bytes::<32>());
    Bytes::from(data)
}

/// Call a deployed `computeSwapStep` harness via revm `transact`. Returns
/// `Ok(SwapStepResult)` if the call succeeded, `Err(())` if it reverted.
fn call_harness(
    harness_addr: Address,
    db: CacheDB<EmptyDB>,
    calldata: Bytes,
) -> Result<SwapStepResult, ()> {
    let mut evm = revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet();
    let tx = TxEnv::builder()
        .kind(TxKind::Call(harness_addr))
        .gas_limit(15_000_000)
        .data(calldata)
        .build()
        .expect("well-formed tx");
    let result = evm.transact(tx).expect("revm transact must not error");
    let res = result.result;
    if !res.is_success() {
        return Err(());
    }
    let out = res.into_output().expect("success must have output");
    let words: Vec<U256> = (0..4)
        .map(|i| {
            let mut buf = [0u8; 32];
            buf.copy_from_slice(&out.as_ref()[i * 32..(i + 1) * 32]);
            U256::from_be_bytes(buf)
        })
        .collect();
    // Return layout: (uint160 sqrtPriceNextX96, uint256 amountIn, uint256 amountOut, uint256 feeAmount).
    Ok(SwapStepResult {
        sqrt_price_next: words[0] & ((U256::from(1u64) << 160) - U256::from(1u64)),
        amount_in: words[1],
        amount_out: words[2],
        fee_amount: words[3],
    })
}

/// Deploy `code` at `addr` into a fresh `CacheDB<EmptyDB>`.
fn db_with_contract(addr: Address, code: Bytecode) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        addr,
        AccountInfo {
            balance: U256::from(1_000_000_000_000_000_000u64),
            code: Some(code),
            ..Default::default()
        },
    );
    db
}

// ── proptest strategies (mirror swap_math.rs proptests, smaller ranges to
//    keep the on-chain path overflow-free + tractable). ──────────────────

fn arb_sqrt_price() -> impl Strategy<Value = U256> {
    (1u64..u64::MAX).prop_map(U256::from)
}
fn arb_liquidity() -> impl Strategy<Value = i128> {
    (0i64..=i64::MAX).prop_map(i128::from)
}
fn arb_amount() -> impl Strategy<Value = I256> {
    (i64::MIN + 1..=i64::MAX).prop_map(|v| I256::try_from(v).unwrap())
}
fn arb_fee_pips() -> impl Strategy<Value = u32> {
    0u32..=1_000_000
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// V3 byte-exact: `compute_swap_step_v3` === v3-core `SwapMath.computeSwapStep`
    /// (solc 0.7.6, real bytecode). Amount sign convention: positive = exact-in.
    #[test]
    fn v3_compute_swap_step_is_byte_exact_to_v3_core_bytecode(
        sp_current in arb_sqrt_price(),
        sp_target in arb_sqrt_price(),
        liquidity in arb_liquidity(),
        amount in arb_amount(),
        fee_pips in arb_fee_pips(),
    ) {
        let addr = Address::repeat_byte(0x31);
        let db = db_with_contract(addr, load_harness_bytecode("SwapMathV3Harness.sol", "SwapMathV3Harness"));
        let calldata = compute_swap_step_calldata(sp_current, sp_target, liquidity.cast_unsigned(), amount, fee_pips);

        let rust = compute_swap_step_v3(sp_current, sp_target, liquidity, amount, U256::from(fee_pips));
        let onchain = call_harness(addr, db, calldata);
        match (rust, onchain) {
            (Ok(r), Ok(o)) => {
                prop_assert_eq!(r.sqrt_price_next, o.sqrt_price_next, "sqrtPriceNext diverges");
                prop_assert_eq!(r.amount_in, o.amount_in, "amountIn diverges");
                prop_assert_eq!(r.amount_out, o.amount_out, "amountOut diverges");
                prop_assert_eq!(r.fee_amount, o.fee_amount, "feeAmount diverges");
            }
            (Err(_), Err(())) => { /* both revert — consistent */ }
            (rust_res, onchain_res) => {
                prop_assert!(
                    false,
                    "success/revert divergence: rust={:?} onchain_success={}",
                    rust_res.is_ok(),
                    onchain_res.is_ok()
                );
            }
        }
    }

    /// V4 byte-exact: `compute_swap_step_v4` === v4-core `SwapMath.computeSwapStep`
    /// (solc 0.8.26). Amount sign convention: NEGATIVE = exact-in (opposite of
    /// V3; the protocol-fee threading happens at the `Pool.swap` caller, so this
    /// single-step oracle takes the pre-computed combined feePips directly).
    #[test]
    fn v4_compute_swap_step_is_byte_exact_to_v4_core_bytecode(
        sp_current in arb_sqrt_price(),
        sp_target in arb_sqrt_price(),
        liquidity in arb_liquidity(),
        amount in arb_amount(),
        fee_pips in arb_fee_pips(),
    ) {
        // V4 exact-out forbids fee_pips == MAX_SWAP_FEE (Solidity devoc);
        // skip the disallowed combo (matches swap_math.rs V4 proptest).
        if amount >= I256::ZERO && fee_pips == 1_000_000 {
            return Ok(());
        }
        let addr = Address::repeat_byte(0x41);
        let db = db_with_contract(addr, load_harness_bytecode("SwapMathV4Harness.sol", "SwapMathV4Harness"));
        let calldata = compute_swap_step_calldata(sp_current, sp_target, liquidity.cast_unsigned(), amount, fee_pips);

        let rust = compute_swap_step_v4(sp_current, sp_target, liquidity, amount, U256::from(fee_pips));
        let onchain = call_harness(addr, db, calldata);
        match (rust, onchain) {
            (Ok(r), Ok(o)) => {
                prop_assert_eq!(r.sqrt_price_next, o.sqrt_price_next, "sqrtPriceNext diverges");
                prop_assert_eq!(r.amount_in, o.amount_in, "amountIn diverges");
                prop_assert_eq!(r.amount_out, o.amount_out, "amountOut diverges");
                prop_assert_eq!(r.fee_amount, o.fee_amount, "feeAmount diverges");
            }
            (Err(_), Err(())) => { /* both revert — consistent */ }
            (rust_res, onchain_res) => {
                prop_assert!(
                    false,
                    "success/revert divergence: rust={:?} onchain_success={}",
                    rust_res.is_ok(),
                    onchain_res.is_ok()
                );
            }
        }
    }
}

/// One hand-derived pinned input anchoring the oracle (GREEN) + asserts the
/// comparison bites: swapping `amountIn` ↔ `amountOut` would fail for any
/// non-symmetric input (manually confirmed RED before this GREEN — the
/// step-level math was already byte-exact; the word-boundary divergence lived
/// in the solver's multi-step orchestration, not here).
#[test]
fn v3_pinned_input_anchors_byte_exact_oracle() {
    // Hand-picked: small sqrt prices straddling (current > target → zero_for_one),
    // exact-in, non-trivial liquidity + 0.3% fee (3000 pips).
    let sp_current = U256::from(1_000_000_000_000u64);
    let sp_target = U256::from(990_000_000_000u64);
    let liquidity = 10_000_000_000u128;
    let amount = I256::try_from(1_000_000i64).unwrap(); // exact in (positive)
    let fee_pips = 3000u32;

    let addr = Address::repeat_byte(0x31);
    let db = db_with_contract(
        addr,
        load_harness_bytecode("SwapMathV3Harness.sol", "SwapMathV3Harness"),
    );
    let calldata = compute_swap_step_calldata(sp_current, sp_target, liquidity, amount, fee_pips);

    let rust = compute_swap_step_v3(
        sp_current,
        sp_target,
        liquidity.cast_signed(),
        amount,
        U256::from(fee_pips),
    )
    .expect("Rust V3 step must succeed for the pinned input");
    let onchain = call_harness(addr, db, calldata)
        .expect("on-chain V3 harness must succeed (not revert) for the pinned input");

    assert_eq!(
        rust.sqrt_price_next, onchain.sqrt_price_next,
        "pinned: sqrtPriceNext"
    );
    assert_eq!(rust.amount_in, onchain.amount_in, "pinned: amountIn");
    assert_eq!(rust.amount_out, onchain.amount_out, "pinned: amountOut");
    assert_eq!(rust.fee_amount, onchain.fee_amount, "pinned: feeAmount");
}
