//! Record the offline oracle-answer corpus for the fork-bound on-chain parity
//! tests into the per-block `OfflineProvider` JSON home
//! (`tests/fixtures/chain_data/<chain_id>/py_oracle_<scenario>_block<N>.json`).
//!
//! Each scenario derives its call surface from the parity tests' committed
//! golden file - the canonical record of which on-chain calls the test's
//! replay asserts against - then drives the real `eth_call`s at the pinned
//! block through [`RecordingTransport`] and projects the recording ledger onto
//! the existing `OfflineProvider` wire shape (the v1 `format` marker,
//! `chain_id`, `block_number`, `timestamp`, `calls`, `code`). One capture
//! format per corpus: no third serialization, and nothing lands in the
//! wire-cassette home.
//!
//! The recorded answers ARE the oracle: every value comes off the wire, never
//! re-derived. A revert recorded at the pin is stored as `null` (the offline
//! transport replays it as an execution revert); a non-revert node error is a
//! fixture gap and fails the recording instead of being stored.
//!
//! Golden-key symbols and indices resolve through the pools' committed
//! cassettes under `tests/fixtures/chain_data` (the same fixtures the parity
//! tests build from), so the corpus stays a pure function of committed state.
//!
//! Every scenario input is a pinned constant (block, contract addresses, pool
//! pins, the amount set implied by the golden keys), so the corpus is a pure
//! function of the pinned state. Each run records every scenario twice and
//! requires the two passes byte-identical; `--check` additionally requires
//! byte-identity with the corpus on disk (exit 1 on any drift).
//!
//! Usage:
//!
//!     cargo run --locked --manifest-path rust/Cargo.toml -p degenbot \
//!         --example record_py_oracle_corpus -- --scenario uniswap_v3_quoter
//!
//!     # Re-record and require byte-identity with the corpus on disk:
//!     cargo run --locked --manifest-path rust/Cargo.toml -p degenbot \
//!         --example record_py_oracle_corpus -- --check \
//!         --scenario uniswap_v3_quoter --scenario uniswap_v4_quoter
//!
//!     # All scenarios (omit --scenario):
//!     cargo run --locked --manifest-path rust/Cargo.toml -p degenbot \
//!         --example record_py_oracle_corpus
//!
//! Node resolution mirrors the parity tests: the ethereum scenarios read
//! `ETHEREUM_ARCHIVE_NODE_HTTP_URI` (the `tests/conftest.py` name) and the
//! base scenarios read `BASE_ARCHIVE_NODE_HTTP_URI` (falling back to the
//! keyless endpoint the aerodrome/pancakeswap tests fork);
//! `--ethereum-node` / `--base-node` / `--arbitrum-node` override. The camelot
//! scenario additionally needs an endpoint that serves contract state at its
//! pinned block - when none does, the scenario fails with the node's own
//! error and writes nothing (an absent corpus is not substituted).

// Run-once diagnostic example: stdout/stderr reports ARE its interface (the
// record_updater_cassette precedent), and scenario wiring reads clearest as
// one body per stage.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::expect_used
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::{hex, keccak256, Address, Bytes, U256};
use degenbot::rpc::cassette::{
    rfc3339_utc, Cassette, CassetteProvenance, CassetteResponse, CassetteSpan, RecordingTransport,
};
use degenbot::rpc::provider::AlloyProvider;
use serde::Serialize;
use serde_json::Value;

/// Ethereum parity pin: the block the ethereum scenarios serve.
const ETHEREUM_PARITY_BLOCK: u64 = 24_407_242;
/// Base parity pin: the block the aerodrome/pancakeswap scenarios serve.
const BASE_PARITY_BLOCK: u64 = 46_875_151;
/// Arbitrum parity pin: the camelot scenario's block.
const CAMELOT_PARITY_BLOCK: u64 = 477_785_000;
/// Curve RAI/3Crv metapool single-block pin.
const CURVE_METAPOOL_BLOCK: u64 = 25_144_000;
/// Curve metapool multiblock pins: 16 blocks, step 30, first..=last inclusive.
const CURVE_MULTIBLOCK_FIRST: u64 = 18_850_030;
const CURVE_MULTIBLOCK_LAST: u64 = 18_850_480;
const CURVE_MULTIBLOCK_STEP: u64 = 30;

/// The v3 parity test's oracle contract: Uniswap V3 `QuoterV2`.
const UNISWAP_V3_QUOTER: &str = "0xb27308f9F90D607463bb33eA1BeBb41C27CE5AB6";
/// The v4 parity test's oracle contract: Uniswap V4 `Quoter`.
const UNISWAP_V4_QUOTER: &str = "0x52F0E24D1c21C8A0cB1e5a5dD6198556BD9E1203";
/// The balancer scenarios' oracle contract: `BalancerQueries`.
const BALANCER_QUERIES: &str = "0xE39B5e3B6D74016b2F6A9673D7d7493B6DF549d5";
/// The `querySwap` fund recipient the parity tests pass.
const VITALIK: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
/// The aerodrome v3 scenario's oracle contract: the slipstream `QuoterV2`.
const AERODROME_V3_QUOTER: &str = "0x254cF9E1E6e233aa1AC962CB9B05b2cfeAaE15b0";
/// The pancakeswap scenario's oracle contract: the V2 router.
const PANCAKE_V2_ROUTER: &str = "0x8cFe327CEc66d1C090Dd72bd0FF11d690C33a2Eb";

/// Curve pool pins (one oracle pool per scenario).
const CURVE_TRIPOOL: &str = "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7";
const CURVE_TRICRYPTO: &str = "0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5";
const CURVE_METAPOOL: &str = "0x618788357D0EBd8A37e763ADab3bc575D54c2C7d";

/// Aerodrome v2 pool pins (the pools themselves are the oracle contracts).
const AERODROME_V2_VOLATILE_POOL: &str = "0x2722C8f9B5E2aC72D1f225f8e8c990E449ba0078";
const AERODROME_V2_STABLE_POOL: &str = "0x0B25c51637c43decd6CC1C1e3da4518D54ddb528";
/// The aerodrome v3 cassette carries no token addresses; the parity test pins
/// them, and the recorder mirrors those pins.
const AERODROME_V3_CBETH: &str = "0x2Ae3F1Ec7F1F5012CFEab0185bfc7aa3cf0DEc22";
const AERODROME_V3_WETH: &str = "0x4200000000000000000000000000000000000006";

/// Pool pins the calldata encoders need (the pools themselves are never
/// called - the golden key embeds them, the quoter call encodes them).
const UNISWAP_V3_FEE: u64 = 3_000;
const UNISWAP_V4_FEE: u64 = 500;
const UNISWAP_V4_TICK_SPACING: u64 = 10;

/// V3 swap boundary prices: the parity tests swap token0->token1 with MIN+1
/// and token1->token0 with MAX-1 as the price limit.
const MIN_SQRT_RATIO: U256 = U256::from_limbs([0x1_0002_76a3, 0, 0, 0]);
const MAX_SQRT_RATIO: U256 =
    U256::from_limbs([0x5d95_1d52_6398_8d26, 0xefd1_fc6a_5064_8849, 0xfffd_8963, 0]);

const GOLDEN_ROOT: &str = "tests/golden/data/tests";
const CHAIN_DATA_ROOT: &str = "tests/fixtures/chain_data";

const BALANCER_STABLE_CASSETTES: &[&str] = &[
    "balancer_stable_wsteth_weth.json",
    "balancer_stable_cbeth_wsteth.json",
    "balancer_stable_tusd_bsp.json",
    "balancer_stable_bb_s_usd.json",
];
const BALANCER_WEIGHTED_CASSETTES: &[&str] = &[
    "balancer_weighted_weth_bal.json",
    "balancer_weighted_usdc_weth.json",
    "balancer_weighted_weth_rpl.json",
];
const BALANCER_EXPANDED_TWO_TOKEN_CASSETTES: &[&str] = &[
    "balancer_weighted_aura_kaiaura_50_50.json",
    "balancer_weighted_gel_dexg_60_40.json",
    "balancer_weighted_par_mimo_75_25.json",
    "balancer_weighted_sdvecrv_crv_90_10.json",
    "balancer_weighted_dai_weth_40_60.json",
    "balancer_weighted_usdc_weth_50_50.json",
];
const BALANCER_EXPANDED_MULTI_TOKEN_CASSETTES: &[&str] = &[
    "balancer_weighted_apu_pepe_spx_3_token.json",
    "balancer_weighted_dai_gp_weth_usdt_4_token.json",
];
const CURVE_METAPOOL_SINGLE_CASSETTE: &str = "curve_metapool_rai_3crv_block_25144000.json";
const CURVE_METAPOOL_MULTIBLOCK_CASSETTE: &str =
    "curve_metapool_rai_3crv_multiblock_18850030_18850480.json";

/// Which endpoint tier serves a scenario, resolved like the parity tests do.
#[derive(Clone, Copy)]
enum NodeTier {
    /// `ETHEREUM_ARCHIVE_NODE_HTTP_URI` - the ethereum parity fork URL.
    Ethereum,
    /// `BASE_ARCHIVE_NODE_HTTP_URI` - what the base parity tests fork.
    Base,
    /// The camelot test's keyless public endpoint.
    Arbitrum,
}

/// One parity scenario: its golden-file surface, its pin, and its node tier.
#[derive(Clone)]
struct Scenario {
    /// Corpus file stem: `py_oracle_<name>_block<N>.json`.
    name: &'static str,
    chain_id: u64,
    block: u64,
    /// Block the golden file's header pins (differs from `block` only for the
    /// multiblock golden, whose header names the loop's base block).
    golden_block: u64,
    /// Committed golden file the call surface derives from, relative to the
    /// repo root.
    golden: String,
    /// Committed pool cassettes the golden keys' symbols and indices resolve
    /// through, relative to the repo root.
    cassettes: Vec<String>,
    /// Contracts whose runtime code joins the corpus `code` map.
    code_targets: Vec<String>,
    node: NodeTier,
}

fn balancer_scenario(
    name: &'static str,
    golden_dir: &str,
    golden_file: &str,
    cassettes: &'static [&'static str],
) -> Scenario {
    Scenario {
        name,
        chain_id: 1,
        block: ETHEREUM_PARITY_BLOCK,
        golden_block: ETHEREUM_PARITY_BLOCK,
        golden: format!("{GOLDEN_ROOT}/balancer/{golden_dir}/{golden_file}.json"),
        cassettes: cassettes
            .iter()
            .map(|f| format!("{CHAIN_DATA_ROOT}/1/{f}"))
            .collect(),
        code_targets: vec![BALANCER_QUERIES.to_string()],
        node: NodeTier::Ethereum,
    }
}

fn curve_scenario(name: &'static str, golden_file: &str, code_target: &'static str) -> Scenario {
    let (cassette, golden_block) = match name {
        "curve_metapool_get_dy" => (CURVE_METAPOOL_SINGLE_CASSETTE, CURVE_METAPOOL_BLOCK),
        "curve_metapool_multiblock" => (CURVE_METAPOOL_MULTIBLOCK_CASSETTE, 18_850_000),
        _ => (
            match name {
                "curve_tripool_get_dy" | "curve_tripool_calc_base_pool" => {
                    "curve_tripool_block_24407242.json"
                }
                _ => "curve_tricrypto_block_24407242.json",
            },
            ETHEREUM_PARITY_BLOCK,
        ),
    };
    Scenario {
        name,
        chain_id: 1,
        block: match name {
            "curve_metapool_get_dy" => CURVE_METAPOOL_BLOCK,
            "curve_metapool_multiblock" => CURVE_MULTIBLOCK_FIRST,
            _ => ETHEREUM_PARITY_BLOCK,
        },
        golden_block,
        golden: format!("{GOLDEN_ROOT}/curve/test_curve_onchain_parity/{golden_file}.json"),
        cassettes: vec![format!("{CHAIN_DATA_ROOT}/1/{cassette}")],
        code_targets: vec![code_target.to_string()],
        node: NodeTier::Ethereum,
    }
}

fn base_scenario(
    name: &'static str,
    golden_file: &str,
    cassette: &str,
    code_target: &'static str,
) -> Scenario {
    Scenario {
        name,
        chain_id: 8453,
        block: BASE_PARITY_BLOCK,
        golden_block: BASE_PARITY_BLOCK,
        golden: format!("{GOLDEN_ROOT}/{golden_file}.json"),
        cassettes: vec![format!("{CHAIN_DATA_ROOT}/8453/{cassette}")],
        code_targets: vec![code_target.to_string()],
        node: NodeTier::Base,
    }
}

/// Every wired scenario, in family order (the multiblock metapool expands to
/// one scenario per pinned block).
fn all_scenarios() -> Vec<Scenario> {
    let mut scenarios = vec![
        // Uniswap (stage-one scenarios).
        Scenario {
            name: "uniswap_v3_quoter",
            chain_id: 1,
            block: ETHEREUM_PARITY_BLOCK,
            golden_block: ETHEREUM_PARITY_BLOCK,
            golden: format!(
                "{GOLDEN_ROOT}/uniswap/v3/test_uniswap_v3_onchain_parity/test_cached_calculations_v3_wbtc_weth.json"
            ),
            cassettes: vec![],
            code_targets: vec![UNISWAP_V3_QUOTER.to_string()],
            node: NodeTier::Ethereum,
        },
        Scenario {
            name: "uniswap_v4_quoter",
            chain_id: 1,
            block: ETHEREUM_PARITY_BLOCK,
            golden_block: ETHEREUM_PARITY_BLOCK,
            golden: format!(
                "{GOLDEN_ROOT}/uniswap/v4/test_uniswap_v4_onchain_parity/test_cached_calculations_v4_eth_usdc.json"
            ),
            cassettes: vec![],
            code_targets: vec![UNISWAP_V4_QUOTER.to_string()],
            node: NodeTier::Ethereum,
        },
        Scenario {
            name: "camelot_v2_get_amount_out",
            chain_id: 42161,
            block: CAMELOT_PARITY_BLOCK,
            golden_block: CAMELOT_PARITY_BLOCK,
            golden: format!(
                "{GOLDEN_ROOT}/uniswap/v2/test_camelot_v2_onchain_parity/test_create_camelot_v2_pool.json"
            ),
            cassettes: vec![],
            code_targets: vec!["0x84652bb2539513BAf36e225c930Fdd8eaa63CE27".to_string()],
            node: NodeTier::Arbitrum,
        },
        // Balancer: one scenario per golden file (each file aggregates the
        // parametrization's pools; the keys name their pool).
        balancer_scenario(
            "balancer_stable_given_in",
            "test_balancer_stable_onchain_parity",
            "test_balancer_v2_stable_query_swap_given_in",
            BALANCER_STABLE_CASSETTES,
        ),
        balancer_scenario(
            "balancer_stable_given_out",
            "test_balancer_stable_onchain_parity",
            "test_balancer_v2_stable_query_swap_given_out",
            BALANCER_STABLE_CASSETTES,
        ),
        balancer_scenario(
            "balancer_weighted_weth_bal",
            "test_balancer_v2_onchain_parity",
            "test_balancer_v2_weth_bal_query_swap",
            &BALANCER_WEIGHTED_CASSETTES[..1],
        ),
        balancer_scenario(
            "balancer_weighted_usdc_weth",
            "test_balancer_v2_onchain_parity",
            "test_balancer_v2_usdc_weth_query_swap",
            &BALANCER_WEIGHTED_CASSETTES[1..2],
        ),
        balancer_scenario(
            "balancer_weighted_weth_rpl",
            "test_balancer_v2_onchain_parity",
            "test_balancer_v2_weth_rpl_query_swap",
            &BALANCER_WEIGHTED_CASSETTES[2..3],
        ),
        balancer_scenario(
            "balancer_expanded_two_token_given_in",
            "test_balancer_v2_onchain_parity",
            "test_balancer_v2_expanded_two_token_given_in",
            BALANCER_EXPANDED_TWO_TOKEN_CASSETTES,
        ),
        balancer_scenario(
            "balancer_expanded_two_token_given_out",
            "test_balancer_v2_onchain_parity",
            "test_balancer_v2_expanded_two_token_given_out",
            BALANCER_EXPANDED_TWO_TOKEN_CASSETTES,
        ),
        balancer_scenario(
            "balancer_expanded_multi_token_given_in",
            "test_balancer_v2_onchain_parity",
            "test_balancer_v2_expanded_multi_token_given_in",
            BALANCER_EXPANDED_MULTI_TOKEN_CASSETTES,
        ),
        balancer_scenario(
            "balancer_expanded_multi_token_given_out",
            "test_balancer_v2_onchain_parity",
            "test_balancer_v2_expanded_multi_token_given_out",
            BALANCER_EXPANDED_MULTI_TOKEN_CASSETTES,
        ),
        // Curve.
        curve_scenario("curve_tripool_get_dy", "test_curve_tripool_get_dy", CURVE_TRIPOOL),
        curve_scenario(
            "curve_tricrypto_get_dy",
            "test_curve_tricrypto_get_dy",
            CURVE_TRICRYPTO,
        ),
        curve_scenario(
            "curve_tripool_calc_base_pool",
            "test_curve_tripool_calc_withdraw_and_token_amount",
            CURVE_TRIPOOL,
        ),
        curve_scenario("curve_metapool_get_dy", "test_curve_metapool_get_dy", CURVE_METAPOOL),
        // Aerodrome + pancakeswap (Base).
        base_scenario(
            "aerodrome_v2_volatile_get_amount_out",
            "aerodrome/test_aerodrome_v2_onchain_parity/test_aerodrome_v2_volatile_get_amount_out",
            "aerodrome_v2_tbtc_weth_volatile_block_46875151.json",
            AERODROME_V2_VOLATILE_POOL,
        ),
        base_scenario(
            "aerodrome_v2_stable_get_amount_out",
            "aerodrome/test_aerodrome_v2_onchain_parity/test_aerodrome_v2_stable_get_amount_out",
            "aerodrome_v2_dola_usdbc_stable_block_46875151.json",
            AERODROME_V2_STABLE_POOL,
        ),
        base_scenario(
            "aerodrome_v3_quote",
            "aerodrome/test_aerodrome_v3_onchain_parity/test_aerodrome_v3_cbeth_weth_quote",
            "aerodrome_v3_cbeth_weth_block_46875151.json",
            AERODROME_V3_QUOTER,
        ),
        base_scenario(
            "pancakeswap_v2_router_get_amounts_out",
            "pancakeswap/test_pancakeswap_v2_onchain_parity/test_pancakeswap_v2_router_get_amounts_out",
            "pancakeswap_v2_weth_usdbc_block_46875151.json",
            PANCAKE_V2_ROUTER,
        ),
    ];
    let multiblock_golden = "test_curve_metapool_multiblock_get_dy";
    let mut block = CURVE_MULTIBLOCK_FIRST;
    while block <= CURVE_MULTIBLOCK_LAST {
        let mut scenario = curve_scenario(
            "curve_metapool_multiblock",
            multiblock_golden,
            CURVE_METAPOOL,
        );
        scenario.block = block;
        scenarios.push(scenario);
        block += CURVE_MULTIBLOCK_STEP;
    }
    scenarios
}

/// The format marker written into every v1 corpus - the wire-cassette
/// `degenbot.cassette/v1` convention for the per-block JSON.
const CHAIN_DATA_FORMAT_V1: &str = "degenbot.chain-data/v1";

/// The per-block `OfflineProvider` wire shape (single-block format), field
/// order matching the recorded fixtures the offline transport consumes.
#[derive(Serialize)]
struct CorpusJson {
    /// The format marker - [`CHAIN_DATA_FORMAT_V1`] on any current corpus.
    format: &'static str,
    chain_id: u64,
    block_number: u64,
    timestamp: u64,
    /// `"<to>:0x<data>"` -> result hex without the `0x` prefix, or `null` for
    /// a recorded revert.
    calls: BTreeMap<String, Option<String>>,
    /// `"<address>"` -> runtime bytecode hex without the `0x` prefix.
    code: BTreeMap<String, String>,
}

/// One derived call: the wire `to`/calldata pair a golden key asks for.
struct DerivedCall {
    to: Address,
    calldata: Vec<u8>,
}

/// lowercase hex of `bytes` without prefix - the calldata half of the
/// `OfflineProvider` call keys.
fn hex_lower(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// `0x`-prefixed lowercase hex of `bytes` - the address half of the
/// `OfflineProvider` call/code keys.
fn hex_lower_prefixed(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn strip_0x(s: &str) -> &str {
    s.strip_prefix("0x").unwrap_or(s)
}

fn selector(sig: &str) -> [u8; 4] {
    keccak256(sig.as_bytes())[..4]
        .try_into()
        .expect("keccak256 is 32 bytes")
}

fn word_address(a: Address) -> Vec<u8> {
    let mut w = vec![0u8; 12];
    w.extend_from_slice(a.as_slice());
    w
}

fn word_uint(v: U256) -> Vec<u8> {
    v.to_be_bytes::<32>().to_vec()
}

/// Two's-complement 32-byte word for a signed value (tick spacings here).
#[expect(clippy::cast_sign_loss)]
fn word_int(v: i64) -> Vec<u8> {
    if v >= 0 {
        word_uint(U256::from(v as u64))
    } else {
        word_uint(U256::MAX - U256::from((-v - 1) as u64))
    }
}

fn encode_words(sel: [u8; 4], args: &[Vec<u8>]) -> Vec<u8> {
    let mut out = sel.to_vec();
    for a in args {
        out.extend_from_slice(a);
    }
    out
}

/// `quoteExact{Input,Output}Single` on the V4 quoter: one dynamic outer tuple
/// argument, so word 0 is the tuple-data offset, the pool key spans five
/// words, and the empty `hookData` tail is its offset word plus a zero length
/// word.
fn encode_v4_single(
    sel: [u8; 4],
    currency0: Address,
    currency1: Address,
    zero_for_one: bool,
    amount: U256,
) -> Vec<u8> {
    encode_words(
        sel,
        &[
            word_uint(U256::from(0x20)),
            word_address(currency0),
            word_address(currency1),
            word_uint(U256::from(UNISWAP_V4_FEE)),
            word_uint(U256::from(UNISWAP_V4_TICK_SPACING)),
            word_address(Address::ZERO),
            word_uint(U256::from(u64::from(zero_for_one))),
            word_uint(amount),
            word_uint(U256::from(0x100)),
            word_uint(U256::ZERO),
        ],
    )
}

/// `BalancerQueries` `querySwap`: the dynamic `SingleSwap` tuple's head word
/// comes first, the static `FundManagement` spans four inline words, then the
/// swap tail whose trailing empty `bytes` carries its offset word.
fn encode_query_swap(
    pool_id_hex: &str,
    kind: u8,
    asset_in: Address,
    asset_out: Address,
    amount: U256,
    funds_owner: Address,
) -> Result<Vec<u8>, String> {
    let pool_id = hex::decode(strip_0x(pool_id_hex))
        .map_err(|e| format!("cassette pool id {pool_id_hex:?}: {e}"))?;
    if pool_id.len() != 32 {
        return Err(format!("cassette pool id {pool_id_hex:?} is not 32 bytes"));
    }
    Ok(encode_words(
        selector(
            "querySwap((bytes32,uint8,address,address,uint256,bytes),(address,bool,address,bool))",
        ),
        &[
            word_uint(U256::from(0xa0)),
            word_address(funds_owner),
            word_uint(U256::ZERO),
            word_address(funds_owner),
            word_uint(U256::ZERO),
            pool_id,
            word_uint(U256::from(kind)),
            word_address(asset_in),
            word_address(asset_out),
            word_uint(amount),
            word_uint(U256::from(0xc0)),
            word_uint(U256::ZERO),
        ],
    ))
}

/// Curve `calc_token_amount(uint256[<n>],bool)`: a static inline array plus
/// the deposit flag.
fn encode_calc_token_amount(amounts: &[U256], deposit: bool) -> Vec<u8> {
    let mut words: Vec<Vec<u8>> = amounts.iter().map(|a| word_uint(*a)).collect();
    words.push(word_uint(U256::from(u64::from(deposit))));
    encode_words(
        selector(&format!(
            "calc_token_amount(uint256[{}],bool)",
            amounts.len()
        )),
        &words,
    )
}

/// Router `getAmountsOut(uint256,address[])`: the dynamic path array's data
/// starts after the amount and offset head words.
fn encode_get_amounts_out(amount: U256, path: &[Address]) -> Vec<u8> {
    let mut out = selector("getAmountsOut(uint256,address[])").to_vec();
    out.extend_from_slice(&word_uint(amount));
    out.extend_from_slice(&word_uint(U256::from(0x40)));
    out.extend_from_slice(&word_uint(U256::from(path.len())));
    for addr in path {
        out.extend_from_slice(&word_address(*addr));
    }
    out
}

/// Slipstream quoter `quoteExactInputSingle`: one STATIC tuple argument (five
/// inline words, no offset).
fn encode_slipstream_quote(
    token_in: Address,
    token_out: Address,
    amount: U256,
    tick_spacing: i64,
    sqrt_limit: U256,
) -> Vec<u8> {
    encode_words(
        selector("quoteExactInputSingle((address,address,uint256,int24,uint160))"),
        &[
            word_address(token_in),
            word_address(token_out),
            word_uint(amount),
            word_int(tick_spacing),
            word_uint(sqrt_limit),
        ],
    )
}

/// Scenario token symbols -> wire addresses (the pairs the golden keys name,
/// for pools whose cassettes carry no token addresses).
fn symbol_address(scenario_name: &str, symbol: &str) -> Result<Address, String> {
    let raw = match (scenario_name, symbol) {
        ("uniswap_v3_quoter", "WBTC") => "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
        ("uniswap_v3_quoter", "WETH") => "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        ("uniswap_v4_quoter", "ETH") => "0x0000000000000000000000000000000000000000",
        ("uniswap_v4_quoter", "USDC") => "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        ("camelot_v2_get_amount_out", "USDC") => "0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8",
        ("camelot_v2_get_amount_out", "WETH") => "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
        ("aerodrome_v3_quote", "cbETH") => AERODROME_V3_CBETH,
        ("aerodrome_v3_quote", "WETH") => AERODROME_V3_WETH,
        _ => {
            return Err(format!(
                "no {symbol} address wired for scenario {scenario_name}"
            ))
        }
    };
    raw.parse::<Address>()
        .map_err(|e| format!("wired address {raw:?}: {e}"))
}

fn scenario_target(raw: &str) -> Result<Address, String> {
    raw.parse::<Address>()
        .map_err(|e| format!("scenario target {raw:?}: {e}"))
}

/// The cassettes a scenario's golden keys resolve through, keyed by the
/// lowercase pool address each cassette carries (the multiblock metapool
/// cassette nests its address under `immutable`).
fn load_cassettes(
    repo_root: &Path,
    scenario: &Scenario,
) -> Result<BTreeMap<String, Value>, String> {
    let mut map = BTreeMap::new();
    for rel in &scenario.cassettes {
        let raw = std::fs::read_to_string(repo_root.join(rel))
            .map_err(|e| format!("read cassette {rel}: {e}"))?;
        let cassette: Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse cassette {rel}: {e}"))?;
        let address = cassette
            .get("address")
            .or_else(|| cassette.get("pool"))
            .or_else(|| cassette["immutable"].get("address"))
            .and_then(Value::as_str)
            .ok_or_else(|| format!("cassette {rel} carries no pool address"))?;
        map.insert(address.to_ascii_lowercase(), cassette);
    }
    Ok(map)
}

fn only_cassette(cassettes: &BTreeMap<String, Value>) -> Result<&Value, String> {
    if cassettes.len() != 1 {
        return Err(format!(
            "expected exactly one cassette, found {}",
            cassettes.len()
        ));
    }
    Ok(cassettes.values().next().expect("len checked"))
}

/// The cassette's token list for a section name, descending into `immutable`
/// for the multiblock metapool's nested shape.
fn cassette_tokens<'a>(cassette: &'a Value, section: &str) -> Result<&'a Vec<Value>, String> {
    let owner = cassette
        .get(section)
        .or_else(|| cassette["immutable"].get(section))
        .ok_or_else(|| format!("cassette carries no {section}"))?;
    owner
        .as_array()
        .ok_or_else(|| format!("cassette {section} is not a list"))
}

fn cassette_symbol_address(cassette: &Value, symbol: &str) -> Result<Address, String> {
    // Ordered token lists first, then the token0/token1 pair the reserves
    // cassettes carry.
    for section in ["tokens", "tokens_underlying"] {
        if let Ok(tokens) = cassette_tokens(cassette, section) {
            for token in tokens {
                if token["symbol"].as_str() == Some(symbol) {
                    let raw = token["address"].as_str().ok_or("token address")?;
                    return raw
                        .parse::<Address>()
                        .map_err(|e| format!("cassette token {symbol}: {e}"));
                }
            }
        }
    }
    for section in ["token0", "token1"] {
        let token = &cassette[section];
        if token["symbol"].as_str() == Some(symbol) {
            let raw = token["address"].as_str().ok_or("token address")?;
            return raw
                .parse::<Address>()
                .map_err(|e| format!("cassette token {symbol}: {e}"));
        }
    }
    Err(format!("cassette carries no token symbol {symbol:?}"))
}

fn cassette_token_at(cassette: &Value, index: usize) -> Result<Address, String> {
    let tokens = cassette_tokens(cassette, "tokens")?;
    let token = tokens
        .get(index)
        .ok_or_else(|| format!("cassette token index {index} out of range"))?;
    let raw = token["address"].as_str().ok_or("token address")?;
    raw.parse::<Address>()
        .map_err(|e| format!("cassette token {raw:?}: {e}"))
}

fn cassette_pool_id(cassette: &Value) -> Result<&str, String> {
    cassette["pool_id"]
        .as_str()
        .ok_or_else(|| "cassette carries no pool_id".to_string())
}

/// A golden key whose per-method head segments do not match its surface.
const BAD_KEY_HEAD: &str = "golden key head segments do not match the method's surface";

/// Parse a golden key's trailing amount.
fn amount_segment(segment: &str) -> Result<U256, String> {
    U256::from_str_radix(segment, 10).map_err(|e| format!("golden key amount {segment:?}: {e}"))
}

/// The multiblock metapool keys tag their block; it must be the scenario's pin.
fn validate_blk_tag(scenario: &Scenario, tag: &str) -> Result<(), String> {
    let parsed = tag
        .strip_prefix("blk")
        .and_then(|rest| rest.parse::<u64>().ok())
        .ok_or_else(|| format!("golden key block tag not blk<N>: {tag:?}"))?;
    if parsed != scenario.block {
        return Err(format!(
            "golden key pins block {parsed} but scenario {} pins {}",
            scenario.name, scenario.block
        ));
    }
    Ok(())
}

fn index_segment(segment: &str, prefix: &str) -> Result<usize, String> {
    segment
        .strip_prefix(prefix)
        .ok_or_else(|| format!("golden key segment {segment:?} lacks {prefix:?} prefix"))?
        .parse::<usize>()
        .map_err(|e| format!("golden key segment {segment:?}: {e}"))
}

fn token_index(tokens: &[Value], symbol: &str) -> Result<usize, String> {
    for (index, token) in tokens.iter().enumerate() {
        if token["symbol"].as_str() == Some(symbol) {
            return Ok(index);
        }
    }
    Err(format!("cassette tokens carry no symbol {symbol:?}"))
}

/// A golden key's `<SYM-A>-><SYM-B>` direction pair.
fn direction_pair(head: &str) -> Result<(String, String), String> {
    head.split_once("->")
        .map(|(from, to)| (from.to_string(), to.to_string()))
        .ok_or_else(|| format!("golden key pair not <SYM-A>-><SYM-B>: {head:?}"))
}

/// Derive one call from a golden key.
///
/// Key grammar: `<pool-or-pin>|<method>|<head…>|<amount>` (the multiblock
/// metapool keys carry a `blk<N>` head segment), parsed by splitting off the
/// pool, the method, and then the amount from the right so per-method heads
/// can hold their own `|` segments.
fn derive_call(
    scenario: &Scenario,
    key: &str,
    cassettes: &BTreeMap<String, Value>,
) -> Result<DerivedCall, String> {
    let mut segments = key.splitn(3, '|');
    let pool = segments
        .next()
        .ok_or_else(|| format!("empty golden key: {key}"))?;
    let method = segments
        .next()
        .ok_or_else(|| format!("golden key lacks a method: {key}"))?;
    let tail = segments
        .next()
        .ok_or_else(|| format!("golden key lacks a head|amount tail: {key}"))?;
    let (head, amount_str) = tail
        .rsplit_once('|')
        .ok_or_else(|| format!("golden key tail not <head>|<amount>: {tail:?}"))?;
    let head_parts: Vec<&str> = head.split('|').collect();
    let amount = amount_segment(amount_str)?;
    // The v4 quoter keys embed the 32-byte pool id (the calldata's pool id
    // comes from the scenario pin, so the key segment is only a label); every
    // other surface's keys carry the pool address the calldata targets or the
    // cassette lookup needs.
    let pool_addr: Address = if scenario.name == "uniswap_v4_quoter" {
        Address::ZERO
    } else {
        pool.parse()
            .map_err(|e| format!("golden key pool {pool:?}: {e}"))?
    };

    let (to, calldata) = match method {
        "quoteExactInputSingle" | "quoteExactOutputSingle" => match scenario.name {
            "uniswap_v3_quoter" => {
                let [pair]: [&str; 1] = head_parts.try_into().map_err(|_| BAD_KEY_HEAD)?;
                let (from_sym, to_sym) = direction_pair(pair)?;
                let token_in = symbol_address(scenario.name, &from_sym)?;
                let token_out = symbol_address(scenario.name, &to_sym)?;
                let token0 = symbol_address(scenario.name, "WBTC")?;
                let sqrt_limit = if token_in == token0 {
                    MIN_SQRT_RATIO + U256::from(1)
                } else {
                    MAX_SQRT_RATIO - U256::from(1)
                };
                let sig = if method == "quoteExactInputSingle" {
                    "quoteExactInputSingle(address,address,uint24,uint256,uint160)"
                } else {
                    "quoteExactOutputSingle(address,address,uint24,uint256,uint160)"
                };
                (
                    scenario_target(UNISWAP_V3_QUOTER)?,
                    encode_words(
                        selector(sig),
                        &[
                            word_address(token_in),
                            word_address(token_out),
                            word_uint(U256::from(UNISWAP_V3_FEE)),
                            word_uint(amount),
                            word_uint(sqrt_limit),
                        ],
                    ),
                )
            }
            "uniswap_v4_quoter" => {
                let [pair]: [&str; 1] = head_parts.try_into().map_err(|_| BAD_KEY_HEAD)?;
                let (from_sym, _to_sym) = direction_pair(pair)?;
                let currency0 = symbol_address(scenario.name, "ETH")?;
                let currency1 = symbol_address(scenario.name, "USDC")?;
                let sig = if method == "quoteExactInputSingle" {
                    "quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))"
                } else {
                    "quoteExactOutputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))"
                };
                (
                    scenario_target(UNISWAP_V4_QUOTER)?,
                    encode_v4_single(
                        selector(sig),
                        currency0,
                        currency1,
                        from_sym == "ETH",
                        amount,
                    ),
                )
            }
            "aerodrome_v3_quote" => {
                if method != "quoteExactInputSingle" {
                    return Err(format!(
                        "unwired golden surface: {}|{method}",
                        scenario.name
                    ));
                }
                let [pair]: [&str; 1] = head_parts.try_into().map_err(|_| BAD_KEY_HEAD)?;
                let (from_sym, to_sym) = direction_pair(pair)?;
                let cassette = only_cassette(cassettes)?;
                let tick_spacing = cassette["scalars"]["tick_spacing"]
                    .as_i64()
                    .ok_or("cassette tick_spacing")?;
                let token_in = symbol_address(scenario.name, &from_sym)?;
                let token_out = symbol_address(scenario.name, &to_sym)?;
                let cbeth: Address = AERODROME_V3_CBETH.parse().expect("pinned address");
                let weth: Address = AERODROME_V3_WETH.parse().expect("pinned address");
                let token0 = if cbeth.as_slice() < weth.as_slice() {
                    cbeth
                } else {
                    weth
                };
                let sqrt_limit = if token_in == token0 {
                    MIN_SQRT_RATIO + U256::from(1)
                } else {
                    MAX_SQRT_RATIO - U256::from(1)
                };
                (
                    scenario_target(AERODROME_V3_QUOTER)?,
                    encode_slipstream_quote(token_in, token_out, amount, tick_spacing, sqrt_limit),
                )
            }
            other => return Err(format!("unwired golden surface: {other}|{method}")),
        },
        "getAmountOut" => {
            let [pair]: [&str; 1] = head_parts.try_into().map_err(|_| BAD_KEY_HEAD)?;
            let (from_sym, _to_sym) = direction_pair(pair)?;
            match scenario.name {
                "camelot_v2_get_amount_out" => {
                    let token_in = symbol_address(scenario.name, &from_sym)?;
                    (
                        pool_addr,
                        encode_words(
                            selector("getAmountOut(uint256,address)"),
                            &[word_uint(amount), word_address(token_in)],
                        ),
                    )
                }
                "aerodrome_v2_volatile_get_amount_out" | "aerodrome_v2_stable_get_amount_out" => {
                    let cassette = cassettes
                        .get(&hex_lower_prefixed(pool_addr.as_slice()))
                        .ok_or_else(|| format!("no cassette keyed for pool {pool}"))?;
                    let token_in = cassette_symbol_address(cassette, &from_sym)?;
                    (
                        pool_addr,
                        encode_words(
                            selector("getAmountOut(uint256,address)"),
                            &[word_uint(amount), word_address(token_in)],
                        ),
                    )
                }
                other => return Err(format!("unwired golden surface: {other}|{method}")),
            }
        }
        "getAmountsOut" => {
            let [pair]: [&str; 1] = head_parts.try_into().map_err(|_| BAD_KEY_HEAD)?;
            let (from_sym, to_sym) = direction_pair(pair)?;
            let cassette = cassettes
                .get(&hex_lower_prefixed(pool_addr.as_slice()))
                .ok_or_else(|| format!("no cassette keyed for pool {pool}"))?;
            let path = [
                cassette_symbol_address(cassette, &from_sym)?,
                cassette_symbol_address(cassette, &to_sym)?,
            ];
            (
                scenario_target(PANCAKE_V2_ROUTER)?,
                encode_get_amounts_out(amount, &path),
            )
        }
        "querySwap" => {
            let cassette = cassettes
                .get(&hex_lower_prefixed(pool_addr.as_slice()))
                .ok_or_else(|| format!("no cassette keyed for pool {pool}"))?;
            let kind = match head_parts.first() {
                Some(&"GIVEN_IN") => 0u8,
                Some(&"GIVEN_OUT") => 1u8,
                other => return Err(format!("golden key swap kind {other:?}")),
            };
            let (asset_in, asset_out) = match head_parts.len() {
                3 => {
                    let i = index_segment(head_parts[1], "i=")?;
                    let j = index_segment(head_parts[2], "j=")?;
                    (
                        cassette_token_at(cassette, i)?,
                        cassette_token_at(cassette, j)?,
                    )
                }
                2 => {
                    let (from_sym, to_sym) = direction_pair(head_parts[1])?;
                    (
                        cassette_symbol_address(cassette, &from_sym)?,
                        cassette_symbol_address(cassette, &to_sym)?,
                    )
                }
                _ => return Err(BAD_KEY_HEAD.to_string()),
            };
            (
                scenario_target(BALANCER_QUERIES)?,
                encode_query_swap(
                    cassette_pool_id(cassette)?,
                    kind,
                    asset_in,
                    asset_out,
                    amount,
                    scenario_target(VITALIK)?,
                )?,
            )
        }
        "get_dy" | "get_dy_underlying" => {
            let (pair, block_tag) = match head_parts.as_slice() {
                [pair] => (*pair, None),
                [pair, tag] => (*pair, Some(*tag)),
                _ => return Err(BAD_KEY_HEAD.to_string()),
            };
            if let Some(tag) = block_tag {
                validate_blk_tag(scenario, tag)?;
            }
            let (from_sym, to_sym) = direction_pair(pair)?;
            let cassette = only_cassette(cassettes)?;
            let section = if method == "get_dy" {
                "tokens"
            } else {
                "tokens_underlying"
            };
            let tokens = cassette_tokens(cassette, section)?;
            let i = token_index(tokens, &from_sym)?;
            let j = token_index(tokens, &to_sym)?;
            let sig = match method {
                "get_dy" if scenario.name == "curve_tricrypto_get_dy" => {
                    "get_dy(uint256,uint256,uint256)"
                }
                "get_dy" => "get_dy(int128,int128,uint256)",
                _ => "get_dy_underlying(int128,int128,uint256)",
            };
            (
                pool_addr,
                encode_words(
                    selector(sig),
                    &[
                        word_uint(U256::from(i)),
                        word_uint(U256::from(j)),
                        word_uint(amount),
                    ],
                ),
            )
        }
        "calc_withdraw_one_coin" => {
            let [index]: [&str; 1] = head_parts.try_into().map_err(|_| BAD_KEY_HEAD)?;
            let i = index_segment(index, "i=")?;
            (
                pool_addr,
                encode_words(
                    selector("calc_withdraw_one_coin(uint256,int128)"),
                    &[word_uint(amount), word_uint(U256::from(i))],
                ),
            )
        }
        "calc_token_amount" => {
            if head_parts.first() != Some(&"deposit") {
                return Err(format!("golden key calc_token_amount head {head:?}"));
            }
            let index = head_parts
                .get(1)
                .ok_or_else(|| format!("golden key calc_token_amount head {head:?}"))?;
            let i = index_segment(index, "i=")?;
            let cassette = only_cassette(cassettes)?;
            let n = cassette_tokens(cassette, "tokens")?.len();
            let mut amounts = vec![U256::ZERO; n];
            amounts[i] = amount;
            (pool_addr, encode_calc_token_amount(&amounts, true))
        }
        other => return Err(format!("unwired golden surface: {}|{other}", scenario.name)),
    };
    Ok(DerivedCall { to, calldata })
}

/// Parse the scenario's golden file and derive its full call surface.
fn derive_calls(
    scenario: &Scenario,
    golden_path: &Path,
    repo_root: &Path,
) -> Result<Vec<DerivedCall>, String> {
    let raw = std::fs::read_to_string(golden_path)
        .map_err(|e| format!("read golden {}: {e}", golden_path.display()))?;
    let golden: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("parse golden {}: {e}", golden_path.display()))?;
    let recorded_chain = golden["chain_id"].as_u64().ok_or("golden chain_id")?;
    let recorded_block = golden["block_number"]
        .as_u64()
        .ok_or("golden block_number")?;
    if recorded_chain != scenario.chain_id || recorded_block != scenario.golden_block {
        return Err(format!(
            "golden {} pins chain {recorded_chain} block {recorded_block}; scenario pins chain {} block {}",
            golden_path.display(),
            scenario.chain_id,
            scenario.golden_block
        ));
    }
    let cassettes = load_cassettes(repo_root, scenario)?;
    let entries = golden["entries"].as_object().ok_or("golden entries")?;
    let mut calls = Vec::with_capacity(entries.len());
    for key in entries.keys() {
        // Multiblock goldens tag each key's block; a block scenario derives
        // only its own keys (the projection's coverage check pins the rest).
        if let Some(index) = key.find("|blk") {
            let tag = key[index + 1..].split('|').next().unwrap_or_default();
            if tag.strip_prefix("blk").and_then(|b| b.parse::<u64>().ok()) != Some(scenario.block) {
                continue;
            }
        }
        calls.push(derive_call(scenario, key, &cassettes)?);
    }
    Ok(calls)
}

/// Project the recording ledger onto the `OfflineProvider` wire shape.
///
/// Reverts store as `null`; a non-revert failure is a fixture gap (the
/// endpoint cannot serve the pinned state) and fails the recording. Every
/// derived call must have a ledger answer - a transport-level failure leaves
/// no entry, which this coverage check turns into an error.
fn project_corpus(
    scenario: &Scenario,
    cassette: &Cassette,
    derived: &[DerivedCall],
) -> Result<CorpusJson, String> {
    let derived_keys: Vec<String> = derived
        .iter()
        .map(|c| {
            format!(
                "{}:0x{}",
                hex_lower_prefixed(c.to.as_slice()),
                hex_lower(c.calldata.as_slice())
            )
        })
        .collect();

    let mut calls: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut code: BTreeMap<String, String> = BTreeMap::new();
    let mut timestamp: Option<u64> = None;
    let mut seen_block: Option<u64> = None;

    for (ledger_key, entry) in &cassette.entries {
        let parsed: Value = serde_json::from_str(ledger_key)
            .map_err(|e| format!("ledger key {ledger_key}: {e}"))?;
        let arr = parsed
            .as_array()
            .ok_or_else(|| format!("ledger key not [method, params]: {ledger_key}"))?;
        let method = arr[0].as_str().ok_or("ledger key method")?;
        let params = &arr[1];
        match method {
            "eth_call" => {
                let tx = &params[0];
                let to = tx["to"].as_str().ok_or("eth_call `to`")?;
                let data = tx["input"]
                    .as_str()
                    .or_else(|| tx["data"].as_str())
                    .unwrap_or("0x");
                let key = format!(
                    "0x{}:0x{}",
                    strip_0x(to).to_ascii_lowercase(),
                    strip_0x(data).to_ascii_lowercase()
                );
                let value = match &entry.response {
                    CassetteResponse::Success { result } => {
                        let hex = result
                            .as_str()
                            .ok_or_else(|| format!("eth_call result not a hex string for {key}"))?;
                        Some(strip_0x(hex).to_ascii_lowercase())
                    }
                    CassetteResponse::Failure { error } => {
                        let msg = error["message"].as_str().unwrap_or("");
                        if msg.contains("revert") {
                            None
                        } else {
                            return Err(format!(
                                "non-revert node failure for {key}: {msg:?} - the endpoint cannot serve the pinned state"
                            ));
                        }
                    }
                };
                calls.insert(key, value);
            }
            "eth_getCode" => {
                let addr = params[0].as_str().ok_or("eth_getCode address")?;
                let result = match &entry.response {
                    CassetteResponse::Success { result } => result
                        .as_str()
                        .ok_or_else(|| format!("eth_getCode result not a hex string for {addr}"))?,
                    CassetteResponse::Failure { error } => {
                        let msg = error["message"].as_str().unwrap_or("");
                        return Err(format!("eth_getCode failure for {addr}: {msg:?}"));
                    }
                };
                code.insert(
                    format!("0x{}", strip_0x(addr).to_ascii_lowercase()),
                    strip_0x(result).to_ascii_lowercase(),
                );
            }
            "eth_getBlockByNumber" => {
                let tag = params[0].as_str().unwrap_or("latest");
                let block = tag
                    .parse::<u64>()
                    .ok()
                    .or_else(|| u64::from_str_radix(strip_0x(tag), 16).ok());
                seen_block = block;
                let result = match &entry.response {
                    CassetteResponse::Success { result } => result,
                    CassetteResponse::Failure { error } => {
                        let msg = error["message"].as_str().unwrap_or("");
                        return Err(format!("block fetch failure: {msg:?}"));
                    }
                };
                let ts = result["timestamp"].as_str().ok_or("block timestamp")?;
                // The recording ledger canonicalizes hex quantities to
                // decimal, so a timestamp arrives as either form.
                let parsed = ts
                    .parse::<u64>()
                    .ok()
                    .or_else(|| u64::from_str_radix(strip_0x(ts), 16).ok());
                timestamp = Some(parsed.ok_or_else(|| format!("block timestamp {ts:?}"))?);
            }
            other => return Err(format!("unexpected RPC in the recording ledger: {other}")),
        }
    }

    for key in &derived_keys {
        if !calls.contains_key(key) {
            return Err(format!(
                "no ledger answer for derived call {key} - the recording is incomplete"
            ));
        }
    }
    let block_seen = seen_block.ok_or("no block header recorded")?;
    if block_seen != scenario.block {
        return Err(format!(
            "node served block header {block_seen}; scenario pins {}",
            scenario.block
        ));
    }

    Ok(CorpusJson {
        format: CHAIN_DATA_FORMAT_V1,
        chain_id: scenario.chain_id,
        block_number: scenario.block,
        timestamp: timestamp.ok_or("no block timestamp recorded")?,
        calls,
        code,
    })
}

fn corpus_path(repo_root: &Path, scenario: &Scenario) -> PathBuf {
    repo_root
        .join("tests/fixtures/chain_data")
        .join(scenario.chain_id.to_string())
        .join(format!(
            "py_oracle_{}_block{}.json",
            scenario.name, scenario.block
        ))
}

fn now_secs() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system clock: {e}"))
        .map(|d| d.as_secs())
}

/// Record one scenario twice (byte-identity between passes is the
/// determinism gate), then write it or check it against the corpus on disk.
async fn record_scenario(
    scenario: &Scenario,
    node: &str,
    repo_root: &Path,
    check: bool,
    pace_ms: u64,
) -> Result<(), String> {
    // Side channel: the endpoint must serve the scenario's chain before
    // anything is recorded (and the probe stays out of the ledger).
    let side = AlloyProvider::new(node, 3)
        .await
        .map_err(|e| format!("connect {node}: {e}"))?;
    let served = side
        .get_chain_id()
        .await
        .map_err(|e| format!("chain id probe {node}: {e}"))?;
    if served != scenario.chain_id {
        return Err(format!(
            "node {node} serves chain {served}; scenario pins chain {}",
            scenario.chain_id
        ));
    }

    let golden_path = repo_root.join(&scenario.golden);
    let calls = derive_calls(scenario, &golden_path, repo_root)?;

    let mut pass_bytes: Option<Vec<u8>> = None;
    let mut report = (0usize, 0usize, 0u64);
    for _pass in 0..2 {
        let recorder = RecordingTransport::connect(node)
            .await
            .map_err(|e| format!("recording connect {node}: {e}"))?;
        let provider = recorder.as_alloy_provider();

        // Block header first: the corpus timestamp and proof the endpoint
        // serves the pin at all.
        let header = provider
            .get_block(scenario.block)
            .await
            .map_err(|e| format!("block {}: {e}", scenario.block))?;
        if header.is_none() {
            return Err(format!(
                "node {node} does not serve block {} - the pin is unreachable",
                scenario.block
            ));
        }

        for call in &calls {
            if pace_ms > 0 {
                // Rate-limited endpoints: stay under the per-call budget. The
                // pause sits between awaits, so no in-flight future is held.
                std::thread::sleep(std::time::Duration::from_millis(pace_ms));
            }
            // A revert is oracle truth (the transport records the error; the
            // projection stores null). Any other failure is caught by the
            // projection's coverage check, so the provider-level result is
            // intentionally dropped.
            let _ = provider
                .eth_call(
                    &call.to,
                    Bytes::from(call.calldata.clone()),
                    Some(scenario.block),
                )
                .await;
        }
        for target in &scenario.code_targets {
            let addr: Address = target
                .parse()
                .map_err(|e| format!("code target {target:?}: {e}"))?;
            provider
                .get_code(&addr, Some(scenario.block))
                .await
                .map_err(|e| format!("get_code {target}: {e}"))?;
        }

        let recorded_at = rfc3339_utc(now_secs()?);
        let cassette = recorder.cassette(
            scenario.chain_id,
            CassetteProvenance {
                source: node.to_string(),
                recorded_at,
                span: CassetteSpan {
                    from_block: scenario.block,
                    to_block: scenario.block,
                },
            },
        );
        let corpus = project_corpus(scenario, &cassette, &calls)?;
        report = (corpus.calls.len(), corpus.code.len(), corpus.timestamp);
        let mut bytes =
            serde_json::to_vec_pretty(&corpus).map_err(|e| format!("corpus serialization: {e}"))?;
        bytes.push(b'\n');

        if let Some(prev) = &pass_bytes {
            if prev != &bytes {
                return Err(format!(
                    "two recording passes diverged for {} - the corpus is not a pure function of the pinned state",
                    scenario.name
                ));
            }
        } else {
            pass_bytes = Some(bytes);
        }
    }

    let bytes = pass_bytes.expect("two passes ran");
    let (call_count, code_count, timestamp) = report;
    let out_path = corpus_path(repo_root, scenario);
    if check {
        let disk = std::fs::read(&out_path).map_err(|e| {
            format!(
                "corpus {} missing or unreadable: {e} - record it first",
                out_path.display()
            )
        })?;
        if disk != bytes {
            return Err(format!(
                "corpus drift: {} differs from a fresh recording ({} bytes on disk vs {} recorded) - regenerate",
                out_path.display(),
                disk.len(),
                bytes.len()
            ));
        }
        println!(
            "[ok] {} ({} calls, {} code entries, timestamp {timestamp}, byte-identical)",
            out_path.display(),
            call_count,
            code_count
        );
    } else {
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&out_path, &bytes)
            .map_err(|e| format!("write {}: {e}", out_path.display()))?;
        println!(
            "[ok] wrote {} ({} calls, {} code entries, timestamp {timestamp})",
            out_path.display(),
            call_count,
            code_count
        );
    }
    Ok(())
}

struct Args {
    check: bool,
    scenarios: Vec<String>,
    root: Option<PathBuf>,
    ethereum_node: Option<String>,
    base_node: Option<String>,
    arbitrum_node: Option<String>,
    pace_ms: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        check: false,
        scenarios: Vec::new(),
        root: None,
        ethereum_node: None,
        base_node: None,
        arbitrum_node: None,
        pace_ms: 0,
    };
    let mut flags = std::env::args().skip(1);
    while let Some(flag) = flags.next() {
        match flag.as_str() {
            "--check" => args.check = true,
            "--scenario" => {
                let name = flags.next().ok_or("--scenario needs a name")?;
                args.scenarios.push(name);
            }
            "--root" => args.root = Some(PathBuf::from(flags.next().ok_or("--root needs a path")?)),
            "--ethereum-node" => {
                args.ethereum_node = Some(flags.next().ok_or("--ethereum-node needs a URL")?);
            }
            "--base-node" => {
                args.base_node = Some(flags.next().ok_or("--base-node needs a URL")?);
            }
            "--arbitrum-node" => {
                args.arbitrum_node = Some(flags.next().ok_or("--arbitrum-node needs a URL")?);
            }
            "--pace-ms" => {
                args.pace_ms = flags
                    .next()
                    .ok_or("--pace-ms needs a duration")?
                    .parse::<u64>()
                    .map_err(|e| format!("--pace-ms: {e}"))?;
            }
            other => return Err(format!("unknown flag {other:?}")),
        }
    }
    Ok(args)
}

fn node_for(tier: NodeTier, args: &Args) -> Result<String, String> {
    match tier {
        NodeTier::Ethereum => args
            .ethereum_node
            .clone()
            .or_else(|| std::env::var("ETHEREUM_ARCHIVE_NODE_HTTP_URI").ok())
            .ok_or_else(|| {
                "no ethereum node: set ETHEREUM_ARCHIVE_NODE_HTTP_URI (the conftest name) or --ethereum-node"
                    .to_string()
            }),
        NodeTier::Base => Ok(args
            .base_node
            .clone()
            .or_else(|| std::env::var("BASE_ARCHIVE_NODE_HTTP_URI").ok())
            .unwrap_or_else(|| "https://mainnet.base.org".to_string())),
        NodeTier::Arbitrum => Ok(args
            .arbitrum_node
            .clone()
            .unwrap_or_else(|| "https://arb1.arbitrum.io/rpc".to_string())),
    }
}

async fn run(args: Args) -> Result<(), String> {
    let repo_root = match &args.root {
        Some(root) => root.clone(),
        None => Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .expect("example lives four levels below the repo root")
            .to_path_buf(),
    };
    let wired = all_scenarios();
    let selected: Vec<Scenario> = if args.scenarios.is_empty() {
        wired.clone()
    } else {
        wired
            .iter()
            .filter(|s| args.scenarios.iter().any(|name| name == s.name))
            .cloned()
            .collect()
    };
    if !args.scenarios.is_empty() {
        let wired_names: std::collections::BTreeSet<&str> = wired.iter().map(|s| s.name).collect();
        if let Some(bad) = args
            .scenarios
            .iter()
            .find(|name| !wired_names.contains(name.as_str()))
        {
            return Err(format!(
                "unknown --scenario name {bad:?}; wired: {}",
                wired_names.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
    }

    let mut failures = 0usize;
    for scenario in &selected {
        let node = node_for(scenario.node, &args)?;
        match record_scenario(scenario, &node, &repo_root, args.check, args.pace_ms).await {
            Ok(()) => {}
            Err(msg) => {
                eprintln!("[fail] {}: {msg}", scenario.name);
                failures += 1;
            }
        }
    }
    if failures > 0 {
        return Err(format!(
            "{failures} of {} scenario(s) failed",
            selected.len()
        ));
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    match parse_args() {
        Ok(args) => match run(args).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("error: {msg}");
                ExitCode::FAILURE
            }
        },
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::from(2)
        }
    }
}
