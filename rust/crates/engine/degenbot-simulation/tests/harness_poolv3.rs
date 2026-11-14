#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

//! Fidelity check for the fake `PoolV3` driven directly through
//! `TestV3SwapDriver`: the pool enforces the **input** token's payment in both
//! swap directions, and the enforcement is non-vacuous.
//!
//! An exact-input swap must leave exactly `amountSpecified` more of the input
//! token in the pool *and* deliver a nonzero output, so a callback that paid
//! nothing (or paid the wrong token) breaks the first claim while the second
//! still holds. The driver's `underpayment` hook then withholds exactly one unit
//! of the positive input delta: the swap reverts with the v3-core `IIA` reason,
//! and every balance plus the price stay where they were, so no partial payment
//! or output survives the revert.

use alloy::primitives::aliases::I256;
use alloy::primitives::{Address, Bytes, U256};
use alloy::sol_types::SolValue;

use degenbot_simulation::harness::{Harness, V3Pool};
use degenbot_simulation::oracle::{decode_error_string, selector, transact, TxSpec, Verdict};

/// Q64.96 price of 1.0 — the token1-per-token0 ratio the pool starts at.
const SQRT_PRICE_ONE: u128 = 1u128 << 96;
/// Active liquidity (one full-range bin, no tick crossing).
const LIQUIDITY: u128 = 10u128.pow(22);
/// The pool's own balance of each token: what backs a swap's output.
const POOL_RESERVE: u128 = 1_000_000_000;
/// The driver's (the callback payer's) balance of each token.
const DRIVER_FUNDING: u128 = 1_000_000_000;
const GAS: u64 = 5_000_000;

fn pad32(a: Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..32].copy_from_slice(a.as_slice());
    w
}

fn pad_word(v: U256) -> [u8; 32] {
    v.to_be_bytes::<32>()
}

/// `doSwap(pool, zfo, amount, zfoLimit)`: the limit flag always follows the
/// direction, so the price limit (`MIN_SQRT_RATIO+1` / `MAX_SQRT_RATIO-1`) sits
/// on the correct side of the current price and the swap is not bounded by it.
fn do_swap_data(pool: Address, zfo: bool, amount: u64) -> Bytes {
    let mut b = selector("doSwap(address,bool,int256,bool)").to_vec();
    b.extend_from_slice(&pad32(pool));
    b.extend_from_slice(&pad_word(U256::from(u8::from(zfo))));
    b.extend_from_slice(&pad_word(U256::from(amount)));
    b.extend_from_slice(&pad_word(U256::from(u8::from(zfo))));
    Bytes::from(b)
}

fn set_underpayment_data(units: u64) -> Bytes {
    let mut b = selector("setUnderpayment(uint256)").to_vec();
    b.extend_from_slice(&pad_word(U256::from(units)));
    Bytes::from(b)
}

/// A fresh `PoolV3` (token0 = weth, token1 = a fresh token) plus a funded
/// `TestV3SwapDriver`.
fn fixture() -> (Harness, V3Pool, Address) {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let pool = h
        .add_v3_pool(
            h.weth,
            t1,
            3000,
            U256::from(SQRT_PRICE_ONE),
            LIQUIDITY,
            POOL_RESERVE,
            POOL_RESERVE,
        )
        .unwrap();
    let driver = h.deploy_stub("TestV3SwapDriver").unwrap();
    h.fund(h.weth, driver, DRIVER_FUNDING).unwrap();
    h.fund(t1, driver, DRIVER_FUNDING).unwrap();
    (h, pool, driver)
}

/// `(token0, token1)` balance of `who`.
fn balances(h: &mut Harness, pool: &V3Pool, who: Address) -> (U256, U256) {
    (
        h.balance_of(pool.token0, who).unwrap(),
        h.balance_of(pool.token1, who).unwrap(),
    )
}

/// Read the pool's `sqrtPriceX96()`.
fn sqrt_price(h: &mut Harness, pool: Address) -> U256 {
    let out = h.call(pool, &selector("sqrtPriceX96()"), 200_000).unwrap();
    U256::from_be_slice(&out)
}

/// Reorder a `(token0, token1)` pair into `(input, output)`: selling token0 is
/// the `zeroForOne` direction, selling token1 the reverse.
fn by_direction(zfo: bool, t0: U256, t1: U256) -> (U256, U256) {
    if zfo {
        (t0, t1)
    } else {
        (t1, t0)
    }
}

/// `doSwap`'s `(amount0, amount1)` return, split into `(input, output)`
/// magnitudes per the v3 sign convention (positive = the pool's input).
fn input_output_amounts(out: &[u8], zfo: bool) -> (U256, U256) {
    let (a0, a1) = <(I256, I256)>::abi_decode(out).expect("doSwap returns (int256, int256)");
    let (input, output) = if zfo { (a0, a1) } else { (a1, a0) };
    assert!(
        input.is_positive(),
        "the pool's input delta must be positive"
    );
    assert!(
        output.is_negative(),
        "the pool's output delta must be negative"
    );
    (input.unsigned_abs(), output.unsigned_abs())
}

/// Both directions, several sizes: exact payment succeeds, moves exactly
/// `amountSpecified` of the input token into the pool, and delivers a nonzero
/// output to the driver. The exact delta is the evidence: a callback that paid
/// nothing, or paid the wrong token, leaves the pool's input balance where it
/// was, so success alone would prove nothing.
#[test]
fn exact_payment_succeeds_in_both_directions() {
    let (mut h, pool, driver) = fixture();

    for &(zfo, amount) in &[
        (true, 100_000u64),
        (true, 181_322),
        (true, 50_000),
        (false, 100_000),
        (false, 181_322),
        (false, 50_000),
    ] {
        let pool_before = balances(&mut h, &pool, pool.pool);
        let driver_before = balances(&mut h, &pool, driver);

        let out = h
            .call(driver, &do_swap_data(pool.pool, zfo, amount), GAS)
            .unwrap_or_else(|e| {
                panic!("zfo={zfo} amount={amount}: exact payment must succeed: {e}")
            });
        let (amount_in, amount_out) = input_output_amounts(&out, zfo);
        assert_eq!(amount_in, U256::from(amount), "reported input");
        assert!(amount_out > U256::ZERO, "zfo={zfo}: output must be nonzero");

        let pool_after = balances(&mut h, &pool, pool.pool);
        let driver_after = balances(&mut h, &pool, driver);

        let (pool_in_before, pool_out_before) = by_direction(zfo, pool_before.0, pool_before.1);
        let (pool_in_after, pool_out_after) = by_direction(zfo, pool_after.0, pool_after.1);
        let (driver_in_before, driver_out_before) =
            by_direction(zfo, driver_before.0, driver_before.1);
        let (driver_in_after, driver_out_after) = by_direction(zfo, driver_after.0, driver_after.1);

        assert_eq!(
            pool_in_after,
            pool_in_before + U256::from(amount),
            "zfo={zfo} amount={amount}: the pool must hold exactly amountSpecified more input"
        );
        assert_eq!(
            driver_in_before - driver_in_after,
            U256::from(amount),
            "zfo={zfo} amount={amount}: the driver must have paid exactly amountSpecified"
        );
        assert_eq!(
            pool_out_before - pool_out_after,
            amount_out,
            "zfo={zfo} amount={amount}: the pool must have sent exactly the reported output"
        );
        assert_eq!(
            driver_out_after - driver_out_before,
            amount_out,
            "zfo={zfo} amount={amount}: the driver must have received the reported output"
        );

        println!("zfo={zfo} amount={amount}: in={amount_in} out={amount_out}");
    }
}

/// The negative control: on one deployment, an exact swap succeeds, the driver
/// then withholds exactly one unit of the positive input delta, and the same
/// swap reverts with `IIA` in both directions — with every balance and the
/// price untouched, proving the revert carried no partial payment or output.
#[test]
fn one_unit_underpayment_reverts_iia_in_both_directions() {
    let (mut h, pool, driver) = fixture();
    let amount = 100_000u64;

    for &zfo in &[true, false] {
        // Baseline: the same swap, the same deployment, pays exactly → succeeds.
        h.call(driver, &do_swap_data(pool.pool, zfo, amount), GAS)
            .unwrap_or_else(|e| panic!("zfo={zfo}: exact payment must succeed first: {e}"));

        h.call(driver, &set_underpayment_data(1), 200_000).unwrap();

        let pool_before = balances(&mut h, &pool, pool.pool);
        let driver_before = balances(&mut h, &pool, driver);
        let price_before = sqrt_price(&mut h, pool.pool);

        let reverted = match transact(
            &mut h.evm,
            TxSpec::Call {
                to: driver,
                data: do_swap_data(pool.pool, zfo, amount),
                gas: GAS,
            },
        ) {
            Verdict::Reverted(reason) => reason,
            Verdict::Accepted { .. } => {
                panic!("zfo={zfo}: a one-unit underpayment must not be accepted")
            }
            Verdict::Halted(halt) => {
                panic!("zfo={zfo}: a one-unit underpayment must revert with IIA, halted: {halt}")
            }
        };
        assert_eq!(
            decode_error_string(&reverted).as_deref(),
            Some("IIA"),
            "zfo={zfo}: underpayment must trip the input-balance check, got {reverted:?}"
        );

        assert_eq!(
            balances(&mut h, &pool, pool.pool),
            pool_before,
            "zfo={zfo}: no partial payment may survive the revert (pool)"
        );
        assert_eq!(
            balances(&mut h, &pool, driver),
            driver_before,
            "zfo={zfo}: no partial payment/output may survive the revert (driver)"
        );
        assert_eq!(
            sqrt_price(&mut h, pool.pool),
            price_before,
            "zfo={zfo}: the price update must roll back with the revert"
        );

        // Restore exact payment so the next direction starts from a clean driver.
        h.call(driver, &set_underpayment_data(0), 200_000).unwrap();
        println!("zfo={zfo}: one-unit underpayment reverted with IIA, state intact");
    }
}
