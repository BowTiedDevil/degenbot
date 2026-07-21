//! Cross-implementation parity fixture (binding #4 — HARD gate).
//!
//! Opens the frozen Alembic-stamped `fixtures/parity.db` (a REAL production-shape
//! DB built by `fixtures/generate_parity.py` via `create_new_sqlite_database` +
//! `Base.metadata.create_all` + `alembic stamp head`) and asserts the Rust
//! read fns produce results identical to what the Python `DatabaseSnapshot`
//! oracle returned against the same DB (recorded in
//! `fixtures/parity_expected.json`), INCLUDING the `VARCHAR(78)` ↔ `U256`
//! boundary: the seed data carries values exceeding `i64::MAX` (2^70) and
//! `u128::MAX` (for the gross column), proving the decimal-string round-trip.
//!
//! The fixture DB stamps `alembic_version` at `ALEMBIC_HEAD`, so [`open`]
//! returns [`SchemaState::AlembicCurrent`] and writes NOTHING — exactly the
//! hybrid-period guarantee that the Rust reader cannot mutate an
//! Alembic-stamped production DB. `PRAGMA query_only=on` is asserted by the
//! `connection.rs` unit tests; here we additionally confirm the open surface
//! detected `AlembicCurrent`.
//!
//! To regenerate the fixture (after changing seed data):
//!   `uv run python rust/crates/degenbot-db/tests/fixtures/generate_parity.py`

use std::collections::HashMap;
use std::path::PathBuf;

use alloy::primitives::{Address, B256, U256};
use serde::Deserialize;

use degenbot_db::{DegenbotDb, ExchangeFamily, SchemaState};

/// Local alias for the streamed result accumulator (mirrors the private
/// `TickMap`). Keeps clippy's `type_complexity` lint quiet in the parity tests.
type StreamedMap = std::collections::HashMap<i32, (U256, alloy::primitives::I256)>;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture_db_path() -> PathBuf {
    PathBuf::from(FIXTURE_DIR).join("parity.db")
}

fn fixture_expected() -> Expected {
    let path = PathBuf::from(FIXTURE_DIR).join("parity_expected.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("parity_expected.json is well-formed")
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Expected {
    chain_id: i64,
    v3_pool_address: String,
    v3_liquidity_map: Option<LiquidityMapJson>,
    v3_all_liquidity_maps: HashMap<String, HashMap<String, [String; 2]>>,
    v3_newest_block: Option<i64>,
    v3_pools: Vec<String>,
    v4_pool_manager: String,
    v4_pool_hash: String,
    v4_liquidity_map: Option<LiquidityMapJson>,
    v4_all_liquidity_maps: Vec<V4AllEntry>,
    v4_newest_block: Option<i64>,
    v4_pools: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct LiquidityMapJson {
    tick_bitmap: HashMap<String, BitmapJson>,
    tick_data: HashMap<String, LiquidityAtTickJson>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BitmapJson {
    bitmap: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct LiquidityAtTickJson {
    liquidity_gross: String,
    liquidity_net: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct V4AllEntry {
    pool_manager: String,
    pool_hash: String,
    ticks: HashMap<String, [String; 2]>,
}

fn parse_addr(s: &str) -> Address {
    Address::parse_checksummed(s, None).unwrap_or_else(|e| panic!("parse {s}: {e}"))
}

fn parse_u256(s: &str) -> U256 {
    U256::from_str_radix(s, 10).unwrap_or_else(|e| panic!("u256 parse {s}: {e}"))
}

fn parse_i256(s: &str) -> alloy::primitives::I256 {
    alloy::primitives::I256::from_dec_str(s).unwrap_or_else(|e| panic!("i256 parse {s}: {e}"))
}

fn parse_tick(s: &str) -> i32 {
    s.parse::<i32>()
        .unwrap_or_else(|e| panic!("tick parse {s}: {e}"))
}

#[test]
fn opens_alembic_stamped_db_as_current_and_writes_nothing() {
    let (db, state) =
        DegenbotDb::open(&fixture_db_path()).expect("parity.db should open as AlembicCurrent");
    assert_eq!(state, SchemaState::AlembicCurrent);
    // No degenbot tables were created by the open (the Alembic DB already has
    // them; ensure_schema wrote nothing). Spot-check the private stamp table is
    // ABSENT (it is only written on the fresh-standalone path).
    let conn = db.lock();
    let stamp: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='_degenbot_db_schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        stamp, 0,
        "Rust private stamp table must NOT exist on an Alembic DB"
    );
}

#[test]
fn fetch_token_by_known_address_round_trips() {
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let exp = fixture_expected();
    let tok_a_addr = "0x236aa50979D5f3De3Bd1Eeb40E81137F22ab794b";
    let tok = db
        .fetch_token_by_address(parse_addr(tok_a_addr), exp.chain_id)
        .unwrap()
        .expect("TokA should exist");
    assert_eq!(tok.address.to_checksum(None), tok_a_addr);
    assert_eq!(tok.symbol.as_deref(), Some("TKA"));
}

#[test]
fn fetch_pool_by_address_matches_python() {
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let exp = fixture_expected();
    let pool = db
        .fetch_pool_by_address(parse_addr(&exp.v3_pool_address), exp.chain_id)
        .unwrap()
        .expect("V3 pool should exist");
    assert_eq!(pool.address.to_checksum(None), exp.v3_pool_address);
    assert_eq!(pool.kind, "uniswap_v3");
    assert_eq!(pool.chain, exp.chain_id);
}

#[test]
fn fetch_pool_kind_v3_matches_shape() {
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let exp = fixture_expected();
    let pool = db
        .fetch_pool_by_address(parse_addr(&exp.v3_pool_address), exp.chain_id)
        .unwrap()
        .unwrap();
    let kind = db.fetch_pool_kind(&pool.kind, pool.id).unwrap().unwrap();
    match kind {
        degenbot_db::PoolKindRow::V3(v3) => {
            assert_eq!(v3.tick_spacing, 60);
            assert_eq!(v3.fee_denominator, 1000);
            assert_eq!(v3.liquidity_update_block, Some(12_345_000));
            assert_eq!(v3.liquidity_update_log_index, Some(42));
        }
        other => panic!("expected V3 pool kind, got {other:?}"),
    }
}

#[test]
fn fetch_liquidity_map_v3_matches_python_oracle() {
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let exp = fixture_expected();
    let map = db
        .fetch_liquidity_map(parse_addr(&exp.v3_pool_address))
        .unwrap()
        .expect("V3 liquidity map should exist");
    let expected = exp.v3_liquidity_map.expect("expected V3 map present");

    // tick_bitmap
    let mut rb: HashMap<i64, U256> = HashMap::new();
    for (ws, b) in &expected.tick_bitmap {
        rb.insert(ws.parse().unwrap(), parse_u256(&b.bitmap));
    }
    assert_eq!(map.tick_bitmap.len(), rb.len(), "tick_bitmap lengths");
    for (word, bm) in &map.tick_bitmap {
        assert_eq!(
            bm.bitmap,
            rb.get(word).copied().unwrap(),
            "tick_bitmap word {word}"
        );
    }

    // tick_data
    let mut rd: HashMap<i32, (U256, alloy::primitives::I256)> = HashMap::new();
    for (ts, v) in &expected.tick_data {
        rd.insert(
            parse_tick(ts),
            (parse_u256(&v.liquidity_gross), parse_i256(&v.liquidity_net)),
        );
    }
    assert_eq!(map.tick_data.len(), rd.len(), "tick_data lengths");
    for (tick, lat) in &map.tick_data {
        let want = rd.get(tick).copied().unwrap();
        assert_eq!(lat.liquidity_gross, want.0, "gross @ tick {tick}");
        assert_eq!(lat.liquidity_net, want.1, "net @ tick {tick}");
    }
}

#[test]
fn fetch_liquidity_map_v4_matches_python_oracle() {
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let exp = fixture_expected();
    let pool_hash =
        B256::from_slice(&alloy::hex::decode(exp.v4_pool_hash.trim_start_matches("0x")).unwrap());
    let map = db
        .fetch_liquidity_map_v4(parse_addr(&exp.v4_pool_manager), pool_hash)
        .unwrap()
        .expect("V4 liquidity map should exist");
    let expected = exp.v4_liquidity_map.expect("expected V4 map present");

    let mut rb: HashMap<i64, U256> = HashMap::new();
    for (ws, b) in &expected.tick_bitmap {
        rb.insert(ws.parse().unwrap(), parse_u256(&b.bitmap));
    }
    assert_eq!(map.tick_bitmap.len(), rb.len(), "V4 bitmap lengths");
    for (word, bm) in &map.tick_bitmap {
        assert_eq!(
            bm.bitmap,
            rb.get(word).copied().unwrap(),
            "V4 bitmap word {word}"
        );
    }

    let mut rd: HashMap<i32, (U256, alloy::primitives::I256)> = HashMap::new();
    for (ts, v) in &expected.tick_data {
        rd.insert(
            parse_tick(ts),
            (parse_u256(&v.liquidity_gross), parse_i256(&v.liquidity_net)),
        );
    }
    assert_eq!(map.tick_data.len(), rd.len(), "V4 tick_data lengths");
    for (tick, lat) in &map.tick_data {
        let want = rd.get(tick).copied().unwrap();
        assert_eq!(lat.liquidity_gross, want.0, "V4 gross @ tick {tick}");
        assert_eq!(lat.liquidity_net, want.1, "V4 net @ tick {tick}");
    }
}

#[test]
fn fetch_all_liquidity_maps_v3_matches_python_oracle() {
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let exp = fixture_expected();
    let all = db
        .fetch_all_liquidity_maps(exp.chain_id, ExchangeFamily::V3)
        .unwrap();
    assert_eq!(
        all.len(),
        exp.v3_all_liquidity_maps.len(),
        "V3 batch pool count"
    );

    for (key, ticks) in &all {
        let addr = match key {
            degenbot_db::PoolKey::V3(a) => a.to_checksum(None),
            degenbot_db::PoolKey::V4 { .. } => panic!("expected V3 pool key"),
        };
        let want = exp
            .v3_all_liquidity_maps
            .get(&addr)
            .unwrap_or_else(|| panic!("pool {addr} not in expected"));
        assert_eq!(ticks.len(), want.len(), "tick count for {addr}");
        for (tick, (gross, net)) in ticks {
            let want_pair = want
                .get(&tick.to_string())
                .unwrap_or_else(|| panic!("tick {tick} for {addr}"));
            assert_eq!(*gross, parse_u256(&want_pair[0]), "gross {addr}/{tick}");
            assert_eq!(*net, parse_i256(&want_pair[1]), "net {addr}/{tick}");
        }
    }
}

#[test]
fn fetch_all_liquidity_maps_v4_matches_python_oracle() {
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let exp = fixture_expected();
    let all = db
        .fetch_all_liquidity_maps(exp.chain_id, ExchangeFamily::V4)
        .unwrap();
    assert_eq!(
        all.len(),
        exp.v4_all_liquidity_maps.len(),
        "V4 batch pool count"
    );

    for (key, ticks) in &all {
        let (pm, hash) = match key {
            degenbot_db::PoolKey::V4 {
                pool_manager,
                pool_hash,
            } => (pool_manager.to_checksum(None), pool_hash.clone()),
            degenbot_db::PoolKey::V3(_) => panic!("expected V4 pool key"),
        };
        let want = exp
            .v4_all_liquidity_maps
            .iter()
            .find(|e| e.pool_manager == pm && e.pool_hash == hash)
            .unwrap_or_else(|| panic!("V4 pool ({pm}, {hash}) not in expected"));
        assert_eq!(
            ticks.len(),
            want.ticks.len(),
            "V4 tick count for {pm}/{hash}"
        );
        for (tick, (gross, net)) in ticks {
            let want_pair = want
                .ticks
                .get(&tick.to_string())
                .unwrap_or_else(|| panic!("V4 tick {tick} for {pm}/{hash}"));
            assert_eq!(
                *gross,
                parse_u256(&want_pair[0]),
                "V4 gross {pm}/{hash}/{tick}"
            );
            assert_eq!(*net, parse_i256(&want_pair[1]), "V4 net {pm}/{hash}/{tick}");
        }
    }
}

#[test]
fn stream_liquidity_maps_v3_matches_materialized() {
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let exp = fixture_expected();
    let materialized = db
        .fetch_all_liquidity_maps(exp.chain_id, ExchangeFamily::V3)
        .unwrap();
    let mut streamed: Vec<(degenbot_db::PoolKey, StreamedMap)> = Vec::new();
    db.stream_liquidity_maps(exp.chain_id, ExchangeFamily::V3, |key, ticks| {
        streamed.push((key, ticks.clone()));
    })
    .unwrap();
    assert_eq!(streamed.len(), materialized.len(), "V3 streamed pool count");
    for (i, ((mk, mt), (sk, st))) in materialized.iter().zip(streamed.iter()).enumerate() {
        assert_eq!(mk, sk, "V3 pool key mismatch at {i}");
        assert_eq!(mt, st, "V3 tick map mismatch at {i}");
    }
}

#[test]
fn stream_liquidity_maps_v4_matches_materialized() {
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let exp = fixture_expected();
    let materialized = db
        .fetch_all_liquidity_maps(exp.chain_id, ExchangeFamily::V4)
        .unwrap();
    let mut streamed: Vec<(degenbot_db::PoolKey, StreamedMap)> = Vec::new();
    db.stream_liquidity_maps(exp.chain_id, ExchangeFamily::V4, |key, ticks| {
        streamed.push((key, ticks.clone()));
    })
    .unwrap();
    assert_eq!(streamed.len(), materialized.len(), "V4 streamed pool count");
    for (i, ((mk, mt), (sk, st))) in materialized.iter().zip(streamed.iter()).enumerate() {
        assert_eq!(mk, sk, "V4 pool key mismatch at {i}");
        assert_eq!(mt, st, "V4 tick map mismatch at {i}");
    }
}

#[test]
fn stream_liquidity_maps_empty_chain_no_callbacks() {
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    // chain_id with no pools — the callback must never fire.
    let mut calls = 0usize;
    db.stream_liquidity_maps(999_999, ExchangeFamily::V3, |_key, _ticks| {
        calls += 1;
    })
    .unwrap();
    assert_eq!(calls, 0, "no callbacks for empty chain");
}

#[test]
fn fetch_newest_update_block_matches_python() {
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let exp = fixture_expected();
    assert_eq!(
        db.fetch_newest_update_block(exp.chain_id, ExchangeFamily::V3)
            .unwrap(),
        exp.v3_newest_block,
    );
    assert_eq!(
        db.fetch_newest_update_block(exp.chain_id, ExchangeFamily::V4)
            .unwrap(),
        exp.v4_newest_block,
    );
}

#[test]
fn fetch_pools_for_chain_matches_python_v3_pools() {
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let exp = fixture_expected();
    let pools = db.fetch_pools_for_chain(exp.chain_id).unwrap();
    // the fixture has exactly one V3 pool; Python's get_pools() V3 returns it.
    let v3_addrs: Vec<String> = pools
        .iter()
        .filter(|p| degenbot_db::schema::table::is_v3_kind(&p.kind))
        .map(|p| p.address.to_checksum(None))
        .collect();
    assert_eq!(
        v3_addrs, exp.v3_pools,
        "V3 pool addresses should match Python get_pools"
    );
}

#[test]
fn fetch_v3_pool_addresses_matches_python() {
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let exp = fixture_expected();
    let mut v3: Vec<String> = db
        .fetch_v3_pool_addresses(exp.chain_id)
        .unwrap()
        .into_iter()
        .map(|a| a.to_checksum(None))
        .collect();
    v3.sort();
    assert_eq!(
        v3, exp.v3_pools,
        "V3 pool addresses should match Python get_pools"
    );
}

#[test]
fn fetch_v4_pool_hashes_matches_python() {
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let exp = fixture_expected();
    let mut v4: Vec<String> = db
        .fetch_v4_pool_hashes(exp.chain_id)
        .unwrap()
        .into_iter()
        .map(|(h, _addr)| h)
        .collect();
    v4.sort();
    assert_eq!(
        v4, exp.v4_pools,
        "V4 pool hashes should match Python get_pools"
    );
}

#[test]
fn fetch_v4_pool_hashes_includes_pool_manager_address_via_fk() {
    // The V4 full-verify resolves the PoolManager address from the DB via the
    // `managed_pools.manager_id` → `pool_managers.address` FK (not a chunk-local
    // in-memory map). This test pins that the DB reader returns the address
    // alongside the hash, matching the committed PoolManager for the fixture's
    // single V4 pool.
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let exp = fixture_expected();
    let rows = db.fetch_v4_pool_hashes(exp.chain_id).unwrap();
    assert_eq!(rows.len(), 1, "fixture has exactly one V4 pool");
    let (hash, addr) = &rows[0];
    assert_eq!(
        hash, &exp.v4_pool_hash,
        "pool_hash should match the fixture"
    );
    assert_eq!(
        addr,
        &parse_addr(&exp.v4_pool_manager),
        "PoolManager address should be resolved via the manager_id FK"
    );
}

#[test]
fn query_only_blocks_writes_on_parity_db() {
    // Asserts binding #2 in-context: the parity connection is read-only.
    let (db, _state) = DegenbotDb::open(&fixture_db_path()).unwrap();
    let conn = db.lock();
    let write: rusqlite::Result<usize> = conn.execute("CREATE TABLE should_fail (a INT)", []);
    assert!(
        write.is_err(),
        "query_only=on must block writes on the parity connection"
    );
}
