#![expect(
    clippy::expect_used,
    reason = "filesystem walks fail loudly in a pin test"
)]

//! Name-level pin for V3/V4 tick-word staging: the backrun strategy crate
//! must reach tick maps only through the ingress or a journal-provenanced
//! seed, never by fetching tick words itself on an admission path nor by
//! fabricating a `TickMapSeed` literal to claim Db/Chain provenance (a
//! private seal field makes that a compile error; this pins the symbol-level
//! intent too).
//!
//! Textual scans keep the check compile-error-free (the `nonce_issuer_unified`
//! pattern) while catching a re-introduced private ladder at the symbol level.
//! The behavioral staging contract lives in `bot_core::pool_ingress` tests.

use std::fs;
use std::path::{Path, PathBuf};

/// Raw tick-word fetch primitives that must stay outside `degenbot-strategy`.
const FORBIDDEN_TICK_FETCHES: &[&str] = &[
    "fetch_tick_bitmap",
    "fetch_tick_data",
    "bootstrap_v3_tick_word",
    "bootstrap_v4_tick_word",
];

fn rust_sources(root: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).expect("the crate src dir is readable") {
        let path = entry.expect("a src dir entry is readable").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn strategy_sources() -> Vec<PathBuf> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    assert!(!files.is_empty(), "found no strategy sources to scan");
    files
}

#[test]
fn no_tick_word_fetch_lives_in_the_strategy_crate() {
    for file in strategy_sources() {
        let text = fs::read_to_string(&file).expect("a strategy source is utf-8");
        for token in FORBIDDEN_TICK_FETCHES {
            assert!(
                !text.contains(token),
                "{} performs a raw tick-word fetch ({token}); route tick-map staging through bot_core::pool_ingress",
                file.display()
            );
        }
    }
}

#[test]
fn strategy_crate_cannot_mint_db_or_chain_seed_provenance() {
    for file in strategy_sources() {
        let text = fs::read_to_string(&file).expect("a strategy source is utf-8");
        for token in ["TickMapSeed::db(", "TickMapSeed::chain(", "TickMapSeed {"] {
            assert!(
                !text.contains(token),
                "{} claims Db/Chain seed provenance ({token}); only bot_core::pool_ingress may mint it",
                file.display()
            );
        }
    }
}
