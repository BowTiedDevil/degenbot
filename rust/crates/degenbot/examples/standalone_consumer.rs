//! Standalone-Rust consumer smoke test (ADR-005 standalone claim, made concrete).
//!
//! Proves a Rust consumer can `cargo add degenbot`, construct a `BotState`,
//! register a Uniswap V2 pool via the `UNISWAP_V2` `DexIdentity` preset, and
//! run a swap calc — **with no Python interpreter, no `pyo3` feature, no
//! maturin in the build graph**. This is the `polars`-equivalent of a Rust
//! binary doing `cargo add polars` and building a `DataFrame` with no Python.
//!
//! Run it with:
//! ```text
//! cargo run -p degenbot --example standalone_consumer
//! ```
//! It `panic!`s on any check failure (exit code != 0), so it doubles as a
//! standalone-consumer gate.

use alloy::primitives::{address, U256};
use degenbot::degenbot_balancer_math::{mul_down, ONE};
use degenbot::degenbot_curve_math::{stableswap_get_d, DVariant};
use degenbot::degenbot_solidly_math::{calc_d as solidly_calc_d, calc_f as solidly_calc_f};
use degenbot::dex_identity::UNISWAP_V2;
use degenbot::{BotState, RegisterV2PoolParams};

fn main() {
    // 1. Construct the Rust-owned per-chain bot state (no Python).
    let mut bot = BotState::new();

    // 2. Derive the V2 pool's registration parameters from the `UNISWAP_V2`
    //    `DexIdentity` preset (ADR-005 slice 6 — the standalone-constraint
    //    data layer). Using the preset's factory + fee params means a
    //    standalone Rust consumer reaches on-chain-correct swap math without
    //    any Python-side ClassVar lookup.
    let token0 = address!("000000000000000000000000000000000000000A");
    let token1 = address!("000000000000000000000000000000000000000B");
    let pool = address!("000000000000000000000000000000000000000C");

    // 1_000_000 USDC (6dp) in / reserves roughly 0.5 WETH (18dp) — on-chain
    // getAmountOut parity reference (slice-5 convention: gamma_numer is the
    // RETAINED post-fee fraction = 997/1000 for a 0.3% Uniswap V2 fee).
    let reserve0 = U256::from(1_000_000_000_000_u64); // 1e6 * 1e6
    let reserve1 = U256::from(500_000_000_000_000_000_u64); // 0.5 * 1e18

    let params = RegisterV2PoolParams {
        address: pool,
        token0,
        token1,
        reserve0,
        reserve1,
        fee_token0: UNISWAP_V2.fee_token0,
        fee_token1: UNISWAP_V2.fee_token1,
        factory: UNISWAP_V2.factory,
        update_block: 19_000_000,
    };
    let pool_id = bot.register_v2_pool(&params);
    assert_eq!(pool_id, 1, "first registered pool gets id 1");

    // 3. Run a swap calc through the Rust core (the Möbius `IntHopState`
    //    constant-product path). The same code path the PyO3 binding ships to
    //    Python — but here reached without a single `pyo3` import.
    let amount_in = U256::from(1_000_000_000_u64); // 1000 USDC in
    let amount_out = bot.calculate_tokens_out(pool_id, true, amount_in);
    assert!(
        amount_out > U256::ZERO,
        "expected a non-zero swap output, got {amount_out}"
    );

    // Round-trip sanity: a larger input must produce a strictly-larger output
    // (constant-product is monotonic, ignoring fee edge cases at the extremes).
    let bigger_in = amount_in * U256::from(2_u64);
    let bigger_out = bot.calculate_tokens_out(pool_id, true, bigger_in);
    assert!(
        bigger_out > amount_out,
        "constant-product calc must be monotonic: {bigger_out} !> {amount_out}"
    );

    // 4. Verify the preset's on-chain-correct fee convention is wired through
    //    (the slice-5 bug was registering the FEE numerator, not the RETAINED
    //    complement). The `UNISWAP_V2` preset's `fee_tokenN.0` is `997`
    //    (retained) over `1000` — a 0.3% fee. Cross-check against the
    //    closed-form Uniswap V2 `getAmountOut` for the configured reserves:
    //      amountInWithFee = amount_in * 997
    //      numerator       = amountInWithFee * reserve_out
    //      denominator     = reserve_in * 1000 + amountInWithFee
    //    — byte-identical to the core's EVM-exact integer path
    //    (`mobius_int::IntHopState::swap`).
    let amount_in_with_fee = amount_in * U256::from(997_u64);
    let numer = amount_in_with_fee * reserve1;
    let denom = reserve0 * U256::from(1000_u64) + amount_in_with_fee;
    let expected = numer / denom;
    assert_eq!(
        amount_out, expected,
        "Rust core calc must match the closed-form Uniswap V2 getAmountOut"
    );

    println!("standalone degenbot consumer OK: pool_id={pool_id} amount_out={amount_out}");

    // 5. Reach the pure-Rust math leaves directly — proves the umbrella
    //    re-exports the `degenbot-curve-math` / `degenbot-balancer-math`
    //    leaves (ADR-005 sub-step B' completion) so a standalone consumer
    //    can call StableSwap / FixedPoint math without `pyo3` in the graph.
    //
    // Curve `stableswap_get_d` over a balanced 2-coin pool: D converges to
    //    sum(xp) = 2e18 when the pool is balanced (the StableSwap invariant
    //    equals the constant-sum invariant at balance — A amplification perturbs
    //    D by < 1 unit at the fixed point; the contraction stops within MAX
    //    loop steps when |d - d_prev| <= 1).
    let xp = [
        U256::from(1_000_000_000_000_000_000_u64),
        U256::from(1_000_000_000_000_000_000_u64),
    ];
    let n_coins = U256::from(2u64);
    let a_precision = U256::from(100u64);
    let amp = U256::from(2000u64); // A = 20
    let d = stableswap_get_d(&xp, amp, n_coins, a_precision, DVariant::Standard)
        .expect("stableswap_get_d converged");
    let sum = xp[0] + xp[1];
    // Balanced pool: D ≈ sum(xp) within the convergence tolerance (±1).
    assert!(
        d.abs_diff(sum) <= U256::from(1u64),
        "balanced StableSwap D must ≈ sum(xp): {d} vs {sum}"
    );

    // Balancer `FixedPoint.mul_down(a, b) = a*b / ONE` (18-dec fixed-point).
    // Identity check: mul_down(x, ONE) == x for any x ≤ max_fp.
    let val = U256::from(42_000_000_000_000_000_000_u128); // 42 * 1e18
    assert_eq!(mul_down(val, ONE).unwrap(), val, "mul_down(x, ONE) == x");

    // 6. Solidly-stable math leaf reach: confirm `calc_d(x0, y)` and
    //    `calc_f(x0, y)` are reachable from the umbrella without `pyo3`.
    //    The Solidly/Aerodrome deployed-contract `calc_d` evaluates the
    //    analytic `D = 3*x0*y^2 + x0^3*y` (1e18-scaled); re-derive it in plain
    //    integer arithmetic for the parity check.
    let x0 = U256::from(2_000_000_000_000_000_000_u64); // 2 * 1e18
    let y = U256::from(3_000_000_000_000_000_000_u64); // 3 * 1e18
    let got_d = solidly_calc_d(x0, y);
    // D = 3*x0*(y^2 / 1e18) / 1e18 + (((x0^2 / 1e18) * x0) / 1e18)
    let yy = y * y / ONE;
    let term1 = U256::from(3u64) * x0 * yy / ONE;
    let x0x0 = x0 * x0 / ONE;
    let term2 = x0x0 * x0 / ONE;
    let expected_d = term1 + term2;
    assert_eq!(got_d, expected_d, "solidly calc_d direct port");
    // And `calc_f(x0, y) = x0*y^3 + x0^3*y`:
    let got_f = solidly_calc_f(x0, y);
    let a = x0 * y / ONE;
    let b = x0 * x0 / ONE + y * y / ONE;
    let expected_f = a * b / ONE;
    assert_eq!(got_f, expected_f, "solidly calc_f direct port");

    println!("standalone degenbot consumer OK: curve D={d} balancer fp.mul_down(identity) solidly calc_d={got_d}");
}
