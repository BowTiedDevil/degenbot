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
//! the production `PathInfo`, applies the row's capital posture, executes and
//! reports `(outcome, predicted_profit, actual_delta)` plus the exact initial/
//! final balance of every touched native, ERC-20, and PoolManager-claim asset.
//! [`assert_profitable`] proves both that the payload EXECUTED and that the
//! encoded amounts moved the predicted WETH (measured executor delta ==
//! predicted profit).
//!
//! Every row runs through the SAME capital interface the live probe uses
//! ([`FixtureCapital`] / [`degenbot_simulation::ProbeCapital`]): the production
//! matrix declares the executor's own entry holding explicitly, the self-funded
//! matrix installs exactly the streaming Plan's requirements, and the
//! zero-balance matrix installs nothing at all — no runner mints a universal
//! WETH or intermediate-token buffer behind a row's back.
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
use degenbot_execution::CapitalRequirement;
use degenbot_executor::composers::{encode_cmd_stream_capital, EncodeOptions, EncodeRequest};
use degenbot_executor::grammar_ledger::FundingSource;
use degenbot_math::concentrated_liquidity::tick_math::{MAX_SQRT_RATIO, MIN_SQRT_RATIO};
use degenbot_math::v2::IntHopState;
use degenbot_pools::solve::{
    solve_path_with_min_profit, ResolvedHop, ResolvedMixedPath, SolvePathResult,
};
use degenbot_pools::{
    GateDeps, IntV3TickRangeHop, IntV3TickRangeSequence, PreparedConcentratedLiquidityHop,
};

use degenbot_simulation::harness::{
    assert_applied_capital, assert_erc6909_capture, assert_profitable, assert_zero_capital,
    v3_amount_out, ChainError, ChainResult, FixtureCapital, Harness, Hop, HopPool,
    STUB_POOL_MANAGER_ERC6909_BALANCE_OF_SLOT, STUB_TOKEN_BALANCE_OF_SLOT,
};
use degenbot_simulation::{CapitalAsset, ProbeCapital};

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

/// The entry amount every matrix row runs at.
const ENTRY_INPUT: u128 = 100_000;

/// The WETH a PRODUCTION row's executor explicitly holds: its own entry capital,
/// declared by the fixture. The production posture installs no override, so the
/// initial snapshot must be exactly this holding and nothing else.
const PRODUCTION_SEED: u128 = ENTRY_INPUT;

fn weth_of(harness: &Harness) -> CapitalAsset {
    CapitalAsset::Erc20 {
        token: harness.weth,
    }
}

/// Assert a production run started from exactly the holding the fixture seeded:
/// no plan, exactly `seed` of WETH, and every other touched asset at zero.
#[track_caller]
fn assert_production_seed(result: &ChainResult, weth: CapitalAsset, seed: u128, label: &str) {
    assert!(
        result.capital_plan.is_empty(),
        "[{label}] production applies no capital plan: {:?}",
        result.capital_plan
    );
    assert_eq!(
        result.initial_capital.balance_of(weth),
        U256::from(seed),
        "[{label}] the executor's WETH must be exactly the seeded holding"
    );
    for (asset, balance) in result.initial_capital.rows() {
        if *asset != weth {
            assert_eq!(
                *balance,
                U256::ZERO,
                "[{label}] {asset} was granted a holding the fixture never seeded"
            );
        }
    }
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
///
/// The row runs the PRODUCTION posture: the fixture explicitly seeds the
/// executor's entry holding and nothing else, and the initial snapshot proves
/// the run started from exactly that seed — no universal WETH or
/// intermediate-token buffer, and no clearing of the fixture's own state.
#[test]
fn full_matrix_executes_with_expected_profit() {
    let mut failures: Vec<String> = Vec::new();
    for c in cases() {
        let mut h = Harness::new().unwrap();
        let hops = (c.build)(&mut h);
        let weth = weth_of(&h);
        h.fund(h.weth, h.executor, PRODUCTION_SEED)
            .unwrap_or_else(|e| panic!("[{}] seed: {e}", c.name));
        println!("── {:<14}", c.name);
        let result = match h.run_chain(&hops, ENTRY_INPUT, c.gas) {
            Ok(result) => result,
            Err(e) => {
                failures.push(format!("{}: {e}", c.name));
                continue;
            }
        };
        println!(
            "   outcome={:?}  predicted={}  actual={}",
            result.outcome, result.predicted_profit, result.actual_weth_delta
        );
        if !result.outcome.executed(c.swaps) {
            failures.push(format!("{}: {result:?}", c.name));
            continue;
        }
        assert_profitable(&result, c.swaps, &c.name);
        assert_production_seed(&result, weth, PRODUCTION_SEED, &c.name);
    }
    assert!(
        failures.is_empty(),
        "rows that did not execute the production posture:\n{}",
        failures.join("\n")
    );
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
    h.fund(h.weth, h.executor, optimal_input).unwrap();
    let weth = weth_of(&h);
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
    assert_production_seed(&result, weth, optimal_input, "solver-v2-v3-v4");
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
        // Production posture: the fixture seeds the executor's own entry holding
        // (the solver's chosen input) and nothing else.
        h.fund(h.weth, h.executor, optimal_input)
            .unwrap_or_else(|e| panic!("[{}] seed: {e}", c.name));
        let weth = weth_of(&h);
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
        assert_production_seed(&result, weth, optimal_input, &c.name);
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

/// Drive a losing amount chain under the SELF-FUND posture.
///
/// Self-funding must not turn a money-losing amount chain into an encoder
/// underflow: the executor holds the entry capital the stream DECLARES (read
/// from the built Plan — never a universal buffer), so the full terminal output
/// can be captured and the contract's `InsufficientProfit` floor classifies the
/// economics. The initial snapshot must be exactly the applied plan, which is
/// what pins "the declared requirement and nothing else".
#[track_caller]
fn run_losing_self_fund(h: &mut Harness, hops: &[Hop], label: &str) -> ChainResult {
    let result = h
        .run_chain_with_consumed_capital(
            hops,
            100_000,
            &[90_000, 80_000, 70_000],
            &[100_000, 90_000, 80_000],
            40_000_000,
            degenbot_executor::composers::EncodeOptions::default(),
            &FixtureCapital::self_fund(),
        )
        .unwrap_or_else(|e| {
            panic!("[{label}] a losing self-funded chain must produce executable calldata: {e}")
        });
    assert!(
        result.outcome.executed(3),
        "[{label}] all three swaps must execute; the mainnet probe checks InsufficientProfit: {result:?}"
    );
    assert_applied_capital(&result, label);
    result
}

/// V2→V2→V4 is the minimal terminal-V4 case that previously returned no
/// calldata at `out_c.checked_sub(optimal_input)`.
#[test]
fn losing_v2_v2_v4_self_fund_executes_to_profit_assert() {
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V2, Prot::V2, Prot::V4]);
    run_losing_self_fund(&mut h, &hops, "losing v2_v2_v4 self-fund");
}

#[test]
fn losing_v2_v4_v4_self_fund_executes_to_profit_assert() {
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V2, Prot::V4, Prot::V4]);
    run_losing_self_fund(&mut h, &hops, "losing v2_v4_v4 self-fund");
}

#[test]
fn losing_v3_v4_v4_self_fund_executes_to_profit_assert() {
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V3, Prot::V4, Prot::V4]);
    run_losing_self_fund(&mut h, &hops, "losing v3_v4_v4 self-fund");

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
    // No intermediate-token buffer: the self-funded posture installs the entry
    // the stream declares (WETH here) and CLEARS the intermediate it touches, so
    // a reverse direction cannot pass on capital the harness happened to hold.
    run_losing_self_fund(&mut h, &reverse, "reverse v3_v3_v4 self-fund");
}

/// The V4-led `v4_v4_v3` family, which declares a self-funded entry: the posture
/// installs exactly that declaration and must reach its economic receipt with no
/// other holding — the fixture seeds NO WETH and NO intermediate-token buffer, so
/// a magical buffer supplied by the harness cannot mask a missing requirement.
#[test]
fn losing_v4_v4_v3_self_fund_executes_to_profit_assert() {
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V4, Prot::V4, Prot::V3]);
    let intermediate = hops[1].dst;
    // The mainnet terminal WBTC/WETH V3 pool is token0=WBTC, token1=WETH.
    // Cover that reverse direction explicitly; pool_for's forward-only
    // fixture would otherwise hide this command-dispatch axis.
    let _terminal = HopPool::V3(
        h.add_v3_pool(
            intermediate,
            h.weth,
            3000,
            sqrt_x(3),
            liq(),
            1_000_000_000_000_000_000_000_000_000_000,
            1_000_000_000_000_000_000_000_000_000_000,
        )
        .unwrap(),
    );
    let result = run_losing_self_fund(&mut h, &hops, "losing v4_v4_v3 self-fund");
    // The plan declares ERC-20 entry capital only: no native ETH and no
    // PoolManager claim may appear in the executor's initial state.
    for (asset, balance) in result.initial_capital.rows() {
        match asset {
            CapitalAsset::Erc20 { .. } => {}
            _ => assert_eq!(
                *balance,
                U256::ZERO,
                "the declared entry names no {asset}, so it must start at zero"
            ),
        }
    }
}

#[test]
fn losing_v3_v3_v4_self_fund_executes_to_profit_assert() {
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V3, Prot::V3, Prot::V4]);
    run_losing_self_fund(&mut h, &hops, "losing v3_v3_v4 self-fund");
}

/// Zero-balance counterpart to the production-solver matrix.
///
/// The posture CLEARS every native, ERC-20, and PoolManager-claim asset the path
/// touches, so every input must come from the path's own flash/V4 credit graph.
/// This is the positive robustness proof for profitable cycles, complementing
/// the typed refusal for losing cycles.
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
        let reference = h
            .encode_path_with_consumed(
                &path,
                optimal_input,
                &outputs,
                &consumed,
                degenbot_executor::composers::EncodeOptions::default(),
            )
            .unwrap_or_else(|e| panic!("[{}] baseline encode: {e}", c.name));
        // The zero-capital STANCE (`InitialCapital::Zero`, the ledger's one
        // solvency evaluation) must find every profitable row solvent and emit
        // byte-identical commands: the stance never changes a valid stream, it
        // only classifies the ones the path's own credit cannot fund.
        let zero_capital = h
            .encode_path_with_consumed(
                &path,
                optimal_input,
                &outputs,
                &consumed,
                degenbot_executor::composers::EncodeOptions {
                    initial_capital: degenbot_executor::grammar_ledger::InitialCapital::Zero,
                    ..Default::default()
                },
            )
            .unwrap_or_else(|e| panic!("[{}] zero-capital stance declined: {e}", c.name));
        assert_eq!(
            zero_capital, reference,
            "{}: the zero-capital stance changed a valid stream's bytes",
            c.name
        );

        let result = h
            .run_chain_with_consumed_capital(
                &hops,
                optimal_input,
                &outputs,
                &consumed,
                c.gas,
                degenbot_executor::composers::EncodeOptions::default(),
                &FixtureCapital::zero_balance(),
            )
            .unwrap_or_else(|e| panic!("[{}] zero-balance run: {e}", c.name));
        assert_eq!(
            result.capital_plan.mode(),
            ProbeCapital::ZeroBalance,
            "{}: the row must run the zero-balance posture",
            c.name
        );
        // EVERY touched holding is exactly zero after the plan — the assertion
        // the old "no funding installed because nobody funded it" test could not
        // make about claims and native balances.
        assert_zero_capital(&result, &c.name);
        if !result.outcome.executed(3) {
            let (verdict, trace) = degenbot_simulation::oracle::transact_with_trace(
                &mut h.evm,
                degenbot_simulation::oracle::TxSpec::Call {
                    to: h.executor,
                    data: degenbot_simulation::harness::execute_data_config(
                        &zero_capital,
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
                "{} zero-balance failure: {:?}; replay={verdict:?};\n{}",
                c.name,
                result.outcome,
                trace.render_debug(),
            );
        }
        let predicted = i128::try_from(solution.profit.to::<u128>()).unwrap();
        let delta = result.actual_weth_delta;
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
/// flash debt, so the executor eats the loss from the entry capital the stream
/// DECLARES (the `SelfFund` posture installs the built Plan's requirement) —
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
        .run_chain_capital(
            &hops,
            100_000,
            5_000_000,
            degenbot_executor::composers::EncodeOptions::default(),
            &FixtureCapital::self_fund(),
        )
        .unwrap();
    assert_applied_capital(&result, "unprofitable self-fund");
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

// ═══════════════════════════════════════════════════════════════════════════
// Capital postures — the fixture half of the live probe's capital interface
// ═══════════════════════════════════════════════════════════════════════════

/// The self-funded counterpart to the production matrices: every three-hop row
/// installs exactly the entry capital its stream's Plan declares — and clears
/// everything else — then executes to its profit.
///
/// The assertion is [`assert_applied_capital`]: the executor's initial state IS
/// the applied plan, so a row whose requirement went missing (and whose stream
/// then leaned on a universal buffer) fails here rather than passing quietly.
#[test]
fn full_three_hop_self_fund_matrix_executes_from_declared_capital() {
    let mut failures: Vec<String> = Vec::new();
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
        println!("── self-fund {:<14}", c.name);
        let result = match h.run_chain_with_consumed_capital(
            &hops,
            optimal_input,
            &outputs,
            &consumed,
            c.gas,
            degenbot_executor::composers::EncodeOptions::default(),
            &FixtureCapital::self_fund(),
        ) {
            Ok(result) => result,
            Err(e) => {
                failures.push(format!("{}: {e}", c.name));
                continue;
            }
        };
        println!(
            "   plan={:?} outcome={:?} predicted={} actual={}",
            result
                .capital_plan
                .overrides()
                .iter()
                .map(|capital_write| (capital_write.asset(), capital_write.value()))
                .collect::<Vec<_>>(),
            result.outcome,
            result.predicted_profit,
            result.actual_weth_delta
        );
        if !result.outcome.executed(c.swaps) {
            failures.push(format!("{}: {:?}", c.name, result.outcome));
            continue;
        }
        assert_applied_capital(&result, &c.name);
        assert_profitable(&result, c.swaps, &c.name);
    }
    assert!(
        failures.is_empty(),
        "rows that did not execute the self-funded posture:\n{}",
        failures.join("\n")
    );
}

/// The self-funded posture installs exactly what the stream's Plan declares.
///
/// The expectation is read INDEPENDENTLY here — `encode_cmd_stream_capital` over
/// the same `EncodeContext`/`EncodeRequest` the fixture encodes — so the
/// assertion cannot merely restate the runner's own bookkeeping. Every other
/// touched holding must be zero: the posture clears what it does not declare.
#[test]
fn self_fund_initial_capital_matches_the_plan_requirements() {
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V2, Prot::V3]);
    let optimal_input = ENTRY_INPUT;
    let (path, outputs, consumed) = h.path_and_amounts(&hops, optimal_input);
    let request = EncodeRequest::new(
        path.clone(),
        optimal_input,
        outputs.clone(),
        consumed.clone(),
        EncodeOptions {
            funding: FundingSource::SelfFund,
            ..Default::default()
        },
    );
    let requirements = encode_cmd_stream_capital(&h.encode_context(), &request);
    assert!(
        !requirements.is_empty(),
        "v2_v3 declares self-funded entry capital"
    );
    let entry = requirements
        .iter()
        .find(|requirement| requirement.currency == h.weth)
        .expect("the declared entry is WETH");
    assert!(
        entry.amount > 0,
        "a declared requirement is never a zero grant"
    );

    let result = h
        .run_chain_with_consumed_capital(
            &hops,
            optimal_input,
            &outputs,
            &consumed,
            8_000_000,
            EncodeOptions::default(),
            &FixtureCapital::self_fund(),
        )
        .unwrap_or_else(|e| panic!("self-fund run: {e}"));
    assert_eq!(result.capital_plan.mode(), ProbeCapital::SelfFund);
    assert_eq!(result.capital_plan.holder(), Some(h.executor));
    assert_eq!(
        result
            .initial_capital
            .balance_of(CapitalAsset::Erc20 { token: h.weth }),
        U256::from(entry.amount),
        "the executor starts holding exactly the Plan's declared entry"
    );
    for requirement in &requirements {
        let asset = if requirement.currency == alloy::primitives::Address::ZERO {
            CapitalAsset::Native
        } else {
            CapitalAsset::Erc20 {
                token: requirement.currency,
            }
        };
        assert_eq!(
            result.initial_capital.balance_of(asset),
            U256::from(requirement.amount),
            "the declared {asset} is installed"
        );
    }
    assert_applied_capital(&result, "self_fund requirements");
    assert_profitable(&result, 2, "self_fund requirements");
}

/// Production applies NO override: the executor starts from exactly the holdings
/// the fixture seeded — a nonzero WETH control plus a nonzero intermediate-token
/// control — with no universal extra grant and nothing cleared.
#[test]
fn production_initial_capital_is_exactly_the_fixture_seed() {
    const WETH_SEED: u128 = 424_242;
    const TOKEN_SEED: u128 = 777;
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V2, Prot::V3]);
    let intermediate = hops[0].dst;
    let weth = CapitalAsset::Erc20 { token: h.weth };
    let token = CapitalAsset::Erc20 {
        token: intermediate,
    };
    h.fund(h.weth, h.executor, WETH_SEED).unwrap();
    h.fund(intermediate, h.executor, TOKEN_SEED).unwrap();

    let result = h
        .run_chain(&hops, ENTRY_INPUT, 8_000_000)
        .unwrap_or_else(|e| panic!("production run: {e}"));
    assert!(
        result.capital_plan.is_empty(),
        "production applies no capital plan: {:?}",
        result.capital_plan
    );
    assert_eq!(
        result.initial_capital.balance_of(weth),
        U256::from(WETH_SEED),
        "no universal `optimal_input * 2` grant may join the seed"
    );
    assert_eq!(
        result.initial_capital.balance_of(token),
        U256::from(TOKEN_SEED),
        "the fixture's own intermediate-token seed survives"
    );
    assert_eq!(
        result.initial_capital.balance_of(CapitalAsset::Native),
        U256::ZERO
    );
    for (asset, balance) in result.initial_capital.rows() {
        if *asset != weth && *asset != token {
            assert_eq!(
                *balance,
                U256::ZERO,
                "{asset} was granted a holding the fixture never seeded"
            );
        }
    }
    assert_profitable(&result, 2, "production seed control");
    // The path spends what it produced, so an explicitly seeded holding is
    // PRESERVED end to end, not cleared by a posture that claims production.
    assert_eq!(
        result.final_capital.balance_of(token),
        U256::from(TOKEN_SEED),
        "the seeded intermediate is not consumed by the path"
    );
}

/// A losing path under the zero-balance posture is a TYPED capital refusal that
/// precedes execution: the ledger answers `InsolventZeroCapital` with the
/// deficit, and the fixture's state is untouched (nothing installed, nothing
/// run).
#[test]
fn zero_balance_losing_path_is_typed_insolvent_before_execution() {
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
    let refusal = h
        .run_chain_capital(
            &hops,
            ENTRY_INPUT,
            5_000_000,
            EncodeOptions::default(),
            &FixtureCapital::zero_balance(),
        )
        .expect_err("a losing cycle cannot be funded by its own credit");
    match refusal {
        ChainError::InsolventZeroCapital(deficit) => {
            // The WETH leg is covered by the pair's own flash; what the path's
            // credit cannot fund is the SECOND hop's input, so the deficit names
            // the intermediate token and states the shortfall.
            assert_eq!(
                deficit.asset, h.weth,
                "the uncovered debit is the flash repay: {deficit:?}"
            );
            assert!(
                deficit.required > deficit.available,
                "the deficit states what is missing: {deficit:?}"
            );
            assert!(
                deficit.available > 0,
                "the stance grants the path's OWN credit and reports only the shortfall: {deficit:?}"
            );
        }
        other => panic!("expected the typed zero-capital deficit, got {other:?}"),
    }
    // The refusal came BEFORE any EVM work: no capital was installed, nothing ran.
    assert_eq!(h.balance_of(h.weth, h.executor).unwrap(), U256::ZERO);
    assert_eq!(h.pm_balance_of(h.executor, h.weth).unwrap(), U256::ZERO);
    assert_eq!(h.native_balance_of(h.executor).unwrap(), U256::ZERO);
}

/// Final snapshots are exact per-asset balances, never one WETH delta: a
/// zero-balance `v2_v3` run ends holding the amount chain's terminal output,
/// leaves NO intermediate token behind, and acquires neither native ETH nor a
/// PoolManager claim.
#[test]
fn zero_balance_final_snapshots_match_the_amount_chain() {
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V2, Prot::V3]);
    let intermediate = hops[0].dst;
    let optimal_input = ENTRY_INPUT;
    let (_, outputs, consumed) = h.path_and_amounts(&hops, optimal_input);
    let predicted = outputs.last().unwrap().cast_signed() - optimal_input.cast_signed();
    let result = h
        .run_chain_with_consumed_capital(
            &hops,
            optimal_input,
            &outputs,
            &consumed,
            8_000_000,
            EncodeOptions::default(),
            &FixtureCapital::zero_balance(),
        )
        .unwrap_or_else(|e| panic!("zero-balance run: {e}"));
    assert_zero_capital(&result, "zero_balance v2_v3");
    assert_profitable(&result, 2, "zero_balance v2_v3");

    let weth = CapitalAsset::Erc20 { token: h.weth };
    let tolerance = (predicted.abs() / 1000).max(64);
    let final_weth = result
        .final_capital
        .balance_of(weth)
        .to::<u128>()
        .cast_signed();
    assert!(
        (final_weth - predicted).abs() <= tolerance,
        "the WETH the run ends holding ({final_weth}) must be the amount chain's own \
         terminal output ({predicted}), tol {tolerance}"
    );
    assert_eq!(
        result.capital_delta(weth),
        result.actual_weth_delta,
        "the WETH row IS the compatibility delta"
    );
    // The intermediate token is a real touched holding (it flowed through the
    // executor) that ends at exactly zero, as do the native and claim rows.
    assert_eq!(
        result.final_capital.balance_of(CapitalAsset::Erc20 {
            token: intermediate
        }),
        U256::ZERO
    );
    assert_eq!(
        result.final_capital.balance_of(CapitalAsset::Native),
        U256::ZERO
    );
    for (asset, balance) in result.final_capital.rows() {
        if matches!(asset, CapitalAsset::Erc6909Claim { .. }) {
            assert_eq!(*balance, U256::ZERO, "{asset} must stay untouched");
        }
    }
    assert_eq!(
        result.final_capital.rows().len(),
        result.initial_capital.rows().len(),
        "both snapshots describe the same asset set"
    );
}

/// A PARTIALLY consumed hop leaves the surplus intermediate token in the
/// executor's custody — a nonzero intermediate row the final snapshot must
/// account for exactly.
///
/// The V3 hop commits half the V2 hop's output, so the exact remainder
/// (`v2_out − committed`) stays behind; the WETH row still receives the
/// terminal output.
#[test]
fn partial_consumption_keeps_an_exactly_accounted_intermediate_balance() {
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V2, Prot::V3]);
    let intermediate = hops[0].dst;
    let optimal_input = ENTRY_INPUT;
    let (_, outputs, _) = h.path_and_amounts(&hops, optimal_input);
    let v2_out = outputs[0];
    let committed = v2_out / 2;
    let terminal = v3_amount_out(
        match hops[1].pool {
            HopPool::V3(pool) => pool.sqrt_price,
            _ => unreachable!("the fixture's second hop is V3"),
        },
        match hops[1].pool {
            HopPool::V3(pool) => pool.liquidity,
            _ => unreachable!("the fixture's second hop is V3"),
        },
        committed,
        true,
        3000,
    );
    let result = h
        .run_chain_with_consumed_capital(
            &hops,
            optimal_input,
            &[v2_out, terminal],
            &[optimal_input, committed],
            8_000_000,
            EncodeOptions::default(),
            &FixtureCapital::zero_balance(),
        )
        .unwrap_or_else(|e| panic!("partial-consumption run: {e}"));
    assert!(
        result.outcome.executed(2),
        "both pools must execute: {:?}",
        result.outcome
    );
    assert_zero_capital(&result, "partial consumption");
    assert_eq!(
        result.final_capital.balance_of(CapitalAsset::Erc20 {
            token: intermediate
        }),
        U256::from(v2_out - committed),
        "the unspent V2 output stays in the executor's custody, exactly"
    );
    // The stream repays the entry the V2 hop walked in on, so the WETH the run
    // ends holding is the terminal output less that entry — the predicted profit.
    assert_eq!(
        result
            .final_capital
            .balance_of(CapitalAsset::Erc20 { token: h.weth }),
        U256::from(terminal - optimal_input),
        "the run ends holding the terminal output less the repaid entry"
    );
    assert_eq!(
        result.actual_weth_delta,
        i128::try_from(terminal - optimal_input).unwrap()
    );
    assert_eq!(
        result.capital_delta(CapitalAsset::Erc20 {
            token: intermediate
        }),
        i128::try_from(v2_out - committed).unwrap()
    );
}

/// A NON-WETH terminal asset in the snapshot: the ERC-6909 mint capture is a
/// WETH-terminal facility, so a native-terminal request leaves the profit in the
/// executor's own NATIVE custody — a row the WETH-delta measurement cannot see.
///
/// The claim rows for the path's non-WETH currencies (native, and the V4
/// intermediate's token) must stay exactly zero: capturing nothing must not look
/// like capturing something on an asset the vault was never asked to mint.
#[test]
fn native_terminal_capture_lands_in_native_custody_and_no_non_weth_claim() {
    let mut h = Harness::new().unwrap();
    let t = h.add_token().unwrap();
    let native = alloy::primitives::Address::ZERO;
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
            dst: native,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t,
                    native,
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
        .run_chain_with_opts(&hops, 100_000, 8_000_000, opts)
        .unwrap_or_else(|e| panic!("native-terminal capture: {e}"));
    assert!(
        result.outcome.executed(2),
        "the native-terminal cycle must execute: {:?}",
        result.outcome
    );
    let manager = h.pool_manager;
    let claim = |currency| CapitalAsset::Erc6909Claim { manager, currency };
    let tolerance = (result.predicted_profit.abs() / 1000).max(64);
    let custody = result
        .final_capital
        .balance_of(CapitalAsset::Native)
        .to::<u128>()
        .cast_signed();
    assert!(
        (custody - result.predicted_profit).abs() <= tolerance,
        "the profit stays in native custody ({custody} vs predicted {}): {:?}",
        result.predicted_profit,
        result.final_capital.rows()
    );
    for currency in [native, t, h.weth] {
        let claims = result.final_capital.balance_of(claim(currency));
        assert_eq!(
            claims,
            U256::ZERO,
            "no claim is minted on {currency}: the vault capture is WETH-terminal"
        );
    }
    assert_eq!(
        result
            .final_capital
            .balance_of(CapitalAsset::Erc20 { token: t }),
        U256::ZERO
    );
    assert_eq!(
        result
            .final_capital
            .balance_of(CapitalAsset::Erc20 { token: h.weth }),
        U256::ZERO
    );
}

/// The native row of the snapshot: a `ProfitCapture::Native` run withdraws the
/// WETH profit to native ETH, so the profit appears in the native row and NOT in
/// the WETH row — the two are different assets, and the snapshot keeps them so.
#[test]
fn native_capture_lands_in_the_native_snapshot_row() {
    use degenbot_executor::grammar_ledger::ProfitCapture;

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
        capture: ProfitCapture::Native,
        ..Default::default()
    };
    let result = h
        .run_chain_with_opts(&hops, 100_000, 8_000_000, opts)
        .unwrap_or_else(|e| panic!("native capture: {e}"));
    assert!(
        result.outcome.executed(2),
        "the native-capture cycle must execute: {:?}",
        result.outcome
    );
    let tolerance = (result.predicted_profit.abs() / 1000).max(64);
    let native = result
        .final_capital
        .balance_of(CapitalAsset::Native)
        .to::<u128>()
        .cast_signed();
    assert!(
        (native - result.predicted_profit).abs() <= tolerance,
        "the withdrawn profit is the native row ({native} vs predicted {})",
        result.predicted_profit
    );
    assert_eq!(
        result
            .final_capital
            .balance_of(CapitalAsset::Erc20 { token: h.weth }),
        U256::ZERO,
        "the profit left WETH for native"
    );
    assert_eq!(
        result.capital_delta(CapitalAsset::Native) - native,
        0,
        "the native row's initial balance is the run's entry (zero here)"
    );
}

/// A raw payload's SelfFund posture installs exactly its DECLARED requirement:
/// the intermediate token the old raw runner granted unconditionally stays at
/// exactly zero.
#[test]
fn raw_self_fund_payload_installs_only_its_declared_requirement() {
    use degenbot_executor::grammar_ledger::FundingSource;

    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V2, Prot::V2]);
    let intermediate = hops[0].dst;
    let (path, outputs, consumed) = h.path_and_amounts(&hops, 100_000);
    let payload = h
        .encode_path_with_consumed(
            &path,
            100_000,
            &outputs,
            &consumed,
            EncodeOptions {
                funding: FundingSource::SelfFund,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("raw self-fund encode: {e}"));
    let capital = FixtureCapital::self_fund_declared([CapitalRequirement {
        currency: h.weth,
        amount: 100_000,
    }]);
    let result = h
        .run_raw_payload_capital(
            &hops,
            &payload,
            100_000,
            8_000_000,
            U256::from(1u8),
            &capital,
        )
        .unwrap_or_else(|e| panic!("raw self-fund run: {e}"));
    assert_applied_capital(&result, "raw self-fund");
    assert_eq!(
        result
            .initial_capital
            .balance_of(CapitalAsset::Erc20 { token: h.weth }),
        U256::from(100_000u64)
    );
    assert_eq!(
        result.initial_capital.balance_of(CapitalAsset::Erc20 {
            token: intermediate
        }),
        U256::ZERO,
        "the raw runner grants no intermediate buffer"
    );
    assert!(
        result.outcome.executed(2),
        "the declared entry funds the stream: {:?}",
        result.outcome
    );
}

/// A raw SelfFund payload with NO declared requirement fails instead of falling
/// back to a universal grant: the V3-led stream spends its entry, so an empty
/// declaration leaves nothing to spend and the payload cannot execute.
#[test]
fn raw_self_fund_payload_without_a_declared_requirement_fails() {
    use degenbot_executor::grammar_ledger::FundingSource;

    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V3, Prot::V2]);
    let (path, outputs, consumed) = h.path_and_amounts(&hops, 100_000);
    let payload = h
        .encode_path_with_consumed(
            &path,
            100_000,
            &outputs,
            &consumed,
            EncodeOptions {
                funding: FundingSource::SelfFund,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("raw self-fund encode: {e}"));
    let result = h
        .run_raw_payload_capital(
            &hops,
            &payload,
            100_000,
            8_000_000,
            U256::from(1u8),
            &FixtureCapital::self_fund_declared([]),
        )
        .unwrap_or_else(|e| {
            panic!("the refusal must be a classified execution, not a fixture error: {e}")
        });
    assert!(
        !result.outcome.executed(2),
        "an undeclared entry must not execute: {:?}",
        result.outcome
    );
    assert_zero_capital(&result, "undeclared raw self-fund");
    // And declaring it executes: the difference is only the capital, never bytes.
    let mut h2 = Harness::new().unwrap();
    let hops2 = build_family(&mut h2, &[Prot::V3, Prot::V2]);
    let (path2, outputs2, consumed2) = h2.path_and_amounts(&hops2, 100_000);
    let payload2 = h2
        .encode_path_with_consumed(
            &path2,
            100_000,
            &outputs2,
            &consumed2,
            EncodeOptions {
                funding: FundingSource::SelfFund,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("raw self-fund encode: {e}"));
    assert_eq!(
        payload2, payload,
        "the capital posture never changes the bytes"
    );
    let declared = FixtureCapital::self_fund_declared([CapitalRequirement {
        currency: h2.weth,
        amount: 100_000,
    }]);
    let funded = h2
        .run_raw_payload_capital(
            &hops2,
            &payload2,
            100_000,
            8_000_000,
            U256::from(1u8),
            &declared,
        )
        .unwrap_or_else(|e| panic!("raw self-fund run: {e}"));
    assert!(
        funded.outcome.executed(2),
        "the declared entry is the only difference: {:?}",
        funded.outcome
    );
}

/// A raw payload cannot read a Plan, so the SelfFund posture refuses a
/// Plan-derived requirement source instead of guessing an amount from the bytes
/// or the hop list.
#[test]
fn raw_payload_self_fund_requires_declared_requirements() {
    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V2, Prot::V2]);
    let (path, outputs, consumed) = h.path_and_amounts(&hops, 100_000);
    let payload = h
        .encode_path_with_consumed(
            &path,
            100_000,
            &outputs,
            &consumed,
            EncodeOptions::default(),
        )
        .unwrap_or_else(|e| panic!("raw encode: {e}"));
    let refusal = h
        .run_raw_payload_capital(
            &hops,
            &payload,
            100_000,
            8_000_000,
            U256::from(1u8),
            &FixtureCapital::self_fund(),
        )
        .expect_err("a raw payload cannot declare a Plan");
    assert!(matches!(refusal, ChainError::Setup(_)), "{refusal:?}");
    assert!(
        refusal.to_string().contains("must be declared"),
        "the refusal names the fix: {refusal}"
    );
}

/// The fixture declares its OWN storage layouts: the stub token's `balanceOf` at
/// slot 3 and the stub PoolManager's ERC-6909 `balanceOf` at slot 6.
///
/// Neither is the canonical mainnet slot the production layout set pins, so a
/// capital plan aimed at a lab contract has to declare them. Seeding each slot
/// and reading it back BOTH through the contract's own `balanceOf` and as the raw
/// storage word proves the numbers are the ones the deployed stubs read and
/// write — and that the production slot is not where the stub keeps anything.
#[test]
fn fixture_capital_layouts_address_the_deployed_stubs() {
    let mut h = Harness::new().unwrap();
    let token = h.add_token().unwrap();
    let executor = h.executor;
    let manager = h.pool_manager;
    let holder_word = U256::from_be_bytes(executor.into_word().into());

    let token_slot =
        degenbot_executor::mapping_slot(U256::from(STUB_TOKEN_BALANCE_OF_SLOT), holder_word);
    degenbot_simulation::seed_slots(&mut h.evm, token, &[(token_slot, U256::from(777u64))]);
    assert_eq!(
        h.balance_of(token, executor).unwrap(),
        U256::from(777u64),
        "the stub token's balanceOf lives at the declared slot"
    );

    let claim_slot = degenbot_executor::nested_mapping_slot(
        U256::from(STUB_POOL_MANAGER_ERC6909_BALANCE_OF_SLOT),
        holder_word,
        degenbot_executor::erc6909_id(token),
    );
    degenbot_simulation::seed_slots(&mut h.evm, manager, &[(claim_slot, U256::from(1234u64))]);
    assert_eq!(
        h.pm_balance_of(executor, token).unwrap(),
        U256::from(1234u64),
        "the stub PoolManager's ERC-6909 balanceOf lives at the declared slot"
    );
    assert_eq!(
        h.storage_word(manager, claim_slot).unwrap(),
        U256::from(1234u64),
        "and the raw word at that slot is the seeded claim"
    );
    let production_claim_slot = degenbot_executor::nested_mapping_slot(
        U256::from(degenbot_executor::POOL_MANAGER_ERC6909_BALANCE_OF_SLOT),
        holder_word,
        degenbot_executor::erc6909_id(token),
    );
    assert_eq!(
        h.storage_word(manager, production_claim_slot).unwrap(),
        U256::ZERO,
        "the canonical mainnet v4-core slot is not where the stub keeps claims, which \
         is why the fixture must declare its own"
    );

    let layouts = h.capital_layouts();
    assert_eq!(
        layouts.erc20(token).map(|layout| layout.balance_slot),
        Some(STUB_TOKEN_BALANCE_OF_SLOT)
    );
    assert_eq!(
        layouts.erc20(h.weth).map(|layout| layout.balance_slot),
        Some(STUB_TOKEN_BALANCE_OF_SLOT),
        "the fixture's WETH is a stub token too"
    );
    assert_eq!(
        layouts.claim(manager).map(|layout| layout.balance_slot),
        Some(STUB_POOL_MANAGER_ERC6909_BALANCE_OF_SLOT)
    );
}

/// The stub WRITES its ERC-6909 claims at the declared slot: a real capture run's
/// contract-side write is readable as the raw word at
/// `(manager, holder, id)` under slot 6, equals the contract's own
/// `balanceOf` answer, and is absent at the production layout's slot.
#[test]
fn stub_pool_manager_writes_claims_at_the_declared_slot() {
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
    let opts = EncodeOptions {
        erc6909_profit: true,
        ..Default::default()
    };
    let result = h
        .run_chain_with_opts(&hops, 100_000, 8_000_000, opts)
        .unwrap_or_else(|e| panic!("claim-layout capture: {e}"));
    assert_erc6909_capture(&result, 2, "claim layout");

    let manager = h.pool_manager;
    let executor_word = U256::from_be_bytes(h.executor.into_word().into());
    let id = degenbot_executor::erc6909_id(h.weth);
    let declared = degenbot_executor::nested_mapping_slot(
        U256::from(STUB_POOL_MANAGER_ERC6909_BALANCE_OF_SLOT),
        executor_word,
        id,
    );
    let written = h.storage_word(manager, declared).unwrap();
    assert!(
        written > U256::ZERO,
        "the capture run's claim is written at the declared slot"
    );
    assert_eq!(
        written,
        h.pm_balance_of(h.executor, h.weth).unwrap(),
        "the raw word IS the contract's own answer"
    );
    let production = degenbot_executor::nested_mapping_slot(
        U256::from(degenbot_executor::POOL_MANAGER_ERC6909_BALANCE_OF_SLOT),
        executor_word,
        id,
    );
    assert_eq!(
        h.storage_word(manager, production).unwrap(),
        U256::ZERO,
        "nothing is written at the production layout's slot"
    );
}

/// The zero-balance posture is the LIVE projection: it forces the in-path-flash
/// STREAM axis together with the zero-capital POLICY, so a caller handing it
/// self-funded options runs the flash stream.
///
/// Starting from `EncodeOptions { funding: SelfFund, .. }`:
///
/// * swapping only the validation stance is NOT the mode — the self-funded stream
///   declares entry capital that stance withholds, so the intake declines it
///   before execution;
/// * the posture's own run executes the flash stream from exact zeros, so the
///   mode is never "insolvent by accident" over a stream it does not drive.
#[test]
fn zero_balance_posture_forces_the_in_path_flash_stream_axis() {
    use degenbot_executor::grammar_ledger::InitialCapital;

    let mut h = Harness::new().unwrap();
    let hops = build_family(&mut h, &[Prot::V2, Prot::V2]);
    let (path, outputs, consumed) = h.path_and_amounts(&hops, 100_000);
    let caller_opts = EncodeOptions {
        funding: FundingSource::SelfFund,
        ..Default::default()
    };

    // Not the mode: the caller's self-funded stream axis under the zero stance.
    let accidental = h.encode_path_with_consumed(
        &path,
        100_000,
        &outputs,
        &consumed,
        EncodeOptions {
            initial_capital: InitialCapital::Zero,
            ..caller_opts
        },
    );
    assert!(
        accidental.is_err(),
        "a self-funded stream is insolvent under the zero stance by construction: {accidental:?}"
    );

    // The mode: the flash stream plus the zero policy, executed from exact zeros.
    let result = h
        .run_chain_with_consumed_capital(
            &hops,
            100_000,
            &outputs,
            &consumed,
            8_000_000,
            caller_opts,
            &FixtureCapital::zero_balance(),
        )
        .unwrap_or_else(|e| panic!("zero-balance posture: {e}"));
    assert!(
        result.outcome.executed(2),
        "the flash stream runs from exact zeros: {:?}",
        result.outcome
    );
    assert_eq!(result.capital_plan.mode(), ProbeCapital::ZeroBalance);
    assert_zero_capital(&result, "zero_balance forced flash");

    // The two stream axes are observably different streams, so "which one ran"
    // is a question with an answer.
    let flash = h
        .encode_path_with_consumed(
            &path,
            100_000,
            &outputs,
            &consumed,
            EncodeOptions {
                funding: FundingSource::InPathFlash,
                initial_capital: InitialCapital::Zero,
                ..caller_opts
            },
        )
        .expect("the flash stream encodes");
    let self_funded = h
        .encode_path_with_consumed(&path, 100_000, &outputs, &consumed, caller_opts)
        .expect("the self-funded stream encodes");
    assert_ne!(
        flash, self_funded,
        "the stream axes differ, so the posture's choice is observable"
    );
}
