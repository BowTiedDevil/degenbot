#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::doc_markdown
)]
//! Executor grammar harness — the generated declarative matrix (UQOAHA).
//!
//! The reachable 2- and 3-hop grammar is the **full cross-product** of
//! `{V2,V3,V4}` per hop: `degenbot_executor::grammar::encode_grammar` (and the
//! all-V2 `encode_all_v2` route) enumerate all `3²=9` two-hop and `3³=27`
//! three-hop family tuples; there is no residual for 2/3-hop paths. Every
//! family is therefore **one generated table row** — the whole matrix is built
//! by a loop over [`family_list`], not by hand-written tests.
//!
//! Each row builds its pools via the generic [`build_family`] scheme (entry/
//! middle legs at ~1×, terminal leg at a ~2–3× return so the path is
//! profitable), then [`Harness::run_chain`] derives the amount chain, assembles
//! the production `PathInfo`, funds, executes and reports
//! `(outcome, predicted_profit, actual_delta)`. [`assert_profitable`] proves
//! both that the payload EXECUTED and that the encoded amounts moved the
//! predicted WETH (measured executor delta == predicted profit).
//!
//! **Fidelity finding.** Two families — `v2_v2_v4` and `v2_v4_v4` — are
//! excluded from the working matrix because they are **broken at runtime**
//! despite being byte-paritied. Both encode `V4_TAKE_COMPACT(WETH, …)` *before*
//! the terminal V4 swap that creates the executor's positive WETH delta, so
//! real v4-core rejects the take (`require(cur > 0)` → `"D0"`). The byte-parity
//! corpus is self-referential (derives expected bytes from the same `enc_*`
//! primitives the composer uses), so it cannot see this ordering defect. The
//! sibling `v2_v3_v4` runs the producing swap first and executes correctly —
//! the fix is to reorder these two adapters to match. They are tracked in
//! [`KNOWN_BROKEN`] and a regression test pins their current-failing behavior.

use alloy::primitives::U256;
use degenbot_simulation::harness::{assert_profitable, ExecOutcome, Harness, Hop, HopPool};

/// Families that are byte-paritied yet fail at runtime (see module doc). They
/// take WETH from the PoolManager before any swap produces it → `"D0"`.
const KNOWN_BROKEN: &[&str] = &["v2_v2_v4", "v2_v4_v4"];

#[derive(Clone, Copy, PartialEq)]
enum Prot {
    V2,
    V3,
    V4,
}

const PROT_NAMES: [&str; 3] = ["v2", "v3", "v4"];

/// Q64.96 sqrt price of 1 (token1 per token0).
fn q96_one() -> U256 {
    U256::from(1u128) << 96
}
/// Q64.96 sqrt of an integer `x` (price `x` token1 per token0 when the income
/// currency is token0).
fn sqrt_x(x: u64) -> U256 {
    if x == 1 {
        q96_one()
    } else {
        let s = ((x as f64).sqrt() * 65536.0) as u64;
        q96_one() * U256::from(s) / U256::from(65536)
    }
}
fn liq() -> u128 {
    10u128.pow(22)
}

/// Generic per-protocol pool over `src -> dst` with an out/in ratio of `mult`
/// (V2 via unbalanced reserves; V3/V4 via the Q64.96 price). `src` is token0
/// for V3/V4 (zfo=true) and either token for V2 (run_chain derives zfo).
fn pool_for(
    h: &mut Harness,
    p: Prot,
    src: alloy::primitives::Address,
    dst: alloy::primitives::Address,
    mult: u64,
) -> HopPool {
    match p {
        Prot::V2 => {
            let r: u128 = 1_000_000_000_000;
            HopPool::V2(h.add_pool(src, dst, r, r * mult as u128).unwrap())
        }
        Prot::V3 => HopPool::V3(
            h.add_v3_pool(
                src,
                dst,
                3000,
                sqrt_x(mult),
                liq(),
                1_000_000_000_000,
                1_000_000_000_000,
            )
            .unwrap(),
        ),
        Prot::V4 => HopPool::V4(
            h.add_v4_pool(
                src,
                dst,
                3000,
                60,
                sqrt_x(mult),
                liq(),
                1_000_000_000_000,
                1_000_000_000_000,
            )
            .unwrap(),
        ),
    }
}

/// Build a `W -> t1 -> … -> W` chain for a protocol sequence `seq`: entry and
/// middle legs at ~1×, the terminal (return-to-WETH) leg at `3×` so the path
/// is profitable. Works for 2- and 3-hop `seq`.
fn build_family(h: &mut Harness, seq: &[Prot]) -> Vec<Hop> {
    let n = seq.len();
    let mut tokens = Vec::with_capacity(n - 1);
    for _ in 0..n - 1 {
        tokens.push(h.add_token().unwrap());
    }
    let mut hops = Vec::with_capacity(n);
    for (i, p) in seq.iter().enumerate() {
        let (src, dst) = if i == 0 {
            (h.weth, tokens[0])
        } else if i == n - 1 {
            (tokens[n - 2], h.weth)
        } else {
            (tokens[i - 1], tokens[i])
        };
        let mult = if i == n - 1 { 3 } else { 1 };
        hops.push(Hop {
            src,
            dst,
            pool: pool_for(h, *p, src, dst, mult),
        });
    }
    hops
}

/// All `{v2,v3,v4}^n` family names in order.
fn family_list(n: usize) -> Vec<String> {
    let mut cur: Vec<String> = vec![String::new()];
    for _ in 0..n {
        let mut next = Vec::new();
        for prefix in &cur {
            for name in PROT_NAMES {
                next.push(if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{prefix}_{name}")
                });
            }
        }
        cur = next;
    }
    cur
}

struct Case {
    name: String,
    build: Box<dyn Fn(&mut Harness) -> Vec<Hop>>,
    gas: u64,
    swaps: usize,
}

/// Every working grammar family (all 36 minus [`KNOWN_BROKEN`]), generated.
fn cases() -> Vec<Case> {
    let mut m = Vec::new();
    for n in [2usize, 3] {
        for fam in family_list(n) {
            if KNOWN_BROKEN.contains(&fam.as_str()) {
                continue;
            }
            let names: Vec<&str> = fam.split('_').collect();
            let seq: Vec<Prot> = names
                .iter()
                .map(|s| match *s {
                    "v2" => Prot::V2,
                    "v3" => Prot::V3,
                    _ => Prot::V4,
                })
                .collect();
            let gas = if n == 2 { 8_000_000 } else { 40_000_000 };
            m.push(Case {
                name: fam,
                build: Box::new(move |h| build_family(h, &seq)),
                gas,
                swaps: n,
            });
        }
    }
    m
}

/// Drive the whole working matrix; each family gets a fresh harness.
#[test]
fn full_matrix_executes_with_expected_profit() {
    for c in cases() {
        let mut h = Harness::new().unwrap();
        let hops = (c.build)(&mut h);
        println!("── {:<14}", c.name);
        let result = h
            .run_chain(&hops, 100_000, c.gas)
            .unwrap_or_else(|e| panic!("[{}] run_chain: {e}", c.name));
        println!(
            "   outcome={:?}  predicted={}  actual={}",
            result.outcome, result.predicted_profit, result.actual_weth_delta
        );
        assert_profitable(&result, c.swaps, &c.name);
    }
}

/// Completeness guard: the matrix covers the FULL reachable grammar
/// (`{V2,V3,V4}²` + `{V2,V3,V4}³`), with the two [`KNOWN_BROKEN`] families
/// accounted for explicitly — so a composer edit can never silently add a
/// reachable family without a guarded row, or delete one while a row lingers.
#[test]
fn matrix_covers_full_reachable_grammar() {
    let covered: std::collections::BTreeSet<String> = cases().into_iter().map(|c| c.name).collect();
    let mut reachable: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    reachable.extend(family_list(2));
    reachable.extend(family_list(3));

    let missing: Vec<String> = reachable
        .iter()
        .filter(|n| !covered.contains(n.as_str()) && !KNOWN_BROKEN.contains(&n.as_str()))
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "reachable grammar families with no matrix row and not KNOWN_BROKEN: {missing:?}"
    );

    let unexpected: Vec<String> = covered
        .iter()
        .map(|s| s.to_string())
        .filter(|n| !reachable.contains(n))
        .collect();
    assert!(
        unexpected.is_empty(),
        "matrix rows that are not reachable grammar families: {unexpected:?}"
    );
    assert_eq!(
        covered.len() + KNOWN_BROKEN.len(),
        reachable.len(),
        "matrix ({}) + known-broken ({}) must equal the full reachable grammar ({})",
        covered.len(),
        KNOWN_BROKEN.len(),
        reachable.len()
    );
}

/// Regression pin on the two known-broken families: they must currently revert
/// with the v4-core `"D0"` (take of a non-positive WETH delta). When the
/// composer is fixed to order the producing swap first (as `v2_v3_v4` does),
/// this test fails loud and the families move into the working `cases()`.
#[test]
fn known_broken_families_still_fail_with_d0() {
    for fam in KNOWN_BROKEN {
        let names: Vec<&str> = fam.split('_').collect();
        let seq: Vec<Prot> = names
            .iter()
            .map(|s| match *s {
                "v2" => Prot::V2,
                "v3" => Prot::V3,
                _ => Prot::V4,
            })
            .collect();
        let mut h = Harness::new().unwrap();
        let hops = build_family(&mut h, &seq);
        let result = h.run_chain(&hops, 100_000, 40_000_000).unwrap();
        match &result.outcome {
            ExecOutcome::Reverted { reason, .. } => {
                assert!(
                    reason.as_deref() == Some("D0"),
                    "[{fam}] expected v4-core 'D0' take-guard revert, got: {result:?}"
                );
            }
            other => {
                panic!("[{fam}] expected 'D0' revert, but it now {other:?} — move it into cases()");
            }
        }
    }
}

/// Negative control: a deliberately unprofitable chain still EXECUTES but with
/// a negative WETH delta, so [`assert_profitable`] must reject it. Proves the
/// delta guard fires on a real losing path rather than being vacuous.
#[test]
#[should_panic(expected = "expected a profitable (positive) WETH delta")]
fn unprofitable_chain_is_rejected() {
    let mut h = Harness::new().unwrap();
    let u = h.add_token().unwrap();
    // Both pairs WETH-poor: WETH -> USDC then USDC -> WETH loses on every hop.
    let pa = h.add_pool(u, h.weth, 2_000_000, 1_000_000).unwrap();
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
    assert_profitable(&result, 2, "unprofitable");
}
