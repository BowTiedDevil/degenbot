//! Intrinsic chain facts — the native asset's name, symbol, and decimals per chain.

//!
//! A chain's native asset has no contract to read `name()` / `symbol()` /
//! `decimals()` from, so those three facts cannot be discovered: they are
//! INTRINSIC to the chain. This module is their one manifest, owned by the
//! core so no consumer hardcodes the triple — Ethereum resolves to ETH/18
//! here, and Curve, V4, and the Python shell all read the same answer.
//!
//! Deliberately NOT operator config (ADR-065) and NOT the pool species
//! manifest: those describe deployments and pool behavior, while these rows
//! describe the chain itself. A row is a fact, not a knob — there is no way
//! to configure a different native asset for a chain, and a chain with no row
//! is a typed refusal ([`UnknownChainFacts`]), never a guessed default.
//!
//! The manifest resolves METADATA only. The conceptual native token's
//! identity (one per chain, no protocol address of its own) lives with the
//! session token identity; the protocol-private sentinel translations (the
//! Curve all-Es coin, the V4 `address(0)` currency) live at their own
//! protocol call sites and are never part of these facts.

/// The intrinsic display facts of one chain's native asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeTokenFacts {
    /// The asset's display name (Ethereum: `"Ether"`).
    pub name: &'static str,
    /// The asset's ticker symbol (Ethereum: `"ETH"`).
    pub symbol: &'static str,
    /// The asset's decimals (Ethereum: `18`).
    pub decimals: u8,
}

/// No native-asset facts are recorded for this chain.
///
/// A loud refusal, not a default: the native asset of an unlisted chain is
/// unknown, and a guessed symbol would surface as fabricated metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("no native-asset facts are recorded for chain {chain_id}")]
pub struct UnknownChainFacts {
    /// The chain whose facts were requested.
    pub chain_id: u64,
}

/// The manifest: intrinsic native-asset facts, one row per chain.
const MANIFEST: &[(u64, NativeTokenFacts)] = &[
    // Ethereum and the chains whose native asset is Ether.
    (
        1,
        NativeTokenFacts {
            name: "Ether",
            symbol: "ETH",
            decimals: 18,
        },
    ),
    (
        10,
        NativeTokenFacts {
            name: "Ether",
            symbol: "ETH",
            decimals: 18,
        },
    ),
    (
        8453,
        NativeTokenFacts {
            name: "Ether",
            symbol: "ETH",
            decimals: 18,
        },
    ),
    (
        42161,
        NativeTokenFacts {
            name: "Ether",
            symbol: "ETH",
            decimals: 18,
        },
    ),
    (
        11_155_111,
        NativeTokenFacts {
            name: "Ether",
            symbol: "ETH",
            decimals: 18,
        },
    ),
    // Other named native assets.
    (
        56,
        NativeTokenFacts {
            name: "BNB",
            symbol: "BNB",
            decimals: 18,
        },
    ),
    (
        137,
        NativeTokenFacts {
            name: "POL",
            symbol: "POL",
            decimals: 18,
        },
    ),
    (
        43114,
        NativeTokenFacts {
            name: "Avalanche",
            symbol: "AVAX",
            decimals: 18,
        },
    ),
];

/// Resolve `chain_id`'s native-asset facts from the manifest.
///
/// # Errors
///
/// [`UnknownChainFacts`] when the manifest holds no row for `chain_id` —
/// there is no default row and no fallback triple.
pub fn native_facts(chain_id: u64) -> Result<NativeTokenFacts, UnknownChainFacts> {
    MANIFEST
        .iter()
        .find(|(row_chain_id, _)| *row_chain_id == chain_id)
        .map(|(_, facts)| *facts)
        .ok_or(UnknownChainFacts { chain_id })
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::{native_facts, NativeTokenFacts};

    #[test]
    fn ethereum_resolves_to_eth_18() {
        assert_eq!(
            native_facts(1).unwrap(),
            NativeTokenFacts {
                name: "Ether",
                symbol: "ETH",
                decimals: 18,
            }
        );
    }

    #[test]
    fn every_manifest_row_is_whole() {
        for (chain_id, facts) in super::MANIFEST {
            let resolved = native_facts(*chain_id).unwrap();
            assert_eq!(resolved, *facts, "chain {chain_id} row is not whole");
            assert!(!resolved.name.is_empty(), "chain {chain_id} has no name");
            assert!(
                !resolved.symbol.is_empty(),
                "chain {chain_id} has no symbol"
            );
        }
    }

    #[test]
    fn an_unlisted_chain_is_a_typed_refusal_not_a_default() {
        let refusal = native_facts(31_337).unwrap_err();
        assert_eq!(refusal.chain_id, 31_337);
    }

    #[test]
    fn the_same_chain_always_resolves_the_same_facts() {
        assert_eq!(native_facts(1).unwrap(), native_facts(1).unwrap());
    }
}
