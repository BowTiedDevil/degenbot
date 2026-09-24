#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown,
    reason = "boot-gate integration test asserts on known-valid fixture output; the ledger identifiers in the parse contract are deliberately unbackticked in prose"
)]
//! RSP-8 fixture boot gate — the executable parity ledger .
//!
//! Shells the built settlement-bot binary against the frozen parity.db
//! fixture with no RPC (--smoke-offline) and diffs its machine-checkable
//! stdout against the shared oracle
//! (tests/standalone_parity/fixtures/settlement_bot_boot.json), the same
//! fixture the Python half
//! (tests/standalone_parity/test_settlement_bot_boot_gate.py) reads.
//!
//! Machine-checkable fields (the extraction contract):
//! - parity-ledger row=<id> status=<status> (grep '^parity-ledger row=')
//! - parity-ledger snapshot-seed-block S=<None|u64>
//! - [boot] discovery enumerated <n> candidate pools
//! - [g3] graph built: <n> nodes, <n> candidate tokens, <n> requested kinds [...]
//! - [g3] offline-dry pipeline: key=value ...
//!
//! This is the fixture-only half of the running dual-driver gate. The
//! recorded/anvil half lives in
//! tests/standalone_parity/dual_driver_gate.py (gated behind
//! DEGENBOT_DUAL_DRIVER_GATE=1).
//!
//! ## Seeded-divergence proof
//!
//! seeded_divergence_oracle_mutation_fails mutates one expected ledger
//! status in an in-memory copy of the oracle and asserts the comparator
//! fails (the checked-in oracle is never touched).
//! seeded_divergence_live_knob_fails re-runs the real binary with the
//! DEGENBOT_DISCOVERY_CHAIN_ID test seam removed, so the enumeration is
//! empty, and asserts the comparator catches the live divergence — proving
//! the gate observes the binary, not itself.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

const ORACLE_REL: &str = "../../../tests/standalone_parity/fixtures/settlement_bot_boot.json";
const DB_REL: &str = "../../crates/foundation/degenbot-db/tests/fixtures/parity.db";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn oracle_path() -> PathBuf {
    manifest_dir().join(ORACLE_REL)
}

fn db_path() -> PathBuf {
    manifest_dir().join(DB_REL)
}

fn load_oracle() -> Value {
    let raw = std::fs::read_to_string(oracle_path())
        .unwrap_or_else(|e| panic!("boot gate: read oracle {}: {e}", oracle_path().display()));
    serde_json::from_str(&raw).expect("boot gate: oracle is valid JSON")
}

/// The parsed machine-checkable boot report.
#[derive(Debug, Default, PartialEq, Eq)]
struct BootReport {
    snapshot_seed_block: Option<u64>,
    discovery_count: u64,
    graph_nodes: u64,
    graph_candidate_tokens: u64,
    graph_requested_kinds: Vec<String>,
    offline_dry: BTreeMap<String, String>,
    ledger_rows: BTreeMap<String, String>,
}

fn parse_boot_report(stdout: &str) -> BootReport {
    let mut report = BootReport::default();
    for line in stdout.lines() {
        if let Some(body) = line.strip_prefix("parity-ledger snapshot-seed-block S=") {
            report.snapshot_seed_block = match body.trim() {
                "None" => None,
                other => Some(other.parse::<u64>().expect("seed block is None or u64")),
            };
        } else if let Some(body) = line.strip_prefix("parity-ledger row=") {
            let mut parts = body.split_whitespace();
            let row = parts.next().expect("row id present").to_string();
            let status = parts
                .next()
                .and_then(|token| token.strip_prefix("status="))
                .expect("status token present")
                .to_string();
            report.ledger_rows.insert(row, status);
        } else if let Some(body) = line.strip_prefix("[boot] discovery enumerated ") {
            report.discovery_count = body
                .split_whitespace()
                .next()
                .expect("discovery count present")
                .parse()
                .expect("discovery count is u64");
        } else if let Some(body) = line.strip_prefix("[g3] graph built: ") {
            let parts: Vec<&str> = body.split(", ").collect();
            assert!(parts.len() >= 3, "graph line has nodes/tokens/kinds");
            report.graph_nodes = parts[0]
                .split_whitespace()
                .next()
                .expect("node count")
                .parse()
                .expect("node count is u64");
            report.graph_candidate_tokens = parts[1]
                .split_whitespace()
                .next()
                .expect("candidate token count")
                .parse()
                .expect("candidate token count is u64");
            let kinds = body
                .split_once('[')
                .and_then(|(_, rest)| rest.split_once(']'))
                .map(|(inner, _)| inner.to_string())
                .unwrap_or_default();
            report.graph_requested_kinds = kinds
                .split(',')
                .map(|kind| kind.trim().to_string())
                .filter(|kind| !kind.is_empty())
                .collect();
        } else if let Some(body) = line.strip_prefix("[g3] offline-dry pipeline: ") {
            for token in body.split_whitespace() {
                if let Some((key, value)) = token.split_once('=') {
                    report
                        .offline_dry
                        .insert(key.to_string(), value.to_string());
                }
            }
        }
    }
    report
}

/// Convert a JSON scalar to its printed-string form (true/false, integer
/// string, or the raw string).
fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Diff the parsed report against the oracle, returning one message per
/// divergence. An empty vector means the gate passes.
fn compare(expected: &Value, actual: &BootReport) -> Vec<String> {
    let mut diffs = Vec::new();
    let exp = &expected["expected"];

    let exp_seed = match &exp["snapshot_seed_block"] {
        Value::Null => None,
        Value::Number(n) => n.as_u64(),
        other => panic!("oracle snapshot_seed_block must be null or a number, got {other}"),
    };
    if exp_seed != actual.snapshot_seed_block {
        diffs.push(format!(
            "snapshot_seed_block: expected {exp_seed:?} actual {:?}",
            actual.snapshot_seed_block
        ));
    }

    let exp_discovery = exp["discovery_count"].as_u64().expect("discovery_count");
    if exp_discovery != actual.discovery_count {
        diffs.push(format!(
            "discovery_count: expected {exp_discovery} actual {}",
            actual.discovery_count
        ));
    }

    let graph = &exp["graph"];
    let exp_nodes = graph["nodes"].as_u64().expect("graph.nodes");
    if exp_nodes != actual.graph_nodes {
        diffs.push(format!(
            "graph.nodes: expected {exp_nodes} actual {}",
            actual.graph_nodes
        ));
    }
    let exp_tokens = graph["candidate_tokens"]
        .as_u64()
        .expect("graph.candidate_tokens");
    if exp_tokens != actual.graph_candidate_tokens {
        diffs.push(format!(
            "graph.candidate_tokens: expected {exp_tokens} actual {}",
            actual.graph_candidate_tokens
        ));
    }
    let exp_kinds: Vec<String> = graph["requested_kinds"]
        .as_array()
        .expect("graph.requested_kinds")
        .iter()
        .map(|kind| kind.as_str().expect("kind is a string").to_string())
        .collect();
    if exp_kinds != actual.graph_requested_kinds {
        diffs.push(format!(
            "graph.requested_kinds: expected {exp_kinds:?} actual {:?}",
            actual.graph_requested_kinds
        ));
    }

    for (key, value) in exp["offline_dry"].as_object().expect("offline_dry") {
        let expected_value = value_to_string(value).expect("offline_dry scalar");
        match actual.offline_dry.get(key) {
            Some(actual_value) if actual_value == &expected_value => {}
            Some(actual_value) => diffs.push(format!(
                "offline_dry.{key}: expected {expected_value} actual {actual_value}"
            )),
            None => diffs.push(format!(
                "offline_dry.{key}: expected {expected_value} actual <missing>"
            )),
        }
    }

    let expected_rows = exp["ledger_rows"].as_object().expect("ledger_rows");
    for (row, value) in expected_rows {
        let expected_status = value.as_str().expect("status string");
        match actual.ledger_rows.get(row) {
            Some(actual_status) if actual_status == expected_status => {}
            Some(actual_status) => diffs.push(format!(
                "ledger_rows.{row}: expected {expected_status} actual {actual_status}"
            )),
            None => diffs.push(format!(
                "ledger_rows.{row}: expected {expected_status} actual <missing>"
            )),
        }
    }
    for row in actual.ledger_rows.keys() {
        if !expected_rows.contains_key(row) {
            diffs.push(format!("ledger_rows.{row}: unexpected row in boot report"));
        }
    }
    diffs
}

/// Run the built binary; discovery_chain_id None omits the test seam so the
/// driver falls back to its settled chain 1 (the live-divergence knob).
fn run_binary(discovery_chain_id: Option<&str>) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_degenbot-settlement-bot-example"));
    command.env_clear();
    command.env("HOME", std::env::temp_dir());
    command.env("PATH", std::env::var("PATH").unwrap_or_default());
    command.env("DEGENBOT_FIXTURE_DB", db_path());
    // The committed chain-8453 fixture is Alembic-head-stamped. Pin the
    // ADR-052 D1 heal-at-open killswitch so the spawned drivers read it
    // read-only: the three #[test]s run on parallel threads and would
    // otherwise race an in-place heal of the shared file (SQLITE_IOERR /
    // "index ix_aave_asset_config_asset already exists") and fail the boot.
    command.env("DEGENBOT_DB_AUTO_HEAL", "0");
    command.env("DEGENBOT_RPC_HTTP_CHAINID_1", "http://127.0.0.1:1");
    command.env("DEGENBOT_RPC_WS_CHAINID_1", "ws://127.0.0.1:1");
    // The hermetic boot has no config file, so its facets fall to schema
    // defaults (every facet inactive) — and the stance-independent arm gate
    // refuses an inactive settlement facet. Activate the settled-block arm
    // the way the 12-factor cascade does for any other DEGENBOT_* key.
    command.env("DEGENBOT_STRATEGY_SETTLEMENT_ACTIVE", "1");
    // Hermetic telemetry (Gap G6 /): the example now boots the
    // Prometheus scrape endpoint, so bind an ephemeral port per test binary
    // instead of racing the default 127.0.0.1:9464 across parallel tests.
    command.env("DEGENBOT_METRICS_ADDR", "127.0.0.1:0");
    if let Some(chain_id) = discovery_chain_id {
        command.env("DEGENBOT_DISCOVERY_CHAIN_ID", chain_id);
    }
    command.arg("--smoke-offline");
    let output = command
        .output()
        .expect("spawn degenbot-settlement-bot-example");
    assert!(
        output.status.success(),
        "offline boot failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("boot stdout is UTF-8")
}

/// The CI gate: the offline fixture boot matches the shared oracle.
#[test]
fn boot_gate_matches_oracle() {
    let oracle = load_oracle();
    let report = parse_boot_report(&run_binary(Some("8453")));
    let diffs = compare(&oracle, &report);
    assert!(
        diffs.is_empty(),
        "fixture boot diverged from {}:\n{}",
        oracle_path().display(),
        diffs.join("\n")
    );
    assert_eq!(
        report.ledger_rows.len(),
        20,
        "the parity ledger has one row per Python-driver surface"
    );
}

/// Teeth proof (fixture side): a mutated expected status in an in-memory copy
/// must fail the comparator. The checked-in oracle is never modified.
#[test]
fn seeded_divergence_oracle_mutation_fails() {
    let report = parse_boot_report(&run_binary(Some("8453")));
    let mut mutated = load_oracle();
    mutated["expected"]["ledger_rows"]["06-engine-subscribe-resume"] =
        Value::String("REACHABLE".into());
    let diffs = compare(&mutated, &report);
    assert!(
        !diffs.is_empty(),
        "a mutated oracle status must fail the gate"
    );
    assert!(
        diffs
            .iter()
            .any(|diff| diff.contains("06-engine-subscribe-resume")),
        "the divergence must name the mutated row: {diffs:?}"
    );
}

/// Teeth proof (live side): dropping the discovery chain seam changes the real
/// binary's enumeration (0 instead of 2); the comparator must catch it.
#[test]
fn seeded_divergence_live_knob_fails() {
    let oracle = load_oracle();
    let report = parse_boot_report(&run_binary(None));
    let diffs = compare(&oracle, &report);
    assert!(
        diffs.iter().any(|diff| diff.contains("discovery_count")),
        "the live discovery-count divergence must be caught: {diffs:?}"
    );
}
