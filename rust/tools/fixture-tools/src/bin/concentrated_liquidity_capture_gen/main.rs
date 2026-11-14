// Run-once fixture generator: stdout/stderr reports ARE its interface, and its
// eyes-on write path reads clearer as one loop nest than behind extraction helpers.
#![expect(
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

//! Offline generator for the all-concentrated-liquidity capture fixture.
//!
//! For a curated set of REAL, liquid UNI V3 pools (heavy tick maps,
//! `tick_spacing` 1/5/10) at a pinned block, fetches on-chain slot0/liquidity,
//! backfills the tick map via a giant bidirectional `v3_simulate_swap` (the
//! production sparse-backfill pattern, pull real tick words from the archive
//! node), builds each pool's `IntV3TickRangeSequence` via the same
//! `build_int_v3_sequence` the bot uses, forms valid 2-hop all-concentrated-liquidity paths by
//! shared token, runs `solve_concentrated_liquidity_piecewise` on each, and writes (input sequences
//! + golden) to a JSONL consumed by `concentrated_liquidity_solve_replay`. Deterministic,
//! network-gated source of real heavy-concentrated-liquidity data, no full bot soak.
//!
//! # Home
//!
//! This program lives in `degenbot-fixture-tools` (`src/bin/<name>/main.rs`), not in
//! `degenbot-rpc`'s `examples/` and not beside the kernel whose goldens it produces. It is
//! fixture tooling, and the placement is the point: a top-level tool package may depend on
//! BOTH product crates, while `degenbot-rpc` already depends on `degenbot-pools`, so a
//! dev-dependency from pools would have closed a publish cycle (pools -> rpc -> pools) that
//! no crates.io version can satisfy. No product crate depends on a tool, so this arrival adds
//! no edge in that direction.
//!
//! Nothing about the generator changed with the move: the curated pool pins, the pinned-block
//! handling, the slot0/liquidity reads, the bidirectional backfill and its tick-word fetch cap,
//! the production `build_int_v3_sequence` / path-formation steps, the solver golden generation,
//! the JSONL schema, the `.block` sidecar's semantics, the four environment keys with their
//! defaults and their error behaviour, and the single optional output argument all travelled
//! with the source. It needs NO feature lane: it opens no statement ledger and no operator
//! database, so `just check-dev-tools default` is the lane that builds it.
//!
//! # Output
//!
//! The output argument is unchanged — one optional positional path:
//!
//!     cargo run --locked --manifest-path rust/Cargo.toml \
//!         -p degenbot-fixture-tools --bin concentrated_liquidity_capture_gen \
//!         -- [<out.jsonl>]
//!
//! Without it the generator writes the corpus fixture the offline replay reads,
//! `rust/crates/foundation/degenbot-pools/tests/fixtures/concentrated_liquidity_capture_offline.jsonl`,
//! resolved from the repository root through `degenbot_fixture_tools::paths`. That is the
//! identical file the old `CARGO_MANIFEST_DIR`-sibling arithmetic named, but the resolver
//! returns it by walking the repository's own markers and fails loudly instead of writing into
//! whichever directory a moved package happens to sit beside. No fixture-home flag was
//! invented: the corpus file belongs to the pools crate's tests, and a second fixture home is
//! exactly what this move must not create.
//!
//! The write contract is unchanged. Rows are written in path order under the per-block path cap,
//! and a `.block` sidecar is written beside the output recording the pinned block plus the regen
//! command — now the tool package/bin form — so the offline fixture stays reproducible. The
//! sidecar overwrite is the same `fs::write` as before; there is no `--check`, no dry-run arm,
//! and no test runs this program.
//!
//! Environment (the only four keys it reads; `DEGENBOT_CLCAP_RPC` unset aborts with exit 1):
//!
//!   `DEGENBOT_CLCAP_RPC=http://host.containers.internal:8545`,
//!   `DEGENBOT_CLCAP_BLOCK=<pinned block>` (default 25826800),
//!   `DEGENBOT_CLCAP_MAX_FETCHES=320` (raise it to backfill a denser active set),
//!   `DEGENBOT_CLCAP_PATH_CAP=12`.
//!
//! This generator is the ONLY sanctioned producer of exact-wei goldens
//! (loop-18 T4): it writes sequences + the current solver's result from ONE
//! pinned block, so goldens and solver share an epoch. Production captures
//! (`DEGENBOT_SOLVER_CAPTURE*`) append working rows to NON-fixture paths —
//! never paste them into a fixture: their pool state and their recorded
//! answer come from different contexts. For stale epochs, refresh via this
//! generator (preferred) or `DR_REPLAY_REGEN=1` on `concentrated_liquidity_solve_replay` for
//! over-shoot-only stale goldens.
//!
//! The replay that reads the result is `cargo run --locked --manifest-path rust/Cargo.toml
//! -p degenbot-pools --features capture-fixtures --example concentrated_liquidity_solve_replay
//! <capture.jsonl>`, wrapped by `just test-golden-replay`.

use hashbrown::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use alloy::primitives::{Address, B256, I256, U256};
use degenbot_fixture_tools::paths;
use degenbot_pools::ConcentratedLiquidityPoolMut;
use degenbot_pools::FetchedTickWord;
use degenbot_pools::IntV3TickRangeSequence;
use degenbot_pools::TickWordKey;
use degenbot_pools::TickWordSource;
use degenbot_pools::{
    v3_simulate_swap, PoolTickCoverage, RegisterV3PoolParams, SimulateSwapError, V3PoolState,
};
use degenbot_rpc::abi::{ContractRead, IUniswapV3Pool, Slot0Prefix};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_rpc::AlloyTickWordSource;
use serde_json::{json, Value};

const MAX_FETCHES: usize = 320; // per-pool tick-word fetch cap, overridable
                                // via DEGENBOT_CLCAP_MAX_FETCHES so a deep backfill can reach dense active
                                // sets (a liquid pool's busy region can span thousands of tick words).
const PATH_CAP: usize = 12; // per-block cap, overridable via DEGENBOT_CLCAP_PATH_CAP

/// The committed capture corpus, repository-relative: the file this program writes when no
/// output path is given, and the file `just test-golden-replay` reads back. It belongs to
/// `degenbot-pools`' tests, so it is named in full instead of being derived from this
/// package's position in the tree.
const DEFAULT_OUTPUT_RELATIVE: &str =
    "rust/crates/foundation/degenbot-pools/tests/fixtures/concentrated_liquidity_capture_offline.jsonl";

/// [`DEFAULT_OUTPUT_RELATIVE`] under the resolved repository root — the default output path.
///
/// An unresolvable root is an error, never a fallback guess: a guessed root would write the
/// capture into a different tree without failing.
fn default_output_path() -> Result<PathBuf, String> {
    paths::repo_relative(DEFAULT_OUTPUT_RELATIVE)
}

/// The `.block` sidecar body: the pinned block plus the exact command that reproduces the
/// capture, in the shape the committed sidecars carry.
fn sidecar_contents(block: u64, max_fetches: usize) -> String {
    format!(
        "block={block}\nregen: DEGENBOT_CLCAP_RPC=<rpc> DEGENBOT_CLCAP_BLOCK={block} DEGENBOT_CLCAP_MAX_FETCHES={max_fetches} cargo run -q -p degenbot-fixture-tools --bin concentrated_liquidity_capture_gen [-- <out.jsonl>]\n"
    )
}

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

/// The miss-path fetch over the one unified source: build the pool's read
/// descriptor key and map `Ok(None)` (all-zero bitmap) to a known-empty word.
fn fetch_word(
    source: &AlloyTickWordSource,
    address: Address,
    word: i32,
    block: u64,
) -> Result<FetchedTickWord, degenbot_pools::TickWordFetchError> {
    let key = TickWordKey::V3 {
        address,
        tick_spacing: 10,
    };
    source
        .fetch_tick_word(&key, word, block)
        .map(|hit| degenbot_pools::checked_word(word, hit).word)
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
    let path_cap: usize = std::env::var("DEGENBOT_CLCAP_PATH_CAP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(PATH_CAP);
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
    // The fixture belongs to `degenbot-pools` (its offline replay gate reads it), so the
    // default is that crate's corpus file under the resolved repository root: a path relative
    // to this package's directory - or to the invocation's CWD - would write it elsewhere.
    // An explicit argv path wins over the default.
    let out = match std::env::args().nth(1) {
        Some(arg) => arg,
        None => match default_output_path() {
            Ok(path) => path.display().to_string(),
            Err(err) => {
                eprintln!("default output path unresolved: {err}");
                return ExitCode::FAILURE;
            }
        },
    };

    // 1) Slot0+liquidity per pool on a throwaway runtime, then drop.
    let setup: Vec<(U256, I256, U256)> =
        tokio::runtime::Runtime::new().expect("rt").block_on(async {
            let provider = Arc::new(AlloyProvider::new(&rpc, 10).await.expect("provider"));
            let mut out = Vec::with_capacity(POOLS.len());
            for (a, _ts, _f, _t0, _t1) in POOLS {
                let pool = addr(a);
                let slot0 = Slot0Prefix::read(&provider, pool, Some(block))
                    .await
                    .expect("slot0");
                let liquidity = provider
                    .read(pool, IUniswapV3Pool::liquidityCall {}, Some(block))
                    .await
                    .expect("liquidity");
                out.push((slot0.sqrt_price_x96, slot0.tick, U256::from(liquidity)));
            }
            out
        });

    // 2) Provider kept for the SYNC bootstrap misses (they use get_runtime).
    let provider = tokio::runtime::Runtime::new()
        .expect("rt")
        .block_on(async { Arc::new(AlloyProvider::new(&rpc, 10).await.expect("p")) });
    let bootstrap = Arc::new(AlloyTickWordSource::new(Arc::clone(&provider)));

    let mut seqs: Vec<(usize, IntV3TickRangeSequence)> = Vec::new();
    for (i, pool) in POOLS.iter().enumerate() {
        let (a, ts, fee, t0s, t1s) = pool;
        let (sqrt, tick_i256, liq_u256) = &setup[i];
        let p = addr(a);
        let fetcher: Arc<dyn degenbot_pools::TickWordSource> =
            Arc::new(AlloyTickWordSource::clone(&bootstrap));
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
            slot_layout: degenbot_pools::ConcentratedLiquiditySlotLayout::UniswapV3,
            known_bitmap_words: Vec::new(),
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
                        match fetch_word(&bootstrap, p, word, block) {
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
        match state.build_int_v3_sequence(*ts, *fee, false) {
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
            let t0 = std::time::Instant::now();
            let out = degenbot_pools::derive_and_solve_concentrated_liquidity_piecewise(
                &[sa, sb],
                &degenbot_pools::SolveRuntimeConfig::default(),
            );
            let res = out.result;
            let micros = t0.elapsed().as_micros();
            let (pieces, sims) = (out.stats.pieces, out.stats.sims);
            let profitable = res.is_some();
            n_paths += 1;
            if profitable {
                n_profitable += 1;
            }
            let golden = match &res {
                None => Value::Null,
                Some(solution) => json!({
                    "optimal_input": solution.optimal_input.to_string(),
                    "hop_outputs": solution.hop_outputs.iter().map(std::string::ToString::to_string).collect::<Vec<_>>(),
                    "n_hops": solution.hop_outputs.len() }),
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
            if n_paths >= path_cap {
                break 'outer;
            }
        }
    }
    eprintln!("---- wrote {n_paths} path(s), {n_profitable} profitable -> {out}");
    // Record the pinned block + regen command in a sidecar so the offline
    // fixture is reproducible and any drift is explainable.
    let _ = std::fs::write(format!("{out}.block"), sidecar_contents(block, max_fetches));
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    // A test module may fail loudly; the binary's own code may not.
    #![expect(clippy::expect_used)]

    use super::{default_output_path, sidecar_contents, DEFAULT_OUTPUT_RELATIVE};
    use degenbot_fixture_tools::paths;

    /// Pins the default output: the committed corpus under the resolved repository root,
    /// spelled out segment by segment, and the corpus the offline replay gate reads.
    #[test]
    fn default_output_is_the_committed_pools_corpus_fixture() {
        let resolved = default_output_path().expect("resolved default output");
        let root = paths::repo_root().expect("repository root");
        assert_eq!(resolved, root.join(DEFAULT_OUTPUT_RELATIVE));
        assert_eq!(
            resolved,
            root.join(
                "rust/crates/foundation/degenbot-pools/tests/fixtures/concentrated_liquidity_capture_offline.jsonl"
            ),
            "the repository-relative path is spelled out, not derived from `..` arithmetic"
        );
        assert_eq!(
            resolved.extension().and_then(|ext| ext.to_str()),
            Some("jsonl")
        );
        assert!(
            resolved.is_file(),
            "the committed corpus fixture exists: {}",
            resolved.display()
        );
        assert!(
            resolved.with_extension("jsonl.block").is_file(),
            "the committed sidecar exists beside it: {}",
            resolved.with_extension("jsonl.block").display()
        );
    }

    /// Pins the sidecar's write contract: the pinned block plus the regen command that
    /// reproduces the capture, naming this package and binary.
    #[test]
    fn sidecar_records_the_pinned_block_and_the_tool_command() {
        let sidecar = sidecar_contents(25_879_446, 320);
        assert!(
            sidecar.starts_with("block=25879446\nregen: "),
            "the block leads the sidecar: {sidecar}"
        );
        assert!(
            sidecar.contains(
                "DEGENBOT_CLCAP_BLOCK=25879446 DEGENBOT_CLCAP_MAX_FETCHES=320 cargo run -q -p degenbot-fixture-tools --bin concentrated_liquidity_capture_gen [-- <out.jsonl>]"
            ),
            "the regen command names the tool package/bin: {sidecar}"
        );
        assert!(
            !sidecar.contains("--example"),
            "no example invocation survives in the recorded provenance: {sidecar}"
        );
    }
}
