//! Fixture recapture: refresh a path-investigation fixture's recorded *state*
//! from fresh DB + chain reads without disturbing its authored identity or
//! narrative.
//!
//! The capture format is owned here next to [`super::fixture`]. A fixture file
//! is a document with three kinds of content:
//!
//! - **Narrative** (`_doc`, `target_block`, `recorded_solve`, `path`): why the
//!   path failed, written once by a human.
//! - **Identity** (`family`, addresses, `pool_id`, currencies, fees,
//!   `tick_spacing`, and per-recorder extras like `managed_pool_id`): which
//!   pools the investigation is about. Recorded verbatim at authoring time.
//! - **State** (DB tick snapshot + chain scalars): perishable, reproducible —
//!   the only content recapture re-reads and rewrites.
//!
//! Field conventions are pinned to the committed
//! `tests/fixtures/path*_block*.json` files so a recapture keeps them
//! meaningful: amounts are decimal strings (`liquidity_net` signed),
//! ticks/fees/block numbers are JSON numbers, and `block_number` on a V2 pair
//! is the `getReserves` timestamp as recorded by the original recorder.

use std::collections::BTreeMap;

use alloy::primitives::U256;
use serde_json::{Map, Value};

/// Why a recapture could not proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// The fixture root is not a JSON object.
    NotAnObject,
    /// `pools` is missing or not a JSON object.
    MissingPools,
    /// A fetched pool key does not name a pool in the fixture's `pools` map.
    UnknownPool(String),
    /// A pool entry is not a JSON object.
    PoolNotAnObject(String),
    /// A required identity field (family / address / pool_id) is absent.
    MissingField(&'static str),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnObject => write!(f, "fixture root is not a JSON object"),
            Self::MissingPools => write!(f, "fixture has no `pools` object"),
            Self::UnknownPool(k) => {
                write!(f, "fetched pool `{k}` is not in the fixture's `pools` map")
            }
            Self::PoolNotAnObject(k) => write!(f, "fixture pool `{k}` is not a JSON object"),
            Self::MissingField(fld) => write!(f, "fixture pool is missing identity field `{fld}`"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// What a pool entry must carry for the recapture driver to find its sources:
/// the family discriminator plus the on-chain identity (address for V2/V3,
/// pool hash for V4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSpec {
    pub family: String,
    pub address: Option<alloy::primitives::Address>,
    pub pool_id: Option<alloy::primitives::B256>,
}

/// Fresh *state* for one pool, as re-read from the database and an archive
/// node. Identity is never carried here: the fixture records it verbatim.
#[derive(Debug, Clone)]
pub enum FetchedState {
    /// V2: reserves read at the fixture's `target_block`. `block_number` is the
    /// pair's `getReserves` timestamp — the key name the corpus records it
    /// under, misleading but load-bearing.
    V2 {
        reserve0: U256,
        reserve1: U256,
        block_number: u32,
    },
    V3 {
        /// The snapshot marker: V3 recordings use the capture block, matching
        /// the corpus convention.
        liquidity_update_block: u64,
        tick_data: BTreeMap<i32, (i128, u128)>,
        sqrt_price_x96: U256,
        tick: i32,
        liquidity: U256,
    },
    V4 {
        /// The database row's `liquidity_update_block` marker (V4 recordings
        /// carry the tracker's marker, not the capture block).
        liquidity_update_block: u64,
        tick_data: BTreeMap<i32, (i128, u128)>,
        sqrt_price_x96: U256,
        tick: i32,
        liquidity: U256,
        protocol_fee: u32,
        lp_fee: u32,
    },
}

fn tick_data_json(ticks: &BTreeMap<i32, (i128, u128)>) -> Value {
    let mut m = Map::new();
    for (tick, (net, gross)) in ticks {
        let mut e = Map::new();
        e.insert("liquidity_net".into(), Value::String(net.to_string()));
        e.insert("liquidity_gross".into(), Value::String(gross.to_string()));
        m.insert(tick.to_string(), Value::Object(e));
    }
    Value::Object(m)
}

impl FetchedState {
    /// The refreshable state keys of this pool family, with their fresh values.
    pub fn state_json(&self) -> Map<String, Value> {
        let mut m = Map::new();
        match self {
            Self::V2 {
                reserve0,
                reserve1,
                block_number,
            } => {
                m.insert("reserve0".into(), Value::String(reserve0.to_string()));
                m.insert("reserve1".into(), Value::String(reserve1.to_string()));
                m.insert("block_number".into(), Value::from(*block_number));
            }
            Self::V3 {
                liquidity_update_block,
                tick_data,
                sqrt_price_x96,
                tick,
                liquidity,
            } => {
                m.insert(
                    "liquidity_update_block".into(),
                    Value::from(*liquidity_update_block),
                );
                m.insert("tick_data".into(), tick_data_json(tick_data));
                m.insert(
                    "sqrt_price_x96".into(),
                    Value::String(sqrt_price_x96.to_string()),
                );
                m.insert("tick".into(), Value::from(*tick));
                m.insert("liquidity".into(), Value::String(liquidity.to_string()));
            }
            Self::V4 {
                liquidity_update_block,
                tick_data,
                sqrt_price_x96,
                tick,
                liquidity,
                protocol_fee,
                lp_fee,
            } => {
                m.insert(
                    "liquidity_update_block".into(),
                    Value::from(*liquidity_update_block),
                );
                m.insert("tick_data".into(), tick_data_json(tick_data));
                m.insert(
                    "sqrt_price_x96".into(),
                    Value::String(sqrt_price_x96.to_string()),
                );
                m.insert("tick".into(), Value::from(*tick));
                m.insert("liquidity".into(), Value::String(liquidity.to_string()));
                m.insert("protocol_fee".into(), Value::from(*protocol_fee));
                m.insert("lp_fee".into(), Value::from(*lp_fee));
            }
        }
        m
    }
}

/// The state keys one pool family refreshes in a fixture entry. Must stay in
/// lockstep with [`FetchedState::state_json`] (pinned by a test).
pub fn state_keys_for_family(family: &str) -> &'static [&'static str] {
    if family.ends_with("_v4") {
        &[
            "liquidity_update_block",
            "tick_data",
            "sqrt_price_x96",
            "tick",
            "liquidity",
            "protocol_fee",
            "lp_fee",
        ]
    } else if family.ends_with("_v3") {
        &[
            "liquidity_update_block",
            "tick_data",
            "sqrt_price_x96",
            "tick",
            "liquidity",
        ]
    } else {
        &["reserve0", "reserve1", "block_number"]
    }
}

/// Extract the recapture identity from one fixture pool entry.
pub fn pool_spec(entry: &Value) -> Result<PoolSpec, CaptureError> {
    let family = entry
        .get("family")
        .and_then(Value::as_str)
        .ok_or(CaptureError::MissingField("family"))?
        .to_string();
    let address = match entry.get("address").and_then(Value::as_str) {
        Some(s) => Some(
            s.parse()
                .map_err(|_| CaptureError::MissingField("address"))?,
        ),
        None => None,
    };
    let pool_id = match entry.get("pool_id").and_then(Value::as_str) {
        Some(s) => Some(
            s.parse()
                .map_err(|_| CaptureError::MissingField("pool_id"))?,
        ),
        None => None,
    };
    Ok(PoolSpec {
        family,
        address,
        pool_id,
    })
}

/// Refresh a fixture document: upsert each fetched pool's state keys onto its
/// existing entry and leave everything else — narrative fields, recorded
/// identity, and per-recorder extras — untouched. A recapture of unchanged
/// sources is therefore an exact identity on the document.
pub fn refresh_fixture(
    original: &Value,
    fetched: &BTreeMap<String, FetchedState>,
) -> Result<Value, CaptureError> {
    let root = original.as_object().ok_or(CaptureError::NotAnObject)?;
    let mut out = root.clone();
    let pools = out
        .get_mut("pools")
        .and_then(Value::as_object_mut)
        .ok_or(CaptureError::MissingPools)?;
    for (key, state) in fetched {
        let entry = pools
            .get_mut(key)
            .ok_or_else(|| CaptureError::UnknownPool(key.clone()))?;
        let obj = entry
            .as_object_mut()
            .ok_or_else(|| CaptureError::PoolNotAnObject(key.clone()))?;
        for (name, value) in state.state_json() {
            obj.insert(name, value);
        }
    }
    Ok(Value::Object(out))
}

/// The `pool.key` paths whose recorded state drifted from the fresh reads.
/// Only keys the entry already documents are compared: a recapture never
/// invents shape the fixture never recorded.
pub fn diff_state(original: &Value, refreshed: &Value) -> Vec<String> {
    let mut drifted = Vec::new();
    let Some(orig_pools) = original.get("pools").and_then(Value::as_object) else {
        return drifted;
    };
    let new_pools = refreshed.get("pools").and_then(Value::as_object);
    for (key, orig_entry) in orig_pools {
        let Some(new_entry) = new_pools.and_then(|pools| pools.get(key)) else {
            drifted.push(format!("pools.{key}"));
            continue;
        };
        let family = orig_entry
            .get("family")
            .and_then(Value::as_str)
            .unwrap_or_default();
        for name in state_keys_for_family(family) {
            let Some(orig_value) = orig_entry.get(*name) else {
                continue;
            };
            if new_entry.get(*name) != Some(orig_value) {
                drifted.push(format!("pools.{key}.{name}"));
            }
        }
    }
    drifted
}

/// Canonical fixture text: serde_json pretty printing with a trailing newline.
/// Key order is serde_json's canonical (sorted) form — a recapture normalizes
/// whatever order the recorder used.
pub fn emit_json(doc: &Value) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(doc).expect("Value serializes")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::investigation::{build_v3_state, build_v4_state, PathFixture};

    const FIXTURE_73385: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../tests/fixtures/path73385_v4_block25706469.json"
    );
    const FIXTURE_5000: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../tests/fixtures/path5000_v2v4v3_block25704509.json"
    );

    fn load(path: &str) -> Value {
        let text = std::fs::read_to_string(path).expect("fixture file is readable");
        serde_json::from_str(&text).expect("fixture parses")
    }

    fn u256(s: &str) -> U256 {
        s.parse().expect("decimal amount")
    }

    /// Inverse of `FetchedState::state_json` for tests: feed a pool entry's own
    /// recorded state back through the fetch path.
    fn fetched_from_entry(entry: &Value) -> FetchedState {
        let family = entry["family"].as_str().expect("family");
        let ticks = |key: &str| -> BTreeMap<i32, (i128, u128)> {
            entry[key]
                .as_object()
                .expect("tick_data object")
                .iter()
                .map(|(t, v)| {
                    (
                        t.parse().expect("tick"),
                        (
                            v["liquidity_net"]
                                .as_str()
                                .expect("net")
                                .parse()
                                .expect("net i128"),
                            v["liquidity_gross"]
                                .as_str()
                                .expect("gross")
                                .parse()
                                .expect("gross u128"),
                        ),
                    )
                })
                .collect()
        };
        if family.ends_with("_v4") {
            FetchedState::V4 {
                liquidity_update_block: entry["liquidity_update_block"].as_u64().expect("ublk"),
                tick_data: ticks("tick_data"),
                sqrt_price_x96: u256(entry["sqrt_price_x96"].as_str().expect("sqrt")),
                tick: entry["tick"].as_i64().expect("tick") as i32,
                liquidity: u256(entry["liquidity"].as_str().expect("liq")),
                protocol_fee: entry["protocol_fee"].as_u64().expect("pf") as u32,
                lp_fee: entry["lp_fee"].as_u64().expect("lf") as u32,
            }
        } else if family.ends_with("_v3") {
            FetchedState::V3 {
                liquidity_update_block: entry["liquidity_update_block"].as_u64().expect("ublk"),
                tick_data: ticks("tick_data"),
                sqrt_price_x96: u256(entry["sqrt_price_x96"].as_str().expect("sqrt")),
                tick: entry["tick"].as_i64().expect("tick") as i32,
                liquidity: u256(entry["liquidity"].as_str().expect("liq")),
            }
        } else {
            FetchedState::V2 {
                reserve0: u256(entry["reserve0"].as_str().expect("r0")),
                reserve1: u256(entry["reserve1"].as_str().expect("r1")),
                block_number: entry["block_number"].as_u64().expect("blk") as u32,
            }
        }
    }

    fn fetch_all(doc: &Value) -> BTreeMap<String, FetchedState> {
        doc["pools"]
            .as_object()
            .expect("pools object")
            .iter()
            .map(|(k, e)| (k.clone(), fetched_from_entry(e)))
            .collect()
    }

    /// The `a.b.c` paths whose values differ between two fixture docs —
    /// readable failures instead of megabyte assertion dumps.
    fn changed_paths(a: &Value, b: &Value, prefix: &str, out: &mut Vec<String>) {
        match (a.as_object(), b.as_object()) {
            (Some(ao), Some(bo)) => {
                for (k, av) in ao {
                    let path = format!("{prefix}.{k}");
                    match bo.get(k) {
                        Some(bv) => changed_paths(av, bv, &path, out),
                        None => out.push(format!("{path} (dropped)")),
                    }
                }
                for k in bo.keys() {
                    if !ao.contains_key(k) {
                        out.push(format!("{prefix}.{k} (added)"));
                    }
                }
            }
            _ => {
                if a != b {
                    out.push(prefix.to_string());
                }
            }
        }
    }

    #[test]
    fn refresh_path73385_is_idempotent_exactly() {
        let original = load(FIXTURE_73385);
        let refreshed = refresh_fixture(&original, &fetch_all(&original)).expect("refresh");
        let mut changed = Vec::new();
        changed_paths(&original, &refreshed, "", &mut changed);
        assert!(
            changed.is_empty(),
            "unchanged sources must recapture to the identical document, changed: {changed:?}"
        );
        assert!(diff_state(&original, &refreshed).is_empty());
    }

    #[test]
    fn refresh_path5000_is_idempotent_and_keeps_extras() {
        let original = load(FIXTURE_5000);
        let refreshed = refresh_fixture(&original, &fetch_all(&original)).expect("refresh");
        let mut changed = Vec::new();
        changed_paths(&original, &refreshed, "", &mut changed);
        assert!(
            changed.is_empty(),
            "unchanged sources must recapture to the identical document, changed: {changed:?}"
        );
        let orig_v4 = &original["pools"]["v4"];
        let new_v4 = &refreshed["pools"]["v4"];
        // Identity extras the 5000 recorder documented stay verbatim.
        for extra in [
            "managed_pool_id",
            "currency0_symbol",
            "currency1_symbol",
            "db_currency0",
            "db_currency1",
            "hooks",
        ] {
            assert_eq!(
                new_v4.get(extra),
                orig_v4.get(extra),
                "extra `{extra}` preserved"
            );
        }
        // Narrative fields untouched.
        for key in ["_doc", "target_block", "recorded_solve", "path"] {
            assert_eq!(
                refreshed.get(key),
                original.get(key),
                "`{key}` is narrative"
            );
        }
    }

    #[test]
    fn refresh_upserts_scalars_and_never_touches_narrative() {
        let original = load(FIXTURE_73385);
        let mut fetched = fetch_all(&original);
        let v4 = fetched.get_mut("v4").expect("v4");
        if let FetchedState::V4 { tick, .. } = v4 {
            *tick += 1;
        }
        let refreshed = refresh_fixture(&original, &fetched).expect("refresh");
        assert_eq!(
            refreshed["pools"]["v4"]["tick"],
            Value::from(original["pools"]["v4"]["tick"].as_i64().expect("tick") + 1)
        );
        assert_eq!(
            refreshed["pools"]["v4"]["tick_data"], original["pools"]["v4"]["tick_data"],
            "unchanged tick snapshots keep their recorded form"
        );
        for key in ["_doc", "target_block", "recorded_solve", "path"] {
            assert_eq!(
                refreshed.get(key),
                original.get(key),
                "`{key}` is narrative"
            );
        }
        assert_eq!(
            diff_state(&original, &refreshed),
            vec!["pools.v4.tick".to_string()]
        );
    }

    #[test]
    fn emitted_fixture_parses_and_reconstructs() {
        let original = load(FIXTURE_73385);
        let refreshed = refresh_fixture(&original, &fetch_all(&original)).expect("refresh");
        let text = emit_json(&refreshed);
        assert!(text.ends_with('\n'));
        let fx: PathFixture = serde_json::from_str(&text).expect("PathFixture parses");
        let v4_count = fx.pools["v4"].tick_data.len();
        assert_eq!(
            v4_count,
            original["pools"]["v4"]["tick_data"]
                .as_object()
                .expect("ticks")
                .len()
        );
        let state = build_v4_state(&fx.pools["v4"]);
        assert_eq!(
            state.tick,
            original["pools"]["v4"]["tick"].as_i64().expect("tick") as i32
        );
        let v3_count = fx.pools["v3_0"].tick_data.len();
        assert_eq!(
            v3_count,
            original["pools"]["v3_0"]["tick_data"]
                .as_object()
                .expect("ticks")
                .len()
        );
        let v3 = build_v3_state(&fx.pools["v3_0"]);
        assert_eq!(
            v3.tick,
            original["pools"]["v3_0"]["tick"].as_i64().expect("tick") as i32
        );
    }

    #[test]
    fn state_keys_match_state_json() {
        for path in [FIXTURE_73385, FIXTURE_5000] {
            let doc = load(path);
            for (key, entry) in doc["pools"].as_object().expect("pools") {
                let fetched = fetched_from_entry(entry);
                let names: Vec<String> = fetched.state_json().keys().cloned().collect();
                let declared: Vec<String> =
                    state_keys_for_family(entry["family"].as_str().expect("family"))
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect();
                assert_eq!(names, declared, "pool `{key}` in {path}");
            }
        }
    }

    #[test]
    fn pool_spec_reads_identity() {
        let original = load(FIXTURE_73385);
        let v4 = pool_spec(&original["pools"]["v4"]).expect("spec");
        assert!(v4.family.ends_with("_v4"));
        assert!(v4.address.is_none());
        assert!(v4.pool_id.is_some());
        let v3 = pool_spec(&original["pools"]["v3_0"]).expect("spec");
        assert!(v3.address.is_some());
        assert!(v3.pool_id.is_none());
    }
}
