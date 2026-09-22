//! The documented add-a-species recipe (ADR-059 D3).
//!
//! A new V4 manager species needs a manifest row, not code: this test parses a
//! test-only fixture and proves the loader + manager-keyed resolution accept it
//! and that it never leaks into the shipped manifest.

#![expect(clippy::unwrap_used, clippy::expect_used)]

use degenbot_db::species::{self, Family};

const FIXTURE: &str = include_str!("fixtures/add_species_v4.toml");

#[test]
fn fixture_v4_species_flows_through_the_loader() {
    let manifest = species::parse_manifest(FIXTURE).expect("fixture must parse");
    let sushi = manifest.get("sushi_v4").expect("fixture species present");
    assert_eq!(sushi.family, Family::V4);
    let manager = "0x1111111111111111111111111111111111111111"
        .parse::<alloy::primitives::Address>()
        .unwrap();
    assert_eq!(sushi.manager_on(1), Some(manager));
    assert_eq!(
        manifest
            .manager_deployment(1, manager)
            .map(|s| s.kind.as_str()),
        Some("sushi_v4")
    );
    assert_eq!(sushi.family.pool_kind(), degenbot_pathfinding::PoolKind::V4);
}

#[test]
fn fixture_does_not_leak_into_the_shipped_manifest() {
    assert!(species::manifest().get("sushi_v4").is_none());
    assert_eq!(species::manifest().species.len(), 11);
}
