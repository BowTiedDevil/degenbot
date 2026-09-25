//! Family-shaped env enumeration (ADR-062 D2 vocabulary).
//!
//! A key whose env layer is one name PER operator-chosen entry cannot be read
//! by a lookup, so the [`EnvVars`] seam must be able to list a name family.
//! These tests drive the seam through the process-independent map source.

use std::collections::BTreeMap;

use degenbot_config::{parse_string_map, EnvVars, MapEnv};

/// A source that only answers single-name lookups — the shape every existing
/// embedder has today, and the reason the trait method has a default.
struct LookupOnlyEnv;

impl EnvVars for LookupOnlyEnv {
    fn get(&self, name: &str) -> Option<String> {
        (name == "DEGENBOT_ONLY_LOOKUP").then(|| "1".to_string())
    }
}

#[test]
fn a_map_env_lists_a_name_family_in_sorted_order() {
    let env = MapEnv::new(BTreeMap::from([
        (
            "FIXTURE_NODES_HTTP_8453".to_string(),
            "https://base".to_string(),
        ),
        (
            "FIXTURE_NODES_HTTP_1".to_string(),
            "https://eth".to_string(),
        ),
        ("DEGENBOT_OTEL".to_string(), "1".to_string()),
        ("FIXTURE_NODES_WS_1".to_string(), "wss://eth".to_string()),
    ]));

    let names = env.names_with_prefix("FIXTURE_NODES_HTTP_");
    assert_eq!(
        names,
        vec![
            "FIXTURE_NODES_HTTP_1".to_string(),
            "FIXTURE_NODES_HTTP_8453".to_string()
        ],
        "the family is exactly the prefix matches, sorted, with no other key leaking in"
    );
    assert!(!names.is_empty());
    assert!(env.names_with_prefix("DEGENBOT_NO_SUCH_FAMILY_").is_empty());
    // Neighbouring prefixes stay separate families: the per-transport name
    // set is one prefix, never a sweep of every name that looks like it.
    assert_eq!(
        env.names_with_prefix("DEGENBOT_"),
        vec!["DEGENBOT_OTEL".to_string()]
    );
}

#[test]
fn a_lookup_only_source_reports_no_family_names() {
    // The default keeps a single-name embedder compiling and honest: it has
    // no inventory, so it claims no family rather than inventing one.
    assert!(LookupOnlyEnv.names_with_prefix("DEGENBOT_").is_empty());
    assert_eq!(
        LookupOnlyEnv.get("DEGENBOT_ONLY_LOOKUP").as_deref(),
        Some("1")
    );
}

#[test]
#[expect(
    clippy::expect_used,
    reason = "a refused parse is the assertion, not an unwrap of an unverified value"
)]
fn the_string_map_parse_trims_entries_and_refuses_a_broken_one() {
    let parsed = parse_string_map(" 1 = https://eth , 8453=https://base ").expect("well-formed");
    assert_eq!(parsed.get("1").map(String::as_str), Some("https://eth"));
    assert_eq!(parsed.get("8453").map(String::as_str), Some("https://base"));
    assert_eq!(parsed.len(), 2);

    // Unset means "no entries", not a malformed list.
    assert!(parse_string_map("").expect("empty raw value").is_empty());
    assert!(parse_string_map("   ").expect("blank raw value").is_empty());

    for bad in [
        "1",
        "1=https://eth,",
        "1=https://eth,,8453=https://base",
        "=https://eth",
    ] {
        assert!(
            parse_string_map(bad).is_err(),
            "{bad:?} must be refused rather than silently losing an entry"
        );
    }
}
