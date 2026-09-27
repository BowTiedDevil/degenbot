//! Cross-surface config acceptance ledger: the closing gate that pins a Rust
//! `degenbot-config` load and a Python FFI load of the SAME operator file +
//! environment to one shared JSON oracle.
//!
//! `rust_resolution_matches_intent_and_writes_the_shared_oracle` loads every
//! recorded environment through `degenbot-config`, asserts the operator's
//! intent for a sample of its keys, and WRITES the whole verdict — every
//! declared key's value, its winning `Source`, and the per-entry layer of each
//! table — to `tests/fixtures/config_parity/oracle.json`. The Python companion
//! (`tests/test_config_parity.py`) loads the same file + environments through
//! the raw-FFI hypothetical entry (`degenbot._ffi.resolve_hypothetical`) in
//! ONE process and compares. A
//! resolver regression on either side, a stale or foreign holder install, or
//! an unknown-layer provenance map that reports the wrong winner shows up as a
//! non-empty diff.
//!
//! The comparison reaches the whole verdict rather than one key because the
//! pre-epic divergence was a Python default that disagreed with the schema's:
//! a key nobody thought to check. The projection is schema-driven, so a key
//! declared later is compared here with no edit.
//!
//! The oracle is committed: the Rust half regenerates it from a real load and
//! the Python half reads it. Both halves must agree with the operator file
//! and with each other.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use degenbot_config::{
    resolve_chain_id, resolve_database_path, resolve_node_uri, BotConfig, BotConfigLoader,
    ConfigValue, LoadedConfig, MapEnv, NodeOverrides, NodeScope, NodeTransport, SCHEMA,
};

/// A fixed environment and the operator's intent for a sample of its keys.
///
/// The sample is not the comparison — the whole verdict is — but it is what
/// makes the Rust half an assertion rather than a recording: a resolver
/// regression fails here instead of silently agreeing with itself.
struct Environment {
    id: &'static str,
    env: &'static [(&'static str, &'static str)],
    key_checks: &'static [KeyCheck],
    entry_checks: &'static [EntryCheck],
}

/// One declared key the environment must resolve to a value and a layer.
struct KeyCheck {
    path: &'static str,
    source: &'static str,
    value: Option<&'static str>,
}

/// One entry of a table-shaped key and the layer that must supply it.
struct EntryCheck {
    env_prefix: &'static str,
    entry: &'static str,
    source: &'static str,
}

/// The fixed environments: the file layer is shared, the environment is the
/// per-environment input, and together they pin the process-wide verdict.
///
/// One environment per layer where the layer can decide a key: a declared
/// default, a file entry, an environment export, and the unprefixed
/// verification-retry names. `cli` is not here because the installed verdict
/// cannot carry it — an explicit override is an argument to a resolution, and
/// it is pinned as its own case below.
const ENVIRONMENTS: &[Environment] = &[
    Environment {
        id: "base",
        env: &[],
        key_checks: &[
            KeyCheck {
                path: "session.chain_id",
                source: "file",
                value: Some("201"),
            },
            KeyCheck {
                path: "database.path",
                source: "default",
                value: Some("~/.local/state/degenbot/db/degenbot.db"),
            },
            KeyCheck {
                path: "telemetry.otel",
                source: "default",
                value: Some("true"),
            },
            KeyCheck {
                path: "nodes.http",
                source: "file",
                value: Some(
                    "101=http://file-101.example:8545,103=http://file-103.example:8545,105=http://file-105.example:8545",
                ),
            },
        ],
        entry_checks: &[
            EntryCheck {
                env_prefix: "DEGENBOT_RPC_HTTP_CHAINID_",
                entry: "101",
                source: "file",
            },
            EntryCheck {
                env_prefix: "DEGENBOT_RPC_IPC_CHAINID_",
                entry: "102",
                source: "file",
            },
        ],
    },
    Environment {
        id: "env_http",
        env: &[
            (
                "DEGENBOT_RPC_HTTP_CHAINID_101",
                "https://env-101.example:8545",
            ),
            (
                "DEGENBOT_RPC_HTTP_CHAINID_102",
                "https://env-102.example:8545",
            ),
        ],
        key_checks: &[
            KeyCheck {
                path: "nodes.http",
                source: "env",
                value: Some(
                    "101=https://env-101.example:8545,102=https://env-102.example:8545,103=http://file-103.example:8545,105=http://file-105.example:8545",
                ),
            },
            KeyCheck {
                path: "session.chain_id",
                source: "file",
                value: Some("201"),
            },
        ],
        entry_checks: &[
            EntryCheck {
                env_prefix: "DEGENBOT_RPC_HTTP_CHAINID_",
                entry: "101",
                source: "env",
            },
            EntryCheck {
                env_prefix: "DEGENBOT_RPC_HTTP_CHAINID_",
                entry: "103",
                source: "file",
            },
            EntryCheck {
                env_prefix: "DEGENBOT_RPC_IPC_CHAINID_",
                entry: "102",
                source: "file",
            },
        ],
    },
    Environment {
        id: "env_chain",
        env: &[("DEGENBOT_DEFAULT_CHAIN_ID", "202")],
        key_checks: &[KeyCheck {
            path: "session.chain_id",
            source: "env",
            value: Some("202"),
        }],
        entry_checks: &[],
    },
    Environment {
        id: "env_retry",
        env: &[
            ("VERIFICATION_RETRY_MAX_ATTEMPTS", "6"),
            ("VERIFICATION_RETRY_JITTER", "0.25"),
        ],
        key_checks: &[
            KeyCheck {
                path: "verify.verify_retry_max_attempts",
                source: "env",
                value: Some("6"),
            },
            KeyCheck {
                path: "verify.verify_retry_jitter",
                source: "env",
                value: Some("0.25"),
            },
        ],
        entry_checks: &[],
    },
];

/// A resolution the ledger pins: the fixed inputs and the verdict the operator
/// intends.
struct Case {
    id: &'static str,
    environment: &'static str,
    resolution: Resolution,
    expected: Expected,
}

/// The argument-taking resolution one case exercises.
enum Resolution {
    /// A node endpoint for a chain and a consumer capability, with the
    /// explicit `node` override that is the `cli` layer when it is set.
    NodeUri {
        chain: u64,
        scope: &'static str,
        node: Option<&'static str>,
    },
    /// The session chain id, with an explicit override.
    ChainId { value: &'static str },
    /// The database path, with an explicit override.
    DatabasePath { value: &'static str },
}

impl Resolution {
    fn render_json(&self) -> String {
        match self {
            Self::NodeUri { chain, scope, node } => {
                let node = node.map_or_else(|| "null".to_string(), json_string);
                format!(
                    "{{\"kind\": \"node_uri\", \"chain_id\": {chain}, \"scope\": \"{scope}\", \"node\": {node}}}"
                )
            }
            Self::ChainId { value } => format!(
                "{{\"kind\": \"chain_id\", \"value\": {}}}",
                json_string(value)
            ),
            Self::DatabasePath { value } => format!(
                "{{\"kind\": \"database_path\", \"value\": {}}}",
                json_string(value)
            ),
        }
    }
}

/// The operator's intent for a case.
enum Expected {
    /// The value and the layer that must supply it.
    Resolved {
        value: &'static str,
        source: &'static str,
    },
    /// No layer supplies an endpoint: the resolver must refuse.
    Refusal,
}

const CASES: &[Case] = &[
    Case {
        id: "file_only_request",
        environment: "base",
        resolution: Resolution::NodeUri {
            chain: 101,
            scope: "request",
            node: None,
        },
        expected: Expected::Resolved {
            value: "http://file-101.example:8545",
            source: "file",
        },
    },
    Case {
        id: "env_http_beats_file_ipc",
        environment: "env_http",
        resolution: Resolution::NodeUri {
            chain: 102,
            scope: "request",
            node: None,
        },
        expected: Expected::Resolved {
            value: "https://env-102.example:8545",
            source: "env",
        },
    },
    Case {
        id: "file_ipc_preferred_request",
        environment: "base",
        resolution: Resolution::NodeUri {
            chain: 105,
            scope: "request",
            node: None,
        },
        expected: Expected::Resolved {
            value: "/tmp/file-105.ipc",
            source: "file",
        },
    },
    Case {
        id: "file_ws_subscription",
        environment: "base",
        resolution: Resolution::NodeUri {
            chain: 104,
            scope: "subscription",
            node: None,
        },
        expected: Expected::Resolved {
            value: "ws://file-104.example:8546",
            source: "file",
        },
    },
    Case {
        id: "subscription_never_selects_http",
        environment: "base",
        resolution: Resolution::NodeUri {
            chain: 103,
            scope: "subscription",
            node: None,
        },
        expected: Expected::Refusal,
    },
    Case {
        id: "explicit_node_override",
        environment: "base",
        resolution: Resolution::NodeUri {
            chain: 101,
            scope: "request",
            node: Some("wss://explicit.example:8546"),
        },
        expected: Expected::Resolved {
            value: "wss://explicit.example:8546",
            source: "cli",
        },
    },
    Case {
        id: "explicit_chain_id_override",
        environment: "base",
        resolution: Resolution::ChainId { value: "999" },
        expected: Expected::Resolved {
            value: "999",
            source: "cli",
        },
    },
    Case {
        id: "explicit_database_path_override",
        environment: "base",
        resolution: Resolution::DatabasePath {
            value: "/tmp/explicit.db",
        },
        expected: Expected::Resolved {
            value: "/tmp/explicit.db",
            source: "cli",
        },
    },
];

/// A resolution's actual verdict, in the shape the shared JSON artifact
/// records.
enum Verdict {
    Resolved {
        value_json: String,
        text: String,
        source: String,
    },
    Refused(String),
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config_parity")
}

fn operator_path() -> PathBuf {
    fixture_dir().join("operator.toml")
}

fn oracle_path() -> PathBuf {
    fixture_dir().join("oracle.json")
}

fn env_map(pairs: &[(&str, &str)]) -> Box<dyn degenbot_config::EnvVars> {
    Box::new(MapEnv::new(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<BTreeMap<_, _>>(),
    ))
}

/// Load the fixed operator file with exactly the environment's variables: the
/// process environment is NOT consulted, so the Rust verdict is deterministic
/// and the Python probe can reproduce it from the same recorded variables.
fn load_for(environment: &Environment) -> LoadedConfig {
    match BotConfigLoader::new()
        .with_config_path(operator_path())
        .with_env(env_map(environment.env))
        .load()
    {
        Ok(loaded) => loaded,
        Err(err) => unreachable!("fixture operator file must load, refused with: {err}"),
    }
}

fn environment(id: &str) -> &'static Environment {
    match ENVIRONMENTS.iter().find(|environment| environment.id == id) {
        Some(environment) => environment,
        None => unreachable!("case names an environment that is not declared: {id}"),
    }
}

fn resolve(case: &Case, loaded: &LoadedConfig) -> Verdict {
    match &case.resolution {
        Resolution::NodeUri { chain, scope, node } => {
            let scope = match scope.parse::<NodeScope>() {
                Ok(scope) => scope,
                Err(err) => unreachable!("fixture scope must parse, refused with: {err}"),
            };
            let overrides = match node {
                Some(uri) => match NodeTransport::classify(uri) {
                    Some(transport) => NodeOverrides::new().with_transport(transport, *uri),
                    None => unreachable!("fixture node override names no transport: {uri}"),
                },
                None => NodeOverrides::new(),
            };
            match resolve_node_uri(loaded, *chain, scope, &overrides) {
                Ok(resolved) => Verdict::Resolved {
                    value_json: json_string(&resolved.value),
                    text: resolved.value,
                    source: resolved.source.to_string(),
                },
                Err(err) => Verdict::Refused(err.to_string()),
            }
        }
        Resolution::ChainId { value } => match resolve_chain_id(loaded, Some(value)) {
            Ok(resolved) => Verdict::Resolved {
                value_json: resolved.value.to_string(),
                text: resolved.value.to_string(),
                source: resolved.source.to_string(),
            },
            Err(err) => Verdict::Refused(err.to_string()),
        },
        Resolution::DatabasePath { value } => {
            let resolved = resolve_database_path(loaded, Some(value));
            Verdict::Resolved {
                value_json: json_string(&resolved.value.to_string_lossy()),
                text: resolved.value.to_string_lossy().into_owned(),
                source: resolved.source.to_string(),
            }
        }
    }
}

/// Every declared key's value, walked out of `SCHEMA` through the generated
/// `BotConfig::value` reader — the same projection the Python verdict performs,
/// so the two halves compare one schema rather than two accessor lists.
fn render_values(config: &BotConfig) -> String {
    let mut out = String::from("{");
    for (index, key) in SCHEMA.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{}: ", json_string(key.toml_path));
        match config.value(key.section, key.field) {
            Some(value) => out.push_str(&render_value(&value)),
            None => out.push_str("null"),
        }
    }
    out.push('}');
    out
}

/// The winning layer per declared key, keyed by the dotted TOML path. A key no
/// layer recorded is absent rather than reported as the floor.
fn render_provenance(layers: &LoadedConfig) -> String {
    let mut out = String::from("{");
    let mut first = true;
    for key in SCHEMA {
        let Some(source) = layers.source_of(key.env) else {
            continue;
        };
        if !first {
            out.push_str(", ");
        }
        first = false;
        let _ = write!(
            out,
            "{}: {}",
            json_string(key.toml_path),
            json_string(&source.to_string())
        );
    }
    out.push('}');
    out
}

/// The per-entry layer of every table-shaped key, keyed by the key's env name
/// and then by the operator-chosen entry.
fn render_entry_provenance(layers: &LoadedConfig) -> String {
    let mut out = String::from("{");
    for (env_index, (env, entries)) in layers.entry_provenance.iter().enumerate() {
        if env_index > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{}: {{", json_string(env));
        for (entry_index, (entry, source)) in entries.iter().enumerate() {
            if entry_index > 0 {
                out.push_str(", ");
            }
            let _ = write!(
                out,
                "{}: {}",
                json_string(entry),
                json_string(&source.to_string())
            );
        }
        out.push('}');
    }
    out.push('}');
    out
}

/// One declared value as JSON. A path is its written text, a wei amount is a
/// decimal integer, and a float keeps a decimal point so `4.0` never renders
/// as the integer `4` Python would compare equal without a type check.
fn render_value(value: &ConfigValue<'_>) -> String {
    match value {
        ConfigValue::Bool(inner) => inner.to_string(),
        ConfigValue::Text(inner) | ConfigValue::Enum(inner) => json_string(inner),
        ConfigValue::Path(inner) => json_string(&inner.to_string_lossy()),
        ConfigValue::Uint(inner) => inner.to_string(),
        ConfigValue::Int(inner) => inner.to_string(),
        ConfigValue::Float(inner) => render_float(*inner),
        ConfigValue::Map(entries) => {
            let mut out = String::from("{");
            for (index, (key, value)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "{}: {}", json_string(key), json_string(value));
            }
            out.push('}');
            out
        }
    }
}

fn render_float(value: f64) -> String {
    if value.is_finite() {
        format!("{value:?}")
    } else {
        // JSON has no `NaN`/`inf` literal; a non-finite configured value is
        // reported as its text so the artifact stays parseable.
        json_string(&value.to_string())
    }
}

/// Minimal JSON string escaping. The values are endpoints, paths, and layer
/// names, but this keeps the artifact valid for any configured text.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn json_string(value: &str) -> String {
    format!("\"{}\"", escape(value))
}

fn render_env(env: &[(&str, &str)]) -> String {
    let mut out = String::from("{");
    for (index, (key, value)) in env.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{}: {}", json_string(key), json_string(value));
    }
    out.push('}');
    out
}

fn render_environment(
    environment: &Environment,
    values: &str,
    provenance: &str,
    entry_provenance: &str,
) -> String {
    let mut out = String::new();
    out.push_str("    {\n");
    let _ = writeln!(out, "      \"id\": {},", json_string(environment.id));
    let _ = writeln!(out, "      \"env\": {},", render_env(environment.env));
    let _ = writeln!(out, "      \"values\": {values},");
    let _ = writeln!(out, "      \"provenance\": {provenance},");
    let _ = writeln!(out, "      \"entry_provenance\": {entry_provenance}");
    out.push_str("    }");
    out
}

fn render_case(case: &Case, verdict: &Verdict) -> String {
    let mut out = String::new();
    out.push_str("    {\n");
    let _ = writeln!(out, "      \"id\": {},", json_string(case.id));
    let _ = writeln!(
        out,
        "      \"environment\": {},",
        json_string(case.environment)
    );
    let _ = writeln!(
        out,
        "      \"resolution\": {},",
        case.resolution.render_json()
    );
    let verdict_json = match verdict {
        Verdict::Resolved {
            value_json, source, ..
        } => format!(
            "{{\"value\": {value_json}, \"source\": {}}}",
            json_string(source)
        ),
        Verdict::Refused(message) => format!("{{\"error\": {}}}", json_string(message)),
    };
    let _ = writeln!(out, "      \"verdict\": {verdict_json}");
    out.push_str("    }");
    out
}

fn join_entries(entries: &[String]) -> String {
    let mut out = String::new();
    for (index, entry) in entries.iter().enumerate() {
        out.push_str(entry);
        if index + 1 < entries.len() {
            out.push_str(",\n");
        } else {
            out.push('\n');
        }
    }
    out
}

fn render_oracle(environments: &[String], cases: &[String]) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"schema\": 2,\n");
    out.push_str(
        "  \"description\": \"Cross-surface config acceptance ledger: the whole \
         degenbot-config verdict (every declared key, its winning Source, and \
         the per-entry layer of each table) for the fixed operator file + each \
         environment, written by \
         rust_resolution_matches_intent_and_writes_the_shared_oracle and read \
         by tests/test_config_parity.py, which loads the same file + \
         environments through degenbot._ffi in a fresh interpreter and \
         compares.\",\n",
    );
    out.push_str("  \"operator_file\": \"operator.toml\",\n");
    out.push_str("  \"environments\": [\n");
    out.push_str(&join_entries(environments));
    out.push_str("  ],\n");
    out.push_str("  \"cases\": [\n");
    out.push_str(&join_entries(cases));
    out.push_str("  ]\n}\n");
    out
}

/// The Rust half of the ledger: load each fixed environment, assert the
/// operator intent, resolve each case, and publish the whole verdict for the
/// Python half.
#[test]
fn rust_resolution_matches_intent_and_writes_the_shared_oracle() {
    let mut environments: Vec<String> = Vec::with_capacity(ENVIRONMENTS.len());
    for environment in ENVIRONMENTS {
        let loaded = load_for(environment);
        for check in environment.key_checks {
            let Some(key) = SCHEMA.iter().find(|key| key.toml_path == check.path) else {
                unreachable!("key check names an undeclared path: {}", check.path);
            };
            assert_eq!(
                loaded.source_of(key.env).map(|source| source.to_string()),
                Some(check.source.to_string()),
                "environment {}: {} winning layer",
                environment.id,
                check.path,
            );
            if let Some(expected) = check.value {
                let Some(value) = loaded.config.value(key.section, key.field) else {
                    unreachable!("{} must have a value to assert", check.path);
                };
                assert_eq!(
                    value.to_text(),
                    expected,
                    "environment {}: {} value",
                    environment.id,
                    check.path,
                );
            }
        }
        for check in environment.entry_checks {
            assert_eq!(
                loaded
                    .entry_source_of(check.env_prefix, check.entry)
                    .map(|source| source.to_string()),
                Some(check.source.to_string()),
                "environment {}: {} entry {} layer",
                environment.id,
                check.env_prefix,
                check.entry,
            );
        }
        environments.push(render_environment(
            environment,
            &render_values(&loaded.config),
            &render_provenance(&loaded),
            &render_entry_provenance(&loaded),
        ));
    }

    let mut cases: Vec<String> = Vec::with_capacity(CASES.len());
    for case in CASES {
        let loaded = load_for(environment(case.environment));
        let verdict = resolve(case, &loaded);
        match (&case.expected, &verdict) {
            (
                Expected::Resolved { value, source },
                Verdict::Resolved {
                    text,
                    source: actual_source,
                    ..
                },
            ) => {
                assert_eq!(text, value, "case {}: value", case.id);
                assert_eq!(actual_source, source, "case {}: reported layer", case.id);
            }
            (Expected::Refusal, Verdict::Refused(message)) => {
                assert!(
                    message.contains("subscription"),
                    "case {}: refusal names the scope: {message}",
                    case.id,
                );
                assert!(
                    message.contains("nodes.http"),
                    "case {}: refusal names the http entry it did not select: {message}",
                    case.id,
                );
            }
            (expected, verdict) => {
                unreachable!(
                    "case {}: expected {}, got {}",
                    case.id,
                    expected_intent_name(expected),
                    verdict_name(verdict),
                );
            }
        }
        cases.push(render_case(case, &verdict));
    }

    let rendered = render_oracle(&environments, &cases);
    let path = oracle_path();
    if let Err(err) = std::fs::write(&path, &rendered) {
        unreachable!("write oracle {}: {err}", path.display());
    }
    let written = match std::fs::read_to_string(&path) {
        Ok(written) => written,
        Err(err) => unreachable!("read back oracle {}: {err}", path.display()),
    };
    assert_eq!(written, rendered, "the artifact round-trips byte-exact");
}

/// A name for the intent in the `unreachable!` diagnostic; the `Expected`
/// payloads are not `Debug` to keep the fixture declarations terse.
fn expected_intent_name(expected: &Expected) -> &'static str {
    match expected {
        Expected::Resolved { .. } => "a resolved value + layer",
        Expected::Refusal => "a refusal",
    }
}

fn verdict_name(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Resolved { .. } => "a resolution",
        Verdict::Refused(_) => "a refusal",
    }
}
