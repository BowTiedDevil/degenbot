//! Tier-3 Balancer weighted/stable swap on-chain accuracy oracle (ergo task
//! `EZLECC`, epic `UP5NH6` — family 3/3 of SH6HAK's Tier-3 cutover). Deploys
//! the `BalancerSwapOracleHarness` (solc-0.7.6 compiled) — a thin glass box
//! over the CANONICAL balancer-v2-monorepo math cores (`FixedPoint`,
//! `LogExpMath`, `WeightedMath`, `StableMath`, vendored at pinned commit
//! f8b6f44) into an offline revm `CacheDB`. The harness reproduces the exact
//! fee/scaling/direction sequence of the Rust `simulate_balancer_weighted_swap`
//! / `simulate_balancer_stable_swap` (`invariant_version` == 1), so the engine
//! output === the canonical-FixedPoint bytecode output byte-for-byte across a
//! pinned case + proptests over balances × weights × fees × amounts ×
//! direction.
//!
//! The stable path here exercises the canonical `StableMath` V1 invariant
//! (`INVARIANT_V1`). The engine's `invariant_version == 2` (`INVARIANT_V2`,
//! the older deployed MetaStable/ComposableStable inline `P_D` revision) is a
//! follow-on slice tracked in the task body — it is not part of the current
//! canonical `StableMath` library.
//!
//! ## Harness build (gated — `#[ignore]`d)
//!
//! Plain `cargo test --workspace` does not build the harness bytecode, so
//! these tests are `#[ignore]`d. `just test-tier3-balancer` runs
//! `tier3-oracle/build-tier3-balancer-swap-harness.sh` then runs them with
//! `--include-ignored`.

use std::path::PathBuf;

use alloy::primitives::{keccak256, Address, Bytes, U256};
use proptest::prelude::*;
use revm::context::TxEnv;
use revm::context_interface::result::{ExecutionResult, Output};
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::primitives::TxKind;
use revm::{ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};

use degenbot_pools::balancer_stable_state::{
    BalancerStablePoolState, RegisterBalancerStablePoolParams,
};
use degenbot_pools::balancer_weighted_state::{
    BalancerWeightedPoolState, RegisterBalancerWeightedPoolParams,
};
use degenbot_pools::registry::PoolEntry;
use degenbot_pools::simulate_swap::simulate_swap;

/// 1.0 in 18-decimal fixed point.
const ONE: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);
/// The stable harness fixed-array width (`StableMath` `_MAX_STABLE_TOKENS`).
const MAX_STABLE_TOKENS: usize = 5;

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
        .join("../../../tier3-oracle/out")
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

/// A 2-token weighted case, mirroring `BalancerWeightedPoolState` (`pow_version`=2).
#[derive(Clone, Debug)]
struct WeightedCase {
    balances: [U256; 2],
    weights: [U256; 2],
    scaling_factors: [U256; 2],
    swap_fee: U256,
}

/// A 2-token stable case mirroring `BalancerStablePoolState` with
/// `invariant_version == 1` (canonical `INVARIANT_V1`) and `bpt_idx = None`.
#[derive(Clone, Debug)]
struct StableCase {
    balances: [U256; 2],
    scaling_factors: [U256; 2],
    swap_fee: U256,
    amp: U256,
}

/// The engine's weighted output via `simulate_swap`. `None` on `NotComputable`
/// (an over-MAX_IN_RATIO / overflow the canonical harness would also revert).
fn engine_weighted_out(case: &WeightedCase, zfo: bool, amount_in: U256) -> Option<U256> {
    let (identity, state) = BalancerWeightedPoolState::from_params(
        RegisterBalancerWeightedPoolParams {
            address: Address::from([0x22u8; 20]),
            vault: Address::from([0x33u8; 20]),
            pool_id: [0u8; 32],
            tokens: vec![Address::from([0xAAu8; 20]), Address::from([0xBBu8; 20])],
            weights: case.weights.to_vec(),
            scaling_factors: case.scaling_factors.to_vec(),
            swap_fee: case.swap_fee.to::<u128>(),
            pow_version: 2,
            balances: case.balances.to_vec(),
            update_block: 100,
        },
        8,
    );
    let entry = PoolEntry::BalancerWeighted(identity, state);
    simulate_swap(&entry, zfo, amount_in).ok()
}

/// The engine's stable output via `simulate_swap` with `invariant_version == 1`
/// (canonical `INVARIANT_V1`), `bpt_idx = None`.
fn engine_stable_out(case: &StableCase, zfo: bool, amount_in: U256) -> Option<U256> {
    let (identity, state) = BalancerStablePoolState::from_params(
        RegisterBalancerStablePoolParams {
            address: Address::from([0x44u8; 20]),
            vault: Address::from([0x55u8; 20]),
            pool_id: [0x44u8; 32],
            tokens: vec![Address::from([0xAAu8; 20]), Address::from([0xBBu8; 20])],
            amp: case.amp.to::<u128>(),
            scaling_factors: case.scaling_factors.to_vec(),
            swap_fee: case.swap_fee.to::<u128>(),
            bpt_idx: None,
            invariant_version: 1,
            balances: case.balances.to_vec(),
            update_block: 100,
            rate_provider: None,
        },
        8,
    );
    let entry = PoolEntry::BalancerStable(identity, state);
    simulate_swap(&entry, zfo, amount_in).ok()
}

/// Encode a `weightedOutGivenIn*` call.
fn weighted_call(sig: &str, case: &WeightedCase, amount_in: U256) -> Vec<u8> {
    let mut d = selector(sig).to_vec();
    for v in [
        amount_in,
        case.swap_fee,
        case.balances[0],
        case.balances[1],
        case.weights[0],
        case.weights[1],
        case.scaling_factors[0],
        case.scaling_factors[1],
    ] {
        d.extend_from_slice(&v.to_be_bytes::<32>());
    }
    d
}

/// Encode a `stableOutGivenIn*` call (fixed `uint256[5]` arrays).
fn stable_call(sig: &str, case: &StableCase, amount_in: U256, token_count: u64) -> Vec<u8> {
    let mut d = selector(sig).to_vec();
    d.extend_from_slice(&amount_in.to_be_bytes::<32>());
    d.extend_from_slice(&case.swap_fee.to_be_bytes::<32>());
    d.extend_from_slice(&case.amp.to_be_bytes::<32>());
    for i in 0..MAX_STABLE_TOKENS {
        d.extend_from_slice(
            &case
                .balances
                .get(i)
                .copied()
                .unwrap_or(U256::ZERO)
                .to_be_bytes::<32>(),
        );
    }
    for i in 0..MAX_STABLE_TOKENS {
        d.extend_from_slice(
            &case
                .scaling_factors
                .get(i)
                .copied()
                .unwrap_or(U256::ZERO)
                .to_be_bytes::<32>(),
        );
    }
    d.extend_from_slice(&U256::from(token_count).to_be_bytes::<32>());
    d
}

/// Run a single stateless harness call: deploy fresh, `call` the given calldata,
/// return the single-`uint256` output or `None` on revert. Fully self-contained
/// (each call rebuilds evm + harness so storage is pristine).
fn call_onchain(calldata: Vec<u8>) -> Option<U256> {
    let db = CacheDB::new(EmptyDB::default());
    let mut evm = revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet();
    evm.ctx.cfg.disable_nonce_check = true;

    let init_code =
        load_creation_bytecode("BalancerSwapOracleHarness.sol", "BalancerSwapOracleHarness");
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

    let res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(6_000_000)
                .data(Bytes::from(calldata))
                .build()
                .expect("call tx"),
        )
        .expect("call transact");

    match &res.result {
        ExecutionResult::Success {
            output: Output::Call(bytes),
            ..
        } => Some(U256::from_be_slice(&bytes[..32])),
        ExecutionResult::Revert { .. } => None,
        other => panic!("unexpected result: {other:?}"),
    }
}

/// Assert engine === on-chain for a weighted case. Both must agree on computable
/// vs rejected, and equal when computable.
fn assert_weighted_parity(case: &WeightedCase, zfo: bool, amount_in: U256) {
    let engine = engine_weighted_out(case, zfo, amount_in);
    let sig = if zfo {
        "weightedOutGivenIn0to1(uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256)"
    } else {
        "weightedOutGivenIn1to0(uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256)"
    };
    let onchain = call_onchain(weighted_call(sig, case, amount_in));
    match (engine, onchain) {
        (Some(e), Some(o)) => assert_eq!(e, o, "engine vs on-chain byte-exact"),
        (None, None) => {}
        (Some(e), None) => panic!("engine produced {e} but on-chain reverted"),
        (None, Some(o)) => panic!("engine rejected but on-chain produced {o}"),
    }
}

/// Assert engine === on-chain for a stable case (`invariant_version` == 1).
fn assert_stable_parity(case: &StableCase, zfo: bool, amount_in: U256) {
    let engine = engine_stable_out(case, zfo, amount_in);
    let sig = if zfo {
        "stableOutGivenIn0to1(uint256,uint256,uint256,uint256[5],uint256[5],uint256)"
    } else {
        "stableOutGivenIn1to0(uint256,uint256,uint256,uint256[5],uint256[5],uint256)"
    };
    let onchain = call_onchain(stable_call(sig, case, amount_in, 2));
    match (engine, onchain) {
        (Some(e), Some(o)) => assert_eq!(e, o, "engine vs on-chain byte-exact"),
        (None, None) => {}
        (Some(e), None) => panic!("engine produced {e} but on-chain reverted"),
        (None, Some(o)) => panic!("engine rejected but on-chain produced {o}"),
    }
}

/// Pinned 50/50 weighted case (equal 1e18 balances, zero fee) — the Tier-2
/// closed-form fixture (constant-product reduction → out = 999 for a `1_000`
/// swap into equal `1_000_000` reserves). Asserted byte-exact to the canonical
/// `WeightedMath`/`FixedPoint` bytecode, which routes the exponent-ONE fast
/// path in `powUp` exactly as the engine's `PowVersion::V2`.
#[test]
#[ignore = "build the harness first: just test-tier3-balancer"]
fn weighted_out_given_in_is_byte_exact_to_onchain_reference() {
    let case = WeightedCase {
        balances: [U256::from(1_000_000u64), U256::from(1_000_000u64)],
        weights: [ONE / U256::from(2u64); 2], // 50/50
        scaling_factors: [ONE, ONE],
        swap_fee: U256::ZERO,
    };
    let amount_in = U256::from(1_000u64);
    // Engine === on-chain byte-exact.
    assert_weighted_parity(&case, true, amount_in);
    // And matches the Tier-2 closed-form constant-product reduction.
    assert_eq!(
        engine_weighted_out(&case, true, amount_in),
        Some(U256::from(999u64))
    );
    // Direction symmetry on equal weights.
    assert_weighted_parity(&case, false, amount_in);
}

/// Pinned 2-token stable case (A=100k, equal 1e18 balances, zero fee) —
/// `invariant_version == 1` (canonical `INVARIANT_V1`). The Tier-2 companion
/// oracle fixture for `1_000` into equal `1_000_000` reserves yields 989 here too
/// for equal balances (the swapped amount is small relative to reserves).
#[test]
#[ignore = "build the harness first: just test-tier3-balancer"]
fn stable_out_given_in_is_byte_exact_to_onchain_reference() {
    let case = StableCase {
        balances: [U256::from(1_000_000u64), U256::from(1_000_000u64)],
        scaling_factors: [ONE, ONE],
        swap_fee: U256::ZERO,
        amp: U256::from(100_000u64), // 100 * 1000 (AMP_PRECISION)
    };
    let amount_in = U256::from(1_000u64);
    assert_stable_parity(&case, true, amount_in);
    assert_stable_parity(&case, false, amount_in);
    // Cross-check against the Tier-2 companion recorded constant (equal
    // reserves → symmetric, invariant_version-independent for this fixture).
    assert_eq!(
        engine_stable_out(&case, true, amount_in),
        Some(U256::from(989u64))
    );
}

proptest! {
    /// Proptest the byte-exact weighted oracle over (balances × weights × fee ×
    /// amount × direction). Amounts kept ≤ 30% of the in-balance so the
    /// MAX_IN_RATIO guard agrees; weights sum to ONE (normalized) with a spread
    /// that exercises both the fast-path (exact 1/2/4 ratios) and general
    /// LogExpMath pow.
    #[test]
    #[ignore = "build the harness first: just test-tier3-balancer"]
    fn weighted_out_given_in_matches_onchain_proptest(
        balance0 in 1_000_000_000_000_000_000u64..5_000_000_000_000_000_000u64,
        balance1 in 1_000_000_000_000_000_000u64..5_000_000_000_000_000_000u64,
        weight_frac in 10u64..90u64, // weight0 = frac/100, weight1 = 1 - frac/100
        swap_fee in 0u64..1_000_000_000_000_000_000u64, // fee ∈ [0, 0.1e18=10%]
        amount_in_frac in 5u64..100u64, // amount_in ≤ balance_in/5 (≤ 30%)
        zfo in any::<bool>(),
    ) {
        let weight0 = ONE * U256::from(weight_frac) / U256::from(100u64);
        let weight1 = ONE - weight0;
        let case = WeightedCase {
            balances: [U256::from(balance0), U256::from(balance1)],
            weights: [weight0, weight1],
            scaling_factors: [ONE, ONE],
            swap_fee: U256::from(swap_fee),
        };
        let in_balance = if zfo { balance0 } else { balance1 };
        let amount_in = U256::from(in_balance) / U256::from(amount_in_frac);
        if amount_in.is_zero() {
            return Ok(());
        }
        assert_weighted_parity(&case, zfo, amount_in);
    }

    /// Proptest the byte-exact stable oracle (invariant_version == 1) over
    /// (balances × scaling-factors × fee × amp × amount × direction).
    #[test]
    #[ignore = "build the harness first: just test-tier3-balancer"]
    fn stable_out_given_in_matches_onchain_proptest(
        balance0 in 1_000_000_000_000_000_000u64..5_000_000_000_000_000_000u64,
        balance1 in 1_000_000_000_000_000_000u64..5_000_000_000_000_000_000u64,
        sf0 in 1_000_000_000_000_000_000u64..2_000_000_000_000_000_000u64,
        sf1 in 1_000_000_000_000_000_000u64..2_000_000_000_000_000_000u64,
        amp in 1000u64..5_000_000u64, // 1..5000 scaled by AMP_PRECISION
        swap_fee in 0u64..1_000_000_000_000_000_000u64,
        amount_in_frac in 5u64..100u64,
        zfo in any::<bool>(),
    ) {
        let case = StableCase {
            balances: [U256::from(balance0), U256::from(balance1)],
            scaling_factors: [U256::from(sf0), U256::from(sf1)],
            swap_fee: U256::from(swap_fee),
            amp: U256::from(amp),
        };
        let in_balance = if zfo { balance0 } else { balance1 };
        let amount_in = U256::from(in_balance) / U256::from(amount_in_frac);
        if amount_in.is_zero() {
            return Ok(());
        }
        assert_stable_parity(&case, zfo, amount_in);
    }
}
