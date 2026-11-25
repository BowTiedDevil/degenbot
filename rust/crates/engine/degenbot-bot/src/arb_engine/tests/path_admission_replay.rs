#![cfg(feature = "otel")]
#![expect(clippy::print_stdout, clippy::expect_used, clippy::too_many_lines)]
//! ADR-107 D10 certification replay — the matched pre-cutover/post-cutover run.
//!
//! One corpus and one measurement method, run against the pre-implementation
//! commit and against the final tree. The run prints one record per corpus
//! candidate plus every watched Prometheus series the production instruments
//! produced; the evidence document records both outputs.
//!
//! The corpus is fixed here, never discovered: the crawl's DB path graph names
//! only V2/V3/V4 (ADR-107 C7), so the solve-only families reach admission
//! through the direct `register_path` door — the live guard ADR-107 D4 names.
//!
//! The run owns its process: the watched families have to describe this replay
//! alone, so a co-runner's samples can never enter the assertions.

use std::collections::BTreeSet;
use std::sync::Arc;

use alloy::primitives::aliases::U112;
use alloy::primitives::{Address, U256};
use degenbot_pools::solve::{HopType, MixedPoolRef, PoolHop};
use degenbot_pools::{PoolTickCoverage, RegisterV3PoolParams, RegisterV4PoolParams, V4PoolKey};
use degenbot_substrate::state_lock::LockSite;
use degenbot_uniswap::dex_identity::DexVariant;
use hashbrown::HashMap;

use crate::arb_engine::delivery_policy::set_profit_thresholds;
use crate::arb_engine::inline_sim::{InlineSimRequest, InlineSimulator};
use crate::arb_engine::lifecycle::{latest_results, path_count, register_path};
use crate::arb_engine::test_harness::run_test_cycle;
use crate::arb_engine::{ArbitrageEngine, BlockMetadata, SimulatedPathResult};
use crate::instruments::PipelineInstruments;
use degenbot_pools::AffectedKey;

const REPLAY_BLOCK: u64 = 100;
const REPLAY_MODE_ENV: &str = "DEGENBOT_ADMISSION_REPLAY_MODE";
const REPLAY_MODE_ISOLATED: &str = "isolated";
const REPLAY_TEST_SUFFIX: &str = "::path_admission_replay::path_admission_replay";
const CO_RUNNER_SOLVE_SAMPLES: usize = 117;
const SOLVE_EVALUATIONS_SERIES: &str = "degenbot_solve_path_duration_seconds_count";
const T0: Address = Address::new([0x01; 20]);
const T1: Address = Address::new([0x02; 20]);
const V4_POOL_MANAGER: Address = Address::new([0x40; 20]);
const CL_LIQUIDITY: u128 = 1_000_000_000_000_000_000_000;

fn t18(n: u64) -> U112 {
    (U256::from(n) * U256::from(10u64).pow(U256::from(18u64))).to::<U112>()
}

fn t18_u256(n: u64) -> U256 {
    U256::from(n) * U256::from(10u64).pow(U256::from(18u64))
}

fn fee() -> degenbot_core::fee::FeeRatio {
    degenbot_core::fee::FeeRatio::new(997, 1000).expect("corpus: retained fee fraction")
}

fn charged_fee() -> degenbot_core::fee::V2Fee {
    degenbot_core::fee::fee_fraction(3, 1000)
}

/// One corpus row: the route, and the family of each of its hops.
struct Row {
    name: &'static str,
    hops: Vec<PoolHop>,
    families: Vec<HopType>,
}

struct Corpus {
    engine: ArbitrageEngine,
    rows: Vec<Row>,
    pools: Vec<(HopType, u64)>,
}

fn v2_pool(engine: &ArbitrageEngine, address: Address, reserve0: u64, reserve1: u64) -> u64 {
    engine
        .core
        .write_at(LockSite::Solver)
        .register_v2_pool(&degenbot_pools::RegisterV2PoolParams {
            address,
            token0: T0,
            token1: T1,
            reserve0: t18(reserve0),
            reserve1: t18(reserve1),
            fee_token0: fee(),
            fee_token1: fee(),
            factory: Address::new([0xfa; 20]),
            update_block: 0,
            variant: DexVariant::UniswapV2,
            stable_swap: false,
            deployer: Address::ZERO,
            init_hash: alloy::primitives::B256::ZERO,
        })
        .expect("corpus: V2 registration")
}

/// Concentrated-liquidity tick data that actually carries in-range liquidity:
/// one initialized tick below the current price and one above it, both with the
/// full in-range depth, so a swap inside the width neither exhausts the active
/// liquidity nor walks into an empty range.
fn cl_tick_data(liquidity: u128) -> HashMap<i32, degenbot_pools::TickInfo> {
    let mut tick_data = HashMap::new();
    tick_data.insert(
        -60,
        degenbot_pools::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(liquidity),
            liquidity_net: i128::try_from(liquidity).expect("corpus: liquidity fits i128"),
            block: 0,
        },
    );
    tick_data.insert(
        60,
        degenbot_pools::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(liquidity),
            liquidity_net: -i128::try_from(liquidity).expect("corpus: liquidity fits i128"),
            block: 0,
        },
    );
    tick_data
}

fn v3_pool(engine: &ArbitrageEngine, address: Address) -> u64 {
    engine.register_v3_pool(&RegisterV3PoolParams {
        address,
        token0: T0,
        token1: T1,
        fee: 500,
        tick_spacing: 10,
        sqrt_price_x96: U256::from(1u64) << 96,
        tick: 0,
        liquidity: CL_LIQUIDITY,
        tick_data: cl_tick_data(CL_LIQUIDITY),
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        ..Default::default()
    })
}

fn v4_pool(engine: &ArbitrageEngine, pool_id: [u8; 32]) -> u64 {
    engine
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager: V4_POOL_MANAGER,
            pool_id,
            pool_key: V4PoolKey {
                currency0: T0,
                currency1: T1,
                fee: 3_000,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            sqrt_price_x96: U256::from(1u64) << 96,
            liquidity: CL_LIQUIDITY,
            tick: 0,
            tick_data: cl_tick_data(CL_LIQUIDITY),
            coverage: PoolTickCoverage::Tracked,
            ..Default::default()
        })
        .expect("corpus: V4 registration")
}

fn aero_pool(engine: &ArbitrageEngine, address: Address, reserve0: u64, reserve1: u64) -> u64 {
    engine
        .core
        .write_at(LockSite::Solver)
        .register_aerodrome_pool(&degenbot_pools::RegisterAerodromeV2PoolParams {
            token0_decimals: 18,
            token1_decimals: 18,
            address,
            token0: T0,
            token1: T1,
            factory: Address::new([0xfa; 20]),
            variant: DexVariant::AerodromeV2Stable,
            stable: true,
            fee: charged_fee(),
            reserve0: t18(reserve0),
            reserve1: t18(reserve1),
            update_block: 0,
        })
        .expect("corpus: Aerodrome registration")
}

fn balancer_weighted_pool(
    engine: &ArbitrageEngine,
    address: Address,
    balance0: u64,
    balance1: u64,
) -> u64 {
    engine
        .core
        .write_at(LockSite::Solver)
        .register_balancer_weighted_pool(&degenbot_pools::RegisterBalancerWeightedPoolParams {
            address,
            vault: Address::new([0xba; 20]),
            pool_id: [0u8; 32],
            tokens: vec![T0, T1],
            weights: vec![
                U256::from(500_000_000_000_000_000u128),
                U256::from(500_000_000_000_000_000u128),
            ],
            scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
            swap_fee: 1_000_000_000_000_000u128,
            pow_version: 2,
            balances: vec![t18_u256(balance0), t18_u256(balance1)],
            update_block: 0,
        })
        .expect("corpus: Balancer weighted registration")
}

fn balancer_stable_pool(
    engine: &ArbitrageEngine,
    address: Address,
    balance0: u64,
    balance1: u64,
) -> u64 {
    engine
        .core
        .write_at(LockSite::Solver)
        .register_balancer_stable_pool(&degenbot_pools::RegisterBalancerStablePoolParams {
            address,
            vault: Address::new([0xba; 20]),
            pool_id: [0u8; 32],
            tokens: vec![T0, T1],
            amp: 200_000,
            scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
            swap_fee: 10_000_000_000_000u128,
            bpt_idx: None,
            invariant_version: 2,
            balances: vec![t18_u256(balance0), t18_u256(balance1)],
            update_block: 0,
            rate_provider: None,
        })
        .expect("corpus: Balancer stable registration")
}

fn curve_pool(engine: &ArbitrageEngine, address: Address, balance0: u64, balance1: u64) -> u64 {
    let one_e18 = t18_u256(1);
    engine
        .core
        .write_at(LockSite::Solver)
        .register_curve_pool(&degenbot_pools::RegisterCurvePoolParams {
            address,
            tokens: vec![T0, T1],
            a_coefficient: 10,
            a_precision: 100,
            fee: 4_000_000,
            admin_fee: 0,
            rate_multipliers: vec![one_e18, one_e18],
            balances: vec![t18_u256(balance0), t18_u256(balance1)],
            update_block: 0,
            swap_style: degenbot_pools::SwapStyle::Standard,
            lending_rate_style: degenbot_pools::LendingRateStyle::None,
            d_variant: degenbot_pools::DVariant::Standard,
            y_variant: degenbot_pools::YVariant::Standard,
            yd_variant: degenbot_pools::YDVariant::Standard,
            base_pool: None,
            initial_a_coefficient: None,
            future_a_coefficient: None,
            initial_a_coefficient_time: None,
            future_a_coefficient_time: None,
            create_timestamp: None,
            fee_gamma: None,
            mid_fee: None,
            offpeg_fee_multiplier: None,
            out_fee: None,
            gamma: None,
            lp_token: None,
            use_lending: vec![false, false],
            precision_multipliers: vec![one_e18, one_e18],
            tokens_underlying: None,
            metapool_rate_style: degenbot_pools::MetapoolRateStyle::Standard,
            metapool_underlying_style: degenbot_pools::MetapoolUnderlyingStyle::Standard,
            data_provider: None,
        })
        .expect("corpus: Curve registration")
}

fn hop(pool_id: u64, zero_for_one: bool) -> PoolHop {
    PoolHop {
        pool_id,
        zero_for_one,
    }
}

fn corpus() -> Corpus {
    let engine = ArbitrageEngine::new();
    {
        let mut core = engine.core.write_at(LockSite::Solver);
        core.register_token(T0, "Token0".into(), "T0".into(), 18, 1);
        core.register_token(T1, "Token1".into(), "T1".into(), 18, 1);
    }
    let v2_a = v2_pool(&engine, Address::new([0x11; 20]), 1_000, 2_000);
    let v2_b = v2_pool(&engine, Address::new([0x12; 20]), 1_000, 1_950);
    let v2_c = v2_pool(&engine, Address::new([0x13; 20]), 1_000, 2_000);
    let v2_d = v2_pool(&engine, Address::new([0x14; 20]), 1_000, 1_950);
    let v3_a = v3_pool(&engine, Address::new([0x21; 20]));
    let v3_b = v3_pool(&engine, Address::new([0x22; 20]));
    let v4_a = v4_pool(&engine, [0x31; 32]);
    let aero_a = aero_pool(&engine, Address::new([0x41; 20]), 1_000, 100);
    let aero_b = aero_pool(&engine, Address::new([0x42; 20]), 2_000, 100);
    let weighted_a = balancer_weighted_pool(&engine, Address::new([0x51; 20]), 1_000, 2_000);
    let weighted_b = balancer_weighted_pool(&engine, Address::new([0x52; 20]), 1_000, 1_950);
    let stable_a = balancer_stable_pool(&engine, Address::new([0x53; 20]), 1_000, 1_001);
    let stable_b = balancer_stable_pool(&engine, Address::new([0x54; 20]), 1_000, 999);
    let curve_a = curve_pool(&engine, Address::new([0x61; 20]), 1_000, 2_000);
    let curve_b = curve_pool(&engine, Address::new([0x62; 20]), 1_000, 1_950);

    let rows = vec![
        Row {
            name: "solidly_2hop",
            hops: vec![hop(aero_a, true), hop(aero_b, false)],
            families: vec![HopType::SolidlyStable, HopType::SolidlyStable],
        },
        Row {
            name: "balancer_weighted_2hop",
            hops: vec![hop(weighted_a, true), hop(weighted_b, false)],
            families: vec![HopType::BalancerWeighted, HopType::BalancerWeighted],
        },
        Row {
            name: "balancer_stable_2hop",
            hops: vec![hop(stable_a, true), hop(stable_b, false)],
            families: vec![HopType::BalancerStable, HopType::BalancerStable],
        },
        Row {
            name: "curve_2hop",
            hops: vec![hop(curve_a, true), hop(curve_b, false)],
            families: vec![HopType::CurveStableswap, HopType::CurveStableswap],
        },
        Row {
            name: "solidly_v3_mixed",
            hops: vec![hop(aero_a, true), hop(v3_a, false)],
            families: vec![HopType::SolidlyStable, HopType::V3],
        },
        Row {
            name: "v2_v3_4hop_out_of_shape",
            hops: vec![
                hop(v2_a, true),
                hop(v3_a, false),
                hop(v2_c, true),
                hop(v3_b, false),
            ],
            families: vec![HopType::V2, HopType::V3, HopType::V2, HopType::V3],
        },
        Row {
            name: "v2_v3_heterogeneous_2hop",
            hops: vec![hop(v2_a, true), hop(v3_a, false)],
            families: vec![HopType::V2, HopType::V3],
        },
        Row {
            name: "v2_v4_heterogeneous_2hop",
            hops: vec![hop(v2_a, true), hop(v4_a, false)],
            families: vec![HopType::V2, HopType::V4],
        },
        Row {
            name: "v2_all_4hop",
            hops: vec![
                hop(v2_a, true),
                hop(v2_b, false),
                hop(v2_c, true),
                hop(v2_d, false),
            ],
            families: vec![HopType::V2; 4],
        },
        Row {
            name: "v2_compose_capable_2hop",
            hops: vec![hop(v2_c, true), hop(v2_d, false)],
            families: vec![HopType::V2, HopType::V2],
        },
    ];
    let pools = vec![
        (HopType::V2, v2_a),
        (HopType::V2, v2_b),
        (HopType::V2, v2_c),
        (HopType::V2, v2_d),
        (HopType::V3, v3_a),
        (HopType::V3, v3_b),
        (HopType::V4, v4_a),
        (HopType::SolidlyStable, aero_a),
        (HopType::SolidlyStable, aero_b),
        (HopType::BalancerWeighted, weighted_a),
        (HopType::BalancerWeighted, weighted_b),
        (HopType::BalancerStable, stable_a),
        (HopType::BalancerStable, stable_b),
        (HopType::CurveStableswap, curve_a),
        (HopType::CurveStableswap, curve_b),
    ];
    Corpus {
        engine,
        rows,
        pools,
    }
}

/// The replay's hook: a payload for every composed route except the one path
/// whose pool de-registered between solve and dispatch — the production
/// hook-refusal cause, injected by the corpus rather than by a blanket stub.
struct ReplayHook {
    refuse: BTreeSet<u64>,
}

/// Whether the command-stream encoder has an arm for a hop family. The real
/// hook projects the whole route into `PathInfo` before it can simulate
/// anything, so a route containing a solve-only family produces no payload —
/// a property of the hook, not of this replay.
const fn encodable(hop: HopType) -> bool {
    matches!(hop, HopType::V2 | HopType::V3 | HopType::V4)
}

/// The encoder's whole-route shape key: all-V2 any-N, otherwise 2-3 hops.
fn route_has_producer(hops: &[MixedPoolRef]) -> bool {
    let all_v2 = hops.iter().all(|hop| hop.hop_type == HopType::V2);
    all_v2 || hops.len() <= 3
}

impl InlineSimulator for ReplayHook {
    fn simulate_path(&self, request: InlineSimRequest) -> Option<SimulatedPathResult> {
        if self.refuse.contains(&request.path_id)
            || request.hops.iter().any(|hop| !encodable(hop.hop_type))
            || !route_has_producer(&request.hops)
        {
            return None;
        }
        let output = request.hop_outputs.last().copied().unwrap_or_default();
        Some(SimulatedPathResult {
            path_id: request.path_id,
            gross_profit: output,
            net_profit: output,
            gas_used: 200_000,
            priority_fee: 1,
            base_fee_next: 1,
            execute_calldata: Vec::new(),
            access_list: None,
            captured_swaps: Vec::new(),
            hop_count: request.hops.len(),
            failure: None,
        })
    }
}

fn record_co_runner_solve_samples() {
    let meter = crate::metrics::try_global_meter().expect("the replay metrics pipeline is running");
    let instruments = PipelineInstruments::new(&meter);
    for _ in 0..CO_RUNNER_SOLVE_SAMPLES {
        instruments.observe_per_path_solve_duration(0.001);
    }
}

fn scrape(addr: std::net::SocketAddr) -> String {
    use std::io::{Read as _, Write as _};
    crate::metrics::flush_global_metrics();
    let mut stream = std::net::TcpStream::connect(addr).expect("scrape endpoint connects");
    stream
        .write_all(b"GET /metrics HTTP/1.0\r\nHost: replay\r\n\r\n")
        .expect("scrape request writes");
    let mut text = String::new();
    stream.read_to_string(&mut text).expect("scrape reads");
    match text.split_once("\r\n\r\n") {
        Some((_, body)) => body.to_owned(),
        None => text,
    }
}

fn default_expectation(key: &str) -> &'static str {
    match key {
        "DEGENBOT_REPLAY_EXPECT_REGISTERED_PATHS"
        | "DEGENBOT_REPLAY_EXPECT_INLINE_CANDIDATES"
        | "DEGENBOT_REPLAY_EXPECT_SOLVE_EVALUATIONS" => "4",
        "DEGENBOT_REPLAY_EXPECT_UNSUPPORTED_ADMISSIONS" => "6",
        "DEGENBOT_REPLAY_EXPECT_HOOK_REFUSED" => "1",
        "DEGENBOT_REPLAY_EXPECT_NO_ENCODER_ARM" => "0",
        other => panic!("{other} has no default expectation"),
    }
}

fn env_usize(key: &str) -> usize {
    let raw = std::env::var(key).unwrap_or_else(|_| default_expectation(key).to_owned());
    raw.parse()
        .unwrap_or_else(|_| panic!("{key} must parse as an integer"))
}

/// Sum every sample of `family` whose line satisfies `keep`. A family with no
/// matching line sums to 0 — the absent-series case, which the expected-value
/// environment variable still distinguishes from a real measurement.
fn series_sum(text: &str, family: &str, keep: impl Fn(&str) -> bool) -> usize {
    text.lines()
        .filter(|line| line.starts_with(family) && keep(line))
        .map(|line| {
            line.rsplit(' ')
                .next()
                .unwrap_or("0")
                .parse::<usize>()
                .expect("a counter sample is an integer")
        })
        .sum()
}

/// The four supported rows' golden solves: `(row, optimal_input, profit,
/// hop_outputs, inline payload)`. Asserted on every run, so supported-path
/// parity can never pass through an unsolved (missing) row.
const GOLDEN_SUPPORTED: [(&str, &str, &str, &str, bool); 4] = [
    (
        "v2_v3_heterogeneous_2hop",
        "1509719849144939995",
        "1485635106765834594",
        "3005856991237538038,2995354955910774589",
        true,
    ),
    (
        "v2_v4_heterogeneous_2hop",
        "1513511218182397611",
        "1481843737728371780",
        "3013394245478349086,2995354955910769391",
        true,
    ),
    (
        "v2_all_4hop",
        "4787558748146992357",
        "93340209480539668",
        "9501041808366426220,4834228852887262191,9593215660120882892,4880898957627532025",
        true,
    ),
    (
        "v2_compose_capable_2hop",
        "4810893256776020769",
        "46671192222482844",
        "9547128727446589320,4857564448998503613",
        false,
    ),
];

const WATCHED: [&str; 7] = [
    "degenbot_sim_degrades_total",
    "degenbot_candidates_found_total",
    "degenbot_registration_skips_total",
    "degenbot_engine_registered_paths",
    "degenbot_solves_executed_total",
    "degenbot_solve_path_duration_seconds_count",
    "degenbot_selection_all_targets_declined_total",
];

fn init_replay_metrics() -> std::net::SocketAddr {
    let init = crate::metrics::init_global_metrics_with_addr(
        "127.0.0.1:0".parse().expect("ephemeral endpoint"),
    )
    .expect("the replay owns the process metrics pipeline");
    match &init {
        crate::metrics::MetricsInit::Started { endpoint }
        | crate::metrics::MetricsInit::AlreadyRunning { endpoint } => endpoint.addr(),
    }
}

fn replay_binary() -> std::process::Command {
    std::process::Command::new(std::env::current_exe().expect("test executable path"))
}

fn replay_test_name() -> String {
    let output = replay_binary()
        .arg("--list")
        .output()
        .expect("the test binary lists its own tests");
    let listing = String::from_utf8_lossy(&output.stdout);
    listing
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .find(|name| name.ends_with(REPLAY_TEST_SUFFIX))
        .expect("the test binary lists the admission replay test")
        .to_owned()
}

fn run_isolated_replay() {
    let name = replay_test_name();
    let output = replay_binary()
        .args(["--exact", name.as_str(), "--nocapture", "--test-threads=1"])
        .env(REPLAY_MODE_ENV, REPLAY_MODE_ISOLATED)
        .output()
        .expect("the isolated replay process runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the isolated replay failed with {}\nstdout:\n{stdout}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("1 passed"),
        "the isolated process ran no replay test: {stdout}"
    );
    println!("{stdout}");
}

/// The watched families must describe the replay alone. This control assembles
/// the co-runner the isolation exists for: the same program, a dirty
/// process-global registry, and no isolation. That run has to fail while the
/// isolated child passes — the only difference between the two is the foreign
/// samples, so a green in-process run means the replay stopped reading the
/// process-global sum, and a failure in both means the difference was not the
/// contamination.
#[test]
fn path_admission_capture_fixture_is_process_global_contamination_free() {
    let addr = init_replay_metrics();
    record_co_runner_solve_samples();
    assert!(
        series_sum(&scrape(addr), SOLVE_EVALUATIONS_SERIES, |_| true) >= CO_RUNNER_SOLVE_SAMPLES,
        "the co-runner samples never reached the process-global registry"
    );

    let in_process = std::panic::catch_unwind(path_admission_replay_body);
    assert!(
        in_process.is_err(),
        "the in-process replay passed with foreign samples in the registry"
    );

    run_isolated_replay();
}

#[test]
fn path_admission_replay() {
    match std::env::var(REPLAY_MODE_ENV).as_deref() {
        Ok(REPLAY_MODE_ISOLATED) => path_admission_replay_body(),
        Ok(other) => panic!("{REPLAY_MODE_ENV}={other} is not a replay mode"),
        Err(std::env::VarError::NotPresent) => run_isolated_replay(),
        Err(std::env::VarError::NotUnicode(_)) => panic!("{REPLAY_MODE_ENV} must be valid UTF-8"),
    }
}

fn path_admission_replay_body() {
    let addr = init_replay_metrics();

    let mut corpus = corpus();
    let mut out: Vec<String> = Vec::new();
    let mut admitted: Vec<(&'static str, u64, Vec<MixedPoolRef>)> = Vec::new();
    for row in &corpus.rows {
        match register_path(&mut corpus.engine, row.hops.clone()) {
            Ok(path_id) => {
                out.push(format!(
                    "row={} admission=registered path_id={path_id}",
                    row.name
                ));
                let refs = row
                    .hops
                    .iter()
                    .zip(row.families.iter())
                    .map(|(hop, &hop_type)| MixedPoolRef {
                        hop_type,
                        pool_key: hop.pool_id,
                        zero_for_one: hop.zero_for_one,
                    })
                    .collect();
                admitted.push((row.name, path_id, refs));
            }
            Err(err) => out.push(format!("row={} admission=refused error={err}", row.name)),
        }
    }
    out.push(format!("registered_paths={}", path_count(&corpus.engine)));

    let refused_sim: BTreeSet<u64> = admitted
        .iter()
        .filter(|(name, ..)| *name == "v2_compose_capable_2hop")
        .map(|(_, path_id, _)| *path_id)
        .collect();
    let hook: Arc<dyn InlineSimulator> = Arc::new(ReplayHook {
        refuse: refused_sim,
    });
    corpus.engine.cycle.inline_sim = Some(Arc::clone(&hook));
    corpus.engine.cycle.inline_sim_enabled = true;
    set_profit_thresholds(&mut corpus.engine, U256::ZERO, U256::MAX);

    let affected: Vec<AffectedKey> = corpus
        .pools
        .iter()
        .map(|&(hop, pool_id)| AffectedKey::new(hop, pool_id))
        .collect();
    let metadata = BlockMetadata::default();
    let _outcome = run_test_cycle(&mut corpus.engine, REPLAY_BLOCK, &metadata, &affected);
    let (results, solved_block) = latest_results(&corpus.engine);
    out.push(format!("solved_block={solved_block}"));
    let mut resolved_ids: Vec<u64> = corpus
        .engine
        .cycle
        .path_resolved
        .iter()
        .map(|(&id, _)| id)
        .collect();
    resolved_ids.sort_unstable();
    out.push(format!("resolved_paths={resolved_ids:?}"));
    let mut result_ids: Vec<u64> = results.keys().copied().collect();
    result_ids.sort_unstable();
    out.push(format!("results_paths={result_ids:?}"));

    for (name, path_id, _refs) in &admitted {
        let Some(report) = results.get(path_id) else {
            out.push(format!("row={name} solve=none"));
            continue;
        };
        out.push(format!(
            "row={name} solve=optimal_input:{} profit:{} hop_outputs:{}",
            report.optimal_input,
            report.profit,
            report
                .hop_outputs
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
        let simmed = corpus.engine.cycle.inline_payloads.contains_key(path_id);
        out.push(format!(
            "row={name} inline_sim={}",
            if simmed { "payload" } else { "none" }
        ));
    }

    let text = scrape(addr);
    for family in WATCHED {
        for line in text.lines().filter(|line| line.starts_with(family)) {
            out.push(format!("metric {line}"));
        }
    }
    out.push(format!(
        "registered_paths_after={}",
        path_count(&corpus.engine)
    ));

    let registered = path_count(&corpus.engine);
    assert_eq!(
        registered,
        env_usize("DEGENBOT_REPLAY_EXPECT_REGISTERED_PATHS"),
        "registered paths"
    );
    let unsupported = corpus
        .rows
        .iter()
        .filter(|row| !admitted.iter().any(|(name, ..)| *name == row.name))
        .count();
    assert_eq!(
        unsupported,
        env_usize("DEGENBOT_REPLAY_EXPECT_UNSUPPORTED_ADMISSIONS"),
        "unsupported admissions refused at the door"
    );
    let inline = series_sum(&text, "degenbot_candidates_found_total", |line| {
        line.contains("arm=\"inline\"")
    });
    assert_eq!(
        inline,
        env_usize("DEGENBOT_REPLAY_EXPECT_INLINE_CANDIDATES"),
        "inline-arm candidates"
    );
    let hook_refused = series_sum(&text, "degenbot_sim_degrades_total", |line| {
        line.contains("reason=\"hook_refused\"") || !line.contains("reason=")
    });
    assert_eq!(
        hook_refused,
        env_usize("DEGENBOT_REPLAY_EXPECT_HOOK_REFUSED"),
        "hook_refused degrades"
    );
    let no_encoder = series_sum(&text, "degenbot_sim_degrades_total", |line| {
        line.contains("reason=\"no_encoder_arm\"")
    });
    assert_eq!(
        no_encoder,
        env_usize("DEGENBOT_REPLAY_EXPECT_NO_ENCODER_ARM"),
        "no_encoder_arm degrades (0 means the series is absent)"
    );
    let solve_evaluations = series_sum(&text, SOLVE_EVALUATIONS_SERIES, |_| true);
    assert_eq!(
        solve_evaluations,
        env_usize("DEGENBOT_REPLAY_EXPECT_SOLVE_EVALUATIONS"),
        "per-path solve gate evaluations"
    );
    for (row, optimal_input, profit, hop_outputs, payload) in GOLDEN_SUPPORTED {
        let solved = out
            .iter()
            .find(|line| line.starts_with(&format!("row={row} solve=")))
            .unwrap_or_else(|| panic!("row {row} produced no solve record"));
        assert!(
            solved.contains(&format!(
                "solve=optimal_input:{optimal_input} profit:{profit} hop_outputs:{hop_outputs}"
            )),
            "row {row} supported solve diverged: {solved}"
        );
        let sim = out
            .iter()
            .find(|line| line.starts_with(&format!("row={row} inline_sim=")))
            .unwrap_or_else(|| panic!("row {row} produced no sim record"));
        let expected_sim = if payload {
            "inline_sim=payload"
        } else {
            "inline_sim=none"
        };
        assert!(
            sim.ends_with(expected_sim),
            "row {row} sim outcome diverged: {sim}"
        );
    }

    let report = out.join("\n");
    if let Ok(path) = std::env::var("DEGENBOT_ADMISSION_REPLAY_OUT") {
        std::fs::write(&path, format!("{report}\n--- scrape ---\n{text}")).expect("report writes");
    }
    println!("{report}");
}
