//! Acceptance criterion: the loader's schema covers the FULL `DEGENBOT_*`
//! key inventory. This test diffs the raw inventory sweep (`rg -o
//! "DEGENBOT_[A-Z_]+" rust/crates --no-filename | sort -u`) against the
//! schema's env names, with the documented artifact/expansion set below.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use degenbot_config::SCHEMA;

/// Raw regex matches that are NOT real static keys:
/// - `DEGENBOT_JAEGER_E` — the sweep regex stops at the digit of `DEGENBOT_JAEGER_E2E`.
/// - `DEGENBOT_V` — the regex stops at the `3` of `DEGENBOT_V3_FIXTURE_*`.
/// - `DEGENBOT_RPC_WS_CHAINID_` — trailing `_` of the DYNAMIC per-chain var
///   `DEGENBOT_RPC_WS_CHAINID_<chain_id>` (documented next to `SCHEMA`).
const SWEEP_ARTIFACTS: &[&str] = &[
    "DEGENBOT_JAEGER_E",
    // the family wildcard in the allocator doc comment (the loader
    // owns the four concrete DEGENBOT_MIMALLOC_* keys).
    "DEGENBOT_MIMALLOC_",
    "DEGENBOT_RPC_WS_CHAINID_",
    // the family wildcard named by the retired-layout refusal text
    // in the loader (the dynamic per-chain var
    // DEGENBOT_RPC_HTTP_CHAINID_<chain_id>).
    "DEGENBOT_RPC_HTTP_CHAINID_",
    "DEGENBOT_V",
    // the family wildcards in the facet-invariant tests' prefix assertions
    // (the schema owns the concrete per-ecosystem backrun keys).
    "DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_",
    "DEGENBOT_STRATEGY_TXPOOL_BACKRUN_",
    // ADR-051 D8: the family wildcard in the cli-core operator doc comment
    // (the loader owns the four concrete DEGENBOT_FLEET_CORDON_* keys).
    "DEGENBOT_FLEET_CORDON_",
    // The retired click per-flag envvar fallback named in the argv doc
    // comment; the console models these inputs as argv only.
    "DEGENBOT_CHUNK_SIZE",
];

/// Build-receipt keys (commit 1e1c0ddf7): compile-time build-artifact
/// variables around `degenbot-python/build.rs` — `DEGENBOT_BUILD_NUMBER_FILE`
/// locates the receipt file at build time, and `DEGENBOT_BUILD_NUMBER` /
/// `DEGENBOT_BUILD_FINGERPRINT` are `cargo:rustc-env` outputs consumed by
/// `build_info`. They are not runtime configuration keys.
const BUILD_ARTIFACT_KEYS: &[&str] = &[
    "DEGENBOT_BUILD_FINGERPRINT",
    "DEGENBOT_BUILD_NUMBER",
    "DEGENBOT_BUILD_NUMBER_FILE",
    // ADR-051 D2: the console cargo:rustc-env build-identity outputs
    // (degenbot-cli/build.rs), the same class as the Python build receipt.
    "DEGENBOT_CLI_BUILD_FINGERPRINT",
    "DEGENBOT_CLI_BUILD_NUMBER",
];

/// Real static env keys the config crate reads BEFORE/OUTSIDE the schema:
/// `DEGENBOT_CONFIG` selects the config FILE layer (see
/// `with_standard_file_paths` in `loader.rs`) — chicken-and-egg, it cannot
/// be an option inside the file it locates, so it is deliberately NOT a
/// schema key.
const BOOTSTRAP_KEYS: &[&str] = &["DEGENBOT_CONFIG"];

/// Driver-domain keys resolved by `resolvers.rs` (ADR-051 D8): the database
/// path, the session chain id, and the per-chain RPC URIs are console inputs
/// that never lived in the typed file layer, so they are deliberately NOT
/// schema keys. The resolvers read them through the loader's `EnvVars` seam
/// and tag each with its own `Source`.
const DRIVER_DOMAIN_KEYS: &[&str] = &[
    "DEGENBOT_DB_PATH",
    "DEGENBOT_DEFAULT_CHAIN_ID",
    // ADR-051 D8: the operator-socket override, a console shell-environment
    // input resolved by cli-core's resolve_socket cascade.
    "DEGENBOT_OPERATOR_SOCKET",
];

/// DB-open toggles read by the persistence layer itself (`degenbot-db`), not
/// the typed config schema: the ADR-052 D1 heal-at-open killswitch is a
/// `cargo add degenbot-db` contract (a consumer must be able to pin the pre-D1
/// posture without a `BotConfig`), so it is deliberately NOT a schema key.
/// Unset => heal on; only an explicit falsey word disables.
const DB_OPEN_KEYS: &[&str] = &["DEGENBOT_DB_AUTO_HEAL"];

/// RETIRED keys the loader guards for one release (P6YXA6 hard cutover):
/// they fail the load loudly and point at the replacement rather than
/// silently falling back — the deprecation-style hard error. They are not
/// schema keys anymore; the guard lives in the env layer of `loader.rs`.
/// Test-harness knobs (live fixtures + guards): these steer the replay
/// test tiers (fixture block pins, scan windows, trace toggles) and the
/// classifier panic-guard — they change which STATIC inputs a test sees,
/// never runtime configuration, so they are deliberately NOT schema keys.
const TEST_HARNESS_KEYS: &[&str] = &[
    // The pathfinding snapshot bench's fixture-path override
    // (`degenbot-db/benches/sweep_snapshot.rs`): selects which static DB the
    // bench opens, the same class as the offline parity-fixture dirs.
    "DEGENBOT_SNAPSHOT_DB",
    "DEGENBOT_CLASSIFIER_GUARD",
    "DEGENBOT_FRAME_ORACLE_CAPTURE",
    "DEGENBOT_ORACLE_",
    "DEGENBOT_ORACLE_SCAN_BLOCKS",
    "DEGENBOT_ORACLE_TRACE",
    "DEGENBOT_PARITY_BLOCK",
];

const RETIRED_KEYS: &[&str] = &[
    "DEGENBOT_LPT_PARTITION",
    "DEGENBOT_SOLVE_EXECUTOR",
    "DEGENBOT_FLEET",
    "DEGENBOT_SOLVE_SIM_INFLIGHT",
    "DEGENBOT_DETACHED_SOLVES",
    // ADR-043 §5 retired verbosity flags. These literals live in
    // `degenbot_core::telemetry::RETIRED_ENV_NAMES` (the boot-time detection
    // list) because the scanner sweeps the crates; the names are detected and
    // warned about, never honored.
    "DEGENBOT_VERIFY_DBG",
    "DEGENBOT_V2_CALC_TRACE",
    "DEGENBOT_SIM_LOG_REVERTED_SWAPS",
    "DEGENBOT_SIM_DIVERGENCE_LOG",
    "DEGENBOT_DUMP_CALL_TRACE",
    "DEGENBOT_DUMP_TICK_MAPS",
    "DEGENBOT_WS_TRACE",
    "DEGENBOT_DRAIN_DBG",
    "DEGENBOT_TRACE_DISPATCH",
    "DEGENBOT_TRACE_REGISTER_SEED",
    "DEGENBOT_TRACE_LIQUIDITY",
    "DEGENBOT_TRACE_TICK",
    "DEGENBOT_GATE_TRACE",
    "DEGENBOT_AAVE_EVTRACE",
    "DEGENBOT_AAVE_TX_TRACE",
    // ADR-043 §6: the retired stderr-fmt two-tunnel switch.
    "DEGENBOT_LOG_FMT",
];

/// Real static keys the artifact regex cannot capture (digit-terminated
/// matches expand to these full names).
const SWEEP_EXPANSIONS: &[&str] = &[
    "DEGENBOT_JAEGER_E2E",
    "DEGENBOT_V3_FIXTURE_RPC",
    "DEGENBOT_V3_FIXTURE_BLOCK",
];

/// Committed snapshot of the sweep, used verbatim when `rg` is not installed
/// (standalone-consumer checkouts). Refresh in-repo with:
/// `rg -o 'DEGENBOT_[A-Z_]+' rust/crates --no-filename −g '!**/degenbot_env_inventory.txt' | sort -u > <snapshot>`
const SNAPSHOT: &str = include_str!("degenbot_env_inventory.txt");

fn repo_root() -> &'static Path {
    let Some(root) = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(4) else {
        unreachable!("repo root is four levels above the role-grouped crate manifest dir");
    };
    root
}

/// Prefer the live sweep; fall back to the committed snapshot.
fn inventory() -> Vec<String> {
    let live = Command::new("rg")
        .args([
            "-o",
            "DEGENBOT_[A-Z_]+",
            "rust/crates",
            "--no-filename",
            "-g",
            "!**/degenbot_env_inventory.txt",
            "-g",
            "!**/degenbot-config/tests/**",
        ])
        .current_dir(repo_root())
        .output();
    match live {
        Ok(out) if out.status.success() && !out.stdout.is_empty() => {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_string)
                .collect()
        }
        // `rg` unavailable or produced nothing: use the committed snapshot.
        _ => SNAPSHOT.lines().map(str::to_string).collect(),
    }
}

#[test]
fn schema_covers_the_full_key_inventory() {
    let sweep: BTreeSet<String> = inventory().into_iter().collect();
    assert!(!sweep.is_empty(), "inventory sweep produced no keys");

    let mut expected: BTreeSet<String> = sweep
        .iter()
        .filter(|k| {
            !SWEEP_ARTIFACTS.contains(&k.as_str())
                && !BOOTSTRAP_KEYS.contains(&k.as_str())
                && !BUILD_ARTIFACT_KEYS.contains(&k.as_str())
                && !DRIVER_DOMAIN_KEYS.contains(&k.as_str())
                && !DB_OPEN_KEYS.contains(&k.as_str())
                && !TEST_HARNESS_KEYS.contains(&k.as_str())
                && !RETIRED_KEYS.contains(&k.as_str())
        })
        .cloned()
        .collect();
    for expansion in SWEEP_EXPANSIONS {
        // Only expand when the artifact that produced it was actually
        // observed (keeps the snapshot fallback consistent).
        if SWEEP_ARTIFACTS.iter().any(|a| sweep.contains(*a)) || sweep.contains(*expansion) {
            let _ = expected.insert((*expansion).to_string());
        }
    }

    let actual: BTreeSet<String> = SCHEMA.iter().map(|k| k.env.to_string()).collect();

    let unknown: Vec<_> = actual.difference(&expected).collect();
    let uncovered: Vec<_> = expected.difference(&actual).collect();
    assert!(
        unknown.is_empty() && uncovered.is_empty(),
        "schema/inventory mismatch\n  schema keys not in inventory: {unknown:?}\n  inventory keys not in schema: {uncovered:?}"
    );
}

#[test]
fn snapshot_fallback_agrees_with_schema() {
    // The committed snapshot must itself satisfy the parity contract — this
    // is the standalone-consumer path (no `rg` in the build environment).
    let sweep: BTreeSet<String> = SNAPSHOT.lines().map(str::to_string).collect();
    assert!(
        !sweep.iter().any(|k| k.contains("REGEN_DOCS")),
        "snapshot is self-contaminated; regenerate it with the exclusion glob"
    );
    // This filter chain must mirror `schema_covers_the_full_key_inventory`
    // exactly — the two inventory paths (live sweep, committed snapshot) hold
    // the identical parity contract.
    let mut expected: BTreeSet<String> = sweep
        .iter()
        .filter(|k| {
            !SWEEP_ARTIFACTS.contains(&k.as_str())
                && !BOOTSTRAP_KEYS.contains(&k.as_str())
                && !BUILD_ARTIFACT_KEYS.contains(&k.as_str())
                && !DRIVER_DOMAIN_KEYS.contains(&k.as_str())
                && !DB_OPEN_KEYS.contains(&k.as_str())
                && !TEST_HARNESS_KEYS.contains(&k.as_str())
                && !RETIRED_KEYS.contains(&k.as_str())
        })
        .cloned()
        .collect();
    for expansion in SWEEP_EXPANSIONS {
        if SWEEP_ARTIFACTS.iter().any(|a| sweep.contains(*a)) || sweep.contains(*expansion) {
            let _ = expected.insert((*expansion).to_string());
        }
    }
    let actual: BTreeSet<String> = SCHEMA.iter().map(|k| k.env.to_string()).collect();
    assert_eq!(actual, expected);
}
