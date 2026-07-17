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

use crate::create2;

/// The canonical Uniswap V3 mainnet CREATE2 init code hash — the fallback
/// for non-`JSON` V3 pools (the documented default the retired Python `ClassVar`
/// `UNISWAP_V3_MAINNET_POOL_INIT_HASH` carried).
///
/// Used only when `register_v3_pool` is called for a `(chain, factory)` that is
/// NOT in the shipped `deployments.json` (test/ad-hoc pools). JSON-registered
/// pools read the per-row verified init hash from the deployment record
/// (covering the `PancakeSwap` V3 separate-deployer case). Keeping this as a
/// Rust `const` retires the second Python copy without dropping the fallback
/// semantics non-JSON pools rely on (P62DKO).
pub const UNISWAP_V3_MAINNET_INIT_HASH: alloy::primitives::B256 =
    alloy::primitives::b256!("e34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54");

/// The canonical Uniswap V2 mainnet CREATE2 init code hash — the fallback for
/// non-`JSON` V2 pools (the retired Python `ClassVar`
/// `UNISWAP_V2_MAINNET_POOL_INIT_HASH`). Used only when `register_v2_pool` is
/// called for a `(chain, factory)` NOT in the shipped `deployments.json`
/// (test/ad-hoc pools). JSON-registered pools read the per-row verified init
/// hash from the deployment record. Rust `const` retires the Python copy
/// (NSAZ4X).
pub const UNISWAP_V2_MAINNET_INIT_HASH: alloy::primitives::B256 =
    alloy::primitives::b256!("96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f");

/// Resolve the CREATE2 init code hash for a V2 `(chain_id, factory)` pair,
/// with a documented fallback.
///
/// Returns the JSON row's `init_hash` when shipped with a CREATE2 init hash;
/// otherwise [`UNISWAP_V2_MAINNET_INIT_HASH`] (the retired Python `ClassVar`'s
/// documented default for non-`JSON` V2 pools). The V2 builder stores this on
/// the identity; the `dex` getter merges it (NSAZ4X).
#[must_use]
pub fn resolve_v2_init_hash(chain_id: u64, factory: Address) -> alloy::primitives::B256 {
    match lookup(chain_id, factory) {
        Some(rec) => rec.init_hash.unwrap_or(UNISWAP_V2_MAINNET_INIT_HASH),
        None => UNISWAP_V2_MAINNET_INIT_HASH,
    }
}

/// Resolve the effective CREATE2 deployer for a `(chain_id, factory)` pair,
/// with the `None -> factory` convention applied.
///
/// Returns the factory itself when the `(chain, factory)` is not in the
/// shipped JSON (non-JSON pools default to factory as deployer). This is the
/// value the builder stores on the pool identity at registration (Fork A,
/// P62DKO) so the companion reads it off the handle with no runtime
/// `chain_id` plumbing.
#[must_use]
pub fn resolve_deployer(chain_id: u64, factory: Address) -> Address {
    lookup(chain_id, factory).map_or(factory, DeploymentRecord::effective_deployer)
}

/// Resolve the CREATE2 init code hash for a V3 `(chain_id, factory)` pair,
/// with a documented fallback.
///
/// Returns the JSON row's `init_hash` when the `(chain, factory)` shipped with
/// a CREATE2 init hash; otherwise [`UNISWAP_V3_MAINNET_INIT_HASH`] (the
/// retired Python `ClassVar`'s documented default for non-`JSON` V3 pools). This
/// is the value the V3 builder stores on the identity; the companion reads it
/// off the handle (P62DKO).
#[must_use]
pub fn resolve_v3_init_hash(chain_id: u64, factory: Address) -> alloy::primitives::B256 {
    match lookup(chain_id, factory) {
        Some(rec) => rec.init_hash.unwrap_or(UNISWAP_V3_MAINNET_INIT_HASH),
        None => UNISWAP_V3_MAINNET_INIT_HASH,
    }
}

/// The canonical deployment data file, embedded at compile time.
///
/// Resolves to `src/degenbot/registry/deployments.json` — the single source the
/// Python `deployment_loader` loader and `pool_type_registry` consume (see
/// `src/degenbot/registry/deployment_loader.py`).
const DEPLOYMENTS_JSON: &str = include_str!("../../../../src/degenbot/registry/deployments.json");

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
    /// The EIP-1167 master implementation contract Aerodrome factories
    /// clone (V2 `stable`/volatile + V3 Slipstream). Absent for V2/V3
    /// rows that use the standard init-hash CREATE2 path. Aerodrome-only
    /// field (Fork A follow-on, S5SJXF/D7VKQX).
    #[serde(default)]
    implementation_address: Option<String>,
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
    /// The EIP-1167 master implementation contract Aerodrome factories
    /// clone when deriving a pool address (V2 stable/volatile + V3
    /// Slipstream). `None` for V2/V3 rows that use the standard init-hash
    /// CREATE2 path. A standalone Rust consumer derives an Aerodrome pool
    /// address from `(deployer, tokens, stable|tick_spacing,
    /// implementation_address)` with no Python (ADR-005 standalone
    /// constraint). (Fork A follow-on, S5SJXF/D7VKQX.)
    pub implementation_address: Option<Address>,
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
            let implementation_address = raw
                .implementation_address
                .as_deref()
                .filter(|s| !s.is_empty())
                .and_then(|s| Address::from_str(s).ok());
            let chain_id = raw.chain_id;
            map.insert(
                (chain_id, factory),
                DeploymentRecord {
                    chain_id,
                    factory,
                    deployer,
                    init_hash,
                    implementation_address,
                },
            );
        }
        map
    })
}

/// Resolve the EIP-1167 master implementation address for an Aerodrome
/// `(chain_id, factory)` pair, if the row carries one.
///
/// Aerodrome V2 (stable/volatile) + V3 Slipstream factories deploy pools as
/// EIP-1167 minimal-proxy clones of a master implementation contract; the
/// clone address is `keccak256(0x363d…5af43d82803e903d91602b57fd5bf3 ++
/// deployer ++ salt ++ implementation)[12:]` (the salt encodes the token
/// pair + `stable`/`tick_spacing`). V2/V3 rows that use the standard
/// init-hash CREATE2 path have no `implementation_address` and this returns
/// `None`. Used by the Rust builder's registration-time verification + the
/// standalone clone-address derivation (Fork A follow-on, S5SJXF/D7VKQX).
#[must_use]
pub fn implementation_address(chain_id: u64, factory: Address) -> Option<Address> {
    lookup(chain_id, factory).and_then(|rec| rec.implementation_address)
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

/// A CREATE2 address mismatch reported by [`verify_v2_pool_address`] /
/// [`verify_v3_pool_address`].
///
/// Carries enough context to render a clear error:
/// "address does not match CREATE2(deployer, salt, `init_hash`) for chain X
/// factory F: expected E, computed C".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressMismatch {
    /// The chain id used for the deployment lookup.
    pub chain_id: u64,
    /// The factory looked up.
    pub factory: Address,
    /// The effective CREATE2 deployer (JSON `deployer` or `factory`).
    pub deployer: Address,
    /// The CREATE2 init code hash used.
    pub init_hash: B256,
    /// The declared (expected) pool address.
    pub expected: Address,
    /// The recomputed CREATE2 address.
    pub computed: Address,
}

impl std::fmt::Display for AddressMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pool address does not match CREATE2 for chain {} \
             factory {}: deployer={}, init_hash={:#x}, \
             expected={:#x}, computed={:#x}",
            self.chain_id,
            self.factory,
            self.deployer,
            self.init_hash,
            self.expected,
            self.computed
        )
    }
}

impl std::error::Error for AddressMismatch {}

/// Recompute the V2 CREATE2 address for a pool registration against the
/// JSON-sourced deployer + init hash.
///
/// `Ok(())` if the address matches, OR if verification is not applicable:
/// the `(chain_id, factory)` is not in the shipped JSON (manual/ad-hoc
/// registration — verification skipped), or the row carries no CREATE2 init
/// hash (Balancer — not a CREATE2 pool). `Err(AddressMismatch)` if the JSON
/// has the row with a CREATE2 init hash but the recomputed address differs
/// from `expected`.
///
/// This is the load-bearing guard covering the `PancakeSwap` V3
/// separate-deployer case: the deployer is resolved per `(chain, factory)`,
/// not from the variant preset's canonical mainnet factory.
///
/// # Errors
///
/// Returns `Err(AddressMismatch)` only when the `(chain_id, factory)` is in
/// the shipped JSON with a CREATE2 `init_hash` AND the recomputed address
/// differs from `expected`. Returns `Ok(())` (no error) for non-applicable
/// cases (not in JSON / no CREATE2 row).
#[must_use = "the verification result must be checked before registration proceeds"]
pub fn verify_v2_pool_address(
    chain_id: u64,
    factory: Address,
    expected: Address,
    token0: Address,
    token1: Address,
) -> Result<(), AddressMismatch> {
    let Some(rec) = lookup(chain_id, factory) else {
        return Ok(()); // not in JSON → skip (ad-hoc registration preserved)
    };
    let Some(init_hash) = rec.init_hash else {
        return Ok(()); // row has no CREATE2 (e.g. Balancer) → skip
    };
    let deployer = rec.effective_deployer();
    let computed = create2::compute_v2_address(deployer, token0, token1, init_hash);
    if computed == expected {
        Ok(())
    } else {
        Err(AddressMismatch {
            chain_id,
            factory,
            deployer,
            init_hash,
            expected,
            computed,
        })
    }
}

/// Recompute the V3 CREATE2 address for a pool registration against the
/// JSON-sourced deployer + init hash. Mirrors [`verify_v2_pool_address`] with
/// the V3 salt (tokens + fee).
///
/// `Ok(())` on match or not-applicable (see [`verify_v2_pool_address`]);
/// `Err(AddressMismatch)` on a verified mismatch.
///
/// # Errors
///
/// Returns `Err(AddressMismatch)` only when the `(chain_id, factory)` is in
/// the shipped JSON with a CREATE2 `init_hash` AND the recomputed address
/// differs from `expected`. Returns `Ok(())` for non-applicable cases.
#[must_use = "the verification result must be checked before registration proceeds"]
pub fn verify_v3_pool_address(
    chain_id: u64,
    factory: Address,
    expected: Address,
    token0: Address,
    token1: Address,
    fee: u32,
) -> Result<(), AddressMismatch> {
    let Some(rec) = lookup(chain_id, factory) else {
        return Ok(()); // not in JSON → skip (ad-hoc registration preserved)
    };
    let Some(init_hash) = rec.init_hash else {
        return Ok(()); // row has no CREATE2 → skip
    };
    let deployer = rec.effective_deployer();
    let computed = create2::compute_v3_address(deployer, token0, token1, fee, init_hash);
    if computed == expected {
        Ok(())
    } else {
        Err(AddressMismatch {
            chain_id,
            factory,
            deployer,
            init_hash,
            expected,
            computed,
        })
    }
}

// ---------------------------------------------------------------------------
// Registration-time verification (Fork A, JC6OFG)
// ---------------------------------------------------------------------------
//
// The Rust builder recomputes the CREATE2 address from the JSON-sourced
// deployer + init hash + the pool's registration params (tokens / fee) and
// asserts it equals the declared pool address. This is the single correctness
// guard that (a) uses the right deployer per (chain, factory) — covering the
// PancakeSwap V3 separate-deployer case — and (b) retires the per-class
// `_verified_address` second copy.
//
// Semantics:
// - `Ok(())` if the (chain, factory) is in the JSON AND the recomputed CREATE2
//   matches; OR the (chain, factory) is NOT in the JSON (verification skipped —
//   preserves the manual/ad-hoc registration path); OR the row has no CREATE2
//   (init_hash is null, e.g. Balancer — not a CREATE2 pool).
// - `Err(AddressMismatch)` if the (chain, factory) is in the JSON with a
//   CREATE2 init hash but the recomputed address != expected.

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
        assert!(
            t.len() >= 32,
            "expected at least 32 shipped rows, got {}",
            t.len()
        );
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

    // --- Registration-time verification (JC6OFG) ----------------------------
    // Cross-checked addresses (Python `generate_v2/v3_pool_address` reference):
    //   V2 DAI/WETH mainnet:            0xA478c2975Ab1Ea89e8196811F51A7B7Ade33eB11
    //   V3 UNI USDC/WETH 500 mainnet:   0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640
    //   V3 PCS USDC/WETH 500 (sep dep): 0x1ac1A8FEaAEa1900C4166dEeed0C11cC10669D36
    //   V3 PCS USDC/WETH 500 (factory): 0xc1CaD0F6b1Cc9124D71DE161a7F133da6aF93c0D

    const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    const DAI: Address = address!("6B175474E89094C44Da98b954EedeAC495271d0F");
    const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    const UNISWAP_V3_MAINNET_FACTORY: Address =
        address!("1F98431c8aD98523631AE4a59f267346ea31F984");
    const V2_DAI_WETH_ADDR: Address = address!("A478c2975Ab1Ea89e8196811F51A7B7Ade33eB11");
    const V3_UNI_USDC_WETH_500: Address = address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640");
    const V3_PCS_USDC_WETH_500_SEPARATE: Address =
        address!("1ac1A8FEaAEa1900C4166dEeed0C11cC10669D36");
    const V3_PCS_USDC_WETH_500_FACTORY: Address =
        address!("c1CaD0F6b1Cc9124D71DE161a7F133da6aF93c0D");

    #[test]
    fn verify_v2_uniswap_mainnet_round_trips() {
        // Uniswap V2 (factory deployer) — verified CREATE2 round-trip.
        assert!(
            verify_v2_pool_address(1, UNISWAP_V2_MAINNET_FACTORY, V2_DAI_WETH_ADDR, WETH, DAI)
                .is_ok(),
            "Uniswap V2 DAI/WETH mainnet must verify"
        );
    }

    #[test]
    fn verify_v2_wrong_address_is_rejected() {
        // A deliberately-wrong address must fail registration with a mismatch.
        let wrong = address!("0000000000000000000000000000000000000001");
        let err = verify_v2_pool_address(1, UNISWAP_V2_MAINNET_FACTORY, wrong, WETH, DAI)
            .expect_err("wrong V2 address must fail verification");
        assert_eq!(err.expected, wrong);
        assert_eq!(err.chain_id, 1);
        assert_eq!(err.factory, UNISWAP_V2_MAINNET_FACTORY);
        // Deployer is the factory (null in JSON → effective = factory).
        assert_eq!(err.deployer, UNISWAP_V2_MAINNET_FACTORY);
    }

    #[test]
    fn verify_v3_pancakeswap_separate_deployer_passes() {
        // PancakeSwap V3 (separate deployer) — the correct address verifies.
        assert!(verify_v3_pool_address(
            1,
            PANCAKESWAP_V3_MAINNET_FACTORY,
            V3_PCS_USDC_WETH_500_SEPARATE,
            USDC,
            WETH,
            500
        )
        .is_ok());
    }

    #[test]
    fn verify_v3_pancakeswap_factory_as_deployer_is_rejected() {
        // The load-bearing case: computing with the factory substituted as the
        // deployer yields a DIFFERENT address — so the factory-derived address
        // must FAIL verification (the JSON row specifies the separate deployer).
        let err = verify_v3_pool_address(
            1,
            PANCAKESWAP_V3_MAINNET_FACTORY,
            V3_PCS_USDC_WETH_500_FACTORY,
            USDC,
            WETH,
            500,
        )
        .expect_err("the factory-derived address must mismatch the separate-deployer row");
        assert_eq!(err.computed, V3_PCS_USDC_WETH_500_SEPARATE);
        assert_eq!(err.deployer, PANCAKESWAP_V3_MAINNET_DEPLOYER);
    }

    #[test]
    fn verify_v3_uniswap_mainnet_round_trips() {
        assert!(verify_v3_pool_address(
            1,
            UNISWAP_V3_MAINNET_FACTORY,
            V3_UNI_USDC_WETH_500,
            USDC,
            WETH,
            500
        )
        .is_ok());
    }

    #[test]
    fn verify_skips_when_factory_not_in_json() {
        // Ad-hoc / non-JSON registration must still work (verification skipped).
        let ad_hoc_factory = address!("1234567890123456789012345678901234567890");
        let ad_hoc_addr = address!("0000000000000000000000000000000000000000");
        assert!(verify_v2_pool_address(1, ad_hoc_factory, ad_hoc_addr, WETH, DAI).is_ok());
        assert!(verify_v3_pool_address(1, ad_hoc_factory, ad_hoc_addr, WETH, DAI, 500).is_ok());
    }

    #[test]
    fn verify_v2_mismatch_error_renders_context() {
        let wrong = address!("deaddeaddeaddeaddeaddeaddeaddeaddeaddead");
        let err = verify_v2_pool_address(1, UNISWAP_V2_MAINNET_FACTORY, wrong, WETH, DAI)
            .expect_err("must mismatch");
        let msg = err.to_string();
        assert!(msg.contains("chain 1"), "error must mention chain: {msg}");
        assert!(msg.contains("CREATE2"), "error must mention CREATE2: {msg}");
    }

    #[test]
    fn uniswap_v3_mainnet_init_hash_const_matches_json() {
        // The fallback const equals the JSON row's init hash for the Uniswap
        // V3 mainnet factory — so non-`JSON` V3 pools default to the same value
        // JSON-registered Uniswap V3 pools verify against.
        let rec = lookup(1, address!("1F98431c8aD98523631AE4a59f267346ea31F984"))
            .expect("Uniswap V3 mainnet present");
        assert_eq!(rec.init_hash, Some(UNISWAP_V3_MAINNET_INIT_HASH));
    }

    // --- Aerodrome EIP-1167 implementation address (S5SJXF/D7VKQX) --------

    const AERODROME_V2_BASE_FACTORY: Address = address!("420DD381b31aEf6683db6B902084cB0FFECe40Da");
    const AERODROME_V3_BASE_FACTORY: Address = address!("5e7BB104d84c7CB9B682AaC2F3d509f5F406809A");
    const AERODROME_V2_POOL_IMPLEMENTATION: Address =
        address!("A4e46b4f701c62e14DF11B48dCe76A7d793CD6d7");
    const AERODROME_V3_POOL_IMPLEMENTATION: Address =
        address!("eC8E5342B19977B4eF8892e02D8DAEcfa1315831");

    #[test]
    fn aerodrome_v2_base_row_carries_implementation_address() {
        // The Aerodrome V2 Base factory ships with the EIP-1167 master
        // implementation address the factory clones — so a standalone Rust
        // consumer can derive a pool address with no Python (ADR-005).
        let rec = lookup(8453, AERODROME_V2_BASE_FACTORY).expect("Aerodrome V2 Base present");
        assert_eq!(rec.factory, AERODROME_V2_BASE_FACTORY);
        // Aerodrome has no CREATE2 init-hash path — the EIP-1167 clone key
        // replaces it.
        assert!(rec.init_hash.is_none());
        assert_eq!(
            rec.implementation_address,
            Some(AERODROME_V2_POOL_IMPLEMENTATION)
        );
        assert_eq!(
            implementation_address(8453, AERODROME_V2_BASE_FACTORY),
            Some(AERODROME_V2_POOL_IMPLEMENTATION)
        );
    }

    #[test]
    fn aerodrome_v3_slipstream_row_carries_implementation_address() {
        // The Aerodrome V3 (Slipstream) Base factory ships with the EIP-1167
        // master implementation the CL factory clones.
        let rec = lookup(8453, AERODROME_V3_BASE_FACTORY).expect("Aerodrome V3 Base present");
        assert_eq!(rec.factory, AERODROME_V3_BASE_FACTORY);
        assert!(rec.init_hash.is_none());
        assert_eq!(
            rec.implementation_address,
            Some(AERODROME_V3_POOL_IMPLEMENTATION)
        );
        assert_eq!(
            implementation_address(8453, AERODROME_V3_BASE_FACTORY),
            Some(AERODROME_V3_POOL_IMPLEMENTATION)
        );
    }

    #[test]
    fn non_aerodrome_rows_have_no_implementation_address() {
        // V2/V3 rows use the standard init-hash CREATE2 path — no EIP-1167
        // master to clone, so the field stays None.
        let v2 = lookup(1, UNISWAP_V2_MAINNET_FACTORY).expect("Uniswap V2 mainnet present");
        assert!(v2.implementation_address.is_none());
        assert!(implementation_address(1, UNISWAP_V2_MAINNET_FACTORY).is_none());
    }
}
