//! ADR-061 D3 declared==wired pin: every plane strategy's kit-cell
//! declaration must match what the kit actually composes, and the three
//! deliberately-absent cells must stay absent.
//!
//! The declaration lives in `degenbot_strategy::strategy_kit::STRATEGY_CELLS`;
//! this test is the mechanical enforcement. A flipped declaration must fail
//! one of these asserts.

#![expect(clippy::expect_used)]

use std::sync::Arc;

use degenbot_bot::bot_core::pool_ingress::VerifyLevel;
use degenbot_bot::bot_core::RouteRegistry;
use degenbot_bot::connector_index::V2ConnectorIndex;
use degenbot_strategy::strategy_kit::{declared_cells, StrategyCell, StrategyKit, STRATEGY_CELLS};
use degenbot_strategy::StrategyName;

fn registry() -> Arc<RouteRegistry> {
    Arc::new(RouteRegistry::new(V2ConnectorIndex::default()))
}

#[test]
fn every_plane_strategy_has_exactly_one_declaration_row() {
    for name in StrategyName::ALL {
        let rows: Vec<_> = STRATEGY_CELLS.iter().filter(|(n, _)| *n == name).collect();
        assert_eq!(rows.len(), 1, "{} must have exactly one row", name.as_str());

        let declared = rows[0].1;
        assert_eq!(declared_cells(name), Some(declared));

        let mut seen = Vec::new();
        for cell in declared {
            assert!(
                !seen.contains(cell),
                "{}: duplicate {} cell",
                name.as_str(),
                cell.label()
            );
            seen.push(*cell);
        }
    }
}

#[test]
fn declared_rows_match_the_kit_composition() {
    // The kit's struct shape is the composition it can carry; a new shared
    // capability edits the struct and the backrun rows in one diff.
    assert_eq!(
        StrategyKit::COMPOSED_CELLS,
        [StrategyCell::Provision, StrategyCell::Discovery]
    );
    for name in [StrategyName::MevblockerBackrun, StrategyName::PeerBackrun] {
        assert_eq!(
            declared_cells(name).expect("row"),
            &StrategyKit::COMPOSED_CELLS[..],
            "{} must declare exactly the kit's composition",
            name.as_str()
        );
    }
    // Settlement is the pump self-driving special case (the settlement
    // ledger): it resolves no kit.
    assert!(
        declared_cells(StrategyName::Settlement)
            .expect("row")
            .is_empty(),
        "settlement composes no kit cell"
    );
}

#[test]
fn absent_cells_stay_absent_everywhere() {
    // Simulate/submit/react have no boot-resolved per-strategy value; a row
    // declaring one would be the hypothetical-seam failure the design rule
    // forbids.
    for name in StrategyName::ALL {
        for cell in declared_cells(name).expect("row") {
            assert!(
                matches!(cell, StrategyCell::Provision | StrategyCell::Discovery),
                "{} declares {} which the kit cannot carry",
                name.as_str(),
                cell.label()
            );
        }
    }
    assert_eq!(StrategyCell::ALL.len(), 5, "the cell vocabulary is closed");
}

#[test]
fn resolved_kit_cells_track_the_wired_handles() {
    let db_only = StrategyKit::resolve(None, None, None, VerifyLevel::default(), None, None, 5_000);
    assert_eq!(db_only.cells(), vec![StrategyCell::Provision]);
    assert!(db_only.dfs().is_none());
    assert!(db_only.registry().is_none());

    let discovered = StrategyKit::resolve(
        Some(registry()),
        None,
        None,
        VerifyLevel::default(),
        None,
        None,
        5_000,
    );
    assert_eq!(
        discovered.cells(),
        vec![StrategyCell::Provision, StrategyCell::Discovery]
    );
    assert!(discovered.dfs().is_some());
    assert!(discovered.registry().is_some());
}
