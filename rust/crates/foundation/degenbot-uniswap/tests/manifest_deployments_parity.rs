//! Manifest ↔ `deployments.json` reconciliation (ADR-059 D3).
//!
//! The species manifest owns species *identity* (kind, family, subclass table,
//! per-chain factory + CREATE2 init hash, V4 manager). `deployments.json` owns
//! the on-chain-resolution data the manifest deliberately does not carry: the
//! separate CREATE2 deployer and the Aerodrome EIP-1167 implementation address.
//! The two files are independent sources, so this gate pins every shared fact
//! in both directions and refuses a one-sided edit.
//!
//! Option (i) of the reconciliation — regenerating `deployments.json` from the
//! manifest — is not viable: the JSON carries presentation/routing fields
//! (`name`, `pool_type`, `variant`, `dex_variant`, `family`) and 13 Balancer
//! rows the manifest does not model, so no projection reproduces the current
//! bytes. These tests are the option (ii) substitute: the strictest parity the
//! two sources can support.

#![expect(clippy::expect_used, clippy::panic)]

use std::collections::HashSet;

use alloy::primitives::{address, Address};

use degenbot_db::species::{self, Family};
use degenbot_uniswap::{deployments, manager_deployments};

#[test]
fn every_manifest_v2_v3_chain_has_a_consistent_deployments_row() {
    for species in &species::manifest().species {
        if species.family == Family::V4 {
            continue;
        }
        for (&chain_id, ids) in &species.chains {
            let factory = ids.factory.expect("V2/V3 factory validated by the loader");
            let record = deployments::lookup(chain_id, factory).unwrap_or_else(|| {
                panic!(
                    "manifest species {} chain {chain_id} factory {factory} is absent from deployments.json",
                    species.kind
                )
            });
            assert_eq!(
                record.init_hash, ids.init_codehash,
                "manifest/deployments.json init hash drift for {} chain {chain_id}",
                species.kind
            );
            assert_eq!(
                record.dex,
                manager_deployments::dex_name_from_species_kind(&species.kind),
                "manifest/deployments.json DEX drift for {} chain {chain_id}",
                species.kind
            );
        }
    }
}

#[test]
fn every_non_balancer_deployments_row_has_a_manifest_species() {
    let manifest_factories: HashSet<(u64, Address)> = species::manifest()
        .species
        .iter()
        .filter(|s| s.family != Family::V4)
        .flat_map(|s| {
            s.chains
                .iter()
                .filter_map(|(&chain_id, ids)| ids.factory.map(|f| (chain_id, f)))
        })
        .collect();

    for record in deployments::records() {
        // Balancer is a different structural family the manifest does not
        // model as a V2/V3/V4 species; every other shipped row must be one.
        if record.pool_type.is_some_and(|p| p.starts_with("balancer")) {
            continue;
        }
        assert!(
            manifest_factories.contains(&(record.chain_id, record.factory)),
            "deployments.json row chain {} factory {} ({:?}) has no manifest species",
            record.chain_id,
            record.factory,
            record.dex
        );
    }
}

#[test]
fn separate_create2_deployers_stay_owned_by_deployments_json() {
    // The manifest has no `deployer` field, so `deployments.json` remains the
    // sole owner of the CREATE2 preimage deployer. Pin the shipped
    // separate-deployer case so an attempted projection that drops it fails.
    let record = deployments::lookup(1, address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"))
        .expect("PancakeSwap V3 mainnet row");
    assert!(record.deployer.is_some());
    assert_ne!(record.effective_deployer(), record.factory);
}

#[test]
fn every_manifest_v4_chain_carries_a_manager_unclaimed_as_a_factory() {
    let mut managers = 0usize;
    for species in &species::manifest().species {
        if species.family != Family::V4 {
            continue;
        }
        for (&chain_id, ids) in &species.chains {
            let manager = ids.manager.expect("V4 manager validated by the loader");
            managers += 1;
            assert!(ids.factory.is_none() && ids.init_codehash.is_none());
            assert!(
                deployments::lookup(chain_id, manager).is_none(),
                "V4 manager {manager} chain {chain_id} must not be keyed as a factory"
            );
            assert_eq!(
                manager_deployments::manager_species(chain_id, manager).map(|s| s.kind.as_str()),
                Some(species.kind.as_str())
            );
        }
    }
    assert_eq!(
        managers,
        manager_deployments::managers().count(),
        "the manager-keyed accessor must enumerate every manifest V4 chain"
    );
}

#[test]
fn aerodrome_rows_keep_their_implementation_address_in_deployments_json() {
    // Aerodrome derives pool addresses from an EIP-1167 implementation, not a
    // CREATE2 init hash. The manifest owns the species identity but not that
    // address, so the deployments row must still carry it.
    for species in &species::manifest().species {
        if species.family == Family::V4 {
            continue;
        }
        for (&chain_id, ids) in &species.chains {
            if ids.init_codehash.is_some() {
                continue;
            }
            let factory = ids.factory.expect("factory validated");
            let record = deployments::lookup(chain_id, factory).expect("row exists");
            assert!(
                record.implementation_address.is_some(),
                "species {} chain {chain_id} carries no CREATE2 hash, so deployments.json must supply the EIP-1167 implementation",
                species.kind
            );
        }
    }
}
