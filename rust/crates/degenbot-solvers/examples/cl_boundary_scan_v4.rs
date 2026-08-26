// Dev/example-only harness: a throwaway V4 boundary-scanning tool for offline analysis.
// Pedantic + restriction lints that production code denies are relaxed here.
#![allow(
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::format_push_string,
    clippy::manual_let_else,
    clippy::match_same_arms,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unwrap_used
)]

//! Offline word-boundary DENSITY scan for the bot's real V4 heavy-solve pools:
//! build the live `V4PoolState` via the V4 `StateView` + tick bootstrap, backfill,
//! build the 24-range integer sequence, report max word-boundary count per
//! range + whether any range is "dense" (>= 128). Shares the exact mechanism
//! with V3 (`IntV3TickRangeSequence` / `compute_tick_ranges` / 128 threshold).
use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use alloy::primitives::{Address, I256, U256};
use degenbot_pools::registry::ConcentratedLiquidityPoolMut;
use degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};
use degenbot_pools::v3_state::PoolTickCoverage;
use degenbot_pools::v4_state::{v4_simulate_swap, RegisterV4PoolParams, V4PoolKey, V4PoolState};
use degenbot_pools::TickBootstrapRpc;
use degenbot_rpc::abi::fetch_v4_slot0_liquidity;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_rpc::AlloyTickBootstrapRpc;

const MAX_FETCHES: usize = 250;
const DENSE: usize = 128;
const STATE_VIEW: &str = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227";
const POOL_MANAGER: &str = "0x000000000004444c5dc75cB358380D2e3De08A90";

fn hex32(s: &str) -> [u8; 32] {
    let h = s.strip_prefix("0x").unwrap_or(s);
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).expect("hex byte");
    }
    out
}

#[derive(Debug)]
struct BF {
    state_view: Address,
    pool_id: [u8; 32],
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
            .bootstrap_v4_tick_word(
                &self.state_view.to_string(),
                &self.pool_id,
                tick,
                self.spacing,
                self.block,
            )
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
        .unwrap_or_else(|| "/tmp/scan_v4.json".to_string());
    let file = std::fs::read_to_string(&list_path).expect("read pool json");
    let raw: Vec<serde_json::Value> = serde_json::from_str(&file).expect("parse pools");
    let triples: Vec<([u8; 32], i32, u32)> = raw
        .iter()
        .map(|o| {
            (
                hex32(o["pool_id"].as_str().unwrap()),
                o["spacing"].as_i64().unwrap() as i32,
                o["fee"].as_u64().unwrap() as u32,
            )
        })
        .collect();

    let state_view: Address = STATE_VIEW.parse().expect("sv addr");
    let pool_manager: Address = POOL_MANAGER.parse().expect("pm addr");
    let block = std::env::var("DEGENBOT_SCAN_BLOCK")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25_827_700);

    // slot0 per pool on a throwaway runtime, then drop.
    let setup: Vec<(U256, I256, u32, u32, u128)> =
        tokio::runtime::Runtime::new().expect("rt").block_on(async {
            let provider = Arc::new(AlloyProvider::new(&rpc, 10).await.expect("provider"));
            let mut out = Vec::with_capacity(triples.len());
            for (pid, _sp, _fee) in &triples {
                match fetch_v4_slot0_liquidity(&provider, &state_view, pid, Some(block)).await {
                    Ok((sqrt, tick, pfee, lfee, liq)) => out.push((
                        sqrt,
                        tick,
                        u32::try_from(pfee).unwrap_or(0),
                        u32::try_from(lfee).unwrap_or(10),
                        u128::try_from(liq).unwrap_or(0),
                    )),
                    Err(e) => {
                        eprintln!("v4 slot0 fail {pid:?}: {e:?}");
                        out.push((U256::ZERO, I256::ZERO, 0, 10, 0));
                    }
                }
            }
            out
        });

    let provider = tokio::runtime::Runtime::new()
        .expect("rt")
        .block_on(async { Arc::new(AlloyProvider::new(&rpc, 10).await.expect("p")) });
    let bootstrap = Arc::new(AlloyTickBootstrapRpc::new(Arc::clone(&provider)));
    let limit = U256::from(degenbot_math::cl::tick_math::MAX_SQRT_RATIO);

    let t0 = Instant::now();
    let mut dense_count = 0usize;
    let mut max_wb_all = 0usize;
    let mut n_liq0 = 0usize;
    for (i, (pool_id, spacing, fee_db)) in triples.iter().enumerate() {
        let (sqrt, tick_i256, protocol_fee, lp_fee, liquidity) = &setup[i];
        if *liquidity == 0 {
            n_liq0 += 1;
            continue;
        }
        let fee = if *lp_fee > 0 { *lp_fee } else { *fee_db };
        let fetcher: Arc<dyn TickWordFetcher> = Arc::new(BF {
            state_view,
            pool_id: *pool_id,
            spacing: *spacing,
            rpc: Arc::clone(&bootstrap),
            block,
        });
        let params = RegisterV4PoolParams {
            pool_manager,
            pool_id: *pool_id,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::ZERO,
                fee,
                tick_spacing: *spacing,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: *protocol_fee,
            sqrt_price_x96: *sqrt,
            liquidity: *liquidity,
            tick: i32::try_from(*tick_i256).unwrap_or(0),
            tick_data: HashMap::new(),
            update_block: block,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: Some(fetcher.clone()),
        };
        let (_identity, mut state) = V4PoolState::from_params(params, 8);
        let mut fetches = 0usize;
        loop {
            match v4_simulate_swap(
                &state,
                fee,
                *spacing,
                false,
                -(I256::try_from(U256::from(u128::MAX)).unwrap()),
                limit,
            ) {
                Ok(_) => break,
                Err(degenbot_pools::v3_state::SimulateSwapError::MissingTickWord(word)) => {
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
        if let Some(seq) = state.build_int_v4_sequence(*spacing, fee, false) {
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
            println!("{i:>4} sp={spacing:>4} fee={fee:>6} pid=0x{} nbnd_max={maxw:>4} nbnd_total={total_bnd:>4} n_ranges={nr:>2} fetches={fetches} {tag}", hex(&pool_id[..8]));
        } else {
            let tag = "seq=None";
            println!(
                "{i:>4} sp={spacing:>4} fee={fee:>6} pid=0x{} fetches={fetches} {tag}",
                hex(&pool_id[..8])
            );
        }
    }
    println!(
        "\n== V4 summary == pools={} (liq0 skipped {}) dense(>={DENSE})={} max_wb_any={max_wb_all} elapsed={:?}",
        triples.len(),
        n_liq0,
        dense_count,
        t0.elapsed()
    );
    drop(provider);
    ExitCode::SUCCESS
}

fn hex(b: &[u8]) -> String {
    let mut s = String::new();
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}
