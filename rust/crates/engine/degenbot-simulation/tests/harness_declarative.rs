#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::doc_markdown,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::type_complexity
)]
//! Executor grammar harness — the generated declarative matrix (UQOAHA).
//!
//! The reachable 2- and 3-hop grammar is the **full cross-product** of
//! `{V2,V3,V4}` per hop: `degenbot_executor::grammar_shape::derive_shape` (and the
//! all-V2 any-N route, since the KO5NNB cutover) enumerate all `3²=9` two-hop
//! and `3³=27` three-hop family tuples; there is no residual for 2/3-hop
//! paths. Every family is therefore **one generated table row** — the whole
//! matrix is built by a loop over [`family_list`], not by hand-written tests.
//!
//! Each row builds its pools via the generic [`build_family`] scheme (entry/
//! middle legs at ~1×, terminal leg at a ~2–3× return so the path is
//! profitable), then [`Harness::run_chain`] derives the amount chain, assembles
//! the production `PathInfo`, funds, executes and reports
//! `(outcome, predicted_profit, actual_delta)`. [`assert_profitable`] proves
//! both that the payload EXECUTED and that the encoded amounts moved the
//! predicted WETH (measured executor delta == predicted profit).
//!
//! **Runtime-fidelity sign-off.** All 36 families (9 two-hop + 27 three-hop)
//! pass through the real cmd_executor bytecode with exact
//! `actual_delta == predicted_profit` — a property byte-parity cannot see.
//! (Earlier this harness caught `v2_v2_v4`/`v2_v4_v4` reverting with v4-core's
//! `"D0"` — they `take`d WETH from the PoolManager before any swap produced a
//! positive WETH delta. `degenbot_executor::grammar` was fixed: those adapters
//! now self-fund the leading V2 hop from the executor's own balance via an
//! ERC20 transfer instead of a `take`, and they are back in the matrix.)

use std::sync::Arc;

use alloy::primitives::U256;
use degenbot_math::concentrated_liquidity::tick_math::{MAX_SQRT_RATIO, MIN_SQRT_RATIO};
use degenbot_math::v2::IntHopState;
use degenbot_pools::solve::{
    solve_path_with_min_profit, ResolvedHop, ResolvedMixedPath, SolvePathResult,
};
use degenbot_pools::{
    GateDeps, IntV3TickRangeHop, IntV3TickRangeSequence, PreparedConcentratedLiquidityHop,
};

use degenbot_simulation::harness::{
    assert_erc6909_capture, assert_profitable, v3_amount_out, Harness, Hop, HopPool,
};

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
            HopPool::V2(h.add_pool(src, dst, r, r * u128::from(mult)).unwrap())
        }
        Prot::V3 => HopPool::V3(
            h.add_v3_pool(
                src,
                dst,
                3000,
                sqrt_x(mult),
                liq(),
                1_000_000_000_000_000_000_000_000_000_000,
                1_000_000_000_000_000_000_000_000_000_000,
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
                1_000_000_000_000_000_000_000_000_000_000,
                1_000_000_000_000_000_000_000_000_000_000,
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

/// Every working grammar family (all 36), generated by protocol cross-product.
fn cases() -> Vec<Case> {
    let mut m = Vec::new();
    for n in [2usize, 3] {
        for fam in family_list(n) {
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

/// Solve a Harness path through the production mixed-path solver. The single
/// full-range concentrated-liquidity projection matches the fake pools'
/// constant-liquidity swap behavior, so solver amounts and EVM execution see
/// the same state.
fn solve_hops(hops: &[Hop]) -> SolvePathResult {
    let resolved_hops = hops
        .iter()
        .map(|hop| match hop.pool {
            HopPool::V2(pool) => {
                let (reserve_in, reserve_out) = if hop.src == pool.token0 {
                    (pool.reserve0, pool.reserve1)
                } else {
                    (pool.reserve1, pool.reserve0)
                };
                ResolvedHop::V2 {
                    state: IntHopState::new(
                        U256::from(reserve_in),
                        U256::from(reserve_out),
                        997,
                        1000,
                    ),
                }
            }
            HopPool::V3(pool) => concentrated_hop(
                pool.liquidity,
                pool.sqrt_price,
                pool.fee,
                hop.src == pool.token0,
                false,
            ),
            HopPool::V4(pool) => concentrated_hop(
                pool.liquidity,
                pool.sqrt_price,
                pool.fee,
                hop.src == pool.currency0,
                true,
            ),
        })
        .collect();
    let path = ResolvedMixedPath {
        hops: resolved_hops,
        valid: true,
        state_nonces: vec![1; hops.len()],
        ..Default::default()
    };
    solve_path_with_min_profit(&path, U256::ZERO, &GateDeps::offline())
        .result
        .unwrap_or_else(|| panic!("the deterministic fixture must be profitable"))
}

fn concentrated_hop(
    liquidity: u128,
    sqrt_price: U256,
    fee: u32,
    zero_for_one: bool,
    v4: bool,
) -> ResolvedHop {
    let range = IntV3TickRangeHop {
        liquidity,
        sqrt_price_x96: sqrt_price,
        sqrt_price_lower_x96: U256::from(MIN_SQRT_RATIO),
        sqrt_price_upper_x96: U256::from(MAX_SQRT_RATIO),
        gamma_numer: 1_000_000 - u64::from(fee),
        fee_denom: 1_000_000,
        zero_for_one,
        word_boundary_prices: Vec::new(),
    };
    let prepared = PreparedConcentratedLiquidityHop::derive(Arc::new(
        IntV3TickRangeSequence::new(vec![range]).unwrap(),
    ));
    if v4 {
        ResolvedHop::V4 { prepared }
    } else {
        ResolvedHop::V3 { prepared }
    }
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

#[test]
fn solver_amounts_execute_one_mixed_path() {
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V2, Prot::V3, Prot::V4]);
    let solution = solve_hops(&hops);
    assert!(solution.profit > U256::ZERO);

    let optimal_input = solution.optimal_input.to::<u128>();
    let outputs: Vec<u128> = solution.hop_outputs.iter().map(U256::to::<u128>).collect();
    let consumed: Vec<u128> = solution
        .consumed_inputs
        .iter()
        .map(U256::to::<u128>)
        .collect();
    let result = h
        .run_chain_with_consumed(
            &hops,
            optimal_input,
            &outputs,
            &consumed,
            40_000_000,
            degenbot_executor::composers::EncodeOptions::default(),
        )
        .unwrap();
    assert_profitable(&result, 3, "solver-v2-v3-v4");
    assert_eq!(
        result.predicted_profit,
        i128::try_from(solution.profit.to::<u128>()).unwrap()
    );
}

#[test]
fn full_three_hop_matrix_executes_production_solver_amounts() {
    for c in cases()
        .into_iter()
        .filter(|c| c.name.matches('_').count() == 2)
    {
        let mut h = Harness::new().unwrap();
        let hops = (c.build)(&mut h);
        let solution = solve_hops(&hops);
        assert!(
            solution.profit > U256::ZERO,
            "{} was not profitable",
            c.name
        );
        let optimal_input = solution.optimal_input.to::<u128>();
        let outputs: Vec<u128> = solution.hop_outputs.iter().map(U256::to::<u128>).collect();
        let consumed: Vec<u128> = solution
            .consumed_inputs
            .iter()
            .map(U256::to::<u128>)
            .collect();
        let result = h
            .run_chain_with_consumed(
                &hops,
                optimal_input,
                &outputs,
                &consumed,
                c.gas,
                degenbot_executor::composers::EncodeOptions::default(),
            )
            .unwrap_or_else(|e| panic!("[{}] solver-driven run_chain: {e}", c.name));
        println!(
            "   solver {:<14} input={} predicted={} actual={}",
            c.name, optimal_input, result.predicted_profit, result.actual_weth_delta
        );
        assert_profitable(&result, c.swaps, &c.name);
        assert_eq!(
            result.predicted_profit,
            i128::try_from(solution.profit.to::<u128>()).unwrap(),
            "{} solver profit differs from the executed amount chain",
            c.name
        );
    }
}

/// Self-funding must not turn a money-losing amount chain into an encoder
/// underflow. The executor holds the entry WETH, so the full terminal output
/// can be captured and the contract's InsufficientProfit check can classify
/// the economics. V2→V2→V4 is the minimal terminal-V4 case that previously
/// returned no calldata at `out_c.checked_sub(optimal_input)`.
#[test]
fn losing_v2_v2_v4_self_fund_executes_to_profit_assert() {
    use degenbot_executor::grammar_ledger::FundingSource;

    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V2, Prot::V2, Prot::V4]);
    let optimal_input = 100_000;
    let hop_outputs = vec![90_000, 80_000, 70_000];
    let consumed_inputs = vec![optimal_input, 90_000, 80_000];
    let result = h
        .run_chain_with_consumed(
            &hops,
            optimal_input,
            &hop_outputs,
            &consumed_inputs,
            40_000_000,
            degenbot_executor::composers::EncodeOptions {
                funding: FundingSource::SelfFund,
                ..Default::default()
            },
        )
        .expect("a losing self-funded V2-V2-V4 chain must produce executable calldata");
    assert!(
        result.outcome.executed(3),
        "all three swaps must execute; the mainnet probe checks InsufficientProfit: {result:?}"
    );
}

#[test]
fn losing_v2_v4_v4_self_fund_executes_to_profit_assert() {
    use degenbot_executor::grammar_ledger::FundingSource;
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V2, Prot::V4, Prot::V4]);
    let result = h
        .run_chain_with_consumed(
            &hops,
            100_000,
            &[90_000, 80_000, 70_000],
            &[100_000, 90_000, 80_000],
            40_000_000,
            degenbot_executor::composers::EncodeOptions {
                funding: FundingSource::SelfFund,
                ..Default::default()
            },
        )
        .expect("a losing self-funded V2-V4-V4 chain must produce executable calldata");
    assert!(
        result.outcome.executed(3),
        "all three swaps must execute: {result:?}"
    );
}

#[test]
fn losing_v3_v4_v4_self_fund_executes_to_profit_assert() {
    use degenbot_executor::grammar_ledger::FundingSource;
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V3, Prot::V4, Prot::V4]);
    let result = h
        .run_chain_with_consumed(
            &hops,
            100_000,
            &[90_000, 80_000, 70_000],
            &[100_000, 90_000, 80_000],
            40_000_000,
            degenbot_executor::composers::EncodeOptions {
                funding: FundingSource::SelfFund,
                ..Default::default()
            },
        )
        .expect("a losing self-funded V3-V4-V4 chain must produce executable calldata");
    assert!(
        result.outcome.executed(3),
        "all three swaps must execute: {result:?}"
    );

    // Mainnet A/B are the reverse orientation: WETH/USDC then WBTC/USDC.
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V3, Prot::V3, Prot::V4]);
    let weth = h.weth;
    let t1 = hops[0].dst;
    let t2 = hops[1].dst;
    let reserve = 1_000_000_000_000_000_000_000_000_000_000;
    let reverse = vec![
        Hop {
            src: weth,
            dst: t1,
            pool: HopPool::V3(
                h.add_v3_pool(t1, weth, 3000, q96_one(), liq(), reserve, reserve)
                    .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V3(
                h.add_v3_pool(t2, t1, 3000, q96_one(), liq(), reserve, reserve)
                    .unwrap(),
            ),
        },
        hops[2],
    ];
    let t1_for_funding = hops[0].dst;
    h.fund(t1_for_funding, h.executor, 1_000_000).unwrap();
    let result = h
        .run_chain_with_consumed(
            &reverse,
            100_000,
            &[90_000, 80_000, 70_000],
            &[100_000, 90_000, 80_000],
            40_000_000,
            degenbot_executor::composers::EncodeOptions {
                funding: FundingSource::SelfFund,
                ..Default::default()
            },
        )
        .expect("reverse V3-V3-V4 must produce executable calldata");
    assert!(
        result.outcome.executed(3),
        "reverse V3 directions must execute: {result:?}"
    );
}

#[test]
fn losing_v4_v4_v3_self_fund_executes_to_profit_assert() {
    use degenbot_executor::grammar_ledger::FundingSource;
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V4, Prot::V4, Prot::V3]);
    let weth = h.weth;
    let intermediate = hops[1].dst;
    // The mainnet terminal WBTC/WETH V3 pool is token0=WBTC, token1=WETH.
    // Cover that reverse direction explicitly; pool_for's forward-only
    // fixture would otherwise hide this command-dispatch axis.
    let hops_for_amounts = hops.clone();
    let _terminal = HopPool::V3(
        h.add_v3_pool(
            intermediate,
            weth,
            3000,
            sqrt_x(3),
            liq(),
            1_000_000_000_000_000_000_000_000_000_000,
            1_000_000_000_000_000_000_000_000_000_000,
        )
        .unwrap(),
    );
    let usdc = hops[0].dst;
    // Match production's explicit probe capital: WETH and the second V4's
    // USDC input, not a magical WBTC buffer supplied by the generic harness.
    h.fund(usdc, h.executor, 1_000_000).unwrap();
    let (path, _, _) = h.path_and_amounts(&hops_for_amounts, 100_000);
    let hop_outputs = [90_000, 80_000, 70_000];
    let consumed_inputs = [100_000, 90_000, 80_000];
    let opts = degenbot_executor::composers::EncodeOptions {
        funding: FundingSource::SelfFund,
        ..Default::default()
    };
    let payload = h
        .encode_path_with_consumed(&path, 100_000, &hop_outputs, &consumed_inputs, opts)
        .expect("V4-V4-V3 must encode with only its declared capital");
    h.fund(weth, h.executor, 200_000).unwrap();
    let outcome = h.execute_payload(&payload, 40_000_000).unwrap();
    assert!(
        outcome.executed(3),
        "WETH + USDC alone must execute: {outcome:?}"
    );
}

#[test]
fn losing_v3_v3_v4_self_fund_executes_to_profit_assert() {
    use degenbot_executor::grammar_ledger::FundingSource;
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V3, Prot::V3, Prot::V4]);
    let result = h
        .run_chain_with_consumed(
            &hops,
            100_000,
            &[90_000, 80_000, 70_000],
            &[100_000, 90_000, 80_000],
            40_000_000,
            degenbot_executor::composers::EncodeOptions {
                funding: FundingSource::SelfFund,
                ..Default::default()
            },
        )
        .expect("a losing self-funded V3-V3-V4 chain must produce executable calldata");
    assert!(
        result.outcome.executed(3),
        "all three swaps must execute: {result:?}"
    );
}

/// Zero-balance counterpart to the production-solver matrix. No WETH or
/// intermediate-token funding is installed: every input must come from the
/// path's own flash/V4 credit graph. This is the positive robustness proof
/// for profitable cycles, complementing the typed refusal for losing cycles.
#[test]
fn full_three_hop_zero_balance_matrix_executes_without_token_funding() {
    for c in cases()
        .into_iter()
        .filter(|c| c.name.matches('_').count() == 2)
    {
        let mut h = Harness::new().unwrap();
        let hops = (c.build)(&mut h);
        let solution = solve_hops(&hops);
        let optimal_input = solution.optimal_input.to::<u128>();
        let outputs: Vec<u128> = solution.hop_outputs.iter().map(U256::to::<u128>).collect();
        let consumed: Vec<u128> = solution
            .consumed_inputs
            .iter()
            .map(U256::to::<u128>)
            .collect();
        let (path, _, _) = h.path_and_amounts(&hops, optimal_input);
        for hop in &hops {
            assert_eq!(
                h.balance_of(hop.src, h.executor).unwrap(),
                U256::ZERO,
                "{} starts with unexpected capital in {:?}",
                c.name,
                hop.src,
            );
        }
        let payload = h
            .encode_path_with_consumed(
                &path,
                optimal_input,
                &outputs,
                &consumed,
                degenbot_executor::composers::EncodeOptions::default(),
            )
            .unwrap_or_else(|e| panic!("[{}] zero-balance encode: {e}", c.name));
        let before =
            i128::try_from(h.balance_of(h.weth, h.executor).unwrap().to::<u128>()).unwrap();
        let outcome = h
            .execute_payload_config(
                &payload,
                c.gas,
                degenbot_executor::composers::config_for_options(
                    degenbot_executor::composers::EncodeOptions::default(),
                    U256::ZERO,
                )
                .unwrap(),
            )
            .unwrap();
        if !outcome.executed(3) {
            let (verdict, trace) = degenbot_simulation::oracle::transact_with_trace(
                &mut h.evm,
                degenbot_simulation::oracle::TxSpec::Call {
                    to: h.executor,
                    data: degenbot_simulation::harness::execute_data_config(
                        &payload,
                        degenbot_executor::composers::config_for_options(
                            degenbot_executor::composers::EncodeOptions::default(),
                            U256::ZERO,
                        )
                        .unwrap(),
                    ),
                    gas: c.gas,
                },
            );
            panic!(
                "{} zero-balance failure: {outcome:?}; replay={verdict:?};\n{}",
                c.name,
                trace.render_debug(),
            );
        }
        let after = i128::try_from(h.balance_of(h.weth, h.executor).unwrap().to::<u128>()).unwrap();
        let predicted = i128::try_from(solution.profit.to::<u128>()).unwrap();
        let delta = after - before;
        let tolerance = (predicted.abs() / 1000).max(64);
        assert!(
            (delta - predicted).abs() <= tolerance,
            "{} zero-balance delta {delta} != predicted {predicted}",
            c.name,
        );
    }
}

/// Completeness guard: the matrix covers the FULL reachable grammar
/// (`{V2,V3,V4}²` + `{V2,V3,V4}³`) — so a composer edit can never silently add
/// a reachable family without a guarded row, or delete one while a row lingers.
#[test]
fn matrix_covers_full_reachable_grammar() {
    let covered: std::collections::BTreeSet<String> = cases().into_iter().map(|c| c.name).collect();
    let mut reachable: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    reachable.extend(family_list(2));
    reachable.extend(family_list(3));

    let missing: Vec<String> = reachable
        .iter()
        .filter(|n| !covered.contains(n.as_str()))
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "reachable grammar families with no matrix row: {missing:?}"
    );

    let unexpected: Vec<String> = covered
        .iter()
        .filter(|&n| !reachable.contains(n))
        .cloned()
        .collect();
    assert!(
        unexpected.is_empty(),
        "matrix rows that are not reachable grammar families: {unexpected:?}"
    );
    assert_eq!(
        covered.len(),
        reachable.len(),
        "matrix ({}) must equal the full reachable grammar ({})",
        covered.len(),
        reachable.len()
    );
}

/// Negative control: a deliberately unprofitable chain under the
/// production axis-aware config (default Custody → `profit_mode=1`) now reverts
/// **on-chain** at the U3WVLL profit assert — the money-loss floor is active
/// by default, so the loss never executes. The harness classifies the revert
/// and [`assert_profitable`]'s first guard ("payload must execute") fires.
///
/// **Ko5NNB cutover note — why SelfFund:** since the all-V2 family routes
/// through the Plan + validator gate, a losing all-V2 **InPathFlash** stream is
/// REJECTED at encode (`Erc20TransferBeforeCredit`: the flash repay `100k`
/// exceeds the stream's ~82k terminal WETH). A losing stream that reaches the
/// profit assert is only representable under `FundingSource::SelfFund`: no
/// flash debt, so the executor would eat the loss from its held WETH buffer —
/// the case the on-chain floor now reverts.
#[test]
#[should_panic(expected = "payload must execute (reach 2 pools)")]
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
    let result = h
        .run_chain_with_opts(
            &hops,
            100_000,
            5_000_000,
            degenbot_executor::composers::EncodeOptions {
                funding: degenbot_executor::grammar_ledger::FundingSource::SelfFund,
                ..Default::default()
            },
        )
        .unwrap();
    // The on-chain `profit_mode=1` assert reverts the losing self-fund path
    // (U3WVLL floor) — the revert IS the protection; classify it as such.
    assert!(
        matches!(
            result.outcome,
            degenbot_simulation::harness::ExecOutcome::Reverted { .. }
        ),
        "losing self-fund path must revert at the profit assert: {result:?}"
    );
    assert_profitable(&result, 2, "unprofitable");
}

/// ADR-033 guard: the harness's session-scoped [`EncodeContext`] projection
/// maps the DEPLOYED addresses in the canonical (executor, pool_manager,
/// weth) order — pinning each field to its own deployment so a field-order
/// footgun in the projection (or the single call site) fails loudly.
/// `EncodeContext` is `PartialEq`, so a transposed pair (pm ↔ weth) is caught.
#[test]
fn harness_encode_context_projects_deployed_addresses() {
    let h = Harness::new().unwrap();
    let ctx = h.encode_context();
    assert_eq!(
        ctx,
        degenbot_executor::composers::EncodeContext::new(h.executor, h.pool_manager, h.weth,),
        "encode_context must project (executor, pool_manager, weth) from the deployed addresses"
    );
    // The projection is Copy + Eq — the session value can be threaded by value.
    let ctx2 = ctx;
    assert_eq!(ctx, ctx2);
}

// ═══════════════════════════════════════════════════════════════════════════
// ADR-033 (D7) — caller-supplied amounts through the production intake
// ═══════════════════════════════════════════════════════════════════════════

/// ADR-033 D7 deepening: `run_chain_with_consumed` is the first declarative
/// entry that takes CALLER-supplied per-hop amounts — the shape the
/// production solver commits after `clamp_concentrated_liquidity_hop_capacity` re-aligns
/// `hop_outputs[i]`/`consumed_inputs[i+1]` (path-73385) — instead of the
/// harness re-deriving a full-consumption chain. These fixtures drive that
/// entry through the production `encode_cmd_stream` intake against the real
/// `cmd_executor` bytecode, which no earlier harness entry could do.
///
/// **Over-feed clamp pair — parked with a note (per the plan's escape
/// clause).** A V3 concentrated liquidity over-feed at runtime needs the pool's capacity bound
/// to sit mid-price-range. The stub `PoolV3` is closed-form (single active
/// range, no tick bitmap): its capacity bound sits at the extreme
/// `MIN`/`MAX` price limits, where the opposing token prices at ~1e38 per
/// unit — u128-amount-infeasible for any loopback path. The profitable
/// clamped-execution / EMPTY-HALT verdict proof lives in the real-
/// `PoolManager` tier-3 regression (`tier3_path5000_v4_clamp.rs`), and the
/// pure clamp rule (`V3SwapOutcome::exact_input_clamp_bound`) is unit-green
/// in `degenbot-pools`. The aligned-shape fixtures below exercise the
/// intake's handling of exactly the amount shape the clamp produces:
/// a V2→V3 chain whose concentrated-liquidity-hop committed input is the previous hop's clamped,
/// re-aligned output (both sides clamped consistently). The amounts must
/// still encode + execute with the measured delta matching the prediction.
///
/// The aligned-shape fixture: a V2→V3 chain where the concentrated liquidity hop's committed
/// input is a fraction of the V2 hop's natural output — the shape
/// `clamp_concentrated_liquidity_hop_capacity` engineers (the previous hop's output re-aligned
/// to the committed concentrated liquidity input, both clamped consistently). The amounts must
/// still encode + execute with the measured delta matching the prediction.
#[test]
fn concentrated_liquidity_hop_aligned_clamp_shape_executes_with_consistent_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let r0: u128 = 1_000_000_000_000;
    let v2 = h.add_pool(h.weth, t1, r0, r0).unwrap();
    let v3 = h
        .add_v3_pool(
            t1,
            h.weth,
            3000,
            sqrt_x(3),
            liq(),
            1_000_000_000_000,
            1_000_000_000_000,
        )
        .unwrap();
    let r_in = if v2.token0 == h.weth {
        v2.reserve0
    } else {
        v2.reserve1
    };
    let r_out = if v2.token0 == h.weth {
        v2.reserve1
    } else {
        v2.reserve0
    };
    let v2_out = |in_weth: u128| {
        let amp = U256::from(in_weth) * U256::from(997u64);
        let num = amp * U256::from(r_out);
        let den = U256::from(r_in) * U256::from(1000u64) + amp;
        (num / den).to::<u128>()
    };

    // Natural chain: 100_000 WETH in → full-consumption amounts.
    let natural_in: u128 = 100_000;
    let natural_v2_out = v2_out(natural_in);
    // The clamped shape: the V2 hop commits enough for 70% of the natural
    // output, and the concentrated liquidity hop's committed input equals that re-aligned
    // output (consumed_inputs[1] == hop_outputs[0] — the alignment
    // clamp_concentrated_liquidity_hop_capacity enforces, with both sides clamped).
    let clamped_v2_out = v2_out(natural_in * 70 / 100);
    assert!(
        clamped_v2_out < natural_v2_out,
        "clamped V2 output must be below the natural output"
    );
    let v3_out = v3_amount_out(v3.sqrt_price, v3.liquidity, clamped_v2_out, true, 3000);

    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V2(v2),
        },
        Hop {
            src: t1,
            dst: h.weth,
            pool: HopPool::V3(v3),
        },
    ];
    let result = h
        .run_chain_with_consumed(
            &hops,
            natural_in * 70 / 100,
            &[clamped_v2_out, v3_out],
            &[natural_in * 70 / 100, clamped_v2_out],
            8_000_000,
            degenbot_executor::composers::EncodeOptions::default(),
        )
        .unwrap_or_else(|e| panic!("run_chain_with_consumed: {e}"));
    assert!(
        result.outcome.executed(2),
        "aligned clamp shape must execute both pools: {result:?}"
    );
    // Profitable by construction (3x terminal return): the measured delta
    // must match the caller-aligned prediction within tolerance — the
    // committed amounts moved exactly the WETH they encoded for.
    assert_profitable(&result, 2, "aligned-clamp-shape");
}

// ═══════════════════════════════════════════════════════════════════════════
// ERC6909-vault profit capture (the `erc6909_profit` operator toggle)
// ═══════════════════════════════════════════════════════════════════════════

/// a 2-hop V4 WETH-terminal path with the `erc6909_profit` toggle,
/// driven through the declarative entry — the production-mirror of the
/// strategy's `SimulatePath → encode_request → execute(axis-aware config)`
/// path. The stream mints the profit as an ERC6909 claim on the PoolManager;
/// the oracle is the contract-computed `PM.balanceOf(executor, weth)` read
/// around `execute` — the independent side of the `profit_mode=2` on-chain
/// assert — measured to the 0.1% `assert_profitable` pattern.
#[test]
fn erc6909_capture_v4v4_lands_in_vault() {
    let mut h = Harness::new().unwrap();
    let t = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t,
            pool: HopPool::V4(
                h.add_v4_pool(
                    h.weth,
                    t,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t,
            dst: h.weth,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t,
                    h.weth,
                    3000,
                    60,
                    sqrt_x(3),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
    ];
    // The oracle probe's regime pin (SMOZG3 open question 1): the executor
    // holds NO ERC6909 position beforehand — the capture is a fresh mint of
    // the profit surplus, not a transfer of a pre-held claim.
    assert_eq!(
        h.pm_balance_of(h.executor, h.weth).unwrap(),
        alloy::primitives::U256::ZERO,
        "fixture starts from a zero ERC6909 position"
    );
    let opts = degenbot_executor::composers::EncodeOptions {
        erc6909_profit: true,
        ..Default::default()
    };
    let result = h
        .run_chain_with_opts(&hops, 100_000, 8_000_000, opts)
        .unwrap_or_else(|e| panic!("run erc6909 capture: {e}"));
    assert_erc6909_capture(&result, 2, "v4_v4 erc6909 capture");
}

/// SMOZG3 parity: the 3-hop pure-V4 family (`v4_v4_v4`, the other family that
/// declares the `capture` axis) captures to the vault with the same oracle.
#[test]
fn erc6909_capture_v4v4v4_lands_in_vault() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V4(
                h.add_v4_pool(
                    h.weth,
                    t1,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t1,
                    t2,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t2,
                    h.weth,
                    3000,
                    60,
                    sqrt_x(3),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
    ];
    let opts = degenbot_executor::composers::EncodeOptions {
        erc6909_profit: true,
        ..Default::default()
    };
    let result = h
        .run_chain_with_opts(&hops, 100_000, 40_000_000, opts)
        .unwrap_or_else(|e| panic!("run erc6909 capture 3-hop: {e}"));
    assert_erc6909_capture(&result, 3, "v4_v4_v4 erc6909 capture");
}

/// TGUZCT/SW42JA (supersedes the SMOZG3 decline): `use_v4_batch` and
/// `erc6909_profit` now DO compose on a WETH-terminal pure-V4 path. The
/// encoder emits the `V4_BATCH_OPEN_WETH` (0x43) variant — the batch leaves
/// the positive WETH delta open, and the trailing `V4_MINT_COMPACT` converts
/// it into the ERC6909 vault. The prior decline (SMOZG3, pre-deployment
/// artifact whose 0x42 tail-settle starved the mint) is lifted.
#[test]
fn erc6909_capture_with_batch_composes_via_open_weth_batch() {
    let mut h = Harness::new().unwrap();
    let t = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t,
            pool: HopPool::V4(
                h.add_v4_pool(
                    h.weth,
                    t,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t,
            dst: h.weth,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t,
                    h.weth,
                    3000,
                    60,
                    sqrt_x(3),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
    ];
    let opts = degenbot_executor::composers::EncodeOptions {
        erc6909_profit: true,
        use_v4_batch: true,
        ..Default::default()
    };
    let result = h
        .run_chain_with_opts(&hops, 100_000, 8_000_000, opts)
        .unwrap_or_else(|e| panic!("batch + erc6909 must execute via 0x43 (TGUZCT): {e}"));
    assert_erc6909_capture(&result, 2, "v4_v4 batch erc6909 capture (0x43)");
}
