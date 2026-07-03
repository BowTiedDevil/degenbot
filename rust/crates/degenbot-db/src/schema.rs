//! Schema constants and the embedded DDL for the fresh-standalone open path.
//!
//! The schema is the consolidated DDL the Alembic head produces, captured as
//! `CREATE TABLE IF NOT EXISTS` (idempotent — safe to re-assert on the
//! fresh-standalone path). Columns mirror `src/degenbot/database/models/*.py`
//! exactly: big-int columns are `VARCHAR(78)` (the decimal string of an EVM
//! value; mirrors Python's `IntMappedToString` `TypeDecorator`), addresses are
//! `VARCHAR(42)` checksum strings, V4 `pool_hash` is `VARCHAR(66)` 0x-hex.
//! See `rust/CONTEXT.md` "degenbot-db substrate" Decision 3.

/// The Alembic `version_num` the Rust core treats as current.
///
/// This is the tip of `src/degenbot/migrations/versions/` — verified as the head
/// because no migration declares it as a `down_revision`. Bumped in lockstep
/// with a new Alembic migration landing in Epic AZGJUN (the writer path stays
/// Alembic-owned; the Rust core only reads).
pub const ALEMBIC_HEAD: &str = "2606a6c7f5ee";

/// The embedded full-schema DDL, applied ONLY on the fresh-standalone open path
/// (a `cargo add degenbot-db` consumer's own empty file). An Alembic-stamped
/// production DB is NEVER touched by this — see [`crate::migrate::ensure_schema`].
pub const SCHEMA_HEAD: &str = include_str!("schema_head.sql");

/// The private Rust-owned schema-version stamp, written to
/// `_degenbot_db_schema_version` only on the fresh-standalone path so future
/// Rust-owned schema bumps (post-Alembic-retirement) can be tracked independently
/// of Alembic. Bumped when a Rust-owned `ALTER` script ships; out of scope here.
pub const RUST_SCHEMA_VERSION: u32 = 1;

/// Name of the private Rust-owned schema stamp table.
pub const SCHEMA_VERSION_TABLE: &str = "_degenbot_db_schema_version";

// ---------------------------------------------------------------------------
// Table-name constants — single source of truth for the SQL strings in `read.rs`
// and `snapshot.rs`, so a rename touches one place.
// ---------------------------------------------------------------------------

pub mod table {
    //! Canonical table names (mirror `models/*.py` `__tablename__`).

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
            _ => None,
        }
    }

    /// `true` if `kind` is a V3 family discriminator
    /// (a `UniswapV3PoolTableBase` subclass polymorphic identity).
    #[must_use]
    pub fn is_v3_kind(kind: &str) -> bool {
        matches!(
            kind,
            "uniswap_v3" | "sushiswap_v3" | "pancakeswap_v3" | "aerodrome_v3"
        )
    }

    /// `true` if `kind` is a V2 family discriminator.
    #[must_use]
    pub fn is_v2_kind(kind: &str) -> bool {
        matches!(
            kind,
            "uniswap_v2"
                | "aerodrome_v2"
                | "camelot_v2"
                | "pancakeswap_v2"
                | "sushiswap_v2"
                | "swapbased_v2"
        )
    }

    /// `true` if `kind` is a V4 discriminator (only `uniswap_v4` today — the
    /// `managed_pools` polymorphic base's `uniswap_v4` identity used by
    /// `uniswap_v4_pools`).
    #[must_use]
    pub fn is_v4_kind(kind: &str) -> bool {
        matches!(kind, "uniswap_v4")
    }
}
