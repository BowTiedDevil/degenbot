#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::doc_markdown
)]
//! Executor grammar harness — the declarative matrix (UQOAHA).
//!
//! Every reachable 2-hop and 3-hop grammar family is one table row (a
//! [`Case`]) instead of a hand-written test. Each row builds its pools, then
//! [`Harness::run_chain`] derives the amounts, assembles the `PathInfo`,
//! funds, executes and reports `(outcome, predicted_profit, actual_delta)`.
//! [`assert_profitable`] then proves not just that the payload EXECUTED but
//! that the encoded amounts moved the predicted WETH (measured executor
//! balance delta == predicted profit), the property the old hand-written
//! `executed(n)`-only assertions couldn't see.
//!
//! This is the superseding home for the per-family hand-written tests (the
//! old `harness_v2/v3/v4_executor.rs` fixtures are ported here as rows).
//! Every family routes through the production `encode_cmd_stream` against the
//! real `cmd_executor` bytecode with the synthesized pools/tokens.

use alloy::primitives::U256;
use degenbot_simulation::harness::{assert_profitable, Harness, Hop, HopPool};

/// Q64.96 price of 1 (token1 per token0).
fn price_one() -> U256 {
    U256::from(1u128) << 96
}
/// Q64.96 price of `x`/100.
fn price_x(x: u64) -> U256 {
    price_one() * U256::from(x) / U256::from(100)
}
/// Large active liquidity (single tick, no curve hits).
fn liq() -> u128 {
    10u128.pow(22)
}

/// Build a `Harness` (fresh revm) and the path hops for one grammar family.
type Build = Box<dyn Fn(&mut Harness) -> (Vec<Hop>, u128)>;

/// One grammar-family case: a builder plus the drive/assert parameters.
struct Case {
    name: &'static str,
    build: Build,
    gas: u64,
    swaps: usize,
}

/// Run every family in the matrix; each gets a fresh harness.
fn run_matrix() {
    for c in cases() {
        let mut h = Harness::new().unwrap();
        let (hops, optimal_input) = (c.build)(&mut h);
        println!("── {:<16} optimal={optimal_input}", c.name);
        let result = h.run_chain(&hops, optimal_input, c.gas).unwrap();
        println!(
            "   outcome={:?}  predicted_profit={}  actual_delta={}",
            result.outcome, result.predicted_profit, result.actual_weth_delta
        );
        assert_profitable(&result, c.swaps, c.name);
    }
}

fn cases() -> Vec<Case> {
    let mut m: Vec<Case> = Vec::new();

    // ── 2-hop ─────────────────────────────────────────────────────────────
    m.push(Case {
        name: "v2_v2",
        gas: 5_000_000,
        swaps: 2,
        build: Box::new(|h| {
            let u = h.add_token().unwrap();
            let pa = h.add_pool(h.weth, u, 1_000_000, 2_000_000).unwrap(); // WETH rich
            let pb = h.add_pool(h.weth, u, 2_000_000, 1_000_000).unwrap(); // WETH poor
            let hops = vec![
                Hop {
                    src: h.weth,
                    dst: u,
                    pool: HopPool::V2(pa),
                },
                Hop {
                    src: u,
                    dst: h.weth,
                    pool: HopPool::V2(pb),
                },
            ];
            (hops, 100_000)
        }),
    });
    m.push(Case {
        name: "v2_v3",
        gas: 5_000_000,
        swaps: 2,
        build: Box::new(|h| {
            let t = h.add_token().unwrap();
            let p0 = h.add_pool(h.weth, t, 1_000_000, 2_000_000).unwrap();
            let p1 = h
                .add_v3_pool(
                    t,
                    h.weth,
                    3000,
                    price_one(),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let hops = vec![
                Hop {
                    src: h.weth,
                    dst: t,
                    pool: HopPool::V2(p0),
                },
                Hop {
                    src: t,
                    dst: h.weth,
                    pool: HopPool::V3(p1),
                },
            ];
            (hops, 100_000)
        }),
    });
    m.push(Case {
        name: "v3_v2",
        gas: 5_000_000,
        swaps: 2,
        build: Box::new(|h| {
            let t = h.add_token().unwrap();
            let p0 = h
                .add_v3_pool(
                    h.weth,
                    t,
                    3000,
                    price_one(),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let p1 = h.add_pool(t, h.weth, 1_000_000, 2_000_000).unwrap();
            let hops = vec![
                Hop {
                    src: h.weth,
                    dst: t,
                    pool: HopPool::V3(p0),
                },
                Hop {
                    src: t,
                    dst: h.weth,
                    pool: HopPool::V2(p1),
                },
            ];
            (hops, 100_000)
        }),
    });
    m.push(Case {
        name: "v3_v3",
        gas: 5_000_000,
        swaps: 2,
        build: Box::new(|h| {
            let t = h.add_token().unwrap();
            let p0 = h
                .add_v3_pool(
                    h.weth,
                    t,
                    3000,
                    price_one(),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let p1 = h
                .add_v3_pool(
                    t,
                    h.weth,
                    3000,
                    price_x(102),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let hops = vec![
                Hop {
                    src: h.weth,
                    dst: t,
                    pool: HopPool::V3(p0),
                },
                Hop {
                    src: t,
                    dst: h.weth,
                    pool: HopPool::V3(p1),
                },
            ];
            (hops, 100_000)
        }),
    });
    m.push(Case {
        name: "v4_v4",
        gas: 8_000_000,
        swaps: 2,
        build: Box::new(|h| {
            let i = h.add_token().unwrap();
            let a = h
                .add_v4_pool(
                    h.weth,
                    i,
                    3000,
                    60,
                    price_one(),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let b = h
                .add_v4_pool(
                    i,
                    h.weth,
                    3000,
                    60,
                    price_x(102),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let hops = vec![
                Hop {
                    src: h.weth,
                    dst: i,
                    pool: HopPool::V4(a),
                },
                Hop {
                    src: i,
                    dst: h.weth,
                    pool: HopPool::V4(b),
                },
            ];
            (hops, 100_000)
        }),
    });
    m.push(Case {
        name: "v4_v3",
        gas: 8_000_000,
        swaps: 2,
        build: Box::new(|h| {
            let t = h.add_token().unwrap();
            let a = h
                .add_v4_pool(
                    h.weth,
                    t,
                    3000,
                    60,
                    price_one(),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let p1 = h
                .add_v3_pool(
                    t,
                    h.weth,
                    3000,
                    price_x(102),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let hops = vec![
                Hop {
                    src: h.weth,
                    dst: t,
                    pool: HopPool::V4(a),
                },
                Hop {
                    src: t,
                    dst: h.weth,
                    pool: HopPool::V3(p1),
                },
            ];
            (hops, 100_000)
        }),
    });
    m.push(Case {
        name: "v3_v4",
        gas: 8_000_000,
        swaps: 2,
        build: Box::new(|h| {
            let t = h.add_token().unwrap();
            let p0 = h
                .add_v3_pool(
                    h.weth,
                    t,
                    3000,
                    price_one(),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let a = h
                .add_v4_pool(
                    t,
                    h.weth,
                    3000,
                    60,
                    price_x(102),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let hops = vec![
                Hop {
                    src: h.weth,
                    dst: t,
                    pool: HopPool::V3(p0),
                },
                Hop {
                    src: t,
                    dst: h.weth,
                    pool: HopPool::V4(a),
                },
            ];
            (hops, 100_000)
        }),
    });
    m.push(Case {
        name: "v2_v4",
        gas: 8_000_000,
        swaps: 2,
        build: Box::new(|h| {
            let t = h.add_token().unwrap();
            let p0 = h.add_pool(h.weth, t, 1_000_000, 2_000_000).unwrap();
            let a = h
                .add_v4_pool(
                    t,
                    h.weth,
                    3000,
                    60,
                    price_one(),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let hops = vec![
                Hop {
                    src: h.weth,
                    dst: t,
                    pool: HopPool::V2(p0),
                },
                Hop {
                    src: t,
                    dst: h.weth,
                    pool: HopPool::V4(a),
                },
            ];
            (hops, 100_000)
        }),
    });
    m.push(Case {
        name: "v4_v2",
        gas: 8_000_000,
        swaps: 2,
        build: Box::new(|h| {
            let f = h.add_token().unwrap();
            let a = h
                .add_v4_pool(
                    h.weth,
                    f,
                    3000,
                    60,
                    price_one(),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let pair = h.add_pool(f, h.weth, 1_000_000_000, 1_020_000_000).unwrap();
            let hops = vec![
                Hop {
                    src: h.weth,
                    dst: f,
                    pool: HopPool::V4(a),
                },
                Hop {
                    src: f,
                    dst: h.weth,
                    pool: HopPool::V2(pair),
                },
            ];
            (hops, 100_000)
        }),
    });

    // ── 3-hop ─────────────────────────────────────────────────────────────
    m.push(Case {
        name: "v2_v2_v2",
        gas: 5_000_000,
        swaps: 3,
        build: Box::new(|h| {
            let (t1, t2) = (h.add_token().unwrap(), h.add_token().unwrap());
            let p0 = h.add_pool(h.weth, t1, 1_000_000, 2_000_000).unwrap();
            let p1 = h.add_pool(t1, t2, 1_000_000, 1_000_000).unwrap();
            let p2 = h.add_pool(t2, h.weth, 1_000_000, 1_000_000).unwrap();
            let hops = vec![
                Hop {
                    src: h.weth,
                    dst: t1,
                    pool: HopPool::V2(p0),
                },
                Hop {
                    src: t1,
                    dst: t2,
                    pool: HopPool::V2(p1),
                },
                Hop {
                    src: t2,
                    dst: h.weth,
                    pool: HopPool::V2(p2),
                },
            ];
            (hops, 100_000)
        }),
    });
    m.push(Case {
        name: "v2_v3_v2",
        gas: 8_000_000,
        swaps: 3,
        build: Box::new(|h| {
            let (t1, t2) = (h.add_token().unwrap(), h.add_token().unwrap());
            let p0 = h
                .add_pool(h.weth, t1, 1_000_000_000, 1_000_000_000)
                .unwrap();
            let p1 = h
                .add_v3_pool(
                    t1,
                    t2,
                    3000,
                    price_x(105),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let p2 = h
                .add_pool(t2, h.weth, 1_000_000_000, 1_000_000_000)
                .unwrap();
            let hops = vec![
                Hop {
                    src: h.weth,
                    dst: t1,
                    pool: HopPool::V2(p0),
                },
                Hop {
                    src: t1,
                    dst: t2,
                    pool: HopPool::V3(p1),
                },
                Hop {
                    src: t2,
                    dst: h.weth,
                    pool: HopPool::V2(p2),
                },
            ];
            (hops, 100_000)
        }),
    });
    m.push(Case {
        name: "v2_v4_v2",
        gas: 8_000_000,
        swaps: 3,
        build: Box::new(|h| {
            let (t1, t2) = (h.add_token().unwrap(), h.add_token().unwrap());
            let p0 = h
                .add_pool(h.weth, t1, 1_000_000_000, 1_000_000_000)
                .unwrap();
            let a = h
                .add_v4_pool(
                    t1,
                    t2,
                    3000,
                    60,
                    price_x(105),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let p2 = h
                .add_pool(t2, h.weth, 1_000_000_000, 1_000_000_000)
                .unwrap();
            let hops = vec![
                Hop {
                    src: h.weth,
                    dst: t1,
                    pool: HopPool::V2(p0),
                },
                Hop {
                    src: t1,
                    dst: t2,
                    pool: HopPool::V4(a),
                },
                Hop {
                    src: t2,
                    dst: h.weth,
                    pool: HopPool::V2(p2),
                },
            ];
            (hops, 100_000)
        }),
    });
    m.push(Case {
        name: "v3_v2_v4",
        gas: 8_000_000,
        swaps: 3,
        build: Box::new(|h| {
            let (t1, t2) = (h.add_token().unwrap(), h.add_token().unwrap());
            let p0 = h
                .add_v3_pool(
                    h.weth,
                    t1,
                    3000,
                    price_one(),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let p1 = h.add_pool(t1, t2, 1_000_000_000, 1_000_000_000).unwrap();
            let a = h
                .add_v4_pool(
                    t2,
                    h.weth,
                    3000,
                    60,
                    price_x(105),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let hops = vec![
                Hop {
                    src: h.weth,
                    dst: t1,
                    pool: HopPool::V3(p0),
                },
                Hop {
                    src: t1,
                    dst: t2,
                    pool: HopPool::V2(p1),
                },
                Hop {
                    src: t2,
                    dst: h.weth,
                    pool: HopPool::V4(a),
                },
            ];
            (hops, 100_000)
        }),
    });
    m.push(Case {
        name: "v3_v4_v2",
        gas: 8_000_000,
        swaps: 3,
        build: Box::new(|h| {
            let (t1, t2) = (h.add_token().unwrap(), h.add_token().unwrap());
            let p0 = h
                .add_v3_pool(
                    h.weth,
                    t1,
                    3000,
                    price_one(),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let a = h
                .add_v4_pool(
                    t1,
                    t2,
                    3000,
                    60,
                    price_x(105),
                    liq(),
                    1_000_000_000,
                    1_000_000_000,
                )
                .unwrap();
            let p2 = h
                .add_pool(t2, h.weth, 1_000_000_000, 1_000_000_000)
                .unwrap();
            let hops = vec![
                Hop {
                    src: h.weth,
                    dst: t1,
                    pool: HopPool::V3(p0),
                },
                Hop {
                    src: t1,
                    dst: t2,
                    pool: HopPool::V4(a),
                },
                Hop {
                    src: t2,
                    dst: h.weth,
                    pool: HopPool::V2(p2),
                },
            ];
            (hops, 100_000)
        }),
    });

    m
}

/// Drive the full 2- and 3-hop grammar matrix (one fresh harness per row).
#[test]
fn full_matrix_executes_with_expected_profit() {
    run_matrix();
}

/// Negative control: a deliberately *unprofitable* chain still EXECUTES (reaches
/// both pools) but yields a negative WETH delta, so [`assert_profitable`] must
/// reject it. Proves the runner's delta guard fires on a real losing path rather
/// than being vacuous (a broken `balance_of` would report delta 0 and also fail
/// the `> 0` check).
#[test]
#[should_panic(expected = "expected a profitable (positive) WETH delta")]
fn unprofitable_chain_is_rejected() {
    let mut h = Harness::new().unwrap();
    let u = h.add_token().unwrap();
    // Both pairs WETH-poor: WETH -> USDC then USDC -> WETH loses on every hop.
    let pa = h.add_pool(u, h.weth, 2_000_000, 1_000_000).unwrap(); // USDC rich, WETH poor
    let pb = h.add_pool(u, h.weth, 2_000_000, 1_000_000).unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: u,
            pool: HopPool::V2(pa),
        },
        Hop {
            src: u,
            dst: h.weth,
            pool: HopPool::V2(pb),
        },
    ];
    let result = h.run_chain(&hops, 100_000, 5_000_000).unwrap();
    assert!(result.outcome.executed(2), "still touches both pools");
    assert!(
        result.actual_weth_delta < 0,
        "must actually lose: {:?}",
        result
    );
    assert_profitable(&result, 2, "unprofitable"); // must panic
}
