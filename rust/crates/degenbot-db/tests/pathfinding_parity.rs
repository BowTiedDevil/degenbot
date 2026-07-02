//! §4.2 parity for the pathfinding graph-construction read fns.
//!
//! Opens the frozen `fixtures/pathfinding.db` (built by
//! `fixtures/generate_pathfinding.py` — a REAL Alembic-stamped DB seeded with
//! V2 + V3 + V4 pools connecting overlapping tokens, some degree-1 to exercise
//! the candidate filter) and asserts the Rust read fns produce results
//! identical to what the Python `_prepare_graph` / `_get_tokens_with_min_degree`
//! oracle returned against the same DB (recorded in
//! `fixtures/pathfinding_expected.json`).
//!
//! To regenerate the fixture:
//!   `uv run python rust/crates/degenbot-db/tests/fixtures/generate_pathfinding.py`

use std::collections::HashMap;
use std::path::PathBuf;

use alloy::primitives::Address;
use degenbot_pathfinding::PoolKind;
use serde::Deserialize;

use degenbot_db::{DegenbotDb, SchemaState};

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture_db_path() -> PathBuf {
    PathBuf::from(FIXTURE_DIR).join("pathfinding.db")
}

fn fixture_expected() -> Expected {
    let path = PathBuf::from(FIXTURE_DIR).join("pathfinding_expected.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("pathfinding_expected.json is well-formed")
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Expected {
    chain_id: i64,
    /// Unfiltered `[token0, token1, pool_id, kind_u8]` edges (sorted).
    edges: Vec<[u64; 4]>,
    /// Degree-2-filtered edges (both endpoints candidates) — tests composition.
    filtered_edges: Vec<[u64; 4]>,
    /// Token IDs with degree >= 2 (sorted).
    candidate_tokens: Vec<u64>,
    /// V2/V3 `pool_id_str` → checksummed address.
    v2v3_addresses: HashMap<String, String>,
    /// V4 `pool_id_str` → `[manager_address, pool_hash]`.
    v4_lookups: HashMap<String, [String; 2]>,
    /// `pool_id_str` → `kind_u8`.
    pool_id_to_kind: HashMap<String, u8>,
    /// Checksummed address → token ID.
    token_by_address: HashMap<String, u64>,
}

fn open_db() -> DegenbotDb {
    let (db, state) = DegenbotDb::open(&fixture_db_path())
        .unwrap_or_else(|e| panic!("open {}: {e}", fixture_db_path().display()));
    assert_eq!(state, SchemaState::AlembicCurrent);
    db
}

#[test]
fn fetch_path_graph_edges_matches_python_oracle() {
    let db = open_db();
    let expected = fixture_expected();
    let data = db
        .fetch_path_graph_edges(
            expected.chain_id,
            &[PoolKind::V2, PoolKind::V3, PoolKind::V4],
        )
        .unwrap();

    // Sort edges for deterministic comparison (the Python oracle sorts too).
    let mut rust_edges: Vec<[u64; 4]> = data
        .edges
        .iter()
        .map(|(t0, t1, pid, kind)| [*t0, *t1, *pid, u64::from(kind.as_u8())])
        .collect();
    rust_edges.sort_unstable();
    assert_eq!(rust_edges, expected.edges, "unfiltered edges mismatch");

    // v2v3_addresses: pool_id → Address.
    let rust_v2v3: std::collections::BTreeMap<String, String> = data
        .v2v3_addresses
        .iter()
        .map(|(pid, addr)| (pid.to_string(), addr.to_checksum(None)))
        .collect();
    let exp_v2v3: std::collections::BTreeMap<String, String> =
        expected.v2v3_addresses.clone().into_iter().collect();
    assert_eq!(rust_v2v3, exp_v2v3, "v2v3_addresses mismatch");

    // v4_lookups: pool_id → (manager_address, pool_hash).
    let rust_v4: std::collections::BTreeMap<String, [String; 2]> = data
        .v4_lookups
        .iter()
        .map(|(pid, (addr, hash))| (pid.to_string(), [addr.to_checksum(None), hash.clone()]))
        .collect();
    let exp_v4: std::collections::BTreeMap<String, [String; 2]> =
        expected.v4_lookups.clone().into_iter().collect();
    assert_eq!(rust_v4, exp_v4, "v4_lookups mismatch");

    // pool_id_to_kind (last-write-wins on V2/V3 vs V4 ID collision — the
    // Python oracle has the same collision; both insert V2/V3 first, then V4).
    let rust_kind: HashMap<String, u8> = data
        .pool_id_to_kind
        .iter()
        .map(|(pid, kind)| (pid.to_string(), kind.as_u8()))
        .collect();
    assert_eq!(
        rust_kind, expected.pool_id_to_kind,
        "pool_id_to_kind mismatch"
    );
}

#[test]
fn fetch_tokens_with_min_degree_matches_python_oracle() {
    let db = open_db();
    let expected = fixture_expected();
    let tokens = db
        .fetch_tokens_with_min_degree(
            expected.chain_id,
            2,
            &[PoolKind::V2, PoolKind::V3, PoolKind::V4],
        )
        .unwrap();
    let mut rust_tokens: Vec<u64> = tokens.into_iter().collect();
    rust_tokens.sort_unstable();
    assert_eq!(
        rust_tokens, expected.candidate_tokens,
        "degree tokens mismatch"
    );
}

#[test]
fn fetch_token_ids_by_address_matches_python_oracle() {
    let db = open_db();
    let expected = fixture_expected();

    // Decode all oracle addresses → query the Rust fn in one batch.
    let addrs: Vec<Address> = expected
        .token_by_address
        .keys()
        .map(|s| Address::parse_checksummed(s, None).expect("parse oracle address"))
        .collect();
    let map = db
        .fetch_token_ids_by_address(expected.chain_id, &addrs)
        .unwrap();
    let rust_map: HashMap<String, u64> = map
        .iter()
        .map(|(addr, tid)| (addr.to_checksum(None), *tid))
        .collect();
    assert_eq!(
        rust_map, expected.token_by_address,
        "token_by_address mismatch"
    );
}

#[test]
fn composed_fetch_and_filter_matches_python_prepare_graph() {
    // Compose the three fns the way `_prepare_graph` does internally:
    // candidate_tokens = fetch_tokens_with_min_degree(degree=2);
    // edges = fetch_path_graph_edges().filter(|(t0,t1,_,_)| both in candidates).
    let db = open_db();
    let expected = fixture_expected();

    let candidates = db
        .fetch_tokens_with_min_degree(
            expected.chain_id,
            2,
            &[PoolKind::V2, PoolKind::V3, PoolKind::V4],
        )
        .unwrap();
    let data = db
        .fetch_path_graph_edges(
            expected.chain_id,
            &[PoolKind::V2, PoolKind::V3, PoolKind::V4],
        )
        .unwrap();

    let mut filtered: Vec<[u64; 4]> = data
        .edges
        .iter()
        .filter(|(t0, t1, _, _)| candidates.contains(t0) && candidates.contains(t1))
        .map(|(t0, t1, pid, kind)| [*t0, *t1, *pid, u64::from(kind.as_u8())])
        .collect();
    filtered.sort_unstable();
    assert_eq!(
        filtered, expected.filtered_edges,
        "composed filter mismatch"
    );

    // Sanity: the DEAD token (id 5) pool — degree 1 — is filtered out.
    assert!(
        !candidates.contains(&5),
        "DEAD token should be degree-1 filtered"
    );
}

#[test]
fn empty_pool_kinds_returns_empty() {
    let db = open_db();
    let data = db.fetch_path_graph_edges(8453, &[]).unwrap();
    assert!(data.edges.is_empty());
    assert!(data.v2v3_addresses.is_empty());
    assert!(data.v4_lookups.is_empty());

    let tokens = db.fetch_tokens_with_min_degree(8453, 2, &[]).unwrap();
    assert!(tokens.is_empty());
}

#[test]
fn v2_only_kind_skips_v4_and_v3() {
    // V2-only: no V4 lookups, no V3 edges.
    let db = open_db();
    let data = db.fetch_path_graph_edges(8453, &[PoolKind::V2]).unwrap();
    assert!(
        data.v4_lookups.is_empty(),
        "V4 lookups present for V2-only query"
    );
    assert!(
        data.edges.iter().all(|(_, _, _, k)| *k == PoolKind::V2),
        "non-V2 edges present for V2-only query",
    );
}

#[test]
fn v4_only_kind_skips_v2v3() {
    let db = open_db();
    let data = db.fetch_path_graph_edges(8453, &[PoolKind::V4]).unwrap();
    assert!(
        data.v2v3_addresses.is_empty(),
        "V2/V3 addresses present for V4-only"
    );
    assert!(
        data.edges.iter().all(|(_, _, _, k)| *k == PoolKind::V4),
        "non-V4 edges present for V4-only query",
    );
}
