#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::doc_markdown
)]
//! Isolate `PoolV3.swap` behind the `TestV3SwapDriver`: does the stub's price
//! math panic (the `Panic(0x11)` seen in v2_v3 / v3_v2)? Calls the pool
//! directly with the driver as recipient + callback payer.
use alloy::primitives::{Address, Bytes, U256};
use degenbot_simulation::harness::Harness;
use degenbot_simulation::oracle::selector;

fn pad32(a: Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..32].copy_from_slice(a.as_slice());
    w
}
fn do_swap_data(pool: Address, zfo: bool, amount: u64, zfo_limit: bool) -> Bytes {
    let mut b = selector("doSwap(address,bool,int256,bool)").to_vec();
    b.extend_from_slice(&pad32(pool));
    b.extend_from_slice(&U256::from(u8::from(zfo)).to_be_bytes::<32>());
    b.extend_from_slice(&U256::from(amount).to_be_bytes::<32>());
    b.extend_from_slice(&U256::from(u8::from(zfo_limit)).to_be_bytes::<32>());
    Bytes::from(b)
}

#[test]
fn poolv3_swap_isolated() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    // token0 = weth, token1 = t1; selling weth(token0)->t1(token1) = zfo=true.
    let pool = h
        .add_v3_pool(
            h.weth,
            t1,
            3000,
            U256::from(1u128) << 96,
            10u128.pow(22),
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();
    let driver = h.deploy_stub("TestV3SwapDriver").unwrap();
    h.fund(h.weth, driver, 1_000_000_000).unwrap();
    h.fund(t1, driver, 1_000_000_000).unwrap();

    for &amount in &[100_000u64, 181_322, 50_000] {
        let data = do_swap_data(pool.pool, true, amount, true);
        let res = match h.call(driver, &data, 5_000_000) {
            Ok(_) => "OK".to_string(),
            Err(e) => format!("REVERT: {e}"),
        };
        println!("zfo=true amt={amount}: {res}");
        assert!(res == "OK", "isolated PoolV3.swap must succeed (zfo=true)");
    }
}
