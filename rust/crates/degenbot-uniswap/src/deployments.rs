//! `(chain_id, factory)`-keyed deployment-identity lookup — the compile-time
//! single source (ADR-005 slice 6 follow-on, Fork A).
//!
//! The canonical `deployments.json` (the same file the Python loader reads) is
//! embedded into the Rust binary via `include_str!` and parsed **once** into a
//! `OnceLock<HashMap<(chain_id, Address), DeploymentRecord>>`. A standalone
//! Rust consumer (no Python) can look up the CREATE2 init hash + deployer for a
//! given ``(chain, factory)`` — the foundation the builder verifies pool
//! addresses against at registration (ADR-005 "standalone constraint").
//!
//! ## The single source
//!
//! `include_str!` resolves to `src/degenbot/registry/deployments.json` — the
//! exact file the Python [`DeploymentRecord`] loader / `pool_type_registry`
//! consume. **There is no second copy.** The pyfunctions exposed by the binding
//! crate (`init_hash_for` / `deployer_for`) are thin views; the truth lives
//! here so the Rust builder and the Python registry cannot drift.
//!
//! ## The separate-deployer case (why this is `(chain, factory)`-keyed)
//!
//! `deployer` is the CREATE2 deployer (the `0xff ++ deployer ++ salt ++
//! init_hash` preimage). Most DEXes set it to `null` in the JSON — meaning
//! "use the factory as deployer". `PancakeSwap` V3 specifies a **separate
//! deployer** (`0x41ff9…`, distinct from the factory). Because the factory is
//! per-chain, the deployer cannot be resolved from the variant alone; the row
//! is keyed by ``(chain_id, factory)`` so the per-row `deployer` is the source
//! of truth. [`effective_deployer`] implements the `None → factory` convention.
//!
//! ## Why this lives in the Uniswap crate
//!
//! These deployment records encode the CREATE2 deployment identity for the
//! Uniswap-V2/V3-protocol family (uniswap-v2/-v3, pancakeswap-v2/-v3,
//! sushiswap-v2/-v3, swapbased, camelot, aerodrome) plus the non-CREATE2
//! Balancer rows the JSON carries (so lookup is exhaustive over shipped
//! deployments). It is protocol-domain data, alongside the [`DexIdentity`]
//! presets — not foundational machinery, so it belongs here rather than in
//! `degenbot-core`.
//!
//! [`DexIdentity`]: crate::dex_identity::DexIdentity

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::OnceLock;

use alloy::primitives::{Address, B256};
use degenbot_core::address_utils;

/// The canonical deployment data file, embedded at compile time.
///
/// Resolves to `src/degenbot/registry/deployments.json` — the single source the
/// Python `deployment_loader` loader and `pool_type_registry` consume (see
/// `src/degenbot/registry/deployment_loader.py`).
const DEPLOYMENTS_JSON: &str =
    include_str!("../../../../src/degenbot/registry/deployments.json");

/// The raw JSON record shape (mirrors the Python loader's schema).
///
/// Only the CREATE2-critical fields are deserialized; `name` / `pool_type` /
/// `variant` / `dex_variant` / `family` are companion-layer concerns (Python
/// resolves `pool_type` → class, `dex_variant` → preset) and are ignored here
/// (serde skips unknown fields by default).
#[derive(serde::Deserialize, Debug)]
struct RawRecord {
    chain_id: u64,
    factory: String,
    #[serde(default)]
    deployer: Option<String>,
    #[serde(default)]
    init_hash: Option<String>,
}

#[derive(serde::Deserialize)]
struct Root {
    deployments: Vec<RawRecord>,
}

/// A parsed deployment row with the CREATE2-critical fields typed.
///
/// Carries the effective deployer (`None` → "use factory" is applied in
/// [`effective_deployer`]) and the init-hash (also optional — Balancer /
/// Aerodrome rows have no CREATE2). Kept separate from
/// [`crate::dex_identity::DexIdentity`] because this is JSON-sourced
/// per-row data (chain-specific, exhaustive over shipped deployments),
/// whereas `DexIdentity` is the compile-time preset table for the
/// variant-keyed identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeploymentRecord {
    /// The chain id this deployment lives on.
    pub chain_id: u64,
    /// The factory contract address.
    pub factory: Address,
    /// The separate CREATE2 deployer, if the JSON specifies one. `None`
    /// means "use [`factory`](Self::factory)" — see [`effective_deployer`].
    pub deployer: Option<Address>,
    /// The CREATE2 init code hash, if the row has CREATE2 address
    /// generation. `None` for Aerodrome/Balancer (no CREATE2).
    pub init_hash: Option<B256>,
}

impl DeploymentRecord {
    /// The effective CREATE2 deployer: the separate `deployer` if set, else
    /// the [`factory`](Self::factory). This is the `null → factory` convention
    /// the Python loader + V2/V3 pool constructors apply.
    #[must_use]
    pub fn effective_deployer(&self) -> Address {
        self.deployer.unwrap_or(self.factory)
    }
}

/// Convenience free function form of [`DeploymentRecord::effective_deployer`].
#[must_use]
pub fn effective_deployer(record: &DeploymentRecord) -> Address {
    record.effective_deployer()
}

/// The parsed lookup table, built once and reused for the process lifetime.
///
/// Keyed by `(chain_id, Address)` so a single `Address` equality (case-
/// insensitive) resolves a row regardless of the JSON's checksum casing.
type Table = HashMap<(u64, Address), DeploymentRecord>;

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        let root: Root = serde_json::from_str(DEPLOYMENTS_JSON)
            .expect("embedded deployments.json must parse (validated at commit time)");
        let mut map = HashMap::with_capacity(root.deployments.len());
        for raw in root.deployments {
            let factory = address_utils::parse_address(&raw.factory)
                .expect("embedded deployments.json factory must be a valid address");
            let deployer = raw
                .deployer
                .as_deref()
                .filter(|s| !s.is_empty())
                .and_then(|s| Address::from_str(s).ok());
            let init_hash = raw
                .init_hash
                .as_deref()
                .filter(|s| !s.is_empty())
                .and_then(|s| B256::from_str(s).ok());
            let chain_id = raw.chain_id;
            map.insert(
                (chain_id, factory),
                DeploymentRecord {
                    chain_id,
                    factory,
                    deployer,
                    init_hash,
                },
            );
        }
        map
    })
}

/// Look up the parsed deployment record for a ``(chain_id, factory)`` pair.
///
/// The lookup is by `Address` equality (case-insensitive on the hex form).
/// Returns `None` for an unregistered ``(chain, factory)``.
///
/// # Examples
///
/// ```
/// use degenbot_uniswap::deployments::lookup;
/// use alloy::primitives::address;
///
/// let uniswap_v2_mainnet = address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
/// let rec = lookup(1, uniswap_v2_mainnet).expect("Uniswap V2 mainnet is shipped");
/// assert!(rec.init_hash.is_some());
/// assert_eq!(rec.effective_deployer(), uniswap_v2_mainnet); // null → factory
/// ```
#[must_use]
pub fn lookup(chain_id: u64, factory: Address) -> Option<&'static DeploymentRecord> {
    table().get(&(chain_id, factory))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, b256};

    const UNISWAP_V2_MAINNET_FACTORY: Address =
        address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
    const PANCAKESWAP_V3_MAINNET_FACTORY: Address =
        address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865");
    const PANCAKESWAP_V3_MAINNET_DEPLOYER: Address =
        address!("41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9");

    #[test]
    fn all_shipped_rows_parse() {
        // Acceptance criterion: every row parses (no serde panic at first
        // access). The table is built lazily; touching it here forces the parse.
        let t = table();
        assert!(t.len() >= 32, "expected at least 32 shipped rows, got {}", t.len());
    }

    #[test]
    fn uniswap_v2_mainnet_lookup() {
        let rec = lookup(1, UNISWAP_V2_MAINNET_FACTORY).expect("Uniswap V2 mainnet present");
        assert_eq!(rec.factory, UNISWAP_V2_MAINNET_FACTORY);
        assert_eq!(
            rec.init_hash,
            Some(b256!(
                "96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
            ))
        );
        // null deployer → effective is the factory.
        assert!(rec.deployer.is_none());
        assert_eq!(rec.effective_deployer(), UNISWAP_V2_MAINNET_FACTORY);
    }

    #[test]
    fn pancakeswap_v3_separate_deployer() {
        // The load-bearing case: a row whose deployer differs from the
        // factory. Proves the (chain, factory) key is required, not variant.
        let rec = lookup(1, PANCAKESWAP_V3_MAINNET_FACTORY).expect("PCS V3 mainnet present");
        assert_eq!(rec.factory, PANCAKESWAP_V3_MAINNET_FACTORY);
        assert_eq!(rec.deployer, Some(PANCAKESWAP_V3_MAINNET_DEPLOYER));
        assert_eq!(rec.effective_deployer(), PANCAKESWAP_V3_MAINNET_DEPLOYER);
        assert!(rec.init_hash.is_some());
    }

    #[test]
    fn lookup_is_case_insensitive() {
        // Address equality, not string equality — lowercase hex must match.
        let lower = Address::from_str("0x5c69bee701ef814a2b6a3edd4b1652cb9cc5aa6f").unwrap();
        let upper = UNISWAP_V2_MAINNET_FACTORY;
        assert_eq!(lookup(1, lower), lookup(1, upper));
    }

    #[test]
    fn unknown_returns_none() {
        assert!(lookup(999_999, Address::ZERO).is_none());
        assert!(lookup(1, Address::ZERO).is_none());
    }

    #[test]
    fn no_create2_row_has_none_init_hash() {
        // Balancer weighted-pool factory (chain 1) — no CREATE2 address gen.
        let balancer = address!("8E9aa87E45e92bad84D5F8DD1bff34Fb92637dE9");
        let rec = lookup(1, balancer).expect("Balancer weighted mainnet present");
        assert!(rec.init_hash.is_none());
    }
}