//! Record the offline oracle-answer corpus for the fork-bound Uniswap parity
//! tests into the per-block `OfflineProvider` JSON home
//! (`tests/fixtures/chain_data/<chain_id>/py_oracle_<scenario>_block<N>.json`).
//!
//! Each scenario derives its call surface from the parity tests' committed
//! golden file - the canonical record of which on-chain calls the test's
//! replay asserts against - then drives the real `eth_call`s at the pinned
//! block through [`RecordingTransport`] and projects the recording ledger onto
//! the existing `OfflineProvider` wire shape (`chain_id`, `block_number`,
//! `timestamp`, `calls`, `code`). One capture format per corpus: no third
//! serialization, and nothing lands in the wire-cassette home.
//!
//! The recorded answers ARE the oracle: every value comes off the wire, never
//! re-derived. A revert recorded at the pin is stored as `null` (the offline
//! transport replays it as an execution revert); a non-revert node error is a
//! fixture gap and fails the recording instead of being stored.
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
//! `ETHEREUM_ARCHIVE_NODE_HTTP_URI` (the `tests/conftest.py` name);
//! `--ethereum-node` / `--arbitrum-node` override, and the camelot scenario
//! defaults to the same keyless public endpoint the camelot test forks. The
//! camelot scenario additionally needs an endpoint that serves contract state
//! at its pinned block - when none does, the scenario fails with the node's
//! own error and writes nothing (an absent corpus is not substituted).

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

/// Ethereum parity pin: the block both Uniswap quoter scenarios serve.
const UNISWAP_PARITY_BLOCK: u64 = 24_407_242;
/// Arbitrum parity pin: the camelot scenario's block.
const CAMELOT_PARITY_BLOCK: u64 = 477_785_000;

/// The v3 parity test's oracle contract: Uniswap V3 `QuoterV2`.
const UNISWAP_V3_QUOTER: &str = "0xb27308f9F90D607463bb33eA1BeBb41C27CE5AB6";
/// The v4 parity test's oracle contract: Uniswap V4 `Quoter`.
const UNISWAP_V4_QUOTER: &str = "0x52F0E24D1c21C8A0cB1e5a5dD6198556BD9E1203";

/// Pool pins the calldata encoders need (the pools themselves are never
/// called - the golden key embeds them, the quoter call encodes them).
const UNISWAP_V3_FEE: u64 = 3_000;
const UNISWAP_V4_FEE: u64 = 500;
const UNISWAP_V4_TICK_SPACING: u64 = 10;

/// V3 swap boundary prices: the parity test swaps token0->token1 with MIN+1
/// and token1->token0 with MAX-1 as the price limit.
const MIN_SQRT_RATIO: U256 = U256::from_limbs([0x1_0002_76a3, 0, 0, 0]);
const MAX_SQRT_RATIO: U256 =
    U256::from_limbs([0x5d95_1d52_6398_8d26, 0xefd1_fc6a_5064_8849, 0xfffd_8963, 0]);

/// Which endpoint tier serves a scenario, resolved like the parity tests do.
enum NodeTier {
    /// `ETHEREUM_ARCHIVE_NODE_HTTP_URI` - the v3/v4 parity fork URL.
    Ethereum,
    /// The camelot test's keyless public endpoint.
    Arbitrum,
}

/// One parity scenario: its golden-file surface, its pin, and its node tier.
struct Scenario {
    /// Corpus file stem: `py_oracle_<name>_block<N>.json`.
    name: &'static str,
    chain_id: u64,
    block: u64,
    /// Committed golden file the call surface derives from, relative to the
    /// repo root.
    golden: &'static str,
    /// Contracts whose runtime code joins the corpus `code` map.
    code_targets: &'static [&'static str],
    node: NodeTier,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "uniswap_v3_quoter",
        chain_id: 1,
        block: UNISWAP_PARITY_BLOCK,
        golden: "tests/golden/data/tests/uniswap/v3/test_uniswap_v3_onchain_parity/test_cached_calculations_v3_wbtc_weth.json",
        code_targets: &[UNISWAP_V3_QUOTER],
        node: NodeTier::Ethereum,
    },
    Scenario {
        name: "uniswap_v4_quoter",
        chain_id: 1,
        block: UNISWAP_PARITY_BLOCK,
        golden: "tests/golden/data/tests/uniswap/v4/test_uniswap_v4_onchain_parity/test_cached_calculations_v4_eth_usdc.json",
        code_targets: &[UNISWAP_V4_QUOTER],
        node: NodeTier::Ethereum,
    },
    Scenario {
        name: "camelot_v2_get_amount_out",
        chain_id: 42161,
        block: CAMELOT_PARITY_BLOCK,
        golden: "tests/golden/data/tests/uniswap/v2/test_camelot_v2_onchain_parity/test_create_camelot_v2_pool.json",
        code_targets: &["0x84652bb2539513BAf36e225c930Fdd8eaa63CE27"],
        node: NodeTier::Arbitrum,
    },
];

/// The per-block `OfflineProvider` wire shape (single-block format), field
/// order matching the recorded fixtures the offline transport consumes.
#[derive(Serialize)]
struct CorpusJson {
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

/// Scenario token symbols -> wire addresses (the pairs the golden keys name).
fn symbol_address(scenario_name: &str, symbol: &str) -> Result<Address, String> {
    let raw = match (scenario_name, symbol) {
        ("uniswap_v3_quoter", "WBTC") => "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
        ("uniswap_v3_quoter", "WETH") => "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        ("uniswap_v4_quoter", "ETH") => "0x0000000000000000000000000000000000000000",
        ("uniswap_v4_quoter", "USDC") => "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        ("camelot_v2_get_amount_out", "USDC") => "0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8",
        ("camelot_v2_get_amount_out", "WETH") => "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
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

/// Derive one call from a golden key `<to>|<method>|<SYM-A>-><SYM-B>|<amount>`.
fn derive_call(scenario: &Scenario, key: &str) -> Result<DerivedCall, String> {
    let parts: Vec<&str> = key.split('|').collect();
    if parts.len() != 4 {
        return Err(format!(
            "golden key not <to>|<method>|<pair>|<amount>: {key}"
        ));
    }
    let (pool, method, pair, amount) = (parts[0], parts[1], parts[2], parts[3]);
    let (from_sym, to_sym) = pair
        .split_once("->")
        .ok_or_else(|| format!("golden key pair not <SYM-A>-><SYM-B>: {key}"))?;
    let amount = U256::from_str_radix(amount, 10)
        .map_err(|e| format!("golden key amount {amount:?}: {e}"))?;

    let (to, calldata) = match (scenario.name, method) {
        ("uniswap_v3_quoter", "quoteExactInputSingle" | "quoteExactOutputSingle") => {
            let token_in = symbol_address(scenario.name, from_sym)?;
            let token_out = symbol_address(scenario.name, to_sym)?;
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
        ("uniswap_v4_quoter", "quoteExactInputSingle" | "quoteExactOutputSingle") => {
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
        ("camelot_v2_get_amount_out", "getAmountOut") => {
            let pool: Address = pool
                .parse()
                .map_err(|e| format!("golden key pool {pool:?}: {e}"))?;
            let token_in = symbol_address(scenario.name, from_sym)?;
            (
                pool,
                encode_words(
                    selector("getAmountOut(uint256,address)"),
                    &[word_uint(amount), word_address(token_in)],
                ),
            )
        }
        _ => {
            return Err(format!(
                "unwired golden surface: {}|{method}",
                scenario.name
            ))
        }
    };
    Ok(DerivedCall { to, calldata })
}

/// Parse the scenario's golden file and derive its full call surface.
fn derive_calls(scenario: &Scenario, golden_path: &Path) -> Result<Vec<DerivedCall>, String> {
    let raw = std::fs::read_to_string(golden_path)
        .map_err(|e| format!("read golden {}: {e}", golden_path.display()))?;
    let golden: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("parse golden {}: {e}", golden_path.display()))?;
    let recorded_chain = golden["chain_id"].as_u64().ok_or("golden chain_id")?;
    let recorded_block = golden["block_number"]
        .as_u64()
        .ok_or("golden block_number")?;
    if recorded_chain != scenario.chain_id || recorded_block != scenario.block {
        return Err(format!(
            "golden {} pins chain {recorded_chain} block {recorded_block}; scenario pins chain {} block {}",
            golden_path.display(),
            scenario.chain_id,
            scenario.block
        ));
    }
    let entries = golden["entries"].as_object().ok_or("golden entries")?;
    let mut calls = Vec::with_capacity(entries.len());
    for key in entries.keys() {
        calls.push(derive_call(scenario, key)?);
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
        let parsed: serde_json::Value = serde_json::from_str(ledger_key)
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

    let golden_path = repo_root.join(scenario.golden);
    let calls = derive_calls(scenario, &golden_path)?;

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
        for target in scenario.code_targets {
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
    arbitrum_node: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        check: false,
        scenarios: Vec::new(),
        root: None,
        ethereum_node: None,
        arbitrum_node: None,
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
            "--arbitrum-node" => {
                args.arbitrum_node = Some(flags.next().ok_or("--arbitrum-node needs a URL")?);
            }
            other => return Err(format!("unknown flag {other:?}")),
        }
    }
    Ok(args)
}

fn node_for(tier: &NodeTier, args: &Args) -> Result<String, String> {
    match tier {
        NodeTier::Ethereum => args
            .ethereum_node
            .clone()
            .or_else(|| std::env::var("ETHEREUM_ARCHIVE_NODE_HTTP_URI").ok())
            .ok_or_else(|| {
                "no ethereum node: set ETHEREUM_ARCHIVE_NODE_HTTP_URI (the conftest name) or --ethereum-node"
                    .to_string()
            }),
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
    let selected: Vec<&Scenario> = if args.scenarios.is_empty() {
        SCENARIOS.iter().collect()
    } else {
        SCENARIOS
            .iter()
            .filter(|s| args.scenarios.iter().any(|name| name == s.name))
            .collect()
    };
    if selected.len() != args.scenarios.len() {
        return Err(format!(
            "unknown --scenario name(s); wired: {}",
            SCENARIOS
                .iter()
                .map(|s| s.name)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let mut failures = 0usize;
    for scenario in &selected {
        let node = node_for(&scenario.node, &args)?;
        match record_scenario(scenario, &node, &repo_root, args.check).await {
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
