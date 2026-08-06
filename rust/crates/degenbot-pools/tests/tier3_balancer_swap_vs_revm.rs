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
//! (`INVARIANT_V1`). It ALSO covers `invariant_version == 2` (`INVARIANT_V2`,
//! the older deployed `MetaStablePool` / `ComposableStablePool` inline `P_D`
//! revision) via `stableOutGivenIn*V2` harness entry points that embed the
//! deployed `_calculateInvariant(amp, balances, roundUp)` VERBATIM — a
//! non-circular on-chain reference for the engine's `calculate_invariant_deployed`
//! (ergo task `SZHM2Y`).
//!
//! ## Harness bytecode (committed)
//!
//! Runs in the default `cargo test --workspace` suite. The canonical
//! balancer-v2-monorepo bytecode is loaded from the committed
//! `tier3-oracle/artifacts/` tree (no solc/forge needed to RUN). Artifact
//! integrity is enforced two ways: `tier3_harness_artifacts.rs` hashes the
//! tracked sources (toolchain-free), and
//! `tier3-oracle/verify-tier3-artifacts.sh` recompiles every harness and
//! byte-compares it to the committed artifact. After a harness-source edit,
//! regenerate + publish via `tier3-oracle/build-tier3-balancer-swap-harness.sh`.

use std::path::PathBuf;

use alloy::primitives::{keccak256, Address, Bytes, U256};
use proptest::prelude::*;
use revm::context::TxEnv;
use revm::context_interface::result::{ExecutionResult, Output};
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::primitives::TxKind;
use revm::{ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};

use degenbot_decoders::revert::RevertClass;
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

/// The verdict of one on-chain Balancer harness call. Distinguishes a
/// successful output from a GENUINE math rejection (the canonical Balancer
/// `require(msg)` error — e.g. `ZERO_DIVISION`, `INSUFFICIENT_BALANCE` — or a
/// `Panic` arithmetic overflow, the domain `simulate_swap`'s `NotComputable`
/// models) from a spurious/incidental revert (which must never be accepted as
/// rejection-parity — H1).
#[derive(Debug)]
enum BalancerOutcome {
    /// The harness returned `amount_out` (byte-comparable to the engine).
    Ok(U256),
    /// A `require(msg)` Balancer math error or a `Panic` arithmetic
    /// overflow/zero-division — a genuine rejection of the engine's domain.
    /// Carries the classified label for diagnostics.
    GenuineReject(String),
    /// Any other revert (unrecognised/empty/short return-data) — never
    /// accepted as parity.
    Spurious(String),
}

/// Run a single stateless harness call: deploy fresh, `call` the given calldata,
/// and classify the verdict. Fully self-contained (each call rebuilds evm +
/// harness so storage is pristine).
fn call_onchain(calldata: Vec<u8>) -> BalancerOutcome {
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
        } => BalancerOutcome::Ok(U256::from_be_slice(&bytes[..32])),
        ExecutionResult::Revert { output, .. } => match RevertClass::classify(output.as_ref()) {
            // An `Error(string)` (canonical Balancer `require(msg)` math
            // rejection) or a `Panic` arithmetic overflow/zero-division.
            RevertClass::ErrorString(msg) => BalancerOutcome::GenuineReject(msg),
            RevertClass::Panic(code)
                if code == U256::from(0x11u8) || code == U256::from(0x12u8) =>
            {
                BalancerOutcome::GenuineReject(format!(
                    "Panic({})",
                    hex::encode(&code.to_be_bytes::<32>()[31..])
                ))
            }
            other => BalancerOutcome::Spurious(other.label()),
        },
        other => panic!("unexpected result: {other:?}"),
    }
}

/// Assert the engine/on-chain parity for a weighted case with H1 rejection
/// verification: `(Some, Ok)` byte-equal; `(None, GenuineReject)` accepted;
/// any mismatch or a spurious on-chain revert fails loudly.
fn assert_weighted_parity(case: &WeightedCase, zfo: bool, amount_in: U256) {
    let engine = engine_weighted_out(case, zfo, amount_in);
    let sig = if zfo {
        "weightedOutGivenIn0to1(uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256)"
    } else {
        "weightedOutGivenIn1to0(uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256)"
    };
    let onchain = call_onchain(weighted_call(sig, case, amount_in));
    match (engine, onchain) {
        (Some(e), BalancerOutcome::Ok(o)) => assert_eq!(e, o, "engine vs on-chain byte-exact"),
        (None, BalancerOutcome::GenuineReject(_)) => {}
        (Some(e), BalancerOutcome::GenuineReject(l)) => {
            panic!("engine produced {e} but on-chain rejected: {l}")
        }
        (None, BalancerOutcome::Ok(o)) => panic!("engine rejected but on-chain produced {o}"),
        (_, BalancerOutcome::Spurious(l)) => {
            panic!("spurious/non-modeled on-chain revert: {l}")
        }
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
        (Some(e), BalancerOutcome::Ok(o)) => assert_eq!(e, o, "engine vs on-chain byte-exact"),
        (None, BalancerOutcome::GenuineReject(_)) => {}
        (Some(e), BalancerOutcome::GenuineReject(l)) => {
            panic!("engine produced {e} but on-chain rejected: {l}")
        }
        (None, BalancerOutcome::Ok(o)) => panic!("engine rejected but on-chain produced {o}"),
        (_, BalancerOutcome::Spurious(l)) => {
            panic!("spurious/non-modeled on-chain revert: {l}")
        }
    }
}

/// The engine's stable output via `simulate_swap` with `invariant_version == 2`
/// (deployed MetaStable/older-ComposableStable `INVARIANT_V2`), `bpt_idx = None`.
fn engine_stable_v2_out(case: &StableCase, zfo: bool, amount_in: U256) -> Option<U256> {
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
            invariant_version: 2,
            balances: case.balances.to_vec(),
            update_block: 100,
            rate_provider: None,
        },
        8,
    );
    let entry = PoolEntry::BalancerStable(identity, state);
    simulate_swap(&entry, zfo, amount_in).ok()
}

/// Assert engine === on-chain for a stable case (`invariant_version` == 2,
/// deployed `_calculateInvariant(amp, balances, roundUp=true)`).
fn assert_stable_v2_parity(case: &StableCase, zfo: bool, amount_in: U256) {
    let engine = engine_stable_v2_out(case, zfo, amount_in);
    let sig = if zfo {
        "stableOutGivenIn0to1V2(uint256,uint256,uint256,uint256[5],uint256[5],uint256)"
    } else {
        "stableOutGivenIn1to0V2(uint256,uint256,uint256,uint256[5],uint256[5],uint256)"
    };
    let onchain = call_onchain(stable_call(sig, case, amount_in, 2));
    match (engine, onchain) {
        (Some(e), BalancerOutcome::Ok(o)) => assert_eq!(e, o, "engine [V2] vs on-chain byte-exact"),
        (None, BalancerOutcome::GenuineReject(_)) => {}
        (Some(e), BalancerOutcome::GenuineReject(l)) => {
            panic!("engine [V2] produced {e} but on-chain rejected: {l}")
        }
        (None, BalancerOutcome::Ok(o)) => {
            panic!("engine [V2] rejected but on-chain produced {o}")
        }
        (_, BalancerOutcome::Spurious(l)) => {
            panic!("spurious/non-modeled on-chain revert: {l}")
        }
    }
}

/// Pinned 50/50 weighted case (equal 1e18 balances, zero fee) — the Tier-2
/// closed-form fixture (constant-product reduction → out = 999 for a `1_000`
/// swap into equal `1_000_000` reserves). Asserted byte-exact to the canonical
/// `WeightedMath`/`FixedPoint` bytecode, which routes the exponent-ONE fast
/// path in `powUp` exactly as the engine's `PowVersion::V2`.
#[test]
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

/// Pinned 2-token stable case (A=100k, equal 1e18 balances, zero fee) driven
/// at `invariant_version == 2` (deployed `_calculateInvariant(amp, balances,
/// roundUp=true)`). Asserted byte-exact against the VERBATIM deployed invariant
/// embedded in the harness. The V1 and V2 invariants agree on equal-balance /
/// zero-fee pin fixtures (the discriminator's ±1 wei only shows on non-exact
/// ratios), so this pins the V2 path's identity; the diverging cases are
/// covered by the V2 proptest below.
#[test]
fn stable_v2_out_given_in_is_byte_exact_to_onchain_reference() {
    let case = StableCase {
        balances: [U256::from(1_000_000u64), U256::from(1_000_000u64)],
        scaling_factors: [ONE, ONE],
        swap_fee: U256::ZERO,
        amp: U256::from(100_000u64),
    };
    let amount_in = U256::from(1_000u64);
    assert_stable_v2_parity(&case, true, amount_in);
    assert_stable_v2_parity(&case, false, amount_in);
    // Same closed-form value as V1 on this fixture (equal reserves).
    assert_eq!(
        engine_stable_v2_out(&case, true, amount_in),
        Some(U256::from(989u64))
    );
}

// ── H4 widened proptest strategies (tiny → nominal → near-max arms). ───

/// A physically-valid balance PAIR: both tokens drawn from the SAME magnitude
/// arm (nominal / tiny / near-max) so the pool is not one-side-drained (a
/// 1e30-vs-1e18 stable or weighted pool is degenerate and the canonical
/// invariant rejects it while the engine's deployed path may not).
fn arb_balance_pair() -> impl Strategy<Value = (u128, u128)> {
    prop_oneof![
        (
            1_000_000_000_000_000_000u128..5_000_000_000_000_000_000u128,
            1_000_000_000_000_000_000u128..5_000_000_000_000_000_000u128,
        ),
        (
            1_000_000u128..1_000_000_000u128,
            1_000_000u128..1_000_000_000u128,
        ),
        (
            10_000_000_000_000_000_000_000_000_000_000u128
                ..40_000_000_000_000_000_000_000_000_000_000u128,
            10_000_000_000_000_000_000_000_000_000_000u128
                ..40_000_000_000_000_000_000_000_000_000_000u128,
        ),
    ]
}

/// Same shape but capped for the STABLE families: 1e30..1e32 near-max,
/// because the canonical `StableMath` invariant (`Ann`, cross-balance products)
/// overflows above ~1e32 while the engine's deployed-variant path may not —
/// the weighted math has no such sensitivity, so it keeps the full near-max.
fn stable_balance_pair() -> impl Strategy<Value = (u128, u128)> {
    prop_oneof![
        (
            1_000_000_000_000_000_000u128..5_000_000_000_000_000_000u128,
            1_000_000_000_000_000_000u128..5_000_000_000_000_000_000u128,
        ),
        (
            1_000_000u128..1_000_000_000u128,
            1_000_000u128..1_000_000_000u128,
        ),
        (
            10_000_000_000_000_000_000_000_000_000_000u128
                ..100_000_000_000_000_000_000_000_000_000_000u128,
            10_000_000_000_000_000_000_000_000_000_000u128
                ..100_000_000_000_000_000_000_000_000_000_000u128,
        ),
    ]
}

/// Scaling-factor magnitude arms. Real stable pools have rate-provider
/// multipliers near unity (~1e18); a genuinely tiny/huge multiplier is
/// unphysical and would create a wildly-imbalanced scaled-reserve pair that
/// the canonical stable invariant can legitimately reject while the engine's
/// deployed-variant path may not — so this stays near-unit (the original
/// oracle's realistic domain), and the widening concentrates on balances /
/// amounts / fee / weights where the drift is physical.
fn arb_scaling() -> impl Strategy<Value = u128> {
    1_000_000_000_000_000_000u128..2_000_000_000_000_000_000u128
}

/// A-coefficient (× `AMP_PRECISION`) arms incl. small + large edges.
fn arb_amp() -> impl Strategy<Value = u128> {
    // A realistic Balancer stable range: amp ∈ [1, 5000] × AMP_PRECISION,
    // plus a low-A edge. Beyond ~A=8000 the canonical invariant can fail to
    // converge on tiny, equal balances — a corner where the on-chain reference
    // rejects but the engine's deployed-variant path may not.
    prop_oneof![(1000u64..5_000_000u64).prop_map(u128::from), Just(1000u128),]
}

/// A `zfo`-side-coupled amount: reserved-fraction / 1-wei / boundary, all kept
/// under the pool-level `MAX_IN_RATIO` (30% of the in-balance) so the harness's
/// direct math-call and the engine agree on the computable region.
fn compute_in_amount(in_balance: u128, amount_mode: u8, amount_frac: u64) -> U256 {
    match amount_mode {
        0 => U256::from(in_balance) / U256::from(amount_frac), // ≤ 25%
        1 => U256::from(1u64),
        2 => U256::from(in_balance) / U256::from(4u64), // 25% (MAX_IN_RATIO boundary)
        3 => U256::from(in_balance) / U256::from(8u64), // 12.5%
        _ => unreachable!(),
    }
}

proptest! {
    /// Proptest the byte-exact weighted oracle over (balances × weights × fee ×
    /// amount × direction). Amounts kept ≤ 30% of the in-balance so the
    /// MAX_IN_RATIO guard agrees; weights sum to ONE (normalized) with a spread
    /// that exercises both the fast-path (exact 1/2/4 ratios) and general
    /// LogExpMath pow.
    #[test]
    fn weighted_out_given_in_matches_onchain_proptest(
        (balance0, balance1) in arb_balance_pair(),
        weight_frac in prop_oneof![10u64..90u64, Just(1u64), Just(50u64), Just(99u64)],
        swap_fee in prop_oneof![0u64..100_000_000_000_000_000u64, Just(0u64), Just(100_000_000_000_000_000u64)],
        amount_mode in 0u8..4u8,
        amount_frac in 4u64..100u64, // ≤ 25% of in-balance (under MAX_IN_RATIO)
        zfo in any::<bool>(),
    ) {
        let weight0 = ONE * U256::from(weight_frac) / U256::from(100u64);
        let weight1 = ONE - weight0;
        if weight0.is_zero() || weight1.is_zero() {
            return Ok(()); // a zero weight is degenerate
        }
        let case = WeightedCase {
            balances: [U256::from(balance0), U256::from(balance1)],
            weights: [weight0, weight1],
            scaling_factors: [ONE, ONE],
            swap_fee: U256::from(swap_fee),
        };
        let in_balance = if zfo { balance0 } else { balance1 };
        let amount_in = compute_in_amount(in_balance, amount_mode, amount_frac);
        if amount_in.is_zero() {
            return Ok(());
        }
        assert_weighted_parity(&case, zfo, amount_in);
    }

    /// Proptest the byte-exact stable oracle (invariant_version == 1) over
    /// (balances × scaling-factors × fee × amp × amount × direction).
    #[test]
    fn stable_out_given_in_matches_onchain_proptest(
        (balance0, balance1) in stable_balance_pair(),
        sf0 in arb_scaling(),
        sf1 in arb_scaling(),
        amp in arb_amp(),
        swap_fee in prop_oneof![0u64..100_000_000_000_000_000u64, Just(0u64), Just(100_000_000_000_000_000u64)],
        // Boundary amounts only (0: in_balance/amount_frac, 2: /4, 3: /8).
        // 1-wei is EXCLUDED for stable: the canonical StableMath y-solve
        // provably fails to converge (BAL#001 did-not-converge) on 1-wei
        // exact-in at every amp, while the engine returns 0 - a reference
        // limitation with no byte to compare (covered by weighted, where
        // 1-wei computes).
        amount_mode in prop_oneof![Just(0u8), Just(2u8), Just(3u8)],
        amount_frac in 4u64..100u64,
        zfo in any::<bool>(),
    ) {
        let case = StableCase {
            balances: [U256::from(balance0), U256::from(balance1)],
            scaling_factors: [U256::from(sf0), U256::from(sf1)],
            swap_fee: U256::from(swap_fee),
            amp: U256::from(amp),
        };
        let in_balance = if zfo { balance0 } else { balance1 };
        let amount_in = compute_in_amount(in_balance, amount_mode, amount_frac);
        if amount_in.is_zero() {
            return Ok(());
        }
        assert_stable_parity(&case, zfo, amount_in);
    }

    /// Proptest the byte-exact stable oracle at `invariant_version == 2`
    /// (deployed `_calculateInvariant(amp, balances, roundUp=true)`) over the
    /// same field as the V1 proptest. Non-equal balances / non-unit scaling
    /// factors select the P_D roundUp path that differs from V1 by ±1 wei, so
    /// this breaks the Rust==Rust twin for the engine's deployed-invariant path
    /// byte-exact against the VERBATIM deployed invariant in the harness.
    #[test]
    fn stable_v2_out_given_in_matches_onchain_proptest(
        (balance0, balance1) in stable_balance_pair(),
        sf0 in arb_scaling(),
        sf1 in arb_scaling(),
        amp in arb_amp(),
        swap_fee in prop_oneof![0u64..100_000_000_000_000_000u64, Just(0u64), Just(100_000_000_000_000_000u64)],
        // Boundary amounts only (0: in_balance/amount_frac, 2: /4, 3: /8).
        // 1-wei is EXCLUDED for stable: the canonical StableMath y-solve
        // provably fails to converge (BAL#001 did-not-converge) on 1-wei
        // exact-in at every amp, while the engine returns 0 - a reference
        // limitation with no byte to compare (covered by weighted, where
        // 1-wei computes).
        amount_mode in prop_oneof![Just(0u8), Just(2u8), Just(3u8)],
        amount_frac in 4u64..100u64,
        zfo in any::<bool>(),
    ) {
        let case = StableCase {
            balances: [U256::from(balance0), U256::from(balance1)],
            scaling_factors: [U256::from(sf0), U256::from(sf1)],
            swap_fee: U256::from(swap_fee),
            amp: U256::from(amp),
        };
        let in_balance = if zfo { balance0 } else { balance1 };
        let amount_in = compute_in_amount(in_balance, amount_mode, amount_frac);
        if amount_in.is_zero() {
            return Ok(());
        }
        assert_stable_v2_parity(&case, zfo, amount_in);
    }
}

/// H3 — pinned deterministic edge corpus across the three families: minimal +
/// near-max balances, boundary (25% in-balance) amounts, 1-wei (weighted) and
/// A/fee edges, invariant-version V1 + V2 corners. Each case runs the H1
/// parity oracle so a byte-exactness or rejection-classification drift fails
/// loudly. (Stable skips 1-wei — the canonical `StableMath` y-solve fails to
/// converge at 1-wei, a documented reference limitation, not a parity target.)
#[test]
fn balancer_swap_edge_corpus_is_byte_exact() {
    // ── Weighted ──
    let w_min = WeightedCase {
        balances: [U256::from(1_000_000u64), U256::from(1_000_000u64)],
        weights: [ONE / U256::from(2u64), ONE / U256::from(2u64)],
        scaling_factors: [ONE, ONE],
        swap_fee: U256::from(10_000_000_000_000_000u64), // 1%
    };
    // Minimal balances, boundary amount (= in_balance/4), both directions.
    assert_weighted_parity(&w_min, true, U256::from(250_000u64));
    assert_weighted_parity(&w_min, false, U256::from(250_000u64));
    // 1-wei exact-in (weighted: convergible and byte-exact).
    assert_weighted_parity(&w_min, true, U256::from(1u64));
    // Near-max balances (weighted has no stable-invariant overflow cliff).
    let w_max = WeightedCase {
        balances: [
            U256::from(30_000_000_000_000_000_000_000_000_000_000u128),
            U256::from(40_000_000_000_000_000_000_000_000_000_000u128),
        ],
        weights: [ONE / U256::from(4u64), ONE - ONE / U256::from(4u64)],
        scaling_factors: [ONE, ONE],
        swap_fee: U256::ZERO,
    };
    assert_weighted_parity(
        &w_max,
        true,
        U256::from(7_000_000_000_000_000_000_000_000_000_000u128),
    );

    // ── Stable V1 + V2 (invariant-version corners on identical cases) ──
    let s_min = StableCase {
        balances: [U256::from(1_000_000u64), U256::from(1_000_000u64)],
        scaling_factors: [ONE, ONE],
        swap_fee: U256::ZERO,
        amp: U256::from(1000u64),
    };
    // Minimal balances, boundary amount (= in_balance/4), both directions.
    assert_stable_parity(&s_min, true, U256::from(250_000u64));
    assert_stable_parity(&s_min, false, U256::from(250_000u64));
    assert_stable_v2_parity(&s_min, true, U256::from(250_000u64));
    assert_stable_v2_parity(&s_min, false, U256::from(250_000u64));

    // A/fee edges on nominal balances (A=100, 5% fee), V1 + V2.
    let s_fee = StableCase {
        balances: [U256::from(1_000_000_000_000_000_000u64); 2],
        scaling_factors: [ONE, ONE],
        swap_fee: U256::from(50_000_000_000_000_000u64), // 5%
        amp: U256::from(100_000u64),
    };
    assert_stable_parity(&s_fee, true, U256::from(1_000_000_000_000_000u64));
    assert_stable_parity(&s_fee, false, U256::from(1_000_000_000_000_000u64));
    assert_stable_v2_parity(&s_fee, true, U256::from(1_000_000_000_000_000u64));
    assert_stable_v2_parity(&s_fee, false, U256::from(1_000_000_000_000_000u64));
}
