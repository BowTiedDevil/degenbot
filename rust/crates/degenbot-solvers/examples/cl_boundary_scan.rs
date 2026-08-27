// Dev/example-only harness: a throwaway boundary-scanning tool for offline analysis.
// Pedantic + restriction lints that production code denies are relaxed here.
#![expect(
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::manual_let_else,
    clippy::match_same_arms,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unwrap_used
)]

//! Offline word-boundary DENSITY scan for real bot pools (V3): build the live
//! `V3PoolState` via RPC bootstrap, backfill tick words, build the 24-range
//! integer sequence, report max word-boundary count per range + whether any
//! range is "dense" (>= the 128 profile threshold).
use hashbrown::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

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

const MAX_FETCHES: usize = 250;
const DENSE: usize = 128;

#[derive(Debug)]
struct BF {
    address: Address,
    spacing: i32,
    rpc: Arc<AlloyTickBootstrapRpc>,
    block: u64,
}
impl TickWordFetcher for BF {
    fn fetch_missing_tick_word(
        &self,
        _pool_id: u64,
        word: i32,
        _block: u64,
    ) -> Result<FetchedTickWord, FetchTickWordError> {
        let tick = word
            .checked_mul(256 * self.spacing)
            .ok_or(FetchTickWordError::OutOfRange)?;
        let res = self
            .rpc
            .bootstrap_v3_tick_word(&self.address.to_string(), tick, self.spacing, self.block)
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

fn main() -> ExitCode {
    let rpc = if let Ok(v) = std::env::var("DEGENBOT_CLCAP_RPC") {
        v
    } else {
        eprintln!("DEGENBOT_CLCAP_RPC unset. Aborting.");
        return ExitCode::FAILURE;
    };
    let list_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/scan_v3.json".to_string());
    let file = std::fs::read_to_string(&list_path).expect("read pool json");
    let raw: Vec<serde_json::Value> = serde_json::from_str(&file).expect("parse pools");
    let pools: Vec<(Address, i32, u32)> = raw
        .iter()
        .map(|o| {
            (
                o["addr"].as_str().unwrap().parse().expect("addr"),
                o["spacing"].as_i64().unwrap() as i32,
                o["fee"].as_u64().unwrap() as u32,
            )
        })
        .collect();

    let block = std::env::var("DEGENBOT_SCAN_BLOCK")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25_827_700);

    let addrs: Vec<Address> = pools.iter().map(|(a, _, _)| *a).collect();
    let setup: Vec<(U256, I256, U256)> =
        tokio::runtime::Runtime::new().expect("rt").block_on(async {
            let provider = Arc::new(AlloyProvider::new(&rpc, 10).await.expect("provider"));
            let mut out = Vec::with_capacity(addrs.len());
            for a in &addrs {
                match fetch_v3_slot0_liquidity(&provider, a, Some(block)).await {
                    Ok(t) => out.push(t),
                    Err(e) => {
                        eprintln!("slot0 fail {a}: {e:?}");
                        out.push((U256::ZERO, I256::ZERO, U256::ZERO));
                    }
                }
            }
            out
        });

    let provider = tokio::runtime::Runtime::new()
        .expect("rt")
        .block_on(async { Arc::new(AlloyProvider::new(&rpc, 10).await.expect("p")) });
    let bootstrap = Arc::new(AlloyTickBootstrapRpc::new(Arc::clone(&provider)));

    let t0 = Instant::now();
    let mut dense_count = 0usize;
    let mut max_wb_all = 0usize;
    for (i, (address, spacing, fee)) in pools.iter().enumerate() {
        let (sqrt, tick_i256, liq_u256) = &setup[i];
        let fetcher: Arc<dyn TickWordFetcher> = Arc::new(BF {
            address: *address,
            spacing: *spacing,
            rpc: Arc::clone(&bootstrap),
            block,
        });
        let params = RegisterV3PoolParams {
            address: *address,
            token0: Address::ZERO,
            token1: Address::ZERO,
            fee: *fee,
            tick_spacing: *spacing,
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
        let mut fetches = 0usize;
        loop {
            match v3_simulate_swap(
                &state,
                *fee,
                *spacing,
                false,
                I256::try_from(U256::from(u128::MAX)).unwrap(),
                V3PoolState::default_sqrt_price_limit(false),
            ) {
                Ok(_) => break,
                Err(SimulateSwapError::MissingTickWord(word)) => {
                    fetches += 1;
                    if fetches >= MAX_FETCHES {
                        break;
                    }
                    match fetcher.fetch_missing_tick_word(0, word, block) {
                        Ok(w) => {
                            state.merge_tick_word(&w);
                        }
                        Err(_) => break,
                    }
                }
                Err(_) => break,
            }
        }
        if let Some(seq) = state.build_int_v3_sequence(*spacing, *fee, false) {
            let per_range: Vec<usize> = seq
                .ranges
                .iter()
                .map(|r| r.word_boundary_prices.len())
                .collect();
            let maxw = *per_range.iter().max().unwrap_or(&0);
            max_wb_all = max_wb_all.max(maxw);
            let dense = maxw >= DENSE;
            if dense {
                dense_count += 1;
            }
            let total_bnd: usize = per_range.iter().sum();
            let nr = seq.ranges.len();
            let tag = if dense { "DENSE" } else { "" };
            println!(
                "{i:>4} sp={spacing:>4} fee={fee:>6} {address} nbnd_max={maxw:>4} nbnd_total={total_bnd:>4} n_ranges={nr:>2} fetches={fetches} {tag}"
            );
        } else {
            let tag = "seq=None";
            println!("{i:>4} sp={spacing:>4} fee={fee:>6} {address} fetches={fetches} {tag}");
        }
    }
    println!(
        "\n== V3 summary == pools={} dense(>={DENSE})={} max_wb_any={max_wb_all} elapsed={:?}",
        pools.len(),
        dense_count,
        t0.elapsed()
    );
    drop(provider);
    ExitCode::SUCCESS
}
