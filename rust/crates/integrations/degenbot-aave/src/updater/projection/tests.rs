//! Red-first acceptance for the forward-only Projection view + the canonical
//! delta vocabulary (ADR-105 D3, D4, D7).
#![expect(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;

use alloy::primitives::{B256, I256, U256};
use degenbot_core::canonical_head::{ChainAnchor, LogRef, TxRef};
use degenbot_db::{DegenbotDb, ScaledTokenPosition};

use super::reduce::{
    reduce_configuration, reduce_debt_reset, reduce_gho_refresh, reduce_scaled_balance,
    reduce_scaled_transfer,
};
use super::{
    CanonicalDelta, ContractRole, GenerationActivation, GenerationEntity, GenerationRef,
    PositionSubject, ProjectionError, ProjectionView,
};

const RAY: u128 = 1_000_000_000_000_000_000_000_000_000;

fn anchor(number: u64) -> ChainAnchor {
    ChainAnchor::new(
        1,
        number,
        B256::from(U256::from(number)),
        B256::repeat_byte(0xaa),
    )
    .expect("the fixture header is a complete identity")
}

fn log_ref(block: u64, tx_index: u64, log_index: u64) -> LogRef {
    let tx = TxRef::new(
        anchor(block),
        B256::from(U256::from(0x1000 + block * 16 + tx_index)),
        tx_index,
    )
    .expect("the fixture transaction names its hash");
    LogRef::new(tx, log_index)
}

fn collateral(position_id: i64) -> PositionSubject {
    PositionSubject {
        position: ScaledTokenPosition::Collateral,
        position_id,
    }
}

fn debt(position_id: i64) -> PositionSubject {
    PositionSubject {
        position: ScaledTokenPosition::Debt,
        position_id,
    }
}

fn contract_generation(revision: u32) -> GenerationRef {
    GenerationRef::new(
        GenerationEntity::Contract {
            market_id: 1,
            role: ContractRole::Pool,
        },
        revision,
    )
}

fn token_generation(asset_id: i64, position: ScaledTokenPosition, revision: u32) -> GenerationRef {
    GenerationRef::new(
        GenerationEntity::ScaledToken { asset_id, position },
        revision,
    )
}

/// A chunk-forward view seeded with a GHO debt position holding a pre-reset
/// balance, exactly where the chunk loop hands resolution its facts.
fn gho_view() -> ProjectionView {
    let mut view = ProjectionView::new();
    view.observe_generation(token_generation(2, ScaledTokenPosition::Debt, 1));
    view.seed_position(debt(7), (U256::from(1_000_000u64), Some(U256::from(RAY))));
    view
}

// ── advanced-view reads (red-first acceptance 1) ──────────────────────────

/// The bad-debt reset advances the view at the reset's own position, so a
/// later same-transaction GHO computation reads the zeroed balance and the
/// advanced index instead of the pre-reset tuple.
#[test]
fn a_bad_debt_reset_advances_the_view_for_a_later_same_transaction_operation() {
    let mut view = gho_view();
    let reset_index = U256::from(3u8) * U256::from(RAY) / U256::from(2u8);
    let reset = reduce_debt_reset(
        &view,
        7,
        reset_index,
        log_ref(100, 0, 661),
        token_generation(2, ScaledTokenPosition::Debt, 1),
    )
    .expect("the reset's position is resolved in the view");
    view.apply(&reset).expect("the reset advances the view");

    assert_eq!(
        view.require_position_state(debt(7)).unwrap(),
        (U256::ZERO, Some(reset_index)),
        "the reset zeroes the balance and advances the index in the view"
    );

    let later_index = U256::from(2u8) * U256::from(RAY);
    let borrow = reduce_scaled_balance(
        &view,
        debt(7),
        I256::try_from(500_000_000_000_000_000_000u128).unwrap(),
        later_index,
        log_ref(100, 0, 670),
        token_generation(2, ScaledTokenPosition::Debt, 1),
    )
    .expect("the later GHO borrow reduces against the advanced view");
    view.apply(&borrow).expect("the borrow advances the view");

    assert_eq!(
        view.require_position_state(debt(7)).unwrap(),
        (
            U256::from(500_000_000_000_000_000_000u128),
            Some(later_index)
        ),
        "the later operation starts from the reset's zero balance, not the pre-reset tuple"
    );
}

/// A position the view has never resolved is a refusal, never a zero default:
/// a reduction cannot invent preceding state.
#[test]
fn an_unresolved_position_is_refused_rather_than_zero_defaulted() {
    let view = ProjectionView::new();
    assert_eq!(
        reduce_scaled_balance(
            &view,
            collateral(3),
            I256::try_from(5u64).unwrap(),
            U256::from(RAY),
            log_ref(100, 0, 1),
            contract_generation(9),
        )
        .unwrap_err(),
        ProjectionError::UnresolvedPosition {
            position: ScaledTokenPosition::Collateral,
            position_id: 3,
        }
    );
    assert_eq!(
        reduce_debt_reset(
            &view,
            3,
            U256::from(RAY),
            log_ref(100, 0, 1),
            contract_generation(9),
        )
        .unwrap_err(),
        ProjectionError::UnresolvedPosition {
            position: ScaledTokenPosition::Debt,
            position_id: 3,
        }
    );
}

// ── exact positions and generations (red-first acceptance 2) ───────────────

/// Every delta states the exact transaction/log position it was emitted at,
/// never a chunk-relative index.
#[test]
fn a_delta_states_its_exact_source_position() {
    let mut view = ProjectionView::new();
    view.seed_position(collateral(4), (U256::ZERO, None));
    let source = log_ref(2_400_000, 3, 417);
    let delta = reduce_scaled_balance(
        &view,
        collateral(4),
        I256::try_from(10u64).unwrap(),
        U256::from(RAY),
        source,
        contract_generation(9),
    )
    .expect("the position is resolved");

    assert_eq!(delta.source(), source);
    assert_eq!(delta.source().index(), 417);
    assert_eq!(delta.source().tx().index(), 3);
    assert_eq!(delta.source().tx().hash(), source.tx().hash());
    assert_eq!(
        delta.source().tx().anchor().hash(),
        anchor(2_400_000).hash()
    );
}

/// A delta names the generation its reduction consumed: the token generation
/// that selected the rounding, or the pool generation that selected the
/// movement rule.
#[test]
fn a_delta_states_the_generation_it_reduced_under() {
    let mut view = ProjectionView::new();
    view.seed_position(collateral(4), (U256::ZERO, None));
    view.observe_generation(contract_generation(9));

    let delta = reduce_scaled_balance(
        &view,
        collateral(4),
        I256::try_from(10u64).unwrap(),
        U256::from(RAY),
        log_ref(100, 1, 5),
        contract_generation(9),
    )
    .expect("an observed generation matches");

    assert_eq!(delta.generation(), Some(contract_generation(9)));
    assert_eq!(delta.generation().unwrap().revision(), Some(9));
    assert_eq!(
        delta.generation().unwrap().entity(),
        GenerationEntity::Contract {
            market_id: 1,
            role: ContractRole::Pool,
        }
    );
}

/// A reduction under a generation the view has not observed, or has observed
/// differently, is a stale-generation refusal: a replacement entity's delta
/// cannot be interpreted under the generation it replaced.
#[test]
fn a_reduction_under_a_stale_generation_is_refused() {
    let mut view = ProjectionView::new();
    view.seed_position(debt(9), (U256::ZERO, None));
    view.observe_generation(token_generation(3, ScaledTokenPosition::Debt, 2));

    assert_eq!(
        reduce_scaled_balance(
            &view,
            debt(9),
            I256::try_from(1u64).unwrap(),
            U256::from(RAY),
            log_ref(100, 0, 1),
            token_generation(3, ScaledTokenPosition::Debt, 4),
        )
        .unwrap_err(),
        ProjectionError::StaleGeneration {
            entity: GenerationEntity::ScaledToken {
                asset_id: 3,
                position: ScaledTokenPosition::Debt,
            },
            reduced_under: token_generation(3, ScaledTokenPosition::Debt, 4).encode(),
            observed: token_generation(3, ScaledTokenPosition::Debt, 2).encode(),
        }
    );
}

// ── reducer purity by construction (red-first acceptance 3) ────────────────

/// The reducer module carries no transport and no storage handle: its only
/// inputs are the view and already-resolved facts. The scan is over the
/// reducer source itself, so adding a transport or storage reach to a reducer
/// fails here before it can be called.
#[test]
fn the_reducer_module_holds_no_transport_or_storage_reach() {
    let source = include_str!("reduce.rs");
    for forbidden in [
        "rusqlite",
        "Connection",
        "DegenbotDb",
        "Provider",
        "execute(",
        "prepare(",
        "std::fs",
        "reqwest",
        "alloy::providers",
    ] {
        assert!(
            !source.contains(forbidden),
            "reduce.rs must not reach {forbidden:?}: reducers receive resolved facts only"
        );
    }
    assert!(
        !source.contains("conn"),
        "no connection handle (in any spelling) reaches a reducer"
    );
}

// ── replacement generations (red-first acceptance: ADR-105 D6) ──────────

/// A same-transaction discount-token replacement activates a NEW generation at
/// its own event: the view reads the seeded generation before the event and the
/// activated one after it, and two replacements in one transaction produce two
/// distinct generations rather than a mutated single one.
#[test]
fn a_same_transaction_discount_token_replacement_exposes_each_generation() {
    use super::ConfigurationEffect;

    let chain_id = 1;
    let entity = GenerationEntity::DiscountToken { chain_id };
    let seeded = GenerationRef::seed(entity);
    let mut view = ProjectionView::new();
    view.observe_generation(seeded);
    assert_eq!(view.generation(entity), Some(seeded));

    let first_replacement = log_ref(100, 1, 410);
    let first_effect = ConfigurationEffect::GhoDiscountTokenUpdated {
        gho_token_id: 1,
        new_discount_token: Some("0x1111111111111111111111111111111111111111".to_string()),
    };
    let first_generation = first_effect
        .generation(chain_id, first_replacement)
        .expect("the discount-token replacement is a construction-valid effect")
        .expect("a replacement activates a discount-token generation");
    let delta = reduce_configuration(
        &view,
        first_effect,
        first_replacement,
        Some(first_generation),
    )
    .expect("the replacement is the activation of the generation it names");
    assert_eq!(
        first_generation.activation(),
        GenerationActivation::Event(first_replacement)
    );
    view.apply(&delta)
        .expect("the activation advances the view");
    assert_eq!(
        view.generation(entity),
        Some(first_generation),
        "the view now reads the generation the event activated"
    );

    let second_replacement = log_ref(100, 1, 420);
    let second_effect = ConfigurationEffect::GhoDiscountTokenUpdated {
        gho_token_id: 1,
        new_discount_token: Some("0x2222222222222222222222222222222222222222".to_string()),
    };
    let second_generation = second_effect
        .generation(chain_id, second_replacement)
        .expect("the second discount-token replacement is construction-valid")
        .expect("the second replacement activates its own generation");
    assert_ne!(
        second_generation, first_generation,
        "a second replacement is a new generation, never the first one mutated"
    );
    let delta = reduce_configuration(
        &view,
        second_effect,
        second_replacement,
        Some(second_generation),
    )
    .expect("the second activation reduces against the generation it replaces");
    view.apply(&delta)
        .expect("the second activation advances the view");
    assert_eq!(view.generation(entity), Some(second_generation));
}

/// An endpoint rotation activates the role's contract generation at its own
/// event, and the generation before the event is the seeded one — the fetch
/// endpoint a chunk resolves for its next read is the generation the rotation
/// installed, never a mutated address under an unchanged generation.
#[test]
fn an_endpoint_rotation_exposes_the_contract_generation_at_its_event() {
    use super::{ConfigurationEffect, ContractRole};

    let market_id = 1;
    let entity = GenerationEntity::Contract {
        market_id,
        role: ContractRole::Pool,
    };
    let seeded = GenerationRef::new(entity, 2);
    let mut view = ProjectionView::new();
    view.observe_generation(seeded);

    let rotation = log_ref(100, 2, 71);
    let effect = ConfigurationEffect::ContractAddressChanged {
        market_id,
        name: "POOL".to_string(),
        old_address: "0x87870bca3f3fd6335c3f4ce8392d69350b4fa4e2".to_string(),
        new_address: "0x1111111111111111111111111111111111111111".to_string(),
    };
    let generation = effect
        .generation(1, rotation)
        .expect("the rotation is a construction-valid effect")
        .expect("a tracked role rotation activates a contract generation");
    assert_eq!(generation.entity(), entity);
    let delta = reduce_configuration(&view, effect, rotation, Some(generation))
        .expect("the rotation is the activation of the generation it names");
    view.apply(&delta).expect("the rotation advances the view");
    assert_eq!(view.generation(entity), Some(generation));

    // A component outside the tracked roles activates nothing: an untracked
    // `AddressSet` name is deliberately out of scope, not a role generation.
    assert_eq!(
        ConfigurationEffect::ContractAddressChanged {
            market_id,
            name: "SOME_OTHER_COMPONENT".to_string(),
            old_address: "0x2222222222222222222222222222222222222222".to_string(),
            new_address: "0x3333333333333333333333333333333333333333".to_string(),
        }
        .generation(1, log_ref(100, 2, 80))
        .expect("an untracked role is not a revision refusal"),
        None
    );
}

/// An upgrade activates the scaled token's new revision generation. A delta
/// reduced under the replaced generation is refused after the activation, and a
/// delta reduced under the new generation is refused BEFORE it — the generation
/// a delta names must be the one the view reads at that position.
#[test]
fn an_upgrade_activates_the_token_generation_and_refuses_the_replaced_one() {
    use super::ConfigurationEffect;

    let asset_id = 2;
    let position = ScaledTokenPosition::Debt;
    let entity = GenerationEntity::ScaledToken { asset_id, position };
    let mut view = ProjectionView::new();
    view.seed_position(debt(7), (U256::ZERO, None));
    view.observe_generation(token_generation(asset_id, position, 3));

    let upgrade = log_ref(100, 3, 55);
    let effect = ConfigurationEffect::Upgraded {
        asset_id,
        market_id: 1,
        is_a_token: false,
        new_revision: 4,
        deprecated_gho_token_id: None,
    };
    let activated = effect
        .generation(1, upgrade)
        .expect("the upgrade is a construction-valid effect")
        .expect("an upgrade activates the token revision generation");
    assert_eq!(activated.revision(), Some(4));
    assert_eq!(activated.activation(), GenerationActivation::Event(upgrade));

    // Before the upgrade lands, a delta reduced under the NEXT generation is
    // refused: the view still reads revision 3.
    let upgraded_generation = token_generation(asset_id, position, 4);
    assert_eq!(
        reduce_scaled_balance(
            &view,
            debt(7),
            I256::try_from(1u64).unwrap(),
            U256::from(RAY),
            log_ref(100, 3, 40),
            upgraded_generation,
        )
        .unwrap_err(),
        ProjectionError::StaleGeneration {
            entity,
            reduced_under: upgraded_generation.encode(),
            observed: token_generation(asset_id, position, 3).encode(),
        }
    );

    let delta = reduce_configuration(&view, effect, upgrade, Some(activated))
        .expect("the upgrade is the activation of the generation it names");
    view.apply(&delta).expect("the upgrade advances the view");

    // After the upgrade, the replaced generation is refused and the activated
    // one is what the later operation reduces under.
    assert_eq!(
        reduce_scaled_balance(
            &view,
            debt(7),
            I256::try_from(1u64).unwrap(),
            U256::from(RAY),
            log_ref(100, 3, 60),
            token_generation(asset_id, position, 3),
        )
        .unwrap_err(),
        ProjectionError::StaleGeneration {
            entity,
            reduced_under: token_generation(asset_id, position, 3).encode(),
            observed: activated.encode(),
        }
    );
    let later = reduce_scaled_balance(
        &view,
        debt(7),
        I256::try_from(1u64).unwrap(),
        U256::from(RAY),
        log_ref(100, 3, 60),
        activated,
    )
    .expect("the operation after the upgrade reduces under its generation");
    view.apply(&later)
        .expect("the later operation advances the view");
}

// ── operation-boundary ordering (red-first: the operation stream advances) ──

/// The operation stream is forward-only even though one operation's legs may
/// sit at earlier logs than its anchor: a later-started operation anchored
/// behind the previous operation's anchor is refused, while the interior
/// deltas of the operation being reduced are accepted.
#[test]
fn a_later_operation_anchored_behind_the_previous_one_is_refused() {
    let mut view = ProjectionView::new();
    view.seed_position(collateral(1), (U256::ZERO, None));

    let first_anchor = log_ref(100, 1, 410);
    view.begin_operation(first_anchor)
        .expect("the first operation opens the stream");
    assert_eq!(view.operation_anchor(), Some(first_anchor));

    // The operation's own legs may precede its anchor: an interior delta is
    // part of the reduction unit, not a position claim.
    let leg = reduce_scaled_balance(
        &view,
        collateral(1),
        I256::try_from(1u64).unwrap(),
        U256::from(RAY),
        log_ref(100, 1, 405),
        contract_generation(9),
    )
    .unwrap();
    view.apply(&leg)
        .expect("an interior leg position belongs to the open operation");

    // A later-started operation may not anchor behind the previous one.
    assert_eq!(
        view.begin_operation(log_ref(100, 1, 400)).unwrap_err(),
        ProjectionError::OperationOutOfOrder {
            held: Box::new(first_anchor),
            arrived: Box::new(log_ref(100, 1, 400)),
        }
    );
    assert_eq!(
        view.operation_anchor(),
        Some(first_anchor),
        "a refused anchor leaves the operation stream where it was"
    );

    // A forward anchor continues the stream.
    let second_anchor = log_ref(100, 1, 430);
    view.begin_operation(second_anchor)
        .expect("a forward anchor continues the operation stream");
    assert_eq!(view.operation_anchor(), Some(second_anchor));
}

/// An operation in an earlier transaction than the view has advanced to is a
/// replay: refused before its anchor can open a reduction unit.
#[test]
fn an_operation_in_an_earlier_transaction_is_not_forward() {
    let mut view = ProjectionView::new();
    view.seed_position(collateral(1), (U256::ZERO, None));
    let first = reduce_scaled_balance(
        &view,
        collateral(1),
        I256::try_from(1u64).unwrap(),
        U256::from(RAY),
        log_ref(100, 1, 410),
        contract_generation(9),
    )
    .unwrap();
    view.apply(&first).expect("the first delta sets the cursor");

    assert!(matches!(
        view.begin_operation(log_ref(100, 0, 9)).unwrap_err(),
        ProjectionError::NotForward { .. }
    ));
}

// ── generation revisions are refused, never synthesized (ADR-105 D6/D11) ───

/// A revision the generation vocabulary cannot hold is a typed refusal: a
/// negative revision and one wider than `u32` never become revision zero.
#[test]
fn an_unrepresentable_revision_is_refused_rather_than_synthesized() {
    use super::ConfigurationEffect;

    let source = log_ref(100, 1, 55);
    assert_eq!(
        ConfigurationEffect::Upgraded {
            asset_id: 2,
            market_id: 1,
            is_a_token: false,
            new_revision: -1,
            deprecated_gho_token_id: None,
        }
        .generation(1, source)
        .unwrap_err(),
        ProjectionError::InvalidGenerationRevision { revision: -1 }
    );
    let overflow = i64::from(u32::MAX) + 1;
    assert_eq!(
        ConfigurationEffect::Upgraded {
            asset_id: 2,
            market_id: 1,
            is_a_token: true,
            new_revision: overflow,
            deprecated_gho_token_id: None,
        }
        .generation(1, source)
        .unwrap_err(),
        ProjectionError::InvalidGenerationRevision { revision: overflow }
    );
    assert_eq!(
        ConfigurationEffect::ContractRevisionUpdated {
            market_id: 1,
            contract_name: "POOL".to_string(),
            new_revision: -7,
        }
        .generation(1, source)
        .unwrap_err(),
        ProjectionError::InvalidGenerationRevision { revision: -7 }
    );
    assert_eq!(
        ConfigurationEffect::ContractInserted {
            market_id: 1,
            name: "POOL".to_string(),
            address: "0x1111111111111111111111111111111111111111".to_string(),
            revision: Some(-2),
        }
        .generation(1, source)
        .unwrap_err(),
        ProjectionError::InvalidGenerationRevision { revision: -2 }
    );
}

// ── forward-only advancement (red-first acceptance: D3/D4) ─────────────────

#[test]
fn the_view_advances_only_forward() {
    let mut view = ProjectionView::new();
    view.seed_position(collateral(1), (U256::ZERO, None));
    let first = reduce_scaled_balance(
        &view,
        collateral(1),
        I256::try_from(1u64).unwrap(),
        U256::from(RAY),
        log_ref(100, 1, 5),
        contract_generation(9),
    )
    .unwrap();
    view.apply(&first).expect("the first delta sets the cursor");
    assert_eq!(view.cursor(), Some(log_ref(100, 1, 5)));

    // One transaction is one reduction unit: an earlier log of the SAME
    // transaction belongs to the operation stream being reduced, not to a
    // replay.
    let interior = reduce_scaled_balance(
        &view,
        collateral(1),
        I256::try_from(1u64).unwrap(),
        U256::from(RAY),
        log_ref(100, 1, 4),
        contract_generation(9),
    )
    .unwrap();
    view.apply(&interior)
        .expect("an interior position of the reduced transaction is accepted");

    // An earlier TRANSACTION is a replay: refused, and the view untouched.
    let behind = reduce_scaled_balance(
        &view,
        collateral(1),
        I256::try_from(1u64).unwrap(),
        U256::from(RAY),
        log_ref(100, 0, 9),
        contract_generation(9),
    )
    .unwrap();
    assert_eq!(
        view.apply(&behind).unwrap_err(),
        ProjectionError::NotForward {
            held: Box::new(log_ref(100, 1, 4)),
            arrived: Box::new(log_ref(100, 0, 9)),
        }
    );
    assert_eq!(
        view.require_position_state(collateral(1)).unwrap(),
        (U256::from(2u64), Some(U256::from(RAY))),
        "a refused delta leaves the view untouched"
    );
}

#[test]
fn a_same_height_other_branch_is_not_forward() {
    let mut view = ProjectionView::new();
    view.seed_position(collateral(1), (U256::ZERO, None));
    let first = reduce_scaled_balance(
        &view,
        collateral(1),
        I256::try_from(1u64).unwrap(),
        U256::from(RAY),
        log_ref(100, 0, 5),
        contract_generation(9),
    )
    .unwrap();
    view.apply(&first).unwrap();

    let other_branch =
        ChainAnchor::new(1, 100, B256::repeat_byte(0xcd), B256::repeat_byte(0xaa)).unwrap();
    let foreign_tx = TxRef::new(other_branch, B256::from(U256::from(0xbeef_u64)), 0).unwrap();
    let foreign = reduce_scaled_balance(
        &view,
        collateral(1),
        I256::try_from(1u64).unwrap(),
        U256::from(RAY),
        LogRef::new(foreign_tx, 6),
        contract_generation(9),
    )
    .unwrap();

    assert!(matches!(
        view.apply(&foreign).unwrap_err(),
        ProjectionError::NotForward { .. }
    ));
    assert_eq!(view.cursor(), Some(log_ref(100, 0, 5)));
}

/// One log position may state more than one delta: a movement and the
/// discount-refresh signal it raises are both emitted by the same event. The
/// cursor is a position the view has advanced to, so an equal position is
/// forward while anything behind it is refused.
#[test]
fn one_log_position_may_state_a_movement_and_its_signal() {
    let mut view = gho_view();
    let movement = reduce_scaled_balance(
        &view,
        debt(7),
        -I256::try_from(3u64).unwrap(),
        U256::from(RAY),
        log_ref(100, 0, 21),
        token_generation(2, ScaledTokenPosition::Debt, 1),
    )
    .unwrap();
    view.apply(&movement).unwrap();

    let signal = reduce_gho_refresh(
        &view,
        7,
        log_ref(100, 0, 21),
        token_generation(2, ScaledTokenPosition::Debt, 1),
    )
    .expect("the refresh signal shares its movement's position");
    view.apply(&signal)
        .expect("a signal at an already-advanced position is forward");
    assert_eq!(view.cursor(), Some(log_ref(100, 0, 21)));
}

// ── refusals: chain identity, oversized amounts, overflow ─────────────────

/// A delta is forward only within the view's own chain. Two synthetic branches
/// can share a height, a block hash, a parent hash and a log index on different
/// chains; a delta from the other chain states a different chain's state and is
/// refused even though every local ordering fact says "ahead".
#[test]
fn a_cross_chain_delta_is_not_forward() {
    let mut view = ProjectionView::new();
    view.seed_position(collateral(1), (U256::ZERO, None));
    let held = log_ref(100, 0, 5);
    let first = reduce_scaled_balance(
        &view,
        collateral(1),
        I256::try_from(1u64).unwrap(),
        U256::from(RAY),
        held,
        contract_generation(9),
    )
    .unwrap();
    view.apply(&first).expect("the first delta sets the cursor");

    let local = anchor(100);
    let foreign_anchor = ChainAnchor::new(2, local.number(), local.hash(), local.parent_hash())
        .expect("the foreign branch states a complete identity");
    let foreign_tx = TxRef::new(foreign_anchor, held.tx().hash(), held.tx().index())
        .expect("the foreign transaction names its hash");
    let foreign = reduce_scaled_balance(
        &view,
        collateral(1),
        I256::try_from(1u64).unwrap(),
        U256::from(RAY),
        LogRef::new(foreign_tx, held.index() + 1),
        contract_generation(9),
    )
    .unwrap();

    assert!(matches!(
        view.apply(&foreign).unwrap_err(),
        ProjectionError::NotForward { .. }
    ));
    assert_eq!(view.cursor(), Some(held));
    assert_eq!(
        view.require_position_state(collateral(1)).unwrap(),
        (U256::from(1u64), Some(U256::from(RAY))),
        "a cross-chain refusal leaves the view untouched"
    );
}

/// A transfer whose amount exceeds the signed balance domain is refused at the
/// sender, not saturated into a negative delta that then overflows on negation.
#[test]
fn a_transfer_above_the_signed_domain_is_refused_at_the_sender() {
    let mut view = ProjectionView::new();
    view.seed_position(collateral(1), (U256::from(10u64), None));
    view.seed_position(collateral(2), (U256::ZERO, None));
    let delta = reduce_scaled_transfer(
        &view,
        collateral(1),
        Some(collateral(2)),
        U256::MAX,
        U256::from(RAY),
        log_ref(100, 0, 7),
        contract_generation(9),
    )
    .expect("the reduction states the amount the evidence supplied");

    assert_eq!(
        view.apply(&delta).unwrap_err(),
        ProjectionError::BalanceOverflow { position_id: 1 }
    );
    assert_eq!(
        view.cursor(),
        None,
        "a refused delta does not move the cursor"
    );
    assert_eq!(
        view.require_position_state(collateral(1)).unwrap(),
        (U256::from(10u64), None)
    );
}

/// A movement whose sum leaves the signed domain is a refusal: the view never
/// saturates a balance into `I256::MAX` and never overflows in unchecked
/// arithmetic.
#[test]
fn a_movement_past_the_signed_domain_is_refused_not_saturated() {
    let mut view = ProjectionView::new();
    let near_max: U256 = I256::MAX.into_raw();
    view.seed_position(collateral(1), (near_max, None));
    let delta = reduce_scaled_balance(
        &view,
        collateral(1),
        I256::try_from(1u64).unwrap(),
        U256::from(RAY),
        log_ref(100, 0, 8),
        contract_generation(9),
    )
    .unwrap();
    assert_eq!(
        view.apply(&delta).unwrap_err(),
        ProjectionError::BalanceOverflow { position_id: 1 }
    );
    assert_eq!(view.cursor(), None);
}

/// A preceding balance the signed domain cannot represent is a refusal, not a
/// silent clamp to `I256::MAX`: the view states it cannot reduce this position.
#[test]
fn an_oversized_current_balance_is_refused_not_clamped() {
    let mut view = ProjectionView::new();
    let oversized: U256 = I256::MAX.into_raw() + U256::from(1u8);
    view.seed_position(collateral(1), (oversized, None));
    let delta = reduce_scaled_balance(
        &view,
        collateral(1),
        I256::ZERO,
        U256::from(RAY),
        log_ref(100, 0, 9),
        contract_generation(9),
    )
    .unwrap();
    assert_eq!(
        view.apply(&delta).unwrap_err(),
        ProjectionError::BalanceOverflow { position_id: 1 }
    );
    assert_eq!(view.cursor(), None);
}

// ── chunk-boundary independence (red-first acceptance 5) ───────────────────

/// One operation sequence reduced under two chunk splittings produces
/// byte-identical canonical deltas: a delta's identity is the chain position
/// it names, never the chunk that carried it.
#[test]
fn supporting_deltas_encode_identically_across_chunk_boundaries() {
    let operations: Vec<(&str, u64, u64, u64)> = vec![
        ("mint", 100, 0, 4),
        ("mint", 100, 0, 9),
        ("transfer", 101, 0, 12),
        ("mint", 101, 1, 20),
    ];

    let reduce_all = |split: fn(u64) -> bool| -> Vec<CanonicalDelta> {
        let mut committed: HashMap<PositionSubject, (U256, Option<U256>)> = HashMap::new();
        committed.insert(collateral(1), (U256::ZERO, None));
        committed.insert(collateral(2), (U256::ZERO, None));
        let mut deltas: Vec<CanonicalDelta> = Vec::new();
        let mut chunk: Vec<&(&str, u64, u64, u64)> = Vec::new();
        let flush = |chunk: &mut Vec<&(&str, u64, u64, u64)>,
                     committed: &mut HashMap<PositionSubject, (U256, Option<U256>)>,
                     deltas: &mut Vec<CanonicalDelta>| {
            let mut view = ProjectionView::new();
            for (subject, state) in committed.iter() {
                view.seed_position(*subject, *state);
            }
            for (kind, block, tx_index, log_index) in chunk.drain(..) {
                let delta = match *kind {
                    "mint" => reduce_scaled_balance(
                        &view,
                        collateral(1),
                        I256::try_from(10u64).unwrap(),
                        U256::from(RAY),
                        log_ref(*block, *tx_index, *log_index),
                        contract_generation(9),
                    ),
                    _ => reduce_scaled_transfer(
                        &view,
                        collateral(1),
                        Some(collateral(2)),
                        U256::from(3u64),
                        U256::from(RAY),
                        log_ref(*block, *tx_index, *log_index),
                        contract_generation(9),
                    ),
                }
                .unwrap();
                view.apply(&delta).unwrap();
                deltas.push(delta);
            }
            for subject in [collateral(1), collateral(2)] {
                committed.insert(subject, view.require_position_state(subject).unwrap());
            }
        };
        for operation in &operations {
            if split(operation.1) && !chunk.is_empty() {
                flush(&mut chunk, &mut committed, &mut deltas);
            }
            chunk.push(operation);
        }
        flush(&mut chunk, &mut committed, &mut deltas);
        deltas
    };

    let one_chunk = reduce_all(|_| false);
    let two_chunks = reduce_all(|block| block == 101);
    assert_ne!(one_chunk.len(), 0);
    assert_eq!(
        one_chunk
            .iter()
            .map(|delta| delta.effect().clone())
            .collect::<Vec<_>>(),
        two_chunks
            .iter()
            .map(|delta| delta.effect().clone())
            .collect::<Vec<_>>()
    );

    let encode = |deltas: &[CanonicalDelta]| {
        deltas
            .iter()
            .map(CanonicalDelta::encode)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        encode(&one_chunk),
        encode(&two_chunks),
        "a chunk boundary is not part of a delta's identity"
    );
    assert!(
        encode(&one_chunk).contains("/100/"),
        "positions are absolute"
    );
}

// ── view advancement agrees with the persistence math ─────────────────────

/// The view's advance is the same state the storage layer writes: the
/// burn cap to the scaled balance and the max-with-prev index reconciliation
/// agree for a clamped overshoot and an out-of-order index.
#[test]
#[expect(clippy::unwrap_used, clippy::too_many_lines)]
fn the_view_advance_matches_the_persisted_state() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
             VALUES (1, 1, 'mainnet', 1, NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_users \
                (id, market_id, address, e_mode, gho_discount, stk_aave_balance, \
                 isolation_mode_collateral_asset_id, isolation_mode_debt) \
             VALUES (1, 1, '0xuser', 0, 0, NULL, NULL, '0')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES (1, 1, '0xunder'), (2, 1, '0xa'), (3, 1, '0xv')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_assets \
                (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                 v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                 borrow_index, borrow_rate) \
             VALUES (1, 1, 1, 2, 1, 3, 1, '0', '0', '0', '0')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_collateral_positions (id, user_id, asset_id, balance, last_index) \
             VALUES (1, 1, 1, '100', '1000')",
            [],
        )
        .unwrap();
    }

    let cases: Vec<(I256, U256, (U256, Option<U256>))> = vec![
        (
            I256::try_from(10u64).unwrap(),
            U256::from(RAY),
            (U256::from(110u64), Some(U256::from(RAY))),
        ),
        (
            -I256::try_from(1_000u64).unwrap(),
            U256::from(RAY),
            (U256::ZERO, Some(U256::from(RAY))),
        ),
        (
            I256::try_from(5u64).unwrap(),
            U256::from(10u64),
            (U256::from(105u64), Some(U256::from(1_000u64))),
        ),
    ];

    for (delta, index, expected) in cases {
        let conn = db.lock();
        conn.execute(
            "UPDATE aave_v3_collateral_positions SET balance = '100', last_index = '1000' WHERE id = 1",
            [],
        )
        .unwrap();
        DegenbotDb::apply_scaled_token_balance_delta_with_current_on_conn(
            &conn,
            ScaledTokenPosition::Collateral,
            1,
            U256::from(100u64),
            Some(U256::from(1_000u64)),
            delta,
            index,
        )
        .unwrap();
        let persisted: (String, Option<String>) = conn
            .query_row(
                "SELECT balance, last_index FROM aave_v3_collateral_positions WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let persisted = (
            persisted.0.parse::<U256>().unwrap(),
            persisted.1.map(|i| i.parse::<U256>().unwrap()),
        );

        let mut view = ProjectionView::new();
        view.seed_position(
            collateral(1),
            (U256::from(100u64), Some(U256::from(1_000u64))),
        );
        let reduced = reduce_scaled_balance(
            &view,
            collateral(1),
            delta,
            index,
            log_ref(100, 0, 1),
            contract_generation(9),
        )
        .unwrap();
        view.apply(&reduced).unwrap();

        assert_eq!(
            view.require_position_state(collateral(1)).unwrap(),
            expected
        );
        assert_eq!(
            persisted, expected,
            "the view advance and the storage write must agree (delta {delta}, index {index})"
        );
    }
}

// ── the GHO refresh signal carries its position ───────────────────────────

#[test]
fn a_gho_refresh_signal_carries_the_debt_position_it_refreshes() {
    let mut view = gho_view();
    let refresh = reduce_gho_refresh(
        &view,
        7,
        log_ref(100, 0, 681),
        token_generation(2, ScaledTokenPosition::Debt, 1),
    )
    .expect("the refresh's position is resolved");
    view.apply(&refresh)
        .expect("the signal advances the cursor");
    assert_eq!(view.cursor(), Some(log_ref(100, 0, 681)));
    assert_eq!(
        view.require_position_state(debt(7)).unwrap(),
        (U256::from(1_000_000u64), Some(U256::from(RAY))),
        "a refresh signal changes no position state"
    );
}

// ── the generation census ─────────────────────────────────────────────────

#[test]
fn the_view_records_the_generations_resolution_observed() {
    let mut view = ProjectionView::new();
    assert_eq!(
        view.generation(GenerationEntity::Contract {
            market_id: 1,
            role: ContractRole::Pool,
        }),
        None
    );
    view.observe_generation(contract_generation(9));
    view.observe_generation(contract_generation(10));
    assert_eq!(
        view.generation(GenerationEntity::Contract {
            market_id: 1,
            role: ContractRole::Pool,
        }),
        Some(contract_generation(10)),
        "a later observation replaces the generation the view is reading under"
    );

    let mut census: HashMap<GenerationEntity, GenerationRef> = HashMap::new();
    for (entity, generation) in view.generations() {
        census.insert(entity, generation);
    }
    assert_eq!(census.len(), 1);
}
