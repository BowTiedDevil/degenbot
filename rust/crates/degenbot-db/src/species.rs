//! Species manifest — the typed loader over the embedded `species.toml`
//! (ADR-059 D3: a fork that differs from its family only in identifiers is
//! data, not a new type).
//!
//! The manifest is parsed and validated once per process. Validation is
//! deliberately strict: a typo'd key, an unknown subclass table, a V3 species
//! without its storage layout, or a chain entry mixing CREATE2 and V4-manager
//! identifiers fails at first use rather than silently dropping a species from
//! the enumerations that project this file.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::OnceLock;

use alloy::primitives::{Address, B256};
use degenbot_core::address_utils;

use crate::schema::table::V2_V3_SUBCLASS_TABLES;

/// The embedded manifest. A published crate can only `include_str!` files
/// under its own directory, so the file lives beside this module.
const SPECIES_TOML: &str = include_str!("species.toml");

/// The `table` sentinel a V4 species carries. V4 pools join `uniswap_v4_pools`
/// through the `managed_pools` polymorphic base, not a V2/V3 subclass table.
pub const V4_MANAGED_TABLE: &str = "managed";

/// The pool family a species belongs to — the ADR-059 D1 graph vocabulary's
/// `PoolKind` projection in manifest-data form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    V2,
    V3,
    V4,
}

/// The EVM storage-layout id a V3 species uses. Mirrors the variants of
/// `degenbot-pools`'s `ClSlotLayout`; kept as a manifest-local enum so this
/// crate does not depend on `degenbot-pools`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotLayout {
    /// Canonical Uniswap V3 — `slot0` one word, liquidity@4, ticks@5.
    UniswapV3,
    /// `PancakeSwap` V3 fork — `slot0` two words, liquidity@5, ticks@6.
    PancakeV3,
}

/// The identifiers a species has on one chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainIdentifiers {
    /// The CREATE2 factory (V2/V3).
    pub factory: Option<Address>,
    /// The CREATE2 init code hash (V2/V3). Absent for Aerodrome, whose address
    /// derivation uses an implementation address this manifest does not yet
    /// carry.
    pub init_codehash: Option<B256>,
    /// The V4 pool-manager singleton address.
    pub manager: Option<Address>,
}

/// A validated species.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Species {
    /// The persisted `pools.kind` / `managed_pools.kind` discriminator.
    pub kind: String,
    /// The family this species projects onto the graph vocabulary.
    pub family: Family,
    /// The per-DEX subclass table, or [`V4_MANAGED_TABLE`] for V4.
    pub table: String,
    /// The V2/V3/V4 fee denominator, where the family or fork fixes one
    /// (Camelot's is per-pool, so it is absent).
    pub fee_denominator: Option<i64>,
    /// The V3 fork storage layout (V3 only).
    pub slot_layout: Option<SlotLayout>,
    /// Whether the V2 subclass table carries the Aerodrome `stable` column.
    pub stable: bool,
    /// Per-chain deployment identifiers, keyed by chain id.
    pub chains: BTreeMap<u64, ChainIdentifiers>,
}

/// The validated species manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// The species, in manifest order (routing depends on this order).
    pub species: Vec<Species>,
}

impl Manifest {
    /// Look up a species by its DB `kind` discriminator.
    #[must_use]
    pub fn get(&self, kind: &str) -> Option<&Species> {
        self.species.iter().find(|s| s.kind == kind)
    }
}

/// A manifest parse or validation failure.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("species manifest is not valid TOML: {0}")]
    Toml(String),
    #[error("species manifest declares no species")]
    Empty,
    #[error("species manifest declares an empty kind")]
    EmptyKind,
    #[error("species kind {0:?} is declared more than once")]
    DuplicateKind(String),
    #[error("species {kind:?} names unknown subclass table {table:?}")]
    UnknownTable { kind: String, table: String },
    #[error("V4 species {kind:?} must name table {V4_MANAGED_TABLE:?}, got {table:?}")]
    V4TableMismatch { kind: String, table: String },
    #[error("V3 species {kind:?} is missing its {field}")]
    MissingV3Field { kind: String, field: &'static str },
    #[error("non-V3 species {0:?} declares a slot_layout")]
    LayoutOnNonV3(String),
    #[error("non-V2 species {0:?} declares stable")]
    StableOnNonV2(String),
    #[error("species {0:?} declares a non-positive fee_denominator")]
    NonPositiveFeeDenominator(String),
    #[error("species {0:?} declares no chain entries")]
    EmptyChains(String),
    #[error("species {kind:?} declares chain {chain_id} more than once")]
    DuplicateChain { kind: String, chain_id: u64 },
    #[error("species {kind:?} chain {chain_id} is missing its {field}")]
    MissingChainIdentifier {
        kind: String,
        chain_id: u64,
        field: &'static str,
    },
    #[error("V4 species {kind:?} chain {chain_id} carries a CREATE2 identifier")]
    Create2OnV4 { kind: String, chain_id: u64 },
    #[error("V2/V3 species {kind:?} chain {chain_id} carries a V4 manager address")]
    ManagerOnCreate2 { kind: String, chain_id: u64 },
    #[error("species {kind:?} chain {chain_id} has invalid address {value:?}")]
    InvalidAddress {
        kind: String,
        chain_id: u64,
        value: String,
    },
    #[error("species {kind:?} chain {chain_id} has invalid init_codehash {value:?}")]
    InvalidHash {
        kind: String,
        chain_id: u64,
        value: String,
    },
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    species: Vec<RawSpecies>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSpecies {
    kind: String,
    family: Family,
    table: String,
    #[serde(default)]
    fee_denominator: Option<i64>,
    #[serde(default)]
    slot_layout: Option<SlotLayout>,
    #[serde(default)]
    stable: bool,
    #[serde(default)]
    chains: Vec<RawChain>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawChain {
    chain_id: u64,
    #[serde(default)]
    factory: Option<String>,
    #[serde(default)]
    init_codehash: Option<String>,
    #[serde(default)]
    manager: Option<String>,
}

/// Parse + validate a manifest string. Exposed so tests can exercise the
/// rejection paths; production callers use [`manifest`].
///
/// # Errors
///
/// Returns [`ManifestError`] for invalid TOML or any violated invariant.
pub fn parse_manifest(toml_str: &str) -> Result<Manifest, ManifestError> {
    let raw: RawManifest =
        toml::from_str(toml_str).map_err(|e| ManifestError::Toml(e.to_string()))?;
    validate(&raw)
}

fn validate(raw: &RawManifest) -> Result<Manifest, ManifestError> {
    if raw.species.is_empty() {
        return Err(ManifestError::Empty);
    }
    let mut species = Vec::with_capacity(raw.species.len());
    for r in &raw.species {
        if r.kind.is_empty() {
            return Err(ManifestError::EmptyKind);
        }
        if species.iter().any(|s: &Species| s.kind == r.kind) {
            return Err(ManifestError::DuplicateKind(r.kind.clone()));
        }
        species.push(validate_species(r)?);
    }
    Ok(Manifest { species })
}

fn validate_species(r: &RawSpecies) -> Result<Species, ManifestError> {
    match r.family {
        Family::V4 => {
            if r.table != V4_MANAGED_TABLE {
                return Err(ManifestError::V4TableMismatch {
                    kind: r.kind.clone(),
                    table: r.table.clone(),
                });
            }
        }
        Family::V2 | Family::V3 => {
            if !V2_V3_SUBCLASS_TABLES.contains(&r.table.as_str()) {
                return Err(ManifestError::UnknownTable {
                    kind: r.kind.clone(),
                    table: r.table.clone(),
                });
            }
        }
    }

    if r.family == Family::V3 {
        if r.slot_layout.is_none() {
            return Err(ManifestError::MissingV3Field {
                kind: r.kind.clone(),
                field: "slot_layout",
            });
        }
        if r.fee_denominator.is_none() {
            return Err(ManifestError::MissingV3Field {
                kind: r.kind.clone(),
                field: "fee_denominator",
            });
        }
    } else if r.slot_layout.is_some() {
        return Err(ManifestError::LayoutOnNonV3(r.kind.clone()));
    }
    if r.stable && r.family != Family::V2 {
        return Err(ManifestError::StableOnNonV2(r.kind.clone()));
    }
    if let Some(fd) = r.fee_denominator {
        if fd <= 0 {
            return Err(ManifestError::NonPositiveFeeDenominator(r.kind.clone()));
        }
    }
    if r.chains.is_empty() {
        return Err(ManifestError::EmptyChains(r.kind.clone()));
    }

    Ok(Species {
        kind: r.kind.clone(),
        family: r.family,
        table: r.table.clone(),
        fee_denominator: r.fee_denominator,
        slot_layout: r.slot_layout,
        stable: r.stable,
        chains: validate_chains(r)?,
    })
}

fn validate_chains(r: &RawSpecies) -> Result<BTreeMap<u64, ChainIdentifiers>, ManifestError> {
    let mut chains = BTreeMap::new();
    for c in &r.chains {
        if chains.contains_key(&c.chain_id) {
            return Err(ManifestError::DuplicateChain {
                kind: r.kind.clone(),
                chain_id: c.chain_id,
            });
        }
        let factory = parse_address(&r.kind, c.chain_id, c.factory.as_deref())?;
        let manager = parse_address(&r.kind, c.chain_id, c.manager.as_deref())?;
        let init_codehash = match c.init_codehash.as_deref() {
            Some(value) => Some(
                B256::from_str(value).map_err(|_| ManifestError::InvalidHash {
                    kind: r.kind.clone(),
                    chain_id: c.chain_id,
                    value: value.to_owned(),
                })?,
            ),
            None => None,
        };

        match r.family {
            Family::V4 => {
                if factory.is_some() || init_codehash.is_some() {
                    return Err(ManifestError::Create2OnV4 {
                        kind: r.kind.clone(),
                        chain_id: c.chain_id,
                    });
                }
                if manager.is_none() {
                    return Err(ManifestError::MissingChainIdentifier {
                        kind: r.kind.clone(),
                        chain_id: c.chain_id,
                        field: "manager",
                    });
                }
            }
            Family::V2 | Family::V3 => {
                if manager.is_some() {
                    return Err(ManifestError::ManagerOnCreate2 {
                        kind: r.kind.clone(),
                        chain_id: c.chain_id,
                    });
                }
                if factory.is_none() {
                    return Err(ManifestError::MissingChainIdentifier {
                        kind: r.kind.clone(),
                        chain_id: c.chain_id,
                        field: "factory",
                    });
                }
            }
        }
        chains.insert(
            c.chain_id,
            ChainIdentifiers {
                factory,
                init_codehash,
                manager,
            },
        );
    }
    Ok(chains)
}

fn parse_address(
    kind: &str,
    chain_id: u64,
    value: Option<&str>,
) -> Result<Option<Address>, ManifestError> {
    value
        .map(|value| {
            address_utils::parse_address(value).map_err(|_| ManifestError::InvalidAddress {
                kind: kind.to_owned(),
                chain_id,
                value: value.to_owned(),
            })
        })
        .transpose()
}

/// The validated manifest, parsed once for the process lifetime.
///
/// The embedded file is commit-time validated; a parse failure is a build
/// artifact regression, so a loud panic is the right terminal state for a
/// `OnceLock` initializer with no error channel.
///
/// # Panics
///
/// Panics if the embedded `species.toml` fails to parse or validate — a
/// build-artifact regression, not a runtime input.
#[must_use]
pub fn manifest() -> &'static Manifest {
    static MANIFEST: OnceLock<Manifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        #[expect(clippy::expect_used)]
        let manifest =
            parse_manifest(SPECIES_TOML).expect("embedded species.toml must parse + validate");
        manifest
    })
}

/// The species of one family, in manifest order.
pub fn species_of(family: Family) -> impl Iterator<Item = &'static Species> {
    manifest()
        .species
        .iter()
        .filter(move |s| s.family == family)
}

/// The per-DEX subclass table for a V2/V3 `kind`, or `None` for a V4 kind or
/// an unknown kind. Projects the manifest so [`crate::schema::table`] needs no
/// second kind-to-table enumeration.
#[must_use]
pub fn subclass_table_for_kind(kind: &str) -> Option<&'static str> {
    let species = manifest().get(kind)?;
    (species.family != Family::V4).then_some(species.table.as_str())
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use degenbot_pathfinding::PoolKind;

    const FACTORY: &str = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f";

    #[test]
    fn shipped_manifest_parses_and_validates() {
        assert!(!manifest().species.is_empty());
    }

    #[test]
    fn manifest_covers_exactly_the_graph_vocabulary() {
        let mut manifest_kinds: Vec<&str> =
            manifest().species.iter().map(|s| s.kind.as_str()).collect();
        manifest_kinds.sort_unstable();
        let mut known: Vec<&str> = PoolKind::KNOWN_KINDS.iter().map(|(k, _)| *k).collect();
        known.sort_unstable();
        assert_eq!(
            manifest_kinds, known,
            "manifest species must match the graph vocabulary exactly"
        );
        assert_eq!(manifest_kinds.len(), 11, "10 V2/V3 + uniswap_v4");
        let create2 = manifest()
            .species
            .iter()
            .filter(|s| s.family != Family::V4)
            .count();
        assert_eq!(create2, 10, "10 V2/V3 species carry a subclass table");
    }

    #[test]
    fn manifest_family_matches_the_graph_vocabulary() {
        for s in &manifest().species {
            let expected = match PoolKind::from_kind_str(&s.kind) {
                Some(PoolKind::V2) => Family::V2,
                Some(PoolKind::V3) => Family::V3,
                Some(PoolKind::V4) => Family::V4,
                _ => unreachable!("parity test pins every kind"),
            };
            assert_eq!(s.family, expected, "{}", s.kind);
        }
    }

    #[test]
    fn subclass_table_projects_the_manifest() {
        for s in &manifest().species {
            let expected = (s.family != Family::V4).then_some(s.table.as_str());
            assert_eq!(subclass_table_for_kind(&s.kind), expected, "{}", s.kind);
        }
        assert_eq!(subclass_table_for_kind("uniswap_v4"), None);
        assert_eq!(subclass_table_for_kind("unsiwap_v3"), None, "typo refused");
    }

    #[test]
    fn rejects_an_unknown_key() {
        let err = parse_manifest(&format!(
            "[[species]]\nkind = \"uniswap_v2\"\nfamily = \"v2\"\n\
             table = \"uniswap_v2_pools\"\nfee_denom = 1000\n\
             [[species.chains]]\nchain_id = 1\nfactory = \"{FACTORY}\"\n"
        ))
        .unwrap_err();
        assert!(matches!(err, ManifestError::Toml(_)), "got {err:?}");
    }

    #[test]
    fn rejects_a_duplicate_kind() {
        let err = parse_manifest(&format!(
            "[[species]]\nkind = \"uniswap_v2\"\nfamily = \"v2\"\n\
             table = \"uniswap_v2_pools\"\n\
             [[species.chains]]\nchain_id = 1\nfactory = \"{FACTORY}\"\n\
             [[species]]\nkind = \"uniswap_v2\"\nfamily = \"v2\"\n\
             table = \"uniswap_v2_pools\"\n\
             [[species.chains]]\nchain_id = 1\nfactory = \"{FACTORY}\"\n"
        ))
        .unwrap_err();
        assert_eq!(err, ManifestError::DuplicateKind("uniswap_v2".to_owned()));
    }

    #[test]
    fn rejects_an_unknown_subclass_table() {
        let err = parse_manifest(&format!(
            "[[species]]\nkind = \"uniswap_v2\"\nfamily = \"v2\"\n\
             table = \"not_a_pool_table\"\n\
             [[species.chains]]\nchain_id = 1\nfactory = \"{FACTORY}\"\n"
        ))
        .unwrap_err();
        assert_eq!(
            err,
            ManifestError::UnknownTable {
                kind: "uniswap_v2".to_owned(),
                table: "not_a_pool_table".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_v3_without_layout_or_denominator() {
        let no_layout = parse_manifest(&format!(
            "[[species]]\nkind = \"uniswap_v3\"\nfamily = \"v3\"\n\
             table = \"uniswap_v3_pools\"\nfee_denominator = 1000000\n\
             [[species.chains]]\nchain_id = 1\nfactory = \"{FACTORY}\"\n"
        ))
        .unwrap_err();
        assert_eq!(
            no_layout,
            ManifestError::MissingV3Field {
                kind: "uniswap_v3".to_owned(),
                field: "slot_layout",
            }
        );
        let no_denominator = parse_manifest(&format!(
            "[[species]]\nkind = \"uniswap_v3\"\nfamily = \"v3\"\n\
             table = \"uniswap_v3_pools\"\nslot_layout = \"uniswap_v3\"\n\
             [[species.chains]]\nchain_id = 1\nfactory = \"{FACTORY}\"\n"
        ))
        .unwrap_err();
        assert_eq!(
            no_denominator,
            ManifestError::MissingV3Field {
                kind: "uniswap_v3".to_owned(),
                field: "fee_denominator",
            }
        );
    }

    #[test]
    fn rejects_a_v4_species_naming_a_subclass_table() {
        let err = parse_manifest("[[species]]\nkind = \"uniswap_v4\"\nfamily = \"v4\"\n\
             table = \"uniswap_v4_pools\"\n\
             [[species.chains]]\nchain_id = 1\nfactory = \"0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f\"\n")
        .unwrap_err();
        assert_eq!(
            err,
            ManifestError::V4TableMismatch {
                kind: "uniswap_v4".to_owned(),
                table: "uniswap_v4_pools".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_a_v4_chain_carrying_a_factory() {
        let err = parse_manifest(&format!(
            "[[species]]\nkind = \"uniswap_v4\"\nfamily = \"v4\"\n\
             table = \"managed\"\n\
             [[species.chains]]\nchain_id = 1\nfactory = \"{FACTORY}\"\n"
        ))
        .unwrap_err();
        assert_eq!(
            err,
            ManifestError::Create2OnV4 {
                kind: "uniswap_v4".to_owned(),
                chain_id: 1,
            }
        );
    }

    #[test]
    fn rejects_a_v2_chain_without_a_factory() {
        let err = parse_manifest(
            "[[species]]\nkind = \"uniswap_v2\"\nfamily = \"v2\"\n\
             table = \"uniswap_v2_pools\"\n\
             [[species.chains]]\nchain_id = 1\n",
        )
        .unwrap_err();
        assert_eq!(
            err,
            ManifestError::MissingChainIdentifier {
                kind: "uniswap_v2".to_owned(),
                chain_id: 1,
                field: "factory",
            }
        );
    }
}
