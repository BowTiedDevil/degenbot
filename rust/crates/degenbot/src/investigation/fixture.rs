//! Shared path-investigation **fixture schema** — one superset of the per-path
//! JSON capture format so a future investigation can load a captured
//! `path<N>_…_block<B>.json` with zero copy-paste.
//!
//! The fixture files produced by `scripts/capture_*_fixture.py` share a common
//! core (pool list, hop list, tick map, recorded solve) with small per-family
//! variation (a V4 pool adds `currency0/1`+`pool_id`; a V2 pool adds
//! `reserve0/1`; amounts were authored as JSON numbers in some files and decimal
//! strings in others). Every field is therefore optional / length-flexible here;
//! `Amount` accepts a JSON number or a decimal string so all historical files
//! load unchanged.

use hashbrown::HashMap;

use alloy::primitives::{Address, U256};
use serde::{Deserialize, Deserializer};

/// A decimal amount that may appear as a JSON number or a decimal string
/// (different fixture authors used each). Parsed into `U256`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Amount(pub U256);

impl<'de> Deserialize<'de> for Amount {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            // serde_json (no arbitrary_precision) can't produce a `u128` from a
            // JSON number, only `u64` — so accept plain numbers as `u64` (fixture
            // plain-int amounts are wei-scale, < u64::MAX) and large values via
            // their string form.
            Num(u64),
            Str(String),
        }
        let u = match Raw::deserialize(d)? {
            Raw::Num(n) => U256::from(n),
            Raw::Str(s) => s.parse::<U256>().map_err(serde::de::Error::custom)?,
        };
        Ok(Amount(u))
    }
}

impl std::fmt::Display for Amount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A single tick's gross/net liquidity, as captured by the fixture writers.
#[derive(Clone, Debug, Deserialize)]
pub struct TickJson {
    pub liquidity_net: String,
    pub liquidity_gross: String,
}

/// One pool in the captured path. All fields optional: a fixture only fills
/// what its family needs. Addresses are hex (alloy parses on deserialize).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PoolData {
    #[serde(default)]
    pub address: Option<Address>,
    #[serde(default)]
    pub token0: Option<Address>,
    #[serde(default)]
    pub token1: Option<Address>,
    #[serde(default)]
    pub pool_manager: Option<Address>,
    #[serde(default)]
    pub pool_id: Option<String>,
    #[serde(default)]
    pub currency0: Option<Address>,
    #[serde(default)]
    pub currency1: Option<Address>,
    #[serde(default)]
    pub tick_spacing: Option<i32>,
    #[serde(default)]
    pub fee_token0: Option<u32>,
    #[serde(default)]
    pub fee_currency0: Option<u32>,
    #[serde(default)]
    pub tick: Option<i32>,
    #[serde(default)]
    pub sqrt_price_x96: Option<Amount>,
    #[serde(default)]
    pub liquidity: Option<Amount>,
    #[serde(default)]
    pub protocol_fee: Option<u32>,
    #[serde(default)]
    pub liquidity_update_block: Option<u64>,
    #[serde(default)]
    pub reserve0: Option<Amount>,
    #[serde(default)]
    pub reserve1: Option<Amount>,
    #[serde(default)]
    pub fee_gamma: Option<u64>,
    #[serde(default)]
    pub fee_denom: Option<u64>,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub tick_data: HashMap<String, TickJson>,
}

/// A single hop in the captured path: `pool` names a key in `PathFixture::pools`.
#[derive(Clone, Debug, Deserialize)]
pub struct PathHop {
    pub hop: usize,
    pub pool: String,
    pub zero_for_one: bool,
}

/// The captured solve/recorded probe for the failing path. All fields optional —
/// each `capture_*` script records a slightly different slice (a V4 middle-hop
/// records `v4_*`; a V2 middle-hop records `v2_*`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RecordedSolve {
    #[serde(default)]
    pub optimal_input: Option<Amount>,
    #[serde(default)]
    pub hop_outputs: Vec<Amount>,
    #[serde(default)]
    pub sim_bucket: Option<String>,
    #[serde(default)]
    pub v4_hop_index: Option<usize>,
    #[serde(default)]
    pub v4_zero_for_one: Option<bool>,
    #[serde(default)]
    pub v4_input: Option<Amount>,
    #[serde(default)]
    pub v4_predicted_output: Option<Amount>,
    /// Free-text on-chain outcome (e.g. `EMPTY-HALT`), not always an amount.
    #[serde(default)]
    pub v4_onchain: Option<String>,
    #[serde(default)]
    pub v2_hop_index: Option<usize>,
    #[serde(default)]
    pub v2_input: Option<Amount>,
    #[serde(default)]
    pub v2_predicted: Option<Amount>,
    #[serde(default)]
    pub v2_actual: Option<Amount>,
}

/// A loaded path-investigation fixture: the pool map, the hop list, and the
/// recorded solve that the investigation is reproducing.
#[derive(Clone, Debug, Deserialize)]
pub struct PathFixture {
    #[serde(default)]
    pub target_block: Option<u64>,
    #[serde(default)]
    pub pools: HashMap<String, PoolData>,
    #[serde(default)]
    pub path: Vec<PathHop>,
    #[serde(default)]
    pub recorded_solve: RecordedSolve,
}

impl PathFixture {
    /// Load + parse a captured-path fixture JSON (see `capture_*_fixture.py`).
    pub fn load(path: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("parse {path}: {e}"))
    }
}
