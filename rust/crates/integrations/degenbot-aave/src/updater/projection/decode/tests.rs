//! Red-first acceptance for the delta vocabulary's decoder: every effect the
//! journal persists round-trips from its row, and a row that disagrees with its
//! encoding is refused rather than reconstructed (ADR-105 D7, D9).
use alloy::primitives::{B256, I256, U256};
use degenbot_core::canonical_head::{ChainAnchor, LogRef, TxRef};

use super::JournaledDelta;
use crate::projection::{
    journal_entry, CanonicalDelta, ConfigurationEffect, ContractRole, DeltaEffect,
    GenerationActivation, GenerationEntity, GenerationRef, PositionSubject, ProjectionError,
    DELTA_ENCODING_VERSION,
};
use degenbot_db::ScaledTokenPosition;

fn source(log_index: u64) -> LogRef {
    let anchor = ChainAnchor::new(
        1,
        26_130_441,
        B256::repeat_byte(0xaa),
        B256::repeat_byte(0x99),
    )
    .unwrap();
    let tx = TxRef::new(anchor, B256::repeat_byte(0xbb), 3).unwrap();
    LogRef::new(tx, log_index)
}

fn collateral(position_id: i64) -> PositionSubject {
    PositionSubject {
        position: ScaledTokenPosition::Collateral,
        position_id,
    }
}

fn effects() -> Vec<DeltaEffect> {
    vec![
        DeltaEffect::ScaledBalance {
            subject: collateral(1),
            balance_delta: I256::try_from(-500_i64).unwrap(),
            index: U256::from(7_u64),
        },
        DeltaEffect::ScaledTransfer {
            from: collateral(1),
            to: Some(collateral(2)),
            scaled_amount: U256::from(11_u64),
            index: U256::from(8_u64),
        },
        DeltaEffect::ScaledTransfer {
            from: collateral(1),
            to: None,
            scaled_amount: U256::from(12_u64),
            index: U256::from(9_u64),
        },
        DeltaEffect::DebtPositionReset {
            position_id: 3,
            index: U256::from(10_u64),
        },
        DeltaEffect::GhoRefreshDiscount { position_id: 4 },
        DeltaEffect::StkAaveBalanceObserved {
            user_id: 5,
            balance: U256::from(13_u64),
        },
        DeltaEffect::GhoDiscountRefreshed {
            user_id: 6,
            gho_discount: 14,
        },
        DeltaEffect::Configuration(ConfigurationEffect::CollateralConfigurationChanged {
            asset_id: 7,
            config_bitmap: U256::from(15_u64),
        }),
        DeltaEffect::Configuration(ConfigurationEffect::EModeCategoryAdded {
            market_id: 1,
            category_id: 8,
            ltv: 900,
            liquidation_threshold: 950,
            liquidation_bonus: 1_050,
            price_source: Some("0xoracle".to_string()),
            label: "stable, correlated".to_string(),
        }),
        DeltaEffect::Configuration(ConfigurationEffect::EModeCategoryAdded {
            market_id: 1,
            category_id: 9,
            ltv: 1,
            liquidation_threshold: 2,
            liquidation_bonus: 3,
            price_source: None,
            label: String::new(),
        }),
        DeltaEffect::Configuration(ConfigurationEffect::EModeAssetCategoryChanged {
            asset_id: 10,
            new_category_id: 2,
        }),
        DeltaEffect::Configuration(ConfigurationEffect::AssetCollateralInEModeChanged {
            asset_id: 11,
            category_id: 3,
            is_collateral: true,
        }),
        DeltaEffect::Configuration(ConfigurationEffect::ReserveUsedAsCollateral {
            user_id: 12,
            asset_id: 13,
            enabled: false,
        }),
        DeltaEffect::Configuration(ConfigurationEffect::UserEModeSet {
            user_id: 14,
            e_mode: 2,
        }),
        DeltaEffect::Configuration(ConfigurationEffect::PriceOracleUpdated {
            market_id: 1,
            new_oracle_address: "0xoracle2".to_string(),
        }),
        DeltaEffect::Configuration(ConfigurationEffect::AssetSourceUpdated {
            asset_id: 15,
            source_address: "0xsource".to_string(),
        }),
        DeltaEffect::Configuration(ConfigurationEffect::ReserveDataUpdated {
            asset_id: 16,
            liquidity_rate: U256::from(17_u64),
            variable_borrow_rate: U256::from(18_u64),
            liquidity_index: U256::from(19_u64),
            variable_borrow_index: U256::from(20_u64),
            block_number: 26_130_441,
        }),
        DeltaEffect::Configuration(ConfigurationEffect::ReserveInitialized {
            market_id: 1,
            underlying_asset_id: 21,
            a_token_id: 22,
            a_token_revision: 4,
            v_token_id: 23,
            v_token_revision: 5,
            price_source: Some("0xsource".to_string()),
            gho_link_token_id: Some(24),
        }),
        DeltaEffect::Configuration(ConfigurationEffect::GhoDiscountPercentUpdated {
            user_id: 25,
            new_discount_percent: 30,
        }),
        DeltaEffect::Configuration(ConfigurationEffect::GhoDiscountRateStrategyUpdated {
            gho_token_id: 26,
            new_strategy: Some("0xstrategy".to_string()),
        }),
        DeltaEffect::Configuration(ConfigurationEffect::GhoDiscountTokenUpdated {
            gho_token_id: 27,
            new_discount_token: None,
        }),
        DeltaEffect::Configuration(ConfigurationEffect::StkAaveTransfer {
            from_user_id: Some(28),
            to_user_id: None,
            amount: U256::from(29_u64),
        }),
        DeltaEffect::Configuration(ConfigurationEffect::RewardsClaimed {
            user_id: 30,
            reward_token_id: 31,
            claimer_id: 32,
            claimed_amount: U256::from(33_u64),
        }),
        DeltaEffect::Configuration(ConfigurationEffect::Upgraded {
            asset_id: 34,
            market_id: 1,
            is_a_token: false,
            new_revision: 4,
            deprecated_gho_token_id: Some(35),
        }),
        DeltaEffect::Configuration(ConfigurationEffect::ContractRevisionUpdated {
            market_id: 1,
            contract_name: "POOL".to_string(),
            new_revision: 6,
        }),
        DeltaEffect::Configuration(ConfigurationEffect::PoolDataProviderUpdated {
            market_id: 1,
            old_address: None,
            new_address: "0xdata".to_string(),
        }),
        DeltaEffect::Configuration(ConfigurationEffect::ContractInserted {
            market_id: 1,
            name: "POOL_CONFIGURATOR".to_string(),
            address: "0xconfigurator".to_string(),
            revision: Some(2),
        }),
        DeltaEffect::Configuration(ConfigurationEffect::ContractAddressChanged {
            market_id: 1,
            name: "PRICE_ORACLE".to_string(),
            old_address: "0xold".to_string(),
            new_address: "0xnew".to_string(),
        }),
    ]
}

fn journaled<'a>(
    source: LogRef,
    generation: Option<degenbot_db::GenerationRecord>,
    encoding: &'a str,
    version: &'a str,
) -> JournaledDelta<'a> {
    JournaledDelta {
        source,
        generation,
        market_id: 1,
        encoding,
        encoding_version: version,
    }
}

#[test]
fn every_journaled_effect_reconstructs_from_its_row() {
    for (index, effect) in effects().into_iter().enumerate() {
        let at = source(index as u64 + 1);
        let delta = CanonicalDelta::new(at, None, effect);
        let entry = journal_entry(&delta);
        let decoded = CanonicalDelta::decode(&journaled(
            at,
            entry.generation,
            &entry.encoding,
            DELTA_ENCODING_VERSION,
        ))
        .unwrap();
        assert_eq!(decoded, delta, "the vocabulary decodes its own encoding");
        assert_eq!(decoded.encode(), delta.encode());
    }
}

#[test]
fn an_event_activated_generation_round_trips_through_its_row() {
    let at = source(7);
    let entity = GenerationEntity::Contract {
        market_id: 1,
        role: ContractRole::Pool,
    };
    let delta = CanonicalDelta::new(
        at,
        Some(GenerationRef::activated_at(entity, Some(3), at)),
        DeltaEffect::GhoRefreshDiscount { position_id: 1 },
    );
    let entry = journal_entry(&delta);
    let decoded = CanonicalDelta::decode(&journaled(
        at,
        entry.generation,
        &entry.encoding,
        DELTA_ENCODING_VERSION,
    ))
    .unwrap();
    assert_eq!(decoded, delta);
    assert_eq!(
        decoded
            .generation()
            .map(|generation| generation.activation()),
        Some(GenerationActivation::Event(at)),
        "the activation position survives the round trip"
    );
}

#[test]
fn a_row_and_its_encoding_that_disagree_are_refused() {
    let at = source(9);
    let delta = CanonicalDelta::new(at, None, DeltaEffect::GhoRefreshDiscount { position_id: 2 });
    let entry = journal_entry(&delta);
    let other = source(10);
    assert!(matches!(
        CanonicalDelta::decode(&journaled(
            other,
            entry.generation.clone(),
            &entry.encoding,
            DELTA_ENCODING_VERSION
        )),
        Err(ProjectionError::DeltaEncoding { .. })
    ));
    assert!(matches!(
        CanonicalDelta::decode(&journaled(
            at,
            Some(degenbot_db::GenerationRecord {
                entity: "contract{market=1,role=pool}".to_string(),
                activation: degenbot_db::GenerationActivation::Seed,
                revision: Some(1),
            }),
            &entry.encoding,
            DELTA_ENCODING_VERSION
        )),
        Err(ProjectionError::DeltaEncoding { .. })
    ));
    assert!(matches!(
        CanonicalDelta::decode(&journaled(
            at,
            None,
            &entry.encoding,
            "aave.canonical-delta/v0"
        )),
        Err(ProjectionError::DeltaEncodingVersion { .. })
    ));
    assert!(matches!(
        CanonicalDelta::decode(&journaled(
            at,
            None,
            "gho-refresh-discount{not-a-number}",
            DELTA_ENCODING_VERSION
        )),
        Err(ProjectionError::DeltaEncoding { .. })
    ));
}
