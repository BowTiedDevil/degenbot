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
//! ## Harness build (gated — `#[ignore]`d)
//!
//! Plain `cargo test --workspace` does not build the harness bytecode, so
//! these tests are `#[ignore]`d. `just test-tier3-curve` runs
//! `tier3-oracle/build-tier3-curve-swap-harness.sh` then runs them with
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

use degenbot_pools::curve_state::{CurvePoolState, RegisterCurvePoolParams};
use degenbot_pools::registry::PoolEntry;
use degenbot_pools::simulate_swap::simulate_swap;

/// Curve native precision scale (1e18) — mirrors the engine constant.
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

/// The engine's Curve standard-stableswap output via `simulate_swap`
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
fn onchain_get_dy(case: &CurveCase, coin_in: u64, coin_out: u64, amount_in: U256) -> Option<U256> {
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
            Some(U256::from_be_slice(&bytes[..32]))
        }
        ExecutionResult::Revert { .. } => None,
        other => panic!("getDy unexpected result: {other:?}"),
    }
}

/// A pinned standard-stableswap case (A=100, `a_precision`=100, equal 1e18
/// balances, ZERO fee) — the Tier-2 recorded-constant fixture (engine
/// cross-checks it against the pure-Python Vyper-port oracle at
/// `934112765606210873`); here the same engine value is asserted byte-exact
/// to the on-chain Solidity reference.
#[test]
#[ignore = "build the harness first: just test-tier3-curve"]
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
    let onchain = onchain_get_dy(&case, 0, 1, amount_in).expect("onchain computable");
    assert_eq!(engine, onchain, "engine vs on-chain byte-exact");
    assert_eq!(
        engine,
        U256::from(934_112_765_606_210_873u64),
        "matches the independent Tier-2 Python Vyper-port oracle"
    );
}

proptest! {
    /// Proptest the byte-exact Curve `get_dy` oracle over (balances × rates ×
    /// fee × A × amount × direction). Bounds keep the invariant arithmetic in
    /// the checked-U256 / Solidity-0.8 domain (no overflow — both sides then
    /// reject identically), and keep `xp[out] > y` so the `-1` is well-formed.
    /// Each assertion runs on a fresh pristine harness.
    #[test]
    #[ignore = "build the harness first: just test-tier3-curve"]
    fn curve_get_dy_matches_onchain_proptest(
        balance0 in 1_000_000_000_000_000_000u64..5_000_000_000_000_000_000u64,
        balance1 in 1_000_000_000_000_000_000u64..5_000_000_000_000_000_000u64,
        rate0 in 1_000_000_000_000_000_000u64..2_000_000_000_000_000_000u64,
        rate1 in 1_000_000_000_000_000_000u64..2_000_000_000_000_000_000u64,
        a_coefficient in 1u64..10_000u64,
        fee in 0u64..10_000_000_000u64, // fee ∈ [0, FEE_DENOMINATOR)
        amount_in_frac in 4u64..1_000u64,
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
        let amount_in = U256::from(amount_in_reserve) / U256::from(amount_in_frac);
        if amount_in.is_zero() {
            return Ok(()); // degenerate (getDy(0)=0 trivial).
        }

        let engine = engine_curve_out(&case, zfo, amount_in);
        let onchain = onchain_get_dy(&case, coin_in, coin_out, amount_in);

        match (engine, onchain) {
            (Some(e), Some(o)) => {
                prop_assert_eq!(e, o, "engine vs on-chain byte-exact");
            }
            (None, None) => {
                // Both rejected (NotComputable / revert) — acceptable parity,
                // but not interesting; keep the case for coverage.
            }
            (Some(e), None) => {
                panic!("engine produced {e} but on-chain rejected (revert)");
            }
            (None, Some(o)) => {
                panic!("engine rejected but on-chain produced {o}");
            }
        }
    }
}
