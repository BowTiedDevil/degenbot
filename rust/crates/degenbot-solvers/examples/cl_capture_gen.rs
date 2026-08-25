// Dev/example-only harness: an offline capture generator run by hand against live RPC.
// Pedantic + restriction lints that production code denies are relaxed here.
#![allow(
    clippy::doc_lazy_continuation,
    clippy::expect_used,
    clippy::if_same_then_else,
    clippy::manual_let_else,
    clippy::no_effect_underscore_binding,
    clippy::print_stderr,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::unwrap_used
)]

//! Offline generator for the all-CL capture fixture.
//!
//! For a curated set of REAL, liquid UNI V3 pools (heavy tick maps,
//! `tick_spacing` 1/5/10) at a pinned block, fetches on-chain slot0/liquidity,
//! backfills the tick map via a giant bidirectional `v3_simulate_swap` (the
//! production sparse-backfill pattern, pull real tick words from the archive
//! node), builds each pool's `IntV3TickRangeSequence` via the same
//! `build_int_v3_sequence` the bot uses, forms valid 2-hop all-CL paths by
//! shared token, runs `int_solve_cl_path` on each, and writes (input sequences
//! + golden) to a JSONL consumed by `cl_solve_replay`. Deterministic,
//! network-gated source of real heavy-CL data, no full bot soak.
//!
//!   `DEGENBOT_CLCAP_RPC=http://host.containers.internal:8545`/ //!   `DEGENBOT_CLCAP_BLOCK=25826800` //!   cargo run -p degenbot-solvers --example `cl_capture_gen`
//!   cargo run -p degenbot-solvers --example `cl_solve_replay` <capture.jsonl>
//!
//! A `.block` sidecar is written next to the output recording the pinned block
//! + regen command so the offline fixture is reproducible. Raise
//! `DEGENBOT_CLCAP_MAX_FETCHES` (default 320) to backfill a denser active set.

use std::collections::HashMap;
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use alloy::primitives::{Address, B256, I256, U256};
use degenbot_pools::registry::ConcentratedLiquidityPoolMut;
use degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};
use degenbot_pools::v3_state::{
    v3_simulate_swap, PoolTickCoverage, RegisterV3PoolParams, SimulateSwapError, V3PoolState,
};
use degenbot_pools::TickBootstrapRpc;
use degenbot_rpc::abi::fetch_v3_slot0_liquidity;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_rpc::AlloyTickBootstrapRpc;
use degenbot_solvers::mobius_v3_int::{
    int_solve_cl_path, reset_walk_stats, take_last_walk_stats, IntV3TickRangeSequence,
};
use serde_json::{json, Value};

const MAX_RANGES: usize = 128; // cap ranges per hop (heavy but bounded)
const MAX_FETCHES: usize = 320; // per-pool tick-word fetch cap, overridable
                                // via DEGENBOT_CLCAP_MAX_FETCHES so a deep backfill can reach dense active
                                // sets (a liquid pool's busy region can span thousands of tick words).
const PATH_CAP: usize = 12;

// (address, tick_spacing, fee_classic, token0, token1) — liquid UNI V3 pools
// from the static degenbot DB (block ~25826800). Shared USDC/USDT/DAI/WETH.
type Pool = (&'static str, i32, u32, &'static str, &'static str);
const POOLS: &[Pool] = &[
    (
        "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",
        10,
        500,
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
    ),
    (
        "0x56534741CD8B152df6d48AdF7ac51f75169A83b2",
        10,
        500,
        "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
        "0xdAC17F958D2ee523a2206206994597C13D831ec7",
    ),
    (
        "0x4585FE77225b41b697C938B018E2Ac67Ac5a20c0",
        10,
        500,
        "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
    ),
    (
        "0x7858E59e0C01EA06Df3aF3D20aC7B0003275D4Bf",
        10,
        500,
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        "0xdAC17F958D2ee523a2206206994597C13D831ec7",
    ),
    (
        "0x6c6Bc977E13Df9b0de53b251522280BB72383700",
        10,
        500,
        "0x6B175474E89094C44Da98b954EedeAC495271d0F",
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
    ),
    (
        "0x11b815efB8f581194ae79006d24E0d814B7697F6",
        10,
        500,
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        "0xdAC17F958D2ee523a2206206994597C13D831ec7",
    ),
    (
        "0x48DA0965ab2d2cbf1C17C09cFB5Cbe67Ad5B1406",
        1,
        100,
        "0x6B175474E89094C44Da98b954EedeAC495271d0F",
        "0xdAC17F958D2ee523a2206206994597C13D831ec7",
    ),
    (
        "0x3416cF6C708Da44DB2624D63ea0AAef7113527C6",
        1,
        100,
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        "0xdAC17F958D2ee523a2206206994597C13D831ec7",
    ),
];

#[derive(Debug)]
struct BootstrapFetcher {
    address: Address,
    rpc: Arc<AlloyTickBootstrapRpc>,
}
impl TickWordFetcher for BootstrapFetcher {
    fn fetch_missing_tick_word(
        &self,
        _pool_id: u64,
        word: i32,
        block: u64,
    ) -> Result<FetchedTickWord, FetchTickWordError> {
        let tick = word
            .checked_mul(256 * 10)
            .ok_or(FetchTickWordError::OutOfRange)?;
        let res = self
            .rpc
            .bootstrap_v3_tick_word(&self.address.to_string(), tick, 10, block)
            .map_err(|_| FetchTickWordError::FetchFailed)?;
        Ok(match res {
            None => FetchedTickWord {
                word,
                ticks: HashMap::new(),
            },
            Some(b) => FetchedTickWord {
                word: b.word,
                ticks: b.ticks,
            },
        })
    }
}
fn addr(s: &str) -> Address {
    s.parse().expect("address")
}

fn hop_ranges(s: &IntV3TickRangeSequence) -> Value {
    let ranges = s.ranges.iter().map(|r| json!({
        "liquidity": r.liquidity.to_string(),
        "sqrt_price_x96": r.sqrt_price_x96.to_string(),
        "sqrt_price_lower_x96": r.sqrt_price_lower_x96.to_string(),
        "sqrt_price_upper_x96": r.sqrt_price_upper_x96.to_string(),
        "gamma_numer": r.gamma_numer, "fee_denom": r.fee_denom,
        "zero_for_one": r.zero_for_one,
        "word_boundary_prices": r.word_boundary_prices.iter().map(std::string::ToString::to_string).collect::<Vec<_>>(),
    })).collect::<Vec<_>>();
    Value::Array(ranges)
}

fn main() -> ExitCode {
    let max_fetches: usize = std::env::var("DEGENBOT_CLCAP_MAX_FETCHES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(MAX_FETCHES);
    let rpc = if let Ok(v) = std::env::var("DEGENBOT_CLCAP_RPC") {
        v
    } else {
        eprintln!("DEGENBOT_CLCAP_RPC unset (network-gated). Aborting.");
        return ExitCode::FAILURE;
    };
    let block = std::env::var("DEGENBOT_CLCAP_BLOCK")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(25_826_800);
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "tests/fixtures/cl_capture_offline.jsonl".to_string());

    // 1) Slot0+liquidity per pool on a throwaway runtime, then drop.
    let setup: Vec<(U256, I256, U256)> =
        tokio::runtime::Runtime::new().expect("rt").block_on(async {
            let provider = Arc::new(AlloyProvider::new(&rpc, 10).await.expect("provider"));
            let mut out = Vec::with_capacity(POOLS.len());
            for (a, _ts, _f, _t0, _t1) in POOLS {
                let (sp, tick, liq) = fetch_v3_slot0_liquidity(&provider, &addr(a), Some(block))
                    .await
                    .expect("slot0");
                out.push((sp, tick, liq));
            }
            out
        });

    // 2) Provider kept for the SYNC bootstrap misses (they use get_runtime).
    let provider = tokio::runtime::Runtime::new()
        .expect("rt")
        .block_on(async { Arc::new(AlloyProvider::new(&rpc, 10).await.expect("p")) });
    let bootstrap = Arc::new(AlloyTickBootstrapRpc::new(Arc::clone(&provider)));

    let mut seqs: Vec<(usize, IntV3TickRangeSequence)> = Vec::new();
    for (i, pool) in POOLS.iter().enumerate() {
        let (a, ts, fee, t0s, t1s) = pool;
        let (sqrt, tick_i256, liq_u256) = &setup[i];
        let p = addr(a);
        let fetcher: Arc<dyn TickWordFetcher> = Arc::new(BootstrapFetcher {
            address: p,
            rpc: Arc::clone(&bootstrap),
        });
        let params = RegisterV3PoolParams {
            address: p,
            token0: addr(t0s),
            token1: addr(t1s),
            fee: *fee,
            tick_spacing: *ts,
            factory: Address::ZERO,
            sqrt_price_x96: *sqrt,
            liquidity: u128::try_from(*liq_u256).unwrap_or(0),
            tick: i32::try_from(*tick_i256).unwrap_or(0),
            tick_data: HashMap::new(),
            update_block: block,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: Some(fetcher.clone()),
            deployer: Address::ZERO,
            init_hash: B256::ZERO,
        };
        let (_identity, mut state) = V3PoolState::from_params(params, 8);

        // Backfill the tick map: giant bidirectional swap sweeps the price
        // range, fetching real tick words on each MissingTickWord (bounded).
        let mut fetches = 0usize;
        for &zfo in &[false, true] {
            let amt = I256::try_from(U256::from(u128::MAX)).unwrap();
            let limit = V3PoolState::default_sqrt_price_limit(zfo);
            loop {
                match v3_simulate_swap(&state, *fee, *ts, zfo, amt, limit) {
                    Ok(_) => break,
                    Err(SimulateSwapError::MissingTickWord(word)) => {
                        fetches += 1;
                        if fetches >= max_fetches {
                            eprintln!("pool {i} {a}: fetch cap {max_fetches}");
                            break;
                        }
                        match fetcher.fetch_missing_tick_word(0, word, block) {
                            Ok(w) => {
                                state.merge_tick_word(&w);
                            }
                            Err(_) => break,
                        }
                    }
                    Err(e) => {
                        eprintln!("pool {i} {a} zfo={zfo} backfill: {e:?}");
                        break;
                    }
                }
            }
        }
        match state.build_int_v3_sequence(*ts, *fee, false, MAX_RANGES) {
            Some(seq) => {
                eprintln!(
                    "pool {i} {a}: {} ranges after {fetches} word-fetches",
                    seq.ranges.len()
                );
                seqs.push((i, seq));
            }
            None => eprintln!("pool {i} {a}: sequence still None after {fetches} fetches"),
        }
    }
    drop(provider);

    if seqs.len() < 2 {
        eprintln!("fewer than 2 sequences ({}); cannot form paths", seqs.len());
        return ExitCode::FAILURE;
    }

    let mut file = std::fs::File::create(&out).expect("create capture file");
    let mut n_paths = 0usize;
    let mut n_profitable = 0usize;
    'outer: for (ia, sa) in &seqs {
        let (a_t0, a_t1) = (addr(POOLS[*ia].3), addr(POOLS[*ia].4));
        for (ib, sb) in &seqs {
            if ia == ib {
                continue;
            }
            let (b_t0, b_t1) = (addr(POOLS[*ib].3), addr(POOLS[*ib].4));
            let shared = if a_t0 == b_t0 {
                Some(a_t0)
            } else if a_t0 == b_t1 {
                Some(a_t0)
            } else if a_t1 == b_t0 {
                Some(a_t1)
            } else if a_t1 == b_t1 {
                Some(a_t1)
            } else {
                None
            };
            let Some(s) = shared else {
                continue;
            };
            let other_a = if a_t0 == s { a_t1 } else { a_t0 };
            let other_b = if b_t0 == s { b_t1 } else { b_t0 };
            if other_a == other_b {
                continue;
            }
            let _zfo_a = other_a == a_t0;
            let _zfo_b = s == b_t0;
            reset_walk_stats();
            let t0 = std::time::Instant::now();
            let res = int_solve_cl_path(&[sa, sb]);
            let micros = t0.elapsed().as_micros();
            let (pieces, sims) = take_last_walk_stats();
            let profitable = res.is_some();
            n_paths += 1;
            if profitable {
                n_profitable += 1;
            }
            let golden = match &res {
                None => Value::Null,
                Some((opt, _profit, hop_outputs)) => json!({
                    "optimal_input": opt.to_string(),
                    "hop_outputs": hop_outputs.iter().map(std::string::ToString::to_string).collect::<Vec<_>>(),
                    "n_hops": hop_outputs.len() }),
            };
            let na = sa.ranges.len();
            let nb = sb.ranges.len();
            let wbp = sa
                .ranges
                .iter()
                .map(|r| r.word_boundary_prices.len())
                .sum::<usize>()
                + sb.ranges
                    .iter()
                    .map(|r| r.word_boundary_prices.len())
                    .sum::<usize>();
            let line = json!({
                "block": block, "path_id": (ia * 100 + ib), "hops": 2,
                "n_ranges": [na, nb], "n_word_bounds": wbp,
                "measured": { "time_us": micros, "sims": sims, "pieces": pieces },
                "golden": golden,
                "hops": [hop_ranges(sa), hop_ranges(sb)],
                "pools": [POOLS[*ia].0, POOLS[*ib].0],
            });
            writeln!(file, "{}", line).expect("write line");
            eprintln!("path {} [{}x{}] ranges=({},{}) t={micros}us sims={sims} pieces={pieces} profitable={profitable}",
                ia * 100 + ib, POOLS[*ia].0, POOLS[*ib].0, na, nb);
            if n_paths >= PATH_CAP {
                break 'outer;
            }
        }
    }
    eprintln!("---- wrote {n_paths} path(s), {n_profitable} profitable -> {out}");
    // Record the pinned block + regen command in a sidecar so the offline
    // fixture is reproducible and any drift is explainable.
    let _ = std::fs::write(
        format!("{out}.block"),
        format!(
            "block={block}\nregen: DEGENBOT_CLCAP_RPC=<rpc> DEGENBOT_CLCAP_BLOCK={block} DEGENBOT_CLCAP_MAX_FETCHES={max_fetches} cargo run -q -p degenbot-solvers --example cl_capture_gen [-- <out.jsonl>]\n"
        ),
    );
    ExitCode::SUCCESS
}
