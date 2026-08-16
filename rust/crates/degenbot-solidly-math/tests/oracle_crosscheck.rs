//! Freeze-parity cross-check against the Python
//! `degenbot.calculations.solidly_stable` + `degenbot.calculations.camelot`
//! oracle.
//!
//! Same shape as `degenbot-curve-math/tests/oracle_crosscheck.rs` +
//! `degenbot-balancer-math/tests/oracle_crosscheck.rs`: load a JSON corpus
//! generated with a fixed RNG seed from the Python oracle
//! (`tests/oracle_snapshot.txt`), feed every `(args, result)` pair through the
//! Rust port, and assert byte-identical output.
//!
//! The snapshot is a frozen, version-checked artifact — re-running the Python
//! to regenerate the corpus is a separate step (the snapshot is the parity
//! ground truth; if the Python oracle changes shape, the snapshot must be
//! regenerated deliberately, not silently).

// The corpus loader + parity assertions below use `unwrap`/`expect`/`panic`
// freely — idiomatic for a test harness that replays a frozen snapshot and
// asserts byte-identical output.
#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use alloy::primitives::U256;
use degenbot_solidly_math::{
    calc_d, calc_exact_in_stable_camelot, calc_exact_in_stable_solidly, calc_exact_in_volatile,
    calc_exact_out_stable_solidly, calc_f, calc_k, f_camelot, get_y_camelot, get_y_solidly,
    k_camelot,
};
use serde_json::Value;

/// Load the snapshot from `tests/oracle_snapshot.txt`. Panic reads the
/// snapshot from the test-binary's `CARGO_MANIFEST_DIR/tests/`.
fn load_snapshot() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("oracle_snapshot.txt");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("read snapshot {}", path.display()));
    serde_json::from_str(&body).unwrap_or_else(|_| panic!("parse snapshot as JSON"))
}

/// Decode a JSON array of arbitrary-precision integers into `Vec<U256>`.
/// Snapshot entries store them as `serde_json::Number`s from `int` — which
/// `serde_json` represents as either `u64`/`i64` or a string fallback when too
/// large. Use the Python-style `int.to_bytes(32, 'big')` convention.
fn args_as_u256(arg: &Value) -> U256 {
    if let Some(v) = arg.as_u64() {
        return U256::from(v);
    }
    if let Some(v) = arg.as_i64() {
        // Negative numbers don't appear in the snapshot (Solidity uint256
        // only); serde_json may surface `i64` for the JSON token `-1` style.
        return U256::try_from(v).expect("snapshot token_in is non-negative");
    }
    if let Some(s) = arg.as_str() {
        // `serde_json` falls back to string for ints that don't fit u64. The
        // snapshot serializes EVERY integer as a string to avoid the `f64`
        // fallback when `serde_json` sees a non-u64-fitting Number literal.
        return U256::from_str_radix(s, 10)
            .unwrap_or_else(|_| panic!("parse U256 from snapshot string '{s}'"));
    }
    // Negative numbers don't appear in the snapshot (Solidity uint256 only);
    // but `serde_json` may surface large positive ints in its `i64` negative
    // representation. Convert via the `to_string` form.
    let s = arg.to_string();
    U256::from_str_radix(&s, 10).unwrap_or_else(|_| panic!("parse U256 from snapshot token '{s}'"))
}

/// Parse a snapshot `token_in` (small non-negative integer, stored as a
/// JSON string in the snapshot). The snapshot serializes every numeric arg
/// as a decimal-string to avoid `serde_json`'s `f64` fallback for >u64 ints;
/// `token_in` is always 0 or 1, so it round-trips cleanly via string.
fn args_as_u8(arg: &Value) -> u8 {
    if let Some(v) = arg.as_u64() {
        return v.try_into().unwrap();
    }
    if let Some(s) = arg.as_str() {
        return s
            .parse::<u8>()
            .unwrap_or_else(|_| panic!("parse u8 token_in from snapshot string '{s}'"));
    }
    arg.to_string().parse::<u8>().unwrap()
}

/// Decode a snapshot result into an expected `U256`. Snapshot writes
/// `"result": int` (arbitrary precision), so the same conversion as `args` is
/// used (string fallback for >u64).
fn expected_u256(v: &Value) -> U256 {
    args_as_u256(v)
}

#[test]
fn cross_check_calc_d() {
    let snap = load_snapshot();
    for case in snap["calc_d"].as_array().unwrap() {
        let args = case["args"].as_array().unwrap();
        let x0 = args_as_u256(&args[0]);
        let y = args_as_u256(&args[1]);
        let got = calc_d(x0, y);
        let want = expected_u256(&case["result"]);
        assert_eq!(got, want, "calc_d({x0}, {y})");
    }
}

#[test]
fn cross_check_calc_k() {
    let snap = load_snapshot();
    for case in snap["calc_k"].as_array().unwrap() {
        let args = case["args"].as_array().unwrap();
        let b0 = args_as_u256(&args[0]);
        let b1 = args_as_u256(&args[1]);
        let d0 = args_as_u256(&args[2]);
        let d1 = args_as_u256(&args[3]);
        let got = calc_k(b0, b1, d0, d1).unwrap();
        let want = expected_u256(&case["result"]);
        assert_eq!(got, want, "calc_k({b0}, {b1}, {d0}, {d1})");
    }
}

#[test]
fn cross_check_calc_f() {
    let snap = load_snapshot();
    for case in snap["calc_f"].as_array().unwrap() {
        let args = case["args"].as_array().unwrap();
        let x0 = args_as_u256(&args[0]);
        let y = args_as_u256(&args[1]);
        let got = calc_f(x0, y);
        let want = expected_u256(&case["result"]);
        assert_eq!(got, want, "calc_f({x0}, {y})");
    }
}

#[test]
fn cross_check_f_camelot() {
    let snap = load_snapshot();
    for case in snap["f_camelot"].as_array().unwrap() {
        let args = case["args"].as_array().unwrap();
        let x0 = args_as_u256(&args[0]);
        let y = args_as_u256(&args[1]);
        let got = f_camelot(x0, y);
        let want = expected_u256(&case["result"]);
        assert_eq!(got, want, "f_camelot({x0}, {y})");
    }
}

#[test]
fn cross_check_k_camelot() {
    let snap = load_snapshot();
    for case in snap["k_camelot"].as_array().unwrap() {
        let args = case["args"].as_array().unwrap();
        let b0 = args_as_u256(&args[0]);
        let b1 = args_as_u256(&args[1]);
        let d0 = args_as_u256(&args[2]);
        let d1 = args_as_u256(&args[3]);
        let got = k_camelot(b0, b1, d0, d1).unwrap();
        let want = expected_u256(&case["result"]);
        assert_eq!(got, want, "k_camelot({b0}, {b1}, {d0}, {d1})");
    }
}

#[test]
fn cross_check_calc_exact_in_volatile() {
    let snap = load_snapshot();
    for case in snap["calc_exact_in_volatile"].as_array().unwrap() {
        let args = case["args"].as_array().unwrap();
        let amount_in = args_as_u256(&args[0]);
        let token_in: u8 = args_as_u8(&args[1]);
        let r0 = args_as_u256(&args[2]);
        let r1 = args_as_u256(&args[3]);
        let fee_numer = args_as_u256(&args[4]);
        let fee_denom = args_as_u256(&args[5]);
        let got = calc_exact_in_volatile(amount_in, token_in, r0, r1, fee_numer, fee_denom)
            .expect("volatile path must not revert in snapshot");
        let want = expected_u256(&case["result"]);
        assert_eq!(got, want, "calc_exact_in_volatile(...)");
    }
}

#[test]
fn cross_check_get_y_solidly() {
    let snap = load_snapshot();
    for case in snap["get_y_solidly"].as_array().unwrap() {
        let args = case["args"].as_array().unwrap();
        let x0 = args_as_u256(&args[0]);
        let xy = args_as_u256(&args[1]);
        let y_seed = args_as_u256(&args[2]);
        let d0 = args_as_u256(&args[3]);
        let d1 = args_as_u256(&args[4]);
        let got = get_y_solidly(x0, xy, y_seed, d0, d1).unwrap();
        let want = expected_u256(&case["result"]);
        assert_eq!(got, want, "get_y_solidly({x0}, {xy}, {y_seed}, {d0}, {d1})");
    }
}

#[test]
fn cross_check_get_y_camelot() {
    let snap = load_snapshot();
    for case in snap["get_y_camelot"].as_array().unwrap() {
        let args = case["args"].as_array().unwrap();
        let x_0 = args_as_u256(&args[0]);
        let xy = args_as_u256(&args[1]);
        let y_seed = args_as_u256(&args[2]);
        // The Rust signature drops decimals0/decimals1 (the Python variant
        // accepts them for call-convention alignment only — unused by the
        // Camelot loop). Cross-checks don't read them.
        let got = get_y_camelot(x_0, xy, y_seed);
        let want = expected_u256(&case["result"]);
        assert_eq!(got, want, "get_y_camelot({x_0}, {xy}, {y_seed})");
    }
}

#[test]
fn cross_check_calc_exact_in_stable_solidly() {
    let snap = load_snapshot();
    for case in snap["calc_exact_in_stable_solidly"].as_array().unwrap() {
        let args = case["args"].as_array().unwrap();
        let amount_in = args_as_u256(&args[0]);
        let token_in: u8 = args_as_u8(&args[1]);
        let r0 = args_as_u256(&args[2]);
        let r1 = args_as_u256(&args[3]);
        let d0 = args_as_u256(&args[4]);
        let d1 = args_as_u256(&args[5]);
        let fee_numer = args_as_u256(&args[6]);
        let fee_denom = args_as_u256(&args[7]);
        let got =
            calc_exact_in_stable_solidly(amount_in, token_in, r0, r1, d0, d1, fee_numer, fee_denom)
                .expect("solidly stable path must not revert in snapshot");
        let want = expected_u256(&case["result"]);
        assert_eq!(got, want, "calc_exact_in_stable_solidly(...)");
    }
}

#[test]
fn cross_check_calc_exact_in_stable_camelot() {
    let snap = load_snapshot();
    for case in snap["calc_exact_in_stable_camelot"].as_array().unwrap() {
        let args = case["args"].as_array().unwrap();
        let amount_in = args_as_u256(&args[0]);
        let token_in: u8 = args_as_u8(&args[1]);
        let r0 = args_as_u256(&args[2]);
        let r1 = args_as_u256(&args[3]);
        let d0 = args_as_u256(&args[4]);
        let d1 = args_as_u256(&args[5]);
        let fee_numer = args_as_u256(&args[6]);
        let fee_denom = args_as_u256(&args[7]);
        let got =
            calc_exact_in_stable_camelot(amount_in, token_in, r0, r1, d0, d1, fee_numer, fee_denom)
                .expect("camelot stable path must not revert in snapshot");
        let want = expected_u256(&case["result"]);
        assert_eq!(got, want, "calc_exact_in_stable_camelot(...)");
    }
}

// ── calc_exact_out_stable_solidly roundtrip property (S5SJXF / LTLR2K) ──
//
// The exact-out leaf has no deployed-contract oracle (the Python `_calc_tokens_in_stable`
// raised `NotImplementedError`). The §4.2 oracle is the property-based check
// `exact_out` is the *inverse* of `exact_in` over the Python-oracle snapshot
// corpus — the same (reserves, decimals, fee, token_in, amount_in) cases the
// exact-in parity pins. For each case:
//   1. out        = calc_exact_in_stable_solidly(amount_in, ...)
//   2. recovered  = calc_exact_out_stable_solidly(out,   ...)
// Asserts:
//   - recovered ≤ amount_in (the exact-in amount over-paid or exactly paid
//     for the floored output; the inverse returns the *minimum* input that
//     produces ≥ out, so it can be strictly less than a snapshot amount_in
//     that over-paid for a truncated output);
//   - exact_in(recovered) ≥ out (correctness — the recovered input does
//     produce at least the target output);
//   - exact_in(recovered − 1) < out (tightness — recovered is the minimum:
//     one wei less would not reach the target).

#[test]
fn cross_check_calc_exact_out_stable_solidly_roundtrip() {
    let snap = load_snapshot();
    let one = U256::from(1u64);
    for case in snap["calc_exact_in_stable_solidly"].as_array().unwrap() {
        let args = case["args"].as_array().unwrap();
        let amount_in = args_as_u256(&args[0]);
        let token_in: u8 = args_as_u8(&args[1]);
        let r0 = args_as_u256(&args[2]);
        let r1 = args_as_u256(&args[3]);
        let d0 = args_as_u256(&args[4]);
        let d1 = args_as_u256(&args[5]);
        let fee_numer = args_as_u256(&args[6]);
        let fee_denom = args_as_u256(&args[7]);

        let out =
            calc_exact_in_stable_solidly(amount_in, token_in, r0, r1, d0, d1, fee_numer, fee_denom)
                .expect("exact_in must not revert in snapshot");
        if out.is_zero() {
            // A zero output (degenerate input) is not a meaningful roundtrip target.
            continue;
        }
        let recovered =
            calc_exact_out_stable_solidly(out, token_in, r0, r1, d0, d1, fee_numer, fee_denom)
                .expect("exact_out must not revert for an in-range output");

        // (a) The inverse returns the MINIMUM input producing ≥ out, so it is
        //     ≤ the snapshot's amount_in (which over-paid or exactly paid for
        //     the floored output).
        assert!(
            recovered <= amount_in,
            "exact_out ({recovered}) > amount_in ({amount_in}) for out={out} \
             (r0={r0}, r1={r1}, d0={d0}, d1={d1}, token_in={token_in})",
        );
        assert!(
            recovered > U256::ZERO,
            "exact_out returned 0 for a non-zero output (out={out})",
        );

        // (b) Correctness: the recovered input produces at least the target
        //     output when fed back through exact-in.
        let out_from_recovered =
            calc_exact_in_stable_solidly(recovered, token_in, r0, r1, d0, d1, fee_numer, fee_denom)
                .expect("exact_in(recovered) must not revert");
        assert!(
            out_from_recovered >= out,
            "exact_in(recovered)={out_from_recovered} < out={out} — inversion is insufficient",
        );

        // (c) Tightness: one wei less than `recovered` no longer reaches `out`
        //     (so `recovered` is the minimum — the true getAmountIn answer).
        let just_below = recovered - one;
        let out_below = calc_exact_in_stable_solidly(
            just_below, token_in, r0, r1, d0, d1, fee_numer, fee_denom,
        )
        .expect("exact_in(just_below) must not revert");
        assert!(
            out_below < out,
            "exact_in(recovered-1)={out_below} >= out={out} — recovered is not the minimum",
        );
    }
}

// ── calc_exact_out_stable_solidly(amount_out=0) must revert (smoke-spelunk) ─
//
// Regression guard against the masking `.unwrap_or(U256::ZERO)` at the `M-1`
// site: a zero `amount_out` drives the solver to the rest state
// (`x0_sol == reserves_a`), so `amount_in_after_fee_scaled == 0`, `M == 0`,
// and `M-1` underflows. On-chain Solidly `getAmountIn` reverts here with
// `INSUFFICIENT_OUTPUT_AMOUNT`; the unwrap_or used to swallow that into a
// phantom `amount_in == 1`. The fix surfaces a typed `SolidlyMathError` so
// the path propagates to the Python layer as an exception (matching the
// on-chain revert) instead of returning a wrong number.
#[test]
fn calc_exact_out_stable_solidly_zero_output_reverts_not_phantom_one() {
    use degenbot_solidly_math::SolidlyMathError;
    // Snapshot corpus reserves/decimals/fees — any registered stable pool
    // would do; we just need a non-degenerate in-range configuration.
    let snap = load_snapshot();
    let case = &snap["calc_exact_in_stable_solidly"].as_array().unwrap()[0];
    let args = case["args"].as_array().unwrap();
    let token_in: u8 = args_as_u8(&args[1]);
    let r0 = args_as_u256(&args[2]);
    let r1 = args_as_u256(&args[3]);
    let d0 = args_as_u256(&args[4]);
    let d1 = args_as_u256(&args[5]);
    let fee_numer = args_as_u256(&args[6]);
    let fee_denom = args_as_u256(&args[7]);

    let result =
        calc_exact_out_stable_solidly(U256::ZERO, token_in, r0, r1, d0, d1, fee_numer, fee_denom);
    assert!(
        matches!(result, Err(SolidlyMathError::InsufficientOutputAmount)),
        "amount_out=0 must revert with InsufficientOutputAmount, got {result:?} \
         (previously the unwrap_or masked this into a phantom amount_in=1)",
    );
}
