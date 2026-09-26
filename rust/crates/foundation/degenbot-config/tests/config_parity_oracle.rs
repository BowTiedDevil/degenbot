//! Cross-surface config acceptance ledger (task BZ6W4Y): the closing gate that
//! pins a Rust `degenbot-config` resolution and a Python FFI resolution of the
//! SAME operator file + environment to one shared JSON oracle.
//!
//! `rust_resolution_matches_intent_and_writes_the_shared_oracle` resolves every
//! case through `degenbot-config` in Rust, asserts each verdict against the
//! operator's intent recorded here, and WRITES the verdict to
//! `tests/fixtures/config_parity/oracle.json`. The Python companion
//! (`tests/test_config_parity.py`) resolves the same cases through
//! `degenbot._ffi` and compares to that artifact. A resolver regression on
//! either side, a stale or foreign holder install, or an unknown-layer
//! provenance map that reports the wrong winner shows up as a non-empty diff.
//!
//! The oracle is committed: the Rust half regenerates it from a real load and
//! the Python half reads it. Both halves must agree with the operator file.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use degenbot_config::{
    resolve_node_uri, BotConfigLoader, LoadedConfig, MapEnv, NodeOverrides, NodeScope,
};

/// One resolution the ledger pins: the fixed inputs and the verdict the
/// operator intends.
struct Case {
    id: &'static str,
    chain: u64,
    scope: &'static str,
    env: &'static [(&'static str, &'static str)],
    expected: Expected,
}

/// The operator's intent for a case.
enum Expected {
    /// The endpoint and the layer that must supply it.
    Uri(&'static str, &'static str),
    /// No layer supplies an endpoint: the resolver must refuse.
    Refusal,
}

impl Case {
    /// A resolution case with no environment override.
    const fn uri(
        id: &'static str,
        chain: u64,
        scope: &'static str,
        uri: &'static str,
        source: &'static str,
    ) -> Self {
        Self {
            id,
            chain,
            scope,
            env: &[],
            expected: Expected::Uri(uri, source),
        }
    }
}

const CASES: &[Case] = &[
    Case::uri(
        "file_only_request",
        101,
        "request",
        "http://file-101.example:8545",
        "file",
    ),
    Case {
        id: "env_http_beats_file_ipc",
        chain: 102,
        scope: "request",
        env: &[(
            "DEGENBOT_RPC_HTTP_CHAINID_102",
            "https://env-102.example:8545",
        )],
        expected: Expected::Uri("https://env-102.example:8545", "env"),
    },
    Case::uri(
        "file_ipc_preferred_request",
        105,
        "request",
        "/tmp/file-105.ipc",
        "file",
    ),
    Case::uri(
        "file_ws_subscription",
        104,
        "subscription",
        "ws://file-104.example:8546",
        "file",
    ),
    Case {
        id: "subscription_never_selects_http",
        chain: 103,
        scope: "subscription",
        env: &[],
        expected: Expected::Refusal,
    },
];

/// The resolved verdict, in the shape the shared JSON artifact records.
struct Verdict {
    uri: Option<String>,
    source: Option<String>,
    error: Option<String>,
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

/// Load the fixed operator file with exactly the case's environment: the
/// process environment is NOT consulted, so the Rust verdict is deterministic.
fn load_for(case: &Case) -> LoadedConfig {
    match BotConfigLoader::new()
        .with_config_path(operator_path())
        .with_env(env_map(case.env))
        .load()
    {
        Ok(loaded) => loaded,
        Err(err) => unreachable!("fixture operator file must load, refused with: {err}"),
    }
}

fn resolve(case: &Case) -> Verdict {
    let loaded = load_for(case);
    let scope = match case.scope.parse::<NodeScope>() {
        Ok(scope) => scope,
        Err(err) => unreachable!("fixture scope must parse, refused with: {err}"),
    };
    match resolve_node_uri(&loaded, case.chain, scope, &NodeOverrides::new()) {
        Ok(resolved) => Verdict {
            uri: Some(resolved.value),
            source: Some(resolved.source.to_string()),
            error: None,
        },
        Err(err) => Verdict {
            uri: None,
            source: None,
            error: Some(err.to_string()),
        },
    }
}

/// Minimal JSON string escaping for the artifact writer. The values are ASCII
/// URIs and one refusal message, but this keeps the artifact valid JSON.
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

fn render_oracle(entries: &[(&Case, Verdict)]) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"schema\": 1,\n");
    out.push_str(
        "  \"description\": \"Cross-surface config acceptance ledger (task BZ6W4Y): \
         the Rust degenbot-config verdicts for the fixed operator file + per-case \
         environment, written by rust_resolution_matches_intent_and_writes_the_shared_oracle \
         and read by tests/test_config_parity.py, which resolves the same cases through \
         degenbot._ffi.\",\n",
    );
    out.push_str("  \"operator_file\": \"operator.toml\",\n");
    out.push_str("  \"cases\": [\n");
    let last = entries.len().saturating_sub(1);
    for (index, (case, verdict)) in entries.iter().enumerate() {
        out.push_str("    {\n");
        let _ = writeln!(out, "      \"id\": \"{}\",", case.id);
        let _ = writeln!(out, "      \"chain_id\": {},", case.chain);
        let _ = writeln!(out, "      \"scope\": \"{}\",", case.scope);
        out.push_str("      \"env\": {");
        for (env_index, (key, value)) in case.env.iter().enumerate() {
            if env_index > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "\"{}\": \"{}\"", key, escape(value));
        }
        out.push_str("},\n");
        match verdict {
            Verdict {
                uri: Some(uri),
                source: Some(source),
                error: None,
            } => {
                let _ = writeln!(
                    out,
                    "      \"verdict\": {{\"uri\": \"{}\", \"source\": \"{}\"}}",
                    escape(uri),
                    escape(source),
                );
            }
            Verdict {
                uri: None,
                source: None,
                error: Some(error),
            } => {
                let _ = writeln!(
                    out,
                    "      \"verdict\": {{\"error\": \"{}\"}}",
                    escape(error),
                );
            }
            _ => unreachable!("a verdict is either a resolution or a refusal"),
        }
        if index == last {
            out.push_str("    }\n");
        } else {
            out.push_str("    },\n");
        }
    }
    out.push_str("  ]\n}\n");
    out
}

/// The Rust half of the ledger: resolve the fixed file + environment, assert
/// the operator intent, and publish the verdicts for the Python half.
#[test]
fn rust_resolution_matches_intent_and_writes_the_shared_oracle() {
    let mut entries: Vec<(&Case, Verdict)> = Vec::with_capacity(CASES.len());
    for case in CASES {
        let verdict = resolve(case);
        match (&case.expected, &verdict) {
            (
                Expected::Uri(uri, source),
                Verdict {
                    uri: Some(actual_uri),
                    source: Some(actual_source),
                    error: None,
                },
            ) => {
                assert_eq!(actual_uri, uri, "case {}: endpoint", case.id);
                assert_eq!(actual_source, source, "case {}: reported layer", case.id);
            }
            (
                Expected::Refusal,
                Verdict {
                    uri: None,
                    source: None,
                    error: Some(message),
                },
            ) => {
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
                    "case {}: expected {}, resolved uri={:?} source={:?} error={:?}",
                    case.id,
                    expected_intent_name(expected),
                    verdict.uri,
                    verdict.source,
                    verdict.error
                );
            }
        }
        entries.push((case, verdict));
    }

    let rendered = render_oracle(&entries);
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
        Expected::Uri(..) => "a resolved endpoint + layer",
        Expected::Refusal => "a refusal",
    }
}
