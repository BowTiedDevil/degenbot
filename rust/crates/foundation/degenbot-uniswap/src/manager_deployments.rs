//! `(chain_id, manager)`-keyed V4 species resolution (ADR-059 D3).
//!
//! V4 pools are not CREATE2 factory deployments: one pool-manager singleton
//! hosts many pool ids, so the ``(chain_id, factory)`` table in
//! [`crate::deployments`] cannot name them. The species manifest
//! (`degenbot-db::species`, `species.toml`) is the single source of V4 manager
//! identity; this module is the manager-keyed view over it, the V4 twin of
//! [`crate::deployments::resolve_dex_name`].
//!
//! The factory-keyed functions in [`crate::deployments`] are untouched: V2/V3
//! identity still resolves through the CREATE2 deployment rows. A manager that
//! the manifest does not declare resolves to `None` (the caller degrades to a
//! generic variant, never an error), matching the factory-keyed convention.

use alloy::primitives::Address;

use crate::deployments::dex_name_from_label;
use crate::dex_identity::DexName;

/// The V4 species the shipped manifest declares for `(chain_id, manager)`.
///
/// Returns `None` for an unknown manager or a chain the species does not
/// cover. V2/V3 species never match (their chain entries carry no manager).
#[must_use]
pub fn manager_species(
    chain_id: u64,
    manager: Address,
) -> Option<&'static degenbot_db::species::Species> {
    degenbot_db::species::manager_deployment(chain_id, manager)
}

/// Resolve the DEX name for a V4 `(chain_id, manager)` deployment.
///
/// Returns the manifest species' DEX when the `(chain, manager)` is declared;
/// otherwise `None` (unknown manager → generic variant, never an error). This
/// is the V4 counterpart of [`crate::deployments::resolve_dex_name`].
#[must_use]
pub fn resolve_manager_dex_name(chain_id: u64, manager: Address) -> Option<DexName> {
    manager_species(chain_id, manager).and_then(|species| dex_name_from_species_kind(&species.kind))
}

/// Map a manifest species `kind` (e.g. `"sushiswap_v4"`) to its [`DexName`].
///
/// The kind is `<dex>_v<family>`, and the known-DEX prefix matching is shared
/// with the `deployments.json` `name`-label resolver, so the two sources agree
/// on what a DEX is called.
#[must_use]
pub fn dex_name_from_species_kind(kind: &str) -> Option<DexName> {
    dex_name_from_label(kind)
}

/// Every V4 manager declared by the shipped manifest, as
/// `(chain_id, manager, species)`.
pub fn managers() -> impl Iterator<Item = (u64, Address, &'static degenbot_db::species::Species)> {
    degenbot_db::species::manifest().v4_managers()
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::primitives::{address, Address};

    /// The shipped Uniswap V4 mainnet manager singleton.
    const UNISWAP_V4_MAINNET_MANAGER: Address =
        address!("000000000004444c5dc75cB358380D2e3dE08A90");

    #[test]
    fn shipped_v4_manager_resolves_to_its_species_and_dex() {
        let species = manager_species(1, UNISWAP_V4_MAINNET_MANAGER)
            .expect("shipped uniswap_v4 mainnet manager resolves");
        assert_eq!(species.kind, "uniswap_v4");
        assert_eq!(
            resolve_manager_dex_name(1, UNISWAP_V4_MAINNET_MANAGER),
            Some(DexName::Uniswap)
        );
    }

    #[test]
    fn unknown_manager_and_chain_degrade_to_none() {
        assert!(manager_species(1, Address::ZERO).is_none());
        assert!(resolve_manager_dex_name(1, Address::ZERO).is_none());
        // The manager is chain-scoped: mainnet's manager is not valid on Base.
        assert!(manager_species(8453, UNISWAP_V4_MAINNET_MANAGER).is_none());
    }

    #[test]
    fn managers_iterates_every_declared_v4_manager() {
        let managers: Vec<_> = managers().collect();
        assert!(
            managers.iter().any(|(chain, manager, species)| *chain == 1
                && *manager == UNISWAP_V4_MAINNET_MANAGER
                && species.kind == "uniswap_v4"),
            "the shipped manifest's uniswap_v4 mainnet manager must be enumerated"
        );
    }

    #[test]
    fn species_kinds_map_to_the_same_dex_names_as_deployment_labels() {
        // A future "sushiswap_v4" row resolves through the same prefix table
        // the JSON `name` labels use, without a second DEX-name enumeration.
        assert_eq!(
            dex_name_from_species_kind("sushiswap_v4"),
            Some(DexName::SushiSwap)
        );
        assert_eq!(
            dex_name_from_species_kind("pancakeswap_v4"),
            Some(DexName::PancakeSwap)
        );
        assert_eq!(dex_name_from_species_kind("unknown_v4"), None);
    }
}
