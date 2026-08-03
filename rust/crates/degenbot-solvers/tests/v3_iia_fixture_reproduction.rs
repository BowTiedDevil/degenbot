//! Network-gated mainnet-fixture reproduction (ergo task `E7ALWT`).
//!
//! The live `DEGENBOT_SIM_EXIT_ON_FAIL=1` trap captured a +13 wei V3-hop
//! over-prediction on pool `0x57D7…dF80` (UNI/DAI fee=500) at block 25647669.
//! The bias is **constant (+13/+14) across four paths with DIFFERENT
//! `amount_in`** — so it lives in the amount-INDEPENDENT `compute_crossing`
//! crossing-output accumulator, not the amount-dependent ending partial step.
//! `v3_crossing_solver_vs_sim_parity.rs` proves the solver's crossing math is
//! byte-exact with `v3_simulate_swap` on SYNTHETIC regular/symmetric tick
//! topologies down to 1e9 liquidity — so the divergence is topology-specific
//! (irregular initialized ticks far from the 1:1 price), reproducible only
//! against the real pool state.
//!
//! This test fetches the real `V3PoolState` at the captured block via the
//! archive RPC and runs `v3_simulate_swap` (the byte-exact twin the revm
//! sim's `actual` mirrors) on byte-identical on-chain state. The fork is
//! decided by comparing the result to the captured values:
//!
//! - sim == captured actual (150836781502) → the revm sim matches the Rust
//!   twin → the +13 is a SOLVER-MATH divergence (fix targets `compute_crossing`
//!   / `compute_tick_ranges`).
//! - sim == captured predicted (150836781515) → the Rust twin matches the
//!   solver → the revm sim's `actual` came from DIFFERENT state than on-chain@N
//!   → STALE ENGINE STATE (fix targets pump ordering).
//!
//! Gated by env `DEGENBOT_V3_FIXTURE_RPC` + `DEGENBOT_V3_FIXTURE_BLOCK` so CI
//! (no archive RPC) never hits the network. Run locally:
//!
//! ```text
//! DEGENBOT_V3_FIXTURE_RPC=http://host.containers.internal:8545/ \
//! DEGENBOT_V3_FIXTURE_BLOCK=25647669 \
//! cargo test -p degenbot-solvers --test v3_iia_fixture_reproduction -- --nocapture
//! ```

#![allow(clippy::too_many_lines, clippy::doc_markdown)]

use std::collections::HashMap;
use std::sync::Arc;

use alloy::primitives::{Address, I256, U256};

use degenbot_pools::registry::ConcentratedLiquidityPoolMut;
use degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};
use degenbot_pools::v3_state::{
    v3_simulate_swap, PoolTickCoverage, RegisterV3PoolParams, SimulateSwapError, V3PoolState,
    V3SwapOutcome,
};
use degenbot_pools::TickBootstrapRpc;
use degenbot_rpc::abi::fetch_v3_slot0_liquidity;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_rpc::AlloyTickBootstrapRpc;
use degenbot_solvers::mobius_v3_int::{int_simulate_v3_swap, IntV3TickRangeSequence};

/// Hardcoded fixture from the captured `DEGENBOT_SIM_EXIT_ON_FAIL=1` trap
/// (`logs/bot_run.log` block 25647669, path 7724). Pool `0x57D7…dF80`,
/// UNI/DAI fee=500, tick_spacing=10, ofz (zero_for_one=false — token1 DAI in,
/// token0 UNI out). `amount_in` = hop[0]'s matched output.
const POOL_ADDRESS: &str = "0x57D7d040438730d4029794799dEEd8601E23fF80";
const FIXTURE_BLOCK: u64 = 25_647_669;
const FEE: u32 = 500;
const TICK_SPACING: i32 = 10;
const AMOUNT_IN: u128 = 50_868_891_135; // hop[0] matched output → hop[1] amount_in
const CAPTURED_ACTUAL: u128 = 150_836_781_502; // revm sim measured hop[1] output
const CAPTURED_PREDICTED: u128 = 150_836_781_515; // solver hop_outputs[1] (+13)

/// `TickWordFetcher` backed by `AlloyTickBootstrapRpc` (address-keyed bootstrap
/// impl). The miss-path trait passes `word`; this picks a tick landing in that
/// word and delegates to the bootstrap (which recomputes the word internally).
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
        // tick whose word-position == `word`: `word * 256 * spacing`.
        // `tick.div_euclid(spacing) >> 8 == word` holds for both signs
        // (arithmetic shift; exact divisibility by spacing).
        let tick = word
            .checked_mul(256)
            .and_then(|t| t.checked_mul(TICK_SPACING))
            .ok_or(FetchTickWordError::OutOfRange)?;
        let result = self
            .rpc
            .bootstrap_v3_tick_word(&self.address.to_string(), tick, TICK_SPACING, block)
            .map_err(|_| FetchTickWordError::FetchFailed)?;
        Ok(match result {
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

/// The solver's CL-hop output for `amount_in` (mirrors
/// `v3_crossing_solver_vs_sim_parity::solver_crossing_output`).
fn solver_crossing_output(amount_in: U256, seq: &IntV3TickRangeSequence) -> Option<U256> {
    let n = seq.ranges.len();
    let mut chosen_k = 0usize;
    for k in 0..n {
        let crossing = seq.compute_crossing(k)?;
        if crossing.crossing_gross_input <= amount_in {
            chosen_k = k;
        } else {
            break;
        }
    }
    let crossing = seq.compute_crossing(chosen_k)?;
    if amount_in < crossing.crossing_gross_input {
        return Some(U256::ZERO);
    }
    let remaining = amount_in - crossing.crossing_gross_input;
    let ending = int_simulate_v3_swap(remaining, &crossing.ending_range);
    Some(crossing.crossing_output.saturating_add(ending.output))
}

/// ofz exact-in → pool sends token0 (amount0).
fn v3_exact_in_output_ofz(outcome: &V3SwapOutcome) -> U256 {
    outcome.amount0
}

#[test]
fn v3_iia_fixture_reproduces_plus_thirteen_divergence() {
    let Ok(rpc_url) = std::env::var("DEGENBOT_V3_FIXTURE_RPC") else {
        eprintln!(
            "[v3-iia-fixture] DEGENBOT_V3_FIXTURE_RPC unset — skipping network-gated reproduction"
        );
        return;
    };
    let block = std::env::var("DEGENBOT_V3_FIXTURE_BLOCK")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(FIXTURE_BLOCK);

    let pool: Address = POOL_ADDRESS.parse().expect("valid address");
    // Run ONLY the async I/O setup (provider construction + slot0/liquidity
    // fetch) on a throwaway tokio runtime, then DROP it so the subsequent
    // `bootstrap_v3_tick_word` calls (which internally call
    // `degenbot_core::runtime::get_runtime().block_on(...)`) do not nest a
    // block_on inside this test's runtime (which panics with
    // "Cannot start a runtime from within a runtime").
    let (provider, sqrt_price_x96, tick_i256, liquidity_u256) = tokio::runtime::Runtime::new()
        .expect("setup runtime")
        .block_on(async {
            let provider = Arc::new(AlloyProvider::new(&rpc_url, 3).await.expect("provider"));
            // 1. Fetch on-chain slot0 + liquidity at the captured block.
            let (sp, tick_wide, liq) = fetch_v3_slot0_liquidity(&provider, &pool, Some(block))
                .await
                .expect("fetch slot0+liquidity");
            (provider, sp, tick_wide, liq)
        });
    let tick: i32 = tick_i256.try_into().expect("tick fits i32");
    let liquidity: u128 = liquidity_u256.to::<u128>();
    eprintln!(
        "[v3-iia-fixture] block={block} sqrtPriceX96={sqrt_price_x96} tick={tick} liquidity={liquidity}"
    );

    // 2. Build a Sparse V3PoolState with the bootstrap-backed fetcher.
    let bootstrap = Arc::new(AlloyTickBootstrapRpc::new(Arc::clone(&provider)));
    let fetcher: Arc<dyn TickWordFetcher> = Arc::new(BootstrapFetcher {
        address: pool,
        rpc: Arc::clone(&bootstrap),
    });
    let params = RegisterV3PoolParams {
        address: pool,
        token0: Address::ZERO,
        token1: Address::ZERO,
        fee: FEE,
        tick_spacing: TICK_SPACING,
        factory: Address::ZERO,
        sqrt_price_x96,
        liquidity,
        tick,
        tick_data: HashMap::new(),
        update_block: block,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: Some(Arc::clone(&fetcher)),
        deployer: Address::ZERO,
        init_hash: alloy::primitives::B256::ZERO,
    };
    let (_identity, mut state) = V3PoolState::from_params(params, 8);

    // 3. Run v3_simulate_swap with the fetch-merge-retry miss loop (the
    //    production pattern — `v3_simulate_swap` returns `MissingTickWord` and
    //    the caller merges + retries).
    let zero_for_one = false; // ofz
    let amount_specified = I256::try_from(U256::from(AMOUNT_IN)).unwrap();
    let limit = V3PoolState::default_sqrt_price_limit(zero_for_one);
    let sim_out = loop {
        match v3_simulate_swap(
            &state,
            FEE,
            TICK_SPACING,
            zero_for_one,
            amount_specified,
            limit,
        ) {
            Ok(o) => break v3_exact_in_output_ofz(&o),
            Err(SimulateSwapError::MissingTickWord(word)) => {
                let new_word = fetcher
                    .fetch_missing_tick_word(0, word, block)
                    .expect("fetch missing word");
                // `merge_tick_word` marks the word known + invalidates the
                // tick-range cache (so the retry sees the new ticks).
                state.merge_tick_word(&new_word);
            }
            Err(e) => panic!("[v3-iia-fixture] v3_simulate_swap error: {e:?}"),
        }
    };

    eprintln!("[v3-iia-fixture] sim(v3_simulate_swap)={sim_out}");

    // 4. Build the solver's sequence from the (now backfilled) state and run
    //    the solver crossing path on the SAME amount_in.
    let Some(seq) = state.build_int_v3_sequence(TICK_SPACING, FEE, zero_for_one, 15) else {
        eprintln!(
            "[v3-iia-fixture] build_int_v3_sequence returned None — sparse backfill only \
             seeded words the swap touched; the solver's 24-range walk needs more. \
             sim_out alone is the decisive signal (see assertion below)."
        );
        assert_eq!(
            sim_out,
            U256::from(CAPTURED_ACTUAL),
            "sim did not reproduce captured actual even without solver side",
        );
        return;
    };
    let solver_out =
        solver_crossing_output(U256::from(AMOUNT_IN), &seq).expect("solver crossing output");

    eprintln!(
        "[v3-iia-fixture] amount_in={AMOUNT_IN} sim(v3_simulate_swap)={sim_out} \
         solver(crossing)={solver_out} captured_actual={CAPTURED_ACTUAL} \
         captured_predicted={CAPTURED_PREDICTED}"
    );
    eprintln!(
        "[v3-iia-fixture] sim_vs_captured_actual delta={}",
        if sim_out > U256::from(CAPTURED_ACTUAL) {
            sim_out - U256::from(CAPTURED_ACTUAL)
        } else {
            U256::from(CAPTURED_ACTUAL) - sim_out
        }
    );
    eprintln!(
        "[v3-iia-fixture] solver_vs_sim delta={}",
        if solver_out > sim_out {
            solver_out - sim_out
        } else {
            sim_out - solver_out
        }
    );

    // DECISIVE ASSERTION. The revm sim's `actual` mirrors `v3_simulate_swap`
    // (the proven byte-exact twin). If `v3_simulate_swap` reproduces the
    // captured ACTUAL on byte-identical on-chain state, then the solver's +13
    // is a topology-specific `compute_crossing` divergence (the fix targets the
    // solver's crossing accumulator, not pump ordering). If instead it matches
    // the captured PREDICTED, the sim ran against different state than
    // on-chain@N (stale engine state — fix pump ordering).
    assert_eq!(
        sim_out,
        U256::from(CAPTURED_ACTUAL),
        "v3_simulate_swap on byte-identical on-chain@N state did NOT reproduce the \
         captured revm-sim actual ({CAPTURED_ACTUAL}) — got {sim_out}. This means the revm sim's \
         `actual` came from DIFFERENT state than on-chain@N → STALE ENGINE STATE (ph1 confirmed; \
         fix pump ordering). [conversely if it matched, the +13 is solver math]"
    );
}
