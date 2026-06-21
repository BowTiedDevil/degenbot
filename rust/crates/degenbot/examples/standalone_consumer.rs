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
}
