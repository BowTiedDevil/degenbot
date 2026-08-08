//! Tier-3 Curve standard-stableswap `get_dy` on-chain accuracy oracle (ergo
//! task `YXMNWB`, epic `UP5NH6` — family 2/3 of SH6HAK's Tier-3 cutover).
//! Deploys the `CurveSwapOracleHarness` (solc-0.8.26 compiled — a faithful
//! Solidity port of the STANDARD stableswap `get_dy`; Curve's canonical source
//! is Vyper, absent here, so the documented algorithm is the on-chain
//! reference — see the .sol header) into an offline revm `CacheDB`, seeds it
//! from a `CurvePoolState` (whole-slot-set: balances, rate multipliers, raw A,
//! `A_PRECISION`, fee), and drives the engine's `simulate_swap`
//! (`simulate_curve_stableswap_swap` standard path) against the on-chain
//! `getDy`, asserting the Rust output === the on-chain output byte-for-byte
//! across a pinned case + a proptest over balances × rates × fee × A × amount
//! × direction.
//!
//! ## Harness bytecode (committed)
//!
//! Runs in the default `cargo test --workspace` suite. The harness bytecode
//! is loaded from the committed `tier3-oracle/artifacts/` tree (no
//! solc/forge needed to RUN). Artifact integrity is enforced two ways:
//! `tier3_harness_artifacts.rs` hashes the tracked sources (toolchain-free),
//! and `tier3-oracle/verify-tier3-artifacts.sh` recompiles every harness and
//! byte-compares it to the committed artifact. After a harness-source edit,
//! regenerate + publish via `tier3-oracle/build-tier3-curve-swap-harness.sh`.

#![expect(clippy::expect_used, clippy::panic)]
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
use degenbot_pools::curve_state::{CurvePoolState, RegisterCurvePoolParams};
use degenbot_pools::registry::PoolEntry;
use degenbot_pools::simulate_swap::simulate_swap;

/// Curve native precision scale (`1e18`) — mirrors the engine constant.
const CURVE_PRECISION: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);

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

/// A 2-coin standard-stableswap case: balances, rate multipliers, raw A,
/// `A_PRECISION`, fee.
#[derive(Clone, Debug)]
struct CurveCase {
    balances: [U256; 2],
    rates: [U256; 2],
    a_coefficient: U256,
    a_precision: U256,
    fee: U256,
}

/// The verdict of one on-chain Curve `getDy` call. Distinguishes a successful
/// output from a GENUINE stableswap rejection (the overflow / Newton-convergence
/// failure the engine's `NotComputable` models) from a spurious/incidental
/// revert (which must never be accepted as rejection-parity — H1).
enum GetDyOutcome {
    /// `getDy` returned `amount_out` (byte-comparable to the engine).
    Ok(U256),
    /// The harness reverted with a genuine rejection class — a `Panic(0x11)`
    /// arithmetic overflow/underflow or a `Panic(0x12)` divide-by-zero (a
    /// zero-normalized balance makes the invariant degenerate), or the
    /// `Error("not converged")` Newton failure — the exact classes
    /// `simulate_swap`'s `NotComputable` models.
    GenuineReject,
    /// Any OTHER revert (an input-`require` hit or an unrecognised reason) —
    /// outside the modeled rejection domain and never accepted as parity.
    Spurious(String),
}

/// The engine's Curve standard-stableswap output via
/// `simulate_swap`
/// (`simulate_curve_stableswap_swap` standard path). `None` on
/// `NotComputable` (an overflow the harness would also reject as a revert).
fn engine_curve_out(case: &CurveCase, zfo: bool, amount_in: U256) -> Option<U256> {
    let (identity, state) = CurvePoolState::from_params(
        RegisterCurvePoolParams {
            address: Address::from([0x22u8; 20]),
            tokens: vec![Address::from([0xAAu8; 20]), Address::from([0xBBu8; 20])],
            a_coefficient: case.a_coefficient.to::<u128>(),
            a_precision: case.a_precision.to::<u128>(),
            fee: case.fee.to::<u64>(),
            admin_fee: 0,
            rate_multipliers: case.rates.to_vec(),
            balances: case.balances.to_vec(),
            update_block: 100,
            swap_style: 1, // STANDARD
            lending_rate_style: 0,
            d_variant: 1, // STANDARD
            y_variant: 1, // STANDARD
            yd_variant: 1,
            base_pool: None,
            initial_a_coefficient: None,
            future_a_coefficient: None,
            initial_a_coefficient_time: None,
            future_a_coefficient_time: None,
            create_timestamp: None,
            fee_gamma: None,
            mid_fee: None,
            offpeg_fee_multiplier: None,
            out_fee: None,
            gamma: None,
            lp_token: None,
            use_lending: vec![false; 2],
            precision_multipliers: vec![CURVE_PRECISION; 2],
            tokens_underlying: None,
            metapool_rate_style: 0,
            metapool_underlying_style: 0,
            data_provider: None,
        },
        8,
    );
    let entry = PoolEntry::Curve(identity, state);
    simulate_swap(&entry, zfo, amount_in).ok()
}

/// Encode `setup(uint256[4],uint256[4],uint256,uint256,uint256,uint8)` — seeds
/// the harness with a 2-coin case in the first two slots of each fixed array.
fn setup_call(case: &CurveCase) -> Vec<u8> {
    let mut d = selector("setup(uint256[4],uint256[4],uint256,uint256,uint256,uint8)").to_vec();
    for i in 0..4 {
        d.extend_from_slice(
            &case
                .balances
                .get(i)
                .copied()
                .unwrap_or(U256::ZERO)
                .to_be_bytes::<32>(),
        );
    }
    for i in 0..4 {
        d.extend_from_slice(
            &case
                .rates
                .get(i)
                .copied()
                .unwrap_or(U256::ZERO)
                .to_be_bytes::<32>(),
        );
    }
    d.extend_from_slice(&case.a_coefficient.to_be_bytes::<32>());
    d.extend_from_slice(&case.a_precision.to_be_bytes::<32>());
    d.extend_from_slice(&case.fee.to_be_bytes::<32>());
    d.extend_from_slice(&U256::from(2u8).to_be_bytes::<32>()); // nCoins = 2
    d
}

/// Encode `getDy(uint256,uint256,uint256)`.
fn get_dy_call(coin_in: u64, coin_out: u64, amount_in: U256) -> Vec<u8> {
    let mut d = selector("getDy(uint256,uint256,uint256)").to_vec();
    d.extend_from_slice(&U256::from(coin_in).to_be_bytes::<32>());
    d.extend_from_slice(&U256::from(coin_out).to_be_bytes::<32>());
    d.extend_from_slice(&amount_in.to_be_bytes::<32>());
    d
}

/// On-chain `getDy` output for a case on a fresh pristine harness. `None` if
/// the call reverts (an overflow / divergence the Rust `NotComputable` path
/// would also reject). Fully self-contained: each call rebuilds evm + harness
/// so every case runs from pristine storage.
fn onchain_get_dy(case: &CurveCase, coin_in: u64, coin_out: u64, amount_in: U256) -> GetDyOutcome {
    let db = CacheDB::new(EmptyDB::default());
    let mut evm = revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet();
    evm.ctx.cfg.disable_nonce_check = true;

    let init_code = load_creation_bytecode("CurveSwapOracleHarness.sol", "CurveSwapOracleHarness");
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

    let setup_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(6_000_000)
                .data(Bytes::from(setup_call(case)))
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

    let get_dy_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(6_000_000)
                .data(Bytes::from(get_dy_call(coin_in, coin_out, amount_in)))
                .build()
                .expect("getDy tx"),
        )
        .expect("getDy transact");

    match &get_dy_res.result {
        ExecutionResult::Success {
            output: Output::Call(bytes),
            ..
        } => {
            // The return value is a single uint256 (32 bytes).
            GetDyOutcome::Ok(U256::from_be_slice(&bytes[..32]))
        }
        ExecutionResult::Revert { output, .. } => {
            // H1: decode the revert — only the GENUINE stableswap rejection
            // (a Solidity-0.8 `Panic(0x11)` arithmetic overflow/underflow, or
            // the Newton `not converged` failure) matches the engine's
            // `NotComputable`. Any other revert (an input-`require` hit or an
            // unrecognised reason) is spurious and must NOT be accepted as
            // rejection-parity.
            match RevertClass::classify(output.as_ref()) {
                // 0x11 = arithmetic overflow/underflow, 0x12 = divide-by-zero
                // (degenerate zero-normalized balance) — both are genuine
                // stableswap failures the engine's `NotComputable` models.
                RevertClass::Panic(code)
                    if code == U256::from(0x11u8) || code == U256::from(0x12u8) =>
                {
                    GetDyOutcome::GenuineReject
                }
                RevertClass::ErrorString(msg) if msg == "not converged" => {
                    GetDyOutcome::GenuineReject
                }
                other => GetDyOutcome::Spurious(other.label()),
            }
        }
        other => panic!("getDy unexpected result: {other:?}"),
    }
}

/// A pinned standard-stableswap case (A=100, `a_precision`=100, equal 1e18
/// balances, ZERO fee) — the Tier-2 recorded-constant fixture (engine
/// cross-checks it against the pure-Python Vyper-port oracle at
/// `934112765606210873`); here the same engine value is asserted byte-exact
/// to the on-chain Solidity reference.
#[test]
fn curve_get_dy_output_is_byte_exact_to_onchain_reference() {
    let case = CurveCase {
        balances: [CURVE_PRECISION, CURVE_PRECISION],
        rates: [CURVE_PRECISION, CURVE_PRECISION],
        a_coefficient: U256::from(100u64),
        a_precision: U256::from(100u64),
        fee: U256::ZERO, // match the zero-fee Tier-2 recorded constant
    };
    let amount_in = CURVE_PRECISION;
    let zfo = true;

    let engine = engine_curve_out(&case, zfo, amount_in).expect("engine computable");
    let onchain = match onchain_get_dy(&case, 0, 1, amount_in) {
        GetDyOutcome::Ok(v) => v,
        GetDyOutcome::GenuineReject => panic!("pinned case must be computable on-chain"),
        GetDyOutcome::Spurious(l) => {
            panic!("pinned case spurious on-chain revert: {l}")
        }
    };
    assert_eq!(engine, onchain, "engine vs on-chain byte-exact");
    assert_eq!(
        engine,
        U256::from(934_112_765_606_210_873u64),
        "matches the independent Tier-2 Python Vyper-port oracle"
    );
}

/// A minimal `CurveCase` builder for the pinned corpus.
fn make_case(
    b0: u128,
    b1: u128,
    r0: u128,
    r1: u128,
    a: u128,
    a_prec: u128,
    fee: u128,
) -> CurveCase {
    CurveCase {
        balances: [U256::from(b0), U256::from(b1)],
        rates: [U256::from(r0), U256::from(r1)],
        a_coefficient: U256::from(a),
        a_precision: U256::from(a_prec),
        fee: U256::from(fee),
    }
}

/// Run one case through both the engine and the on-chain harness and assert
/// H3/H1 parity: `(Some, Ok)` byte-equal; `(None, GenuineReject)` accepted;
/// any mismatch or a spurious on-chain revert fails loudly.
fn assert_curve_parity(case: &CurveCase, zfo: bool, amount_in: U256) {
    let (coin_in, coin_out) = if zfo { (0u64, 1u64) } else { (1u64, 0u64) };
    let engine = engine_curve_out(case, zfo, amount_in);
    let onchain = onchain_get_dy(case, coin_in, coin_out, amount_in);
    match (engine, onchain) {
        (Some(e), GetDyOutcome::Ok(o)) => {
            assert_eq!(e, o, "engine vs on-chain byte-exact");
        }
        (None, GetDyOutcome::GenuineReject) => {
            // Both rejected with the genuine overflow / non-convergence.
        }
        (Some(e), GetDyOutcome::GenuineReject) => {
            panic!("engine produced {e} but on-chain overflow-rejected")
        }
        (None, GetDyOutcome::Ok(o)) => {
            panic!("engine rejected but on-chain produced {o}")
        }
        (_, GetDyOutcome::Spurious(l)) => {
            panic!("spurious/non-modeled on-chain revert: {l}")
        }
    }
}

/// H3 — pinned deterministic edge corpus: minimal + near-max balances, 1-wei
/// and boundary amounts, A + fee edges, both directions. Runs the H1 parity
/// oracle so a byte-exactness or rejection-classification drift fails loudly.
#[test]
fn curve_get_dy_edge_corpus_is_byte_exact() {
    let cases: &[(CurveCase, bool, U256)] = &[
        // Minimal balances (1e6) with a 1-wei amount.
        (
            make_case(
                1_000_000,
                1_000_000,
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
                100,
                100,
                0,
            ),
            true,
            U256::from(1u64),
        ),
        (
            make_case(
                1_000_000,
                1_000_000,
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
                100,
                100,
                0,
            ),
            false,
            U256::from(1u64),
        ),
        // Tiny balances with a boundary amount (half the reserve).
        (
            make_case(
                1_000_000,
                1_000_000,
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
                100,
                100,
                0,
            ),
            true,
            U256::from(500_000u64),
        ),
        // A/fee edges on a nominal balance.
        (
            make_case(
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
                1,
                100,
                0,
            ),
            true,
            U256::from(1_000_000_000_000_000u64), // 1e15
        ),
        (
            make_case(
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
                100_000,
                100,
                9_999_999_999,
            ),
            false,
            U256::from(1_000_000_000_000_000u64),
        ),
        // Near-max balances, proportional amount (16-bit-ish magnitudes).
        (
            make_case(
                10_000_000_000_000_000_000_000_000_000_000_000u128,
                12_000_000_000_000_000_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
                100,
                100,
                0,
            ),
            true,
            U256::from(1_000_000_000_000_000_000_000_000_000_000_000u128), // ~balance0/10
        ),
        (
            make_case(
                10_000_000_000_000_000_000_000_000_000_000_000u128,
                12_000_000_000_000_000_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
                100,
                100,
                0,
            ),
            false,
            U256::from(1_000_000_000_000_000_000_000_000_000_000_000u128),
        ),
    ];

    for &(ref case, zfo, amount_in) in cases {
        assert_curve_parity(case, zfo, amount_in);
    }
}

/// Widen a balance/rate magnitude to cover tiny, nominal, and near-max arms
/// (so the invariant arithmetic explores both the well-formed byte-exact
/// region and the overflow region that both sides must reject identically).
fn arb_balance() -> impl Strategy<Value = u128> {
    prop_oneof![
        1_000_000_000_000_000_000u128..5_000_000_000_000_000_000u128,
        1_000_000u128..1_000_000_000u128,
        10_000_000_000_000_000_000_000_000_000_000u128
            ..40_000_000_000_000_000_000_000_000_000_000u128,
    ]
}

/// Rate multiplier magnitude arms (tiny / 1_000_000_000_000_000_000u128-scale / 1e30-scale).
fn arb_rate() -> impl Strategy<Value = u128> {
    prop_oneof![
        1_000_000_000_000_000_000u128..2_000_000_000_000_000_000u128,
        1_000_000u128..1_000_000_000u128,
        1_000_000_000_000_000_000_000_000_000_000u128
            ..5_000_000_000_000_000_000_000_000_000_000u128,
    ]
}

proptest! {
    /// Proptest the byte-exact Curve `get_dy` oracle over a widened domain:
    /// balances × rates × A × fee × amount × direction, with balances/rates
    /// spanning tiny→nominal→near-max, amounts covering 1-wei and boundary
    /// fractions (well-formed `xp[out] > y`), and A/fee at their edges. H1
    /// rejection-parity: both-reject is only accepted when the on-chain revert
    /// decodes as the GENUINE overflow/non-convergence, never a spurious one.
    /// Each assertion runs on a fresh pristine harness.
    #[test]
    fn curve_get_dy_matches_onchain_proptest(
        balance0 in arb_balance(),
        balance1 in arb_balance(),
        rate0 in arb_rate(),
        rate1 in arb_rate(),
        a_coefficient in prop_oneof![1u64..10_000u64, Just(1u64), Just(100_000u64)],
        fee in prop_oneof![0u64..10_000_000_000u64, Just(0u64), Just(9_999_999_999u64)],
        amount_mode in 0u8..4u8, // 0: reserve/frac, 1: 1 wei, 2: reserve/16, 3: reserve/4
        amount_frac in 4u64..1_000u64,
        zfo in any::<bool>(),
    ) {
        let case = CurveCase {
            balances: [U256::from(balance0), U256::from(balance1)],
            rates: [U256::from(rate0), U256::from(rate1)],
            a_coefficient: U256::from(a_coefficient),
            a_precision: U256::from(100u64),
            fee: U256::from(fee),
        };
        let (coin_in, coin_out) = if zfo { (0u64, 1u64) } else { (1u64, 0u64) };
        let amount_in_reserve = if zfo { balance0 } else { balance1 };
        let amount_in = match amount_mode {
            0 => U256::from(amount_in_reserve) / U256::from(amount_frac),
            1 => U256::from(1u64),
            2 => U256::from(amount_in_reserve) / U256::from(16u64),
            3 => U256::from(amount_in_reserve) / U256::from(4u64),
            _ => unreachable!(),
        };
        if amount_in.is_zero() {
            return Ok(()); // degenerate (getDy(0)=0 trivial).
        }

        let engine = engine_curve_out(&case, zfo, amount_in);
        let onchain = onchain_get_dy(&case, coin_in, coin_out, amount_in);

        match (engine, onchain) {
            (Some(e), GetDyOutcome::Ok(o)) => {
                prop_assert_eq!(e, o, "engine vs on-chain byte-exact");
            }
            (None, GetDyOutcome::GenuineReject) => {
                // Both rejected with the genuine overflow / non-convergence.
            }
            (Some(e), GetDyOutcome::GenuineReject) => {
                panic!("engine produced {e} but on-chain overflow-rejected");
            }
            (None, GetDyOutcome::Ok(o)) => {
                panic!("engine rejected but on-chain produced {o}");
            }
            (_, GetDyOutcome::Spurious(l)) => {
                panic!("spurious/non-modeled on-chain revert: {l}");
            }
        }
    }
}
