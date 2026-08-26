#![expect(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Tier-2 behavioral dual-driver parity — V2 swap calc (ADR-005 standalone
//! claim, the behavioral tier).
//!
//! The Tier-1 `reachability.rs` test proves the symbol is *resolvable* from a
//! standalone Rust consumer. This Tier-2 test proves the FFI delegation is
//! *lossless*: the **same** canonical fixture, driven through the **Rust
//! consumer** path (`BotState` directly, as a `cargo add degenbot` user would)
//! produces the **same** `amount_out` as the closed-form Uniswap V2
//! `getAmountOut` reference.
//!
//! The matching Python side lives at
//! `tests/standalone_parity/test_v2_swap_dual_driver.py` — it drives the
//! **same** fixture through the Python consumer path (`PyBot`, the PyO3
//! binding). Both consumers hit the same `BotState::register_v2_pool` +
//! `BotState::calculate_tokens_out_miss_aware` core; the closed form is the shared
//! oracle. If the PyO3 arg-extraction → core-call → result-wrap seam ever
//! drops precision, changes rounding, or swaps a direction flag, the two
//! tests diverge from the closed form and the parity breaks.
//!
//! ## Fixture (the shared contract)
//!
//! Both tests MUST use these exact canonical values. The expected output is
//! derived from the closed-form Uniswap V2 `getAmountOut`:
//!
//! ```text
//! amount_in_with_fee = amount_in * 997
//! numerator          = amount_in_with_fee * reserve_out
//! denominator        = reserve_in * 1000 + amount_in_with_fee
//! amount_out         = numerator / denominator   (EVM integer division)
//! ```
//!
//! This is the same closed form `examples/standalone_consumer.rs` asserts
//! (the byte-identical EVM-exact integer path via
//! `degenbot_v2_math::IntHopState::swap`).

#![expect(clippy::doc_markdown)]

use alloy::primitives::{address, aliases::U112, I256, U256};
use degenbot::bot_core::swap_simulation::{SwapOutcome, SwapRead, SwapRequest};
use degenbot::dex_identity::UNISWAP_V2;
use degenbot::{BotState, RegisterV2PoolParams};

// ---- the shared canonical fixture (mirror in the Python parity test) ----
const TOKEN0: alloy::primitives::Address = address!("000000000000000000000000000000000000000A");
const TOKEN1: alloy::primitives::Address = address!("000000000000000000000000000000000000000B");
const POOL: alloy::primitives::Address = address!("000000000000000000000000000000000000000C");

const RESERVE0: u128 = 1_000_000_000_000; // 1e6 USDC (6dp)
const RESERVE1: u128 = 500_000_000_000_000_000; // 0.5 WETH (18dp)

const AMOUNT_IN: u128 = 1_000_000_000; // 1000 USDC (6dp)
const ZERO_FOR_ONE: bool = true; // token0 (USDC) → token1 (WETH)

/// Canonical expected output, derived from the closed form (see file doc).
///   amount_in_with_fee = 1_000_000_000 * 997               = 997_000_000_000
///   numerator          = 997_000_000_000 * 5e17            = 498_500_000_000_000_000_000_000_000
///   denominator        = 1e15 * 1000 + 997_000_000_000     = 1_000_997_000_000_000
///   amount_out         = numerator / denominator           = 498_003_490_519_951
const EXPECTED_AMOUNT_OUT: u128 = 498_003_490_519_951;

/// Canonical expected input, derived from the closed-form V2 `getAmountIn`:
///   amount_in = 1 + (reserve_in * amount_out * fee_denom) //
///                  ((reserve_out - amount_out) * gamma_numer)
/// where the target `amount_out` is `EXPECTED_AMOUNT_OUT` (round-trips back to
/// `AMOUNT_IN`). This is the closed form `BotState::calculate_tokens_in`
/// implements (`constant_product_calc_exact_out`).
///   numerator   = 1e15 * 498_003_490_519_951 * 1000        = 498_003_490_519_951_000_000_000_000_000
///   denominator = (5e17 - 498_003_490_519_951) * 997       = 498_001_502_017_480_049_498_953
///   amount_in   = 1 + numerator // denominator             = 1_000_000_000 (= AMOUNT_IN, round-trip)
const EXPECTED_AMOUNT_IN: u128 = 1_000_000_000;

/// Register the canonical V2 pool through the standalone-Rust consumer path
/// (`BotState::register_v2_pool` via the `UNISWAP_V2` `DexIdentity` preset)
/// and return the pool id.
///
/// This mirrors `examples/standalone_consumer.rs` steps 1–2 exactly — no
/// Python interpreter, no `pyo3` feature, no maturin in the build graph.
fn register_canonical_v2_pool() -> u64 {
    let mut bot = BotState::new();
    let params = RegisterV2PoolParams {
        address: POOL,
        token0: TOKEN0,
        token1: TOKEN1,
        reserve0: U256::from(RESERVE0).to::<U112>(),
        reserve1: U256::from(RESERVE1).to::<U112>(),
        fee_token0: UNISWAP_V2.fee_token0,
        fee_token1: UNISWAP_V2.fee_token1,
        factory: UNISWAP_V2.factory,
        update_block: 19_000_000,
        variant: UNISWAP_V2.variant,
        stable_swap: false,
        fee_denominator: None,
        ..Default::default()
    };
    bot.register_v2_pool(&params)
        .expect("standalone parity: register canonical V2 pool")
}

#[test]
fn standalone_rust_consumer_matches_closed_form() {
    // Tier-2 dual-driver gate — Rust consumer side.
    //
    // A `cargo add degenbot` standalone consumer, driving the canonical
    // fixture, MUST reproduce the closed-form Uniswap V2 `getAmountOut`. The
    // Python side (`tests/standalone_parity/test_v2_swap_dual_driver.py`)
    // drives the same fixture through `PyBot` and asserts the same expected
    // value — the closed form is the shared oracle proving the FFI seam is
    // lossless.
    let mut bot = BotState::new();
    // Re-register on a fresh bot (the helper consumes self) to keep the test
    // self-contained — pool_id is deterministic (first pool = 1).
    let params = RegisterV2PoolParams {
        address: POOL,
        token0: TOKEN0,
        token1: TOKEN1,
        reserve0: U256::from(RESERVE0).to::<U112>(),
        reserve1: U256::from(RESERVE1).to::<U112>(),
        fee_token0: UNISWAP_V2.fee_token0,
        fee_token1: UNISWAP_V2.fee_token1,
        factory: UNISWAP_V2.factory,
        update_block: 19_000_000,
        variant: UNISWAP_V2.variant,
        stable_swap: false,
        fee_denominator: None,
        ..Default::default()
    };
    let pid = bot
        .register_v2_pool(&params)
        .expect("register canonical V2 pool");
    assert_eq!(pid, 1, "first registered pool gets id 1");

    let amount_out = match bot.swap_simulation(
        0,
        pid,
        SwapRequest {
            zero_for_one: ZERO_FOR_ONE,
            amount_specified: -I256::try_from(U256::from(AMOUNT_IN)).unwrap(),
            sqrt_price_limit: None,
        },
    ) {
        SwapRead::Computed(outcome) => outcome.delivered_unsigned(),
        f => panic!(
            "small in-tick amount with pre-populated tick data; V2 calc must not miss or overflow: {f:?}"
        ),
    };
    assert_eq!(
        amount_out,
        U256::from(EXPECTED_AMOUNT_OUT),
        "standalone Rust consumer must match the closed-form V2 getAmountOut"
    );
}

#[test]
fn standalone_rust_consumer_reverse_matches_closed_form() {
    // Tier-2 dual-driver gate — Rust consumer side, reverse direction.
    //
    // The same canonical fixture, driving `calculate_tokens_in` (the
    // getAmountIn closed form) for the target `amount_out` the first test
    // derived. The round-trip property ties the two tests together: the
    // `amount_in` recovered MUST equal `AMOUNT_IN`. The Python side
    // mirrors this — both consumers must agree on the reverse calc, proving
    // the FFI seam is lossless in both directions (not just `tokens_out`).
    let mut bot = BotState::new();
    let params = RegisterV2PoolParams {
        address: POOL,
        token0: TOKEN0,
        token1: TOKEN1,
        reserve0: U256::from(RESERVE0).to::<U112>(),
        reserve1: U256::from(RESERVE1).to::<U112>(),
        fee_token0: UNISWAP_V2.fee_token0,
        fee_token1: UNISWAP_V2.fee_token1,
        factory: UNISWAP_V2.factory,
        update_block: 19_000_000,
        variant: UNISWAP_V2.variant,
        stable_swap: false,
        fee_denominator: None,
        ..Default::default()
    };
    let pid = bot
        .register_v2_pool(&params)
        .expect("register canonical V2 pool");
    assert_eq!(pid, 1, "first registered pool gets id 1");

    // ADR-037: exact-output read through the swap-simulation gate; the
    // required input is the consumed magnitude.
    let amount_in = match bot.swap_simulation(
        0,
        pid,
        SwapRequest {
            zero_for_one: ZERO_FOR_ONE,
            amount_specified: I256::try_from(U256::from(EXPECTED_AMOUNT_OUT)).unwrap(),
            sqrt_price_limit: None,
        },
    ) {
        SwapRead::Computed(SwapOutcome::V2(o)) => (-o.consumed).into_raw(),
        f => panic!("reverse calc must not fail on this fixture: {f:?}"),
    };
    assert_eq!(
        amount_in,
        U256::from(EXPECTED_AMOUNT_IN),
        "standalone Rust consumer reverse calc must match the closed-form V2 \
         getAmountIn and round-trip AMOUNT_IN"
    );
}

#[test]
fn closed_form_reference_is_self_consistent() {
    // Guard against the expected constant drifting from the closed form. If
    // someone edits RESERVE0/RESERVE1/AMOUNT_IN without updating
    // EXPECTED_AMOUNT_OUT, this re-derives the closed form and catches it —
    // so the shared contract can't silently break.
    let amount_in_with_fee = U256::from(AMOUNT_IN) * U256::from(997_u64);
    let numerator = amount_in_with_fee * U256::from(RESERVE1);
    let denominator = U256::from(RESERVE0) * U256::from(1000_u64) + amount_in_with_fee;
    let derived = numerator / denominator;
    assert_eq!(
        derived,
        U256::from(EXPECTED_AMOUNT_OUT),
        "EXPECTED_AMOUNT_OUT constant must match the closed-form re-derivation"
    );

    // Reverse-direction closed form: amount_in = 1 + (reserve_in * amount_out
    // * fee_denom) // ((reserve_out - amount_out) * gamma_numer). Verifies
    // EXPECTED_AMOUNT_IN round-trips back to AMOUNT_IN.
    let rev_numerator = U256::from(RESERVE0)
        .saturating_mul(U256::from(EXPECTED_AMOUNT_OUT))
        .saturating_mul(U256::from(1000_u64));
    let rev_denominator = (U256::from(RESERVE1).saturating_sub(U256::from(EXPECTED_AMOUNT_OUT)))
        .saturating_mul(U256::from(997_u64));
    let rev_derived = U256::from(1_u64) + rev_numerator / rev_denominator;
    assert_eq!(
        rev_derived,
        U256::from(EXPECTED_AMOUNT_IN),
        "EXPECTED_AMOUNT_IN constant must match the closed-form getAmountIn \
         re-derivation and round-trip AMOUNT_IN"
    );

    // Also confirm the helper's pool_id is deterministic — the parity test
    // relies on the Python side seeing pool_id 1 too.
    let pool_id = register_canonical_v2_pool();
    assert_eq!(pool_id, 1, "deterministic first-pool id (parity contract)");
}
