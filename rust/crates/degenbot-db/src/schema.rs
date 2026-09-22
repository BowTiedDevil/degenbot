//! Schema constants and the embedded DDL for the fresh-standalone open path.
//!
//! The schema is the consolidated DDL the Alembic head produces, captured as
//! `CREATE TABLE IF NOT EXISTS` (idempotent — safe to re-assert on the
//! fresh-standalone path). Columns mirror `src/degenbot/database/models/*.py`
//! exactly: big-int columns are `VARCHAR(78)` (the decimal string of an EVM
//! value; mirrors Python's `IntMappedToString` `TypeDecorator`), addresses are
//! `VARCHAR(42)` checksum strings, V4 `pool_hash` is `VARCHAR(66)` 0x-hex.

/// The embedded full-schema DDL, applied ONLY on the fresh-standalone open path
/// (a `cargo add degenbot-db` consumer's own empty file). A legacy DB carrying
/// the `alembic_version` marker table is never touched by this — see
/// [`crate::migrate::ensure_schema`].
pub const SCHEMA_HEAD: &str = include_str!("schema_head.sql");

/// The private Rust-owned schema-version stamp, written to
/// `_degenbot_db_schema_version` on the fresh-standalone, cutover, and heal
/// paths so Rust-owned schema bumps (post-Alembic-retirement) are tracked
/// independently of Alembic.
///
/// # Bump ritual (mechanical — three steps, in this order)
///
/// 1. **Bump this constant** (`RUST_SCHEMA_VERSION = N + 1`).
/// 2. **Append the step** to [`crate::migrations::RUST_MIGRATIONS`]: a
///    [`crate::migrations::MigrationStep`] with `version = N + 1` and the
///    `ALTER`/DDL SQL that upgrades schema `N` to `N + 1`. The
///    registry must stay contiguous from `1` — `apply_forward_migrations`
///    refuses a gap with [`crate::error::DbError::MissingMigrationStep`].
/// 3. **Extend the release matrix** (ADR-052 D5): add the `N`-stamped fixture
///    so the open → version-lock → current chain is proven for the prior
///    release.
///
/// The open path applies pending steps automatically (ADR-052 D2), so no
/// consumer calls a migration verb; a binary older than the DB refuses with
/// [`crate::error::DbError::SchemaAhead`] and never writes ahead.
pub const RUST_SCHEMA_VERSION: u32 = 1;

/// Name of the private Rust-owned schema stamp table.
pub const SCHEMA_VERSION_TABLE: &str = "_degenbot_db_schema_version";

// ---------------------------------------------------------------------------
// Table-name constants — single source of truth for the SQL strings in `read.rs`
// and `snapshot.rs`, so a rename touches one place.
// ---------------------------------------------------------------------------

pub mod table {
    //! Canonical table names (mirror `models/*.py` `__tablename__`).

    use degenbot_pathfinding::PoolKind;

    pub const EXCHANGES: &str = "exchanges";
    pub const ERC20_TOKENS: &str = "erc20_tokens";
    pub const POOLS: &str = "pools";
    pub const LIQUIDITY_POSITIONS: &str = "liquidity_positions";
    pub const INITIALIZATION_MAPS: &str = "initialization_maps";
    pub const POOL_MANAGERS: &str = "pool_managers";
    pub const MANAGED_POOLS: &str = "managed_pools";
    pub const UNISWAP_V4_POOLS: &str = "uniswap_v4_pools";
    pub const MANAGED_POOL_LIQUIDITY_POSITIONS: &str = "managed_pool_liquidity_positions";
    pub const MANAGED_POOL_INITIALIZATION_MAPS: &str = "managed_pool_initialization_maps";
    pub const UNISWAP_V3_POOLS: &str = "uniswap_v3_pools";

    /// The per-DEX subclass table for a V2/V3 `kind` discriminator.
    ///
    /// Returns `None` for `base`/non-subclass kinds (no subclass row).
    /// V4 has no V2/V3-style subclass table — it joins `uniswap_v4_pools`
    /// via the `managed_pools` polymorphic base, handled separately.
    #[must_use]
    pub fn v2_v3_subclass_table(kind: &str) -> Option<&'static str> {
        match kind {
            "aerodrome_v2" => Some("aerodrome_v2_pools"),
            "camelot_v2" => Some("camelot_v2_pools"),
            "pancakeswap_v2" => Some("pancakeswap_v2_pools"),
            "sushiswap_v2" => Some("sushiswap_v2_pools"),
            "swapbased_v2" => Some("swapbased_v2_pools"),
            "uniswap_v2" => Some("uniswap_v2_pools"),
            "aerodrome_v3" => Some("aerodrome_v3_pools"),
            "uniswap_v3" => Some("uniswap_v3_pools"),
            "pancakeswap_v3" => Some("pancakeswap_v3_pools"),
            "sushiswap_v3" => Some("sushiswap_v3_pools"),
            _ => None, // ExpectedAbsent: only V2/V3 subclass kinds have a table.
        }
    }

    /// `true` if `kind` is a V3 family discriminator
    /// (a `UniswapV3PoolTableBase` subclass polymorphic identity).
    ///
    /// Projects through the graph vocabulary's single kind table
    /// ([`PoolKind::from_kind_str`]) — ADR-059 D1: the schema helper is a
    /// view of the taxonomy projection, not a second enumeration.
    #[must_use]
    pub fn is_v3_kind(kind: &str) -> bool {
        matches!(PoolKind::from_kind_str(kind), Some(PoolKind::V3))
    }

    /// `true` if `kind` is a V2 family discriminator.
    #[must_use]
    pub fn is_v2_kind(kind: &str) -> bool {
        matches!(PoolKind::from_kind_str(kind), Some(PoolKind::V2))
    }

    /// `true` if `kind` is a V4 discriminator (only `uniswap_v4` today — the
    /// `managed_pools` polymorphic base's `uniswap_v4` identity used by
    /// `uniswap_v4_pools`).
    #[must_use]
    pub fn is_v4_kind(kind: &str) -> bool {
        matches!(PoolKind::from_kind_str(kind), Some(PoolKind::V4))
    }
}

#[cfg(test)]
mod table_tests {
    //! ADR-059 D1 golden table: the graph vocabulary's kind table is the
    //! single source the schema helpers project through, and it covers exactly
    //! the species the schema admits. A taxonomy species added without a graph
    //! tag fails here instead of vanishing from every consumer's graph.
    use super::table::{is_v2_kind, is_v3_kind, is_v4_kind, v2_v3_subclass_table};
    use degenbot_pathfinding::PoolKind;

    /// The golden projection, independent of the implementation: every
    /// `pools.kind` / `managed_pools.kind` string and its graph tag.
    const GOLDEN: &[(&str, PoolKind)] = &[
        ("uniswap_v2", PoolKind::V2),
        ("sushiswap_v2", PoolKind::V2),
        ("pancakeswap_v2", PoolKind::V2),
        ("aerodrome_v2", PoolKind::V2),
        ("camelot_v2", PoolKind::V2),
        ("swapbased_v2", PoolKind::V2),
        ("uniswap_v3", PoolKind::V3),
        ("sushiswap_v3", PoolKind::V3),
        ("pancakeswap_v3", PoolKind::V3),
        ("aerodrome_v3", PoolKind::V3),
        ("uniswap_v4", PoolKind::V4),
    ];

    #[test]
    fn kind_table_matches_the_golden_projection() {
        let actual: Vec<(&str, PoolKind)> = PoolKind::KNOWN_KINDS
            .iter()
            .map(|(name, kind)| (*name, *kind))
            .collect();
        assert_eq!(actual, GOLDEN);
    }

    #[test]
    fn schema_helpers_agree_with_the_projection() {
        for (kind, expected) in GOLDEN {
            assert_eq!(PoolKind::from_kind_str(kind), Some(*expected));
            assert_eq!(is_v2_kind(kind), *expected == PoolKind::V2);
            assert_eq!(is_v3_kind(kind), *expected == PoolKind::V3);
            assert_eq!(is_v4_kind(kind), *expected == PoolKind::V4);
        }
    }

    #[test]
    fn every_kind_has_a_graph_tag_or_is_loudly_refused() {
        assert_eq!(PoolKind::from_kind_str("lfj_binned"), None);
        assert_eq!(PoolKind::from_kind_str(""), None);
    }

    #[test]
    fn every_subclass_species_has_a_graph_tag() {
        for (kind, _) in GOLDEN {
            if *kind == "uniswap_v4" {
                continue;
            }
            assert!(
                v2_v3_subclass_table(kind).is_some(),
                "{kind} lost its subclass table"
            );
            assert!(
                PoolKind::from_kind_str(kind).is_some(),
                "{kind} lost its graph tag"
            );
        }
    }
}
