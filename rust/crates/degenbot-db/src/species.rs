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

use crate::schema::table::{LFJ_POOLS, V2_V3_SUBCLASS_TABLES};

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
    /// LFJ (Trader Joe) binned liquidity — declared in the taxonomy (ADR-059
    /// E3) but without a graph pool kind, so [`Self::pool_kind`] returns
    /// `None`. Its `pools.kind` discriminator is `lfj_binned`.
    Lfj,
}

impl Family {
    /// Project this manifest family onto the graph vocabulary's pool kind.
    ///
    /// The graph tier keys on family, not species: every V4 species resolves
    /// to the same `PoolKind::V4` arm, so adding a manager species needs no
    /// new graph enumeration. The LFJ binned family has no graph kind, so it
    /// LAGS the taxonomy and returns `None` (ADR-059 D8).
    #[must_use]
    pub const fn pool_kind(self) -> Option<degenbot_pathfinding::PoolKind> {
        match self {
            Self::V2 => Some(degenbot_pathfinding::PoolKind::V2),
            Self::V3 => Some(degenbot_pathfinding::PoolKind::V3),
            Self::V4 => Some(degenbot_pathfinding::PoolKind::V4),
            Self::Lfj => None,
        }
    }
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

impl Species {
    /// The V4 pool-manager singleton this species uses on `chain_id`.
    ///
    /// `None` for a V2/V3 species (its chain entry carries a factory) or an
    /// out-of-scope chain. A V4 manager hosts many pool ids on one chain, so
    /// the manager address is the deployment key the factory-keyed table
    /// cannot express.
    #[must_use]
    pub fn manager_on(&self, chain_id: u64) -> Option<Address> {
        self.chains.get(&chain_id).and_then(|ids| ids.manager)
    }
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

    /// The V4 species whose manager on `chain_id` is `manager`.
    ///
    /// Manager address + chain is the V4 deployment key — V4 has no CREATE2
    /// factory and one manager hosts many pool ids. V2/V3 species never match
    /// (their chain entries carry no manager).
    #[must_use]
    pub fn manager_deployment(&self, chain_id: u64, manager: Address) -> Option<&Species> {
        self.species
            .iter()
            .find(|s| s.manager_on(chain_id) == Some(manager))
    }

    /// Every V4 manager the manifest declares, as `(chain_id, manager, species)`.
    pub fn v4_managers(&self) -> impl Iterator<Item = (u64, Address, &Species)> {
        self.species
            .iter()
            .filter(|s| s.family == Family::V4)
            .flat_map(|s| {
                s.chains.iter().filter_map(move |(&chain_id, ids)| {
                    ids.manager.map(|manager| (chain_id, manager, s))
                })
            })
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
    #[error("LFJ species {kind:?} must name table {LFJ_POOLS:?}, got {table:?}")]
    LfjTableMismatch { kind: String, table: String },
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
        Family::Lfj => {
            if r.table != LFJ_POOLS {
                return Err(ManifestError::LfjTableMismatch {
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
            Family::V2 | Family::V3 | Family::Lfj => {
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

/// The V4 species the shipped manifest declares for `(chain_id, manager)`.
///
/// The manager-keyed twin of the factory-keyed `degenbot-uniswap` deployment
/// lookup: V4 pools are keyed by their manager singleton and pool id, not a
/// CREATE2 factory, so this is the resolution seam for V4 species identity.
#[must_use]
pub fn manager_deployment(chain_id: u64, manager: Address) -> Option<&'static Species> {
    manifest().manager_deployment(chain_id, manager)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
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

    // ── add-a-species recipe (fixture manifest, no global mutation) ──────
    //
    // The documented path for landing a real V4 manager species: a manifest
    // row with `family = "v4"`, `table = "managed"`, and a `manager` per
    // chain. This fixture proves the loader + manager-keyed resolution accept
    // it without touching the shipped `species.toml` or the graph roster.

    const FIXTURE_V4_MANAGER: &str = "0x1111111111111111111111111111111111111111";

    fn fixture_v4_manifest() -> Manifest {
        parse_manifest(&format!(
            "[[species]]\nkind = \"sushi_v4\"\nfamily = \"v4\"\n\
             table = \"managed\"\nfee_denominator = 1000000\n\
             [[species.chains]]\nchain_id = 1\nmanager = \"{FIXTURE_V4_MANAGER}\"\n"
        ))
        .expect("a V4 manager species must load")
    }

    #[test]
    fn loads_a_fixture_v4_species_and_resolves_it_by_manager() {
        let parsed = fixture_v4_manifest();
        let species = parsed.get("sushi_v4").expect("fixture species loads");
        assert_eq!(species.family, Family::V4);
        assert_eq!(species.table, V4_MANAGED_TABLE);

        let manager = Address::from_str(FIXTURE_V4_MANAGER).unwrap();
        assert_eq!(species.manager_on(1), Some(manager));
        assert_eq!(species.manager_on(8453), None, "chain not in fixture");
        assert_eq!(
            parsed.v4_managers().next().map(|(chain, m, _)| (chain, m)),
            Some((1, manager))
        );

        assert_eq!(
            parsed
                .manager_deployment(1, manager)
                .map(|s| s.kind.as_str()),
            Some("sushi_v4")
        );
        assert!(parsed.manager_deployment(1, Address::ZERO).is_none());
        assert!(parsed.manager_deployment(8453, manager).is_none());
    }

    const FIXTURE_LFJ_FACTORY: &str = "0x000000000000000000000000000000000000dead";

    #[test]
    fn loads_a_fixture_lfj_species_without_a_graph_kind() {
        // The LFJ species SHAPE (no shipped deployment addresses). Fixture
        // addresses are clearly placeholder markers, never deployments.
        let parsed = parse_manifest(&format!(
            "[[species]]\nkind = \"lfj_binned\"\nfamily = \"lfj\"\n\
             table = \"lfj_pools\"\n\
             [[species.chains]]\nchain_id = 1\nfactory = \"{FIXTURE_LFJ_FACTORY}\"\n"
        ))
        .expect("the LFJ species shape must load");
        let lfj = parsed.get("lfj_binned").expect("fixture species loads");
        assert_eq!(lfj.family, Family::Lfj);
        assert_eq!(lfj.table, LFJ_POOLS);
        assert_eq!(lfj.family.pool_kind(), None, "LFJ has no graph kind");
        assert_eq!(lfj.manager_on(1), None, "LFJ is factory-keyed, not managed");
    }

    #[test]
    fn rejects_an_lfj_species_naming_a_v2_table() {
        let err = parse_manifest(&format!(
            "[[species]]\nkind = \"lfj_binned\"\nfamily = \"lfj\"\n\
             table = \"uniswap_v2_pools\"\n\
             [[species.chains]]\nchain_id = 1\nfactory = \"{FIXTURE_LFJ_FACTORY}\"\n"
        ))
        .unwrap_err();
        assert_eq!(
            err,
            ManifestError::LfjTableMismatch {
                kind: "lfj_binned".to_owned(),
                table: "uniswap_v2_pools".to_owned(),
            }
        );
    }

    #[test]
    fn a_v4_fixture_species_needs_no_new_graph_pool_kind() {
        let parsed = fixture_v4_manifest();
        let species = parsed.get("sushi_v4").expect("fixture species loads");
        // The graph vocabulary projects by FAMILY: a new V4 species reuses the
        // existing `PoolKind::V4` arm, and the shipped roster is untouched by
        // a fixture parse (the loader holds no global state).
        assert_eq!(species.family.pool_kind(), Some(PoolKind::V4));
        assert_eq!(
            species.family.pool_kind(),
            Some(PoolKind::from_kind_str("uniswap_v4").unwrap())
        );
        assert_eq!(PoolKind::KNOWN_KINDS.len(), 11);
        // A V4 kind never carries a V2/V3 subclass table.
        assert!(subclass_table_for_kind("sushi_v4").is_none());
    }
}
