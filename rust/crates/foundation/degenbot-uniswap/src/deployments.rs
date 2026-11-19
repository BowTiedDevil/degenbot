//! `(chain_id, factory)`-keyed deployment-identity lookup — the compile-time
//! single source (Fork A).
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
//! The canonical registry file is `src/degenbot/registry/deployments.json` in
//! the Python tree — what the Python `DeploymentRecord` loader /
//! `pool_type_registry` consume. It is mirrored byte-identically at
//! `src/deployments.json` in this crate (a crate can only `include_str!` files
//! inside its own directory — the published tarball), and `just test-rust`
//! byte-compares the two, so this is a single logical source with zero drift
//! enforced: the pyfunctions exposed by the binding crate
//! (`init_hash_for` / `deployer_for`) are thin views over it.
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
use crate::dex_identity::DexName;

/// The measurement behind [`InitHashProvenance::AssumedMainnet`] for V3: the
/// canonical Uniswap V3 mainnet CREATE2 init code hash, i.e. the default the
/// retired Python `ClassVar` `UNISWAP_V3_MAINNET_POOL_INIT_HASH` carried.
///
/// The value is measured for the Uniswap V3 mainnet factory alone. A
/// `(chain, factory)` the shipped `deployments.json` does not name resolves to
/// it as an ASSUMED hash — admitted by [`admit_init_hash`] on
/// [`MAINNET_CHAIN_ID`] and the [`PLACEHOLDER_CHAIN_ID`] ad-hoc path, refused
/// on every real network. JSON-registered pools read their row's verified init
/// hash instead (covering the `PancakeSwap` V3 separate-deployer case).
pub const UNISWAP_V3_MAINNET_INIT_HASH: alloy::primitives::B256 =
    alloy::primitives::b256!("e34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54");

/// The measurement behind [`InitHashProvenance::AssumedMainnet`] for V2: the
/// canonical Uniswap V2 mainnet CREATE2 init code hash, i.e. the default the
/// retired Python `ClassVar` `UNISWAP_V2_MAINNET_POOL_INIT_HASH` carried.
///
/// The value is measured for the Uniswap V2 mainnet factory alone; a
/// `(chain, factory)` the shipped `deployments.json` does not name resolves to
/// it as an ASSUMED hash (see [`UNISWAP_V3_MAINNET_INIT_HASH`] for the
/// admission policy). JSON-registered pools read their row's verified init
/// hash from the deployment record.
pub const UNISWAP_V2_MAINNET_INIT_HASH: alloy::primitives::B256 =
    alloy::primitives::b256!("96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f");

/// The one chain the documented mainnet init-hash default belongs to.
pub const MAINNET_CHAIN_ID: u64 = 1;

/// The chain every offline/ad-hoc consumer registers under: `Bot()` builds a
/// `PyBot::new(chain_id = 0)` (also the bare-engine session scope), and the
/// test fixtures register synthetic factories on it. No network exists at
/// chain 0, so a CREATE2 address can never be misderived against a real
/// deployment there — which is exactly what makes the documented mainnet
/// default harmless on it.
pub const PLACEHOLDER_CHAIN_ID: u64 = 0;

/// Whether `chain_id` is one the documented mainnet default is admitted on: the
/// chain the Uniswap hashes were measured for, or the offline placeholder.
#[must_use]
pub const fn admits_assumed_default(chain_id: u64) -> bool {
    chain_id == MAINNET_CHAIN_ID || chain_id == PLACEHOLDER_CHAIN_ID
}

/// The provenance of a resolved CREATE2 init code hash.
///
/// A bare hash cannot say whether it was measured or guessed, and a guessed
/// hash derives a wrong pool address with no signal anywhere downstream.
/// Resolution answers this type instead; [`admit_init_hash`] is the one place
/// an unverified outcome may cross into a pool identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitHashProvenance {
    /// The shipped `deployments.json` row measures this hash: verbatim from its
    /// `init_hash`, or derived from its verified `implementation_address` for an
    /// EIP-1167 clone row (the clone bytecode is that deployment's init code).
    Record(B256),
    /// No shipped row names this `(chain, factory)`: the value is the
    /// documented Uniswap mainnet default the retired Python `ClassVar`
    /// carried, not a measurement of this deployment.
    AssumedMainnet(B256),
    /// The shipped row carries neither an `init_hash` nor an EIP-1167
    /// `implementation_address` (the Balancer rows): its pools are not
    /// address-derived at all, so there is no init hash to resolve.
    NoCreate2,
}

impl InitHashProvenance {
    /// The hash this provenance carries, if any.
    #[must_use]
    pub fn init_hash(self) -> Option<B256> {
        match self {
            Self::Record(init_hash) | Self::AssumedMainnet(init_hash) => Some(init_hash),
            Self::NoCreate2 => None,
        }
    }

    /// Whether the hash is the shipped row's measurement (the verified case).
    #[must_use]
    pub fn is_verified(self) -> bool {
        matches!(self, Self::Record(_))
    }
}

/// A resolved init hash that must not reach a pool identity: the
/// `(chain, factory)` has no shipped record and the chain is not the one the
/// documented mainnet default belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnverifiedInitHash {
    /// The chain the resolution asked about.
    pub chain_id: u64,
    /// The factory the resolution asked about.
    pub factory: Address,
    /// What resolution answered.
    pub provenance: InitHashProvenance,
}

impl std::fmt::Display for UnverifiedInitHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let source = match self.provenance {
            InitHashProvenance::Record(_) => "a shipped deployment record",
            InitHashProvenance::AssumedMainnet(_) => {
                "no shipped deployment row, so only the Uniswap mainnet default \
                 the retired Python `ClassVar` carried"
            }
            InitHashProvenance::NoCreate2 => {
                "a shipped deployment row with neither a CREATE2 init hash nor an \
                 EIP-1167 clone implementation"
            }
        };
        write!(
            f,
            "refusing to store an unverified CREATE2 init hash for chain {} factory {}: \
             {source} — the mainnet default is admitted on chain {MAINNET_CHAIN_ID} and the \
             chain-{PLACEHOLDER_CHAIN_ID} placeholder only, so a real network's pool identity \
             must come from a shipped deployment record",
            self.chain_id, self.factory
        )
    }
}

impl std::error::Error for UnverifiedInitHash {}

/// The one admission policy for a resolved init-hash provenance.
///
/// [`InitHashProvenance::Record`] passes through: a shipped row's hash is
/// measured, so a consumer may store it on the chain that row names (and the
/// register-time CREATE2 verify round-trips it).
///
/// [`InitHashProvenance::AssumedMainnet`] is admitted on the two chains the
/// default is truthful on (see [`admits_assumed_default`]): [`MAINNET_CHAIN_ID`],
/// where those Uniswap hashes were measured, and [`PLACEHOLDER_CHAIN_ID`], the
/// offline/ad-hoc scope no real deployment exists on. Every real network refuses.
///
/// [`InitHashProvenance::NoCreate2`] never admits: a row with neither an init
/// hash nor a clone implementation (the Balancer rows) has no address-derived
/// identity to complete, and inventing the mainnet guess for it is the silent
/// default this policy removes.
///
/// # Errors
///
/// [`UnverifiedInitHash`] naming the chain and factory.
pub fn admit_init_hash(
    chain_id: u64,
    factory: Address,
    provenance: InitHashProvenance,
) -> Result<B256, UnverifiedInitHash> {
    match provenance {
        InitHashProvenance::Record(init_hash) => Ok(init_hash),
        InitHashProvenance::AssumedMainnet(init_hash) if admits_assumed_default(chain_id) => {
            Ok(init_hash)
        }
        InitHashProvenance::AssumedMainnet(_) | InitHashProvenance::NoCreate2 => {
            Err(UnverifiedInitHash {
                chain_id,
                factory,
                provenance,
            })
        }
    }
}

/// Resolve the CREATE2 init code hash provenance for a V2 `(chain_id, factory)`
/// pair.
///
/// A shipped row answers [`InitHashProvenance::Record`] with its measured hash —
/// its own `init_hash`, or the init code hash of the EIP-1167 clone its
/// `implementation_address` names (the Aerodrome V2/V3 rows deploy pools as
/// minimal-proxy clones of that implementation). A row carrying neither (the
/// Balancer rows) answers [`InitHashProvenance::NoCreate2`]; an unshipped
/// `(chain, factory)` answers
/// [`InitHashProvenance::AssumedMainnet`] carrying
/// [`UNISWAP_V2_MAINNET_INIT_HASH`]. A consumer storing the value on an
/// identity goes through [`admit_init_hash`], the one place the unverified
/// outcomes are allowed to cross.
#[must_use]
pub fn resolve_v2_init_hash(chain_id: u64, factory: Address) -> InitHashProvenance {
    match lookup(chain_id, factory) {
        Some(record) => match measured_init_hash(record) {
            Some(init_hash) => InitHashProvenance::Record(init_hash),
            None => InitHashProvenance::NoCreate2,
        },
        None => InitHashProvenance::AssumedMainnet(UNISWAP_V2_MAINNET_INIT_HASH),
    }
}

/// Resolve the effective CREATE2 deployer for a `(chain_id, factory)` pair,
/// with the `None -> factory` convention applied.
///
/// Returns the factory itself when the `(chain, factory)` is not in the
/// shipped JSON (non-JSON pools default to factory as deployer). This is the
/// value the builder stores on the pool identity at registration (Fork A) so the companion reads it off the handle with no runtime
/// `chain_id` plumbing.
#[must_use]
pub fn resolve_deployer(chain_id: u64, factory: Address) -> Address {
    lookup(chain_id, factory).map_or(factory, DeploymentRecord::effective_deployer)
}

/// Resolve the CREATE2 init code hash provenance for a V3 `(chain_id, factory)`
/// pair — the V3 twin of [`resolve_v2_init_hash`], carrying
/// [`UNISWAP_V3_MAINNET_INIT_HASH`] as its assumed default.
#[must_use]
pub fn resolve_v3_init_hash(chain_id: u64, factory: Address) -> InitHashProvenance {
    match lookup(chain_id, factory) {
        Some(record) => match measured_init_hash(record) {
            Some(init_hash) => InitHashProvenance::Record(init_hash),
            None => InitHashProvenance::NoCreate2,
        },
        None => InitHashProvenance::AssumedMainnet(UNISWAP_V3_MAINNET_INIT_HASH),
    }
}

/// The canonical deployment data, embedded at compile time.
///
/// `src/deployments.json` is a vendored, byte-identical mirror of the canonical
/// `src/degenbot/registry/deployments.json` (Python tree — the file the
/// `deployment_loader` and `pool_type_registry` consume). The include points at
/// the mirror because a published crate may only embed files from inside its
/// own directory (the tarball); `just test-rust` cmps the two copies, so the
/// mirror cannot drift from the canonical registry.
const DEPLOYMENTS_JSON: &str = include_str!("deployments.json");

/// The raw JSON record shape, exactly as the deployments file spells it.
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
    /// The DEX name label (`"Uniswap V2"`, `"SushiSwap V3"`, …).
    /// Consumed *only* for DEX-name resolution — `pool_type` collapses all
    /// V2 variants to `"uniswap-v2"` and `dex_variant` is mostly absent, so
    /// `name` is the single complete per-row DEX discriminator.
    #[serde(default)]
    name: Option<String>,
    /// The JSON `pool_type` label (`"uniswap-v2"`, `"pancakeswap-v3"`,
    /// …). Needed by the concentrated liquidity slot-layout discriminator (VERIFY2 T4): its
    /// V2 rows collapse to `"uniswap-v2"`-style slugs, so the slug carries
    /// the V2-vs-V3 distinction the `name` label alone cannot.
    #[serde(default)]
    pool_type: Option<String>,
    /// The EIP-1167 master implementation contract Aerodrome factories
    /// clone (V2 `stable`/volatile + V3 Slipstream). Absent for V2/V3
    /// rows that use the standard init-hash CREATE2 path. Aerodrome-only
    /// field (Fork A follow-on).
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
/// [`effective_deployer`]) and the row's own `init_hash` (optional: an EIP-1167
/// clone row derives its init code hash from its
/// [`implementation_address`](Self::implementation_address) instead, and a
/// Balancer row has no address-derived identity at all). Kept separate from
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
    /// The CREATE2 init code hash as the JSON spells it. `None` when the row
    /// does not measure it directly: the EIP-1167 clone rows (Aerodrome) derive
    /// their hash from [`implementation_address`](Self::implementation_address),
    /// and the Balancer rows have no address-derived identity at all. Read it
    /// through [`measured_init_hash`] rather than this field.
    pub init_hash: Option<B256>,
    /// The EIP-1167 master implementation contract Aerodrome factories
    /// clone when deriving a pool address (V2 stable/volatile + V3
    /// Slipstream). `None` for V2/V3 rows that use the standard init-hash
    /// CREATE2 path. A standalone Rust consumer derives an Aerodrome pool
    /// address from `(deployer, tokens, stable|tick_spacing,
    /// implementation_address)` with no Python (ADR-005 standalone
    /// constraint). (Fork A follow-on.)
    pub implementation_address: Option<Address>,
    /// The resolved DEX name for this deployment (derived from the JSON
    /// `name` label). `None` if the label names no known DEX.
    pub dex: Option<DexName>,
    /// The JSON `pool_type` slug (`"uniswap-v3"`, `"pancakeswap-v3"`,
    /// …) — the V2/V3 structural discriminator, exactly what the Python
    /// `pool_type_registry` keys on.
    pub pool_type: Option<Box<str>>,
}

/// Map a `deployments.json` `name` label to a [`DexName`].
///
/// `pool_type` collapses every V2-family row to `"uniswap-v2"` and
/// `dex_variant` is absent for most rows, so the human `name` label
/// (`"SushiSwap V2"`, `"PancakeSwap V3"`, `"Aerodrome V2"`, …) is the only
/// complete per-row DEX discriminator — matched here by a lowercase prefix.
/// Returns `None` for a label that names no known DEX.
#[must_use]
pub fn dex_name_from_label(label: &str) -> Option<DexName> {
    let lower = label.to_ascii_lowercase();
    if lower.starts_with("uniswap") {
        Some(DexName::Uniswap)
    } else if lower.starts_with("sush") {
        Some(DexName::SushiSwap)
    } else if lower.starts_with("pancakeswap") || lower.starts_with("pancake") {
        Some(DexName::PancakeSwap)
    } else if lower.starts_with("camelot") {
        Some(DexName::Camelot)
    } else if lower.starts_with("aerodrome") {
        Some(DexName::Aerodrome)
    } else if lower.starts_with("swapbased") {
        Some(DexName::SwapBased)
    } else if lower.starts_with("balancer") {
        Some(DexName::Balancer)
    } else {
        None
    }
}

/// The row's measured CREATE2 init code hash.
///
/// A standard CREATE2 row measures it directly (`init_hash`). An EIP-1167 clone
/// row (the Aerodrome V2/V3 deployments) sets no `init_hash` because its init
/// code is generated per pool: it is the minimal-proxy bytecode pinned to the
/// row's verified `implementation_address`, whose keccak is
/// [`create2::eip1167_init_code_hash`]. A row with neither measures nothing —
/// its pools are not address-derived.
#[must_use]
fn measured_init_hash(record: &DeploymentRecord) -> Option<B256> {
    record.init_hash.or_else(|| {
        record
            .implementation_address
            .map(create2::eip1167_init_code_hash)
    })
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

/// Why a deployments JSON text could not become a [`DeploymentTable`].
///
/// The shipped table is validated at commit time, so these variants surface
/// only for caller-supplied JSON — nothing in the bot silently degrades on a
/// malformed table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeploymentTableError {
    /// The text does not match the deployments schema.
    Json(String),
    /// A row's `factory` is not a parseable address.
    Factory {
        /// The chain id of the offending row.
        chain_id: u64,
        /// The raw `factory` text that failed to parse.
        value: String,
    },
}

impl std::fmt::Display for DeploymentTableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(message) => write!(f, "deployments JSON does not parse: {message}"),
            Self::Factory { chain_id, value } => write!(
                f,
                "deployments JSON row on chain {chain_id} has an unparseable factory {value:?}"
            ),
        }
    }
}

impl std::error::Error for DeploymentTableError {}

/// A parsed deployments table, keyed by `(chain_id, Address)`.
///
/// The key is the pair so a single `Address` equality (case-insensitive)
/// resolves a row regardless of the JSON's checksum casing. [`from_json`] is
/// the one parser: the shipped constant (`table()`) and any caller-supplied
/// JSON (a unit test's minimal table) both build through it, so the registry
/// is testable without the shipped file.
///
/// [`from_json`]: Self::from_json
#[derive(Clone, Debug, Default)]
pub struct DeploymentTable {
    rows: HashMap<(u64, Address), DeploymentRecord>,
}

impl DeploymentTable {
    /// Parse a deployments table from `deployments.json` text.
    ///
    /// # Errors
    ///
    /// Returns [`DeploymentTableError::Json`] when the text is not a
    /// `{"deployments": [...]}` document, and
    /// [`DeploymentTableError::Factory`] when a row's `factory` is not an
    /// address.
    pub fn from_json(json: &str) -> Result<Self, DeploymentTableError> {
        let root: Root = serde_json::from_str(json)
            .map_err(|err| DeploymentTableError::Json(err.to_string()))?;
        let mut rows = HashMap::with_capacity(root.deployments.len());
        for raw in root.deployments {
            let factory = address_utils::parse_address(&raw.factory).map_err(|_| {
                DeploymentTableError::Factory {
                    chain_id: raw.chain_id,
                    value: raw.factory.clone(),
                }
            })?;
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
            let dex = raw
                .name
                .as_deref()
                .filter(|s| !s.is_empty())
                .and_then(dex_name_from_label);
            let pool_type: Option<Box<str>> = raw
                .pool_type
                .filter(|s| !s.is_empty())
                .map(String::into_boxed_str);
            let chain_id = raw.chain_id;
            rows.insert(
                (chain_id, factory),
                DeploymentRecord {
                    chain_id,
                    factory,
                    deployer,
                    init_hash,
                    implementation_address,
                    dex,
                    pool_type,
                },
            );
        }
        Ok(Self { rows })
    }

    /// The record for a `(chain_id, factory)` pair, or `None` when absent.
    #[must_use]
    pub fn get(&self, chain_id: u64, factory: Address) -> Option<&DeploymentRecord> {
        self.rows.get(&(chain_id, factory))
    }

    /// Every record in the table; the order is unspecified (the backing map
    /// is a `HashMap`).
    pub fn records(&self) -> impl Iterator<Item = &DeploymentRecord> {
        self.rows.values()
    }
}

/// The shipped table, parsed once and reused for the process lifetime.
fn table() -> &'static DeploymentTable {
    static TABLE: OnceLock<DeploymentTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        // Embedded, commit-time-validated constant; a parse failure is a build
        // artifact regression, so a loud panic (targeted expect, fulfilled) with
        // no error channel from the `&'static DeploymentTable` OnceLock
        // initializer.
        #[expect(clippy::expect_used)]
        DeploymentTable::from_json(DEPLOYMENTS_JSON)
            .expect("embedded deployments.json must parse (validated at commit time)")
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
/// standalone clone-address derivation (Fork A follow-on).
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
    table().get(chain_id, factory)
}

/// Iterate every parsed deployment record in the shipped `deployments.json`.
///
/// The single-row [`lookup`] cannot answer "is this factory anywhere in the
/// shipped table", which the species-manifest reconciliation gate needs. The
/// iterator order is unspecified (the backing map is a `HashMap`).
pub fn records() -> impl Iterator<Item = &'static DeploymentRecord> {
    table().records()
}

/// Resolve the DEX name for a `(chain_id, factory)` pool deployment.
///
/// Returns the deployment's derived [`DexName`] when the `(chain, factory)` is
/// in the shipped `deployments.json` and its `name` label names a known DEX;
/// otherwise `None` (unknown deployment → the caller degrades to a generic
/// variant, never an error). This is the Rust-owned, single-source lookup the
/// structural `Pool` handle uses to surface a DEX name on `Identity`.
#[must_use]
pub fn resolve_dex_name(chain_id: u64, factory: Address) -> Option<DexName> {
    lookup(chain_id, factory).and_then(|rec| rec.dex)
}

/// Whether the `(chain, factory)` deployment row names a Pancakes V3
/// deployment — the concentrated liquidity slot-layout discriminator.
/// Unknown deployments (not in the shipped JSON) return `false`; callers
/// degrade to the canonical Uniswap layout unless the driver passes an
/// explicit layout override. Only meaningful for V3 pools (the V2 pancake
/// pools share the `DexName::PancakeSwap` label).
#[must_use]
pub fn is_pancakeswap_v3_factory(chain_id: u64, factory: Address) -> bool {
    matches!(
        lookup(chain_id, factory),
        Some(rec) if rec.pool_type.as_deref() == Some("pancakeswap-v3")
    )
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
/// `Ok(())` if the address matches, OR if the standard-salt check does not
/// apply: resolution answered an unverified provenance — no shipped row (the
/// manual/ad-hoc path) or a row with no address-derived identity (Balancer) —
/// or the row is an EIP-1167 clone (its address is derived from its
/// `implementation_address`, which [`verify_aerodrome_v2_pool_address`] checks).
/// `Err(AddressMismatch)` when the row is a standard CREATE2 row and the
/// recomputed address differs from `expected`.
///
/// The applicability condition is read off [`resolve_v2_init_hash`] plus the
/// row's clone shape rather than re-derived from `init_hash`, so "this guard has
/// something to check" and "the row's identity is a standard CREATE2 hash" are
/// the same fact.
///
/// This is the load-bearing guard covering the `PancakeSwap` V3
/// separate-deployer case: the deployer is resolved per `(chain, factory)`,
/// not from the variant preset's canonical mainnet factory.
///
/// # Errors
///
/// Returns `Err(AddressMismatch)` only for a standard CREATE2 row whose
/// recomputed address differs from `expected`. Returns `Ok(())` (no error) for
/// the unverified cases (no shipped row / no address-derived identity) and for
/// the EIP-1167 clone rows the clone guard owns.
#[must_use = "the verification result must be checked before registration proceeds"]
pub fn verify_v2_pool_address(
    chain_id: u64,
    factory: Address,
    expected: Address,
    token0: Address,
    token1: Address,
) -> Result<(), AddressMismatch> {
    let InitHashProvenance::Record(init_hash) = resolve_v2_init_hash(chain_id, factory) else {
        // Unverified (no shipped row, or a row with no address-derived
        // identity): there is nothing to check the declared address against.
        return Ok(());
    };
    if implementation_address(chain_id, factory).is_some() {
        // An EIP-1167 clone row derives its pool address from its
        // `implementation_address` rather than the standard V2 salt, so the
        // clone's own guard ([`verify_aerodrome_v2_pool_address`]) owns it.
        return Ok(());
    }
    let deployer = resolve_deployer(chain_id, factory);
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

/// The boot layout probe's verdict for one sampled fork pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutVerdict {
    /// The layout's liquidity slot holds the on-chain value.
    Conforms,
    /// Both sources read zero — a dead pool agrees with any layout, so this
    /// sample proves nothing and must be skipped.
    Inconclusive,
    /// On-chain `liquidity()` and the layout's storage slot disagree — the
    /// fork table label is dishonest.
    Mismatch,
}

/// Classify one sampled pool: on-chain `liquidity()` vs the word read from
/// the layout's liquidity slot (the caller pre-masks to the field width).
#[must_use]
pub fn layout_verdict(onchain_liq: u128, stored_liq: u128) -> LayoutVerdict {
    match (onchain_liq, stored_liq) {
        (0, 0) => LayoutVerdict::Inconclusive,
        (a, b) if a == b => LayoutVerdict::Conforms,
        _ => LayoutVerdict::Mismatch,
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
    let InitHashProvenance::Record(init_hash) = resolve_v3_init_hash(chain_id, factory) else {
        // Unverified — see `verify_v2_pool_address`.
        return Ok(());
    };
    if implementation_address(chain_id, factory).is_some() {
        // An EIP-1167 clone row — see `verify_v2_pool_address`; the clone's
        // own guard ([`verify_aerodrome_v3_pool_address`]) owns the check.
        return Ok(());
    }
    let deployer = resolve_deployer(chain_id, factory);
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
// Registration-time verification (Fork A)
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
// - `Ok(())` if the (chain, factory) resolves to a `Record` init hash AND the
//   recomputed CREATE2 matches; OR resolution answered an unverified provenance
//   (not in the JSON — the manual/ad-hoc path — or a row with no address-derived
//   identity, e.g. Balancer); OR the row is an EIP-1167 clone row, whose address
//   the clone guard checks instead.
// - `Err(AddressMismatch)` if resolution answered `Record` for a standard
//   CREATE2 row but the recomputed address != expected.

/// Recompute an Aerodrome V2 EIP-1167 clone address for a pool registration
/// against the JSON-sourced deployer + implementation address. Mirrors
/// [`verify_v2_pool_address`] with the Aerodrome V2 salt (tokens + `stable`).
///
/// `Ok(())` on match or not-applicable (see [`verify_v2_pool_address`]);
/// `Err(AddressMismatch)` on a verified mismatch. The reported `init_hash`
/// field is the CREATE2 init code hash for the EIP-1167 clone —
/// [`create2::eip1167_init_code_hash`] of the implementation — so the
/// mismatch context reproduces the same derivation.
///
/// # Errors
///
/// Returns `Err(AddressMismatch)` only when the `(chain_id, factory)` is in
/// the shipped JSON with an `implementation_address` AND the recomputed
/// EIP-1167 clone address differs from `expected`. Returns `Ok(())` for
/// non-applicable cases (Fork A follow-on).
#[must_use = "the verification result must be checked before registration proceeds"]
pub fn verify_aerodrome_v2_pool_address(
    chain_id: u64,
    factory: Address,
    expected: Address,
    token0: Address,
    token1: Address,
    stable: bool,
) -> Result<(), AddressMismatch> {
    let Some(rec) = lookup(chain_id, factory) else {
        return Ok(()); // not in JSON → skip (ad-hoc registration preserved)
    };
    let Some(implementation) = rec.implementation_address else {
        return Ok(()); // row has no EIP-1167 clone path → skip
    };
    let deployer = rec.effective_deployer();
    let computed =
        create2::compute_aerodrome_v2_address(deployer, token0, token1, stable, implementation);
    let init_hash = create2::eip1167_init_code_hash(implementation);
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

/// Recompute an Aerodrome V3 (Slipstream) EIP-1167 clone address for a pool
/// registration against the JSON-sourced deployer + implementation address.
/// Mirrors [`verify_aerodrome_v2_pool_address`] with the V3 salt (tokens +
/// `tick_spacing`).
///
/// # Errors
///
/// Returns `Err(AddressMismatch)` only when the `(chain_id, factory)` is in
/// the shipped JSON with an `implementation_address` AND the recomputed
/// EIP-1167 clone address differs from `expected`. Returns `Ok(())` for
/// non-applicable cases (Fork A follow-on).
#[must_use = "the verification result must be checked before registration proceeds"]
pub fn verify_aerodrome_v3_pool_address(
    chain_id: u64,
    factory: Address,
    expected: Address,
    token0: Address,
    token1: Address,
    tick_spacing: i32,
) -> Result<(), AddressMismatch> {
    let Some(rec) = lookup(chain_id, factory) else {
        return Ok(()); // not in JSON → skip (ad-hoc registration preserved)
    };
    let Some(implementation) = rec.implementation_address else {
        return Ok(()); // row has no EIP-1167 clone path → skip
    };
    let deployer = rec.effective_deployer();
    let computed = create2::compute_aerodrome_v3_address(
        deployer,
        token0,
        token1,
        tick_spacing,
        implementation,
    );
    let init_hash = create2::eip1167_init_code_hash(implementation);
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

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
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
        let rows = t.records().count();
        assert!(rows >= 32, "expected at least 32 shipped rows, got {rows}");
    }

    #[test]
    fn from_json_builds_a_table_without_the_shipped_file() {
        // Minimal two-row table: proves the constructor is pure (no shipped
        // JSON, no global OnceLock) so registry consumers are unit-testable.
        let json = r#"{
            "deployments": [
                {
                    "name": "Uniswap V2",
                    "chain_id": 1,
                    "pool_type": "uniswap-v2",
                    "factory": "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
                    "deployer": null,
                    "init_hash": "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
                },
                {
                    "name": "PancakeSwap V3",
                    "chain_id": 1,
                    "pool_type": "pancakeswap-v3",
                    "factory": "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865",
                    "deployer": "0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9",
                    "init_hash": ""
                }
            ]
        }"#;
        let table = DeploymentTable::from_json(json).expect("minimal table parses");
        assert_eq!(table.records().count(), 2);

        let v2 = table
            .get(1, UNISWAP_V2_MAINNET_FACTORY)
            .expect("v2 row present");
        assert_eq!(v2.pool_type.as_deref(), Some("uniswap-v2"));
        assert_eq!(v2.effective_deployer(), UNISWAP_V2_MAINNET_FACTORY);
        assert_eq!(v2.init_hash, Some(UNISWAP_V2_MAINNET_INIT_HASH));
        assert_eq!(v2.dex, Some(DexName::Uniswap));

        let v3 = table
            .get(1, PANCAKESWAP_V3_MAINNET_FACTORY)
            .expect("v3 row present");
        assert_eq!(v3.pool_type.as_deref(), Some("pancakeswap-v3"));
        assert_eq!(v3.effective_deployer(), PANCAKESWAP_V3_MAINNET_DEPLOYER);
        assert_eq!(
            v3.init_hash, None,
            "an empty init_hash parses to None (the row shape decides the provenance)"
        );

        assert!(table
            .get(1, address!("1234567890123456789012345678901234567890"))
            .is_none());
    }

    #[test]
    fn from_json_reports_schema_and_address_faults() {
        let err = DeploymentTable::from_json("[]").expect_err("a JSON array is not the schema");
        assert!(matches!(err, DeploymentTableError::Json(_)));

        let err = DeploymentTable::from_json(
            r#"{"deployments": [{"chain_id": 1, "factory": "0xnot-an-address"}]}"#,
        )
        .expect_err("an unparseable factory is a loud fault");
        assert_eq!(
            err,
            DeploymentTableError::Factory {
                chain_id: 1,
                value: "0xnot-an-address".to_owned(),
            }
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

    // --- concentrated liquidity slot-layout discriminator -----------------

    #[test]
    fn pancake_v3_factories_discriminate_from_uniswap_and_unknown() {
        // Shipped pancake V3 rows (chains 1 + 8453, same factory address).
        assert!(is_pancakeswap_v3_factory(
            1,
            address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865")
        ));
        assert!(is_pancakeswap_v3_factory(
            8453,
            address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865")
        ));

        // The Uniswap V3 mainnet factory is NOT pancake.
        let uni_v3 = address!("1F98431c8aD98523631AE4a59f267346ea31F984");
        assert!(!is_pancakeswap_v3_factory(1, uni_v3));

        // A pancake V2 row shares the DexName label — the discriminator is
        // only consulted on the V3 registration path, and an unknown (chain,
        // factory) degrades to the canonical layout.
        let pancake_v2_factory = address!("1097053Fd2ea711dad45caCcc45EfF7548fCB362");
        assert!(!is_pancakeswap_v3_factory(1, pancake_v2_factory));
    }

    // --- Registration-time verification ----------------------------
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

    // --- Aerodrome EIP-1167 implementation address --------

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
        // Aerodrome sets no `init_hash`: its pools are EIP-1167 clones, so the
        // row's init code hash derives from the implementation address.
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
        // master implementation the concentrated liquidity factory clones.
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

    // --- Aerodrome register-time verification -------------

    #[test]
    fn verify_aerodrome_v2_round_trips() {
        // The correct on-chain AERO/WETH V2 volatile address verifies.
        let expected = address!("7f670f78B17dEC44d5Ef68a48740b6f8849cc2e6");
        assert!(verify_aerodrome_v2_pool_address(
            8453,
            AERODROME_V2_BASE_FACTORY,
            expected,
            address!("4200000000000000000000000000000000000006"),
            address!("940181a94a35a4569e4529a3cdfb74e38fd98631"),
            false,
        )
        .is_ok());
    }

    #[test]
    fn verify_aerodrome_v2_rejects_wrong_address() {
        // A deliberately-wrong address must fail verification with a mismatch.
        let wrong = address!("0000000000000000000000000000000000000001");
        let err = verify_aerodrome_v2_pool_address(
            8453,
            AERODROME_V2_BASE_FACTORY,
            wrong,
            address!("4200000000000000000000000000000000000006"),
            address!("940181a94a35a4569e4529a3cdfb74e38fd98631"),
            false,
        )
        .expect_err("wrong Aerodrome V2 address must fail verification");
        assert_eq!(err.expected, wrong);
        assert_eq!(err.chain_id, 8453);
        assert_eq!(err.factory, AERODROME_V2_BASE_FACTORY);
        // The reported init_hash is the CREATE2 init code hash for the
        // EIP-1167 clone of the implementation (keccak of the 55-byte
        // minimal-proxy bytecode).
        assert_eq!(
            err.init_hash,
            create2::eip1167_init_code_hash(AERODROME_V2_POOL_IMPLEMENTATION)
        );
    }

    #[test]
    fn verify_aerodrome_v2_stable_flag_changes_address() {
        // The correct volatile address must NOT verify for the stable flag
        // (the salt differs) — proving stable matters for verification.
        let volatile = address!("7f670f78B17dEC44d5Ef68a48740b6f8849cc2e6");
        assert!(verify_aerodrome_v2_pool_address(
            8453,
            AERODROME_V2_BASE_FACTORY,
            volatile,
            address!("4200000000000000000000000000000000000006"),
            address!("940181a94a35a4569e4529a3cdfb74e38fd98631"),
            true, // stable flag mismatch
        )
        .is_err());
    }

    #[test]
    fn verify_aerodrome_v2_skips_for_non_json_factory() {
        // A non-JSON Aerodrome factory must skip verification (ad-hoc path).
        let ad_hoc_factory = address!("1234567890123456789012345678901234567890");
        let ad_hoc_addr = address!("0000000000000000000000000000000000000000");
        assert!(verify_aerodrome_v2_pool_address(
            8453,
            ad_hoc_factory,
            ad_hoc_addr,
            address!("4200000000000000000000000000000000000006"),
            address!("940181a94a35a4569e4529a3cdfb74e38fd98631"),
            false,
        )
        .is_ok());
    }

    // --- init-hash provenance: one resolve/verify/admission policy ---------

    /// A factory no shipped row names, on any chain.
    const AD_HOC_FACTORY: Address = address!("1234567890123456789012345678901234567890");

    /// BSC — no `deployments.json` row names any of its factories.
    const BSC: u64 = 56;

    /// The Uniswap V2 factory as shipped on Base (a CREATE2 row off mainnet).
    const UNISWAP_V2_BASE_FACTORY: Address = address!("8909Dc15e40173Ff4699343b6eB8132c65e18eC6");

    /// The Uniswap V3 factory as shipped on Base.
    const UNISWAP_V3_BASE_FACTORY: Address = address!("33128a8fC17869897dcE68Ed026d694621f6FDfD");

    /// A chain-1 row that declares no CREATE2 derivation (Balancer weighted).
    const BALANCER_WEIGHTED_FACTORY: Address = address!("8E9aa87E45e92bad84D5F8DD1bff34Fb92637dE9");

    /// An unshipped `(chain, factory)` resolves the documented mainnet default
    /// as ASSUMED, and the boundary refuses to store it off chain 1 — the
    /// silent mainnet guess on a foreign chain is the defect this policy
    /// removes.
    #[test]
    fn an_unshipped_factory_on_a_foreign_chain_refuses_instead_of_guessing() {
        let v2 = resolve_v2_init_hash(BSC, AD_HOC_FACTORY);
        assert_eq!(
            v2,
            InitHashProvenance::AssumedMainnet(UNISWAP_V2_MAINNET_INIT_HASH)
        );
        assert!(
            !v2.is_verified(),
            "the mainnet default is assumed, never verified"
        );

        let refusal = admit_init_hash(BSC, AD_HOC_FACTORY, v2)
            .expect_err("chain 56 has no measured init hash");
        assert_eq!(refusal.chain_id, BSC);
        assert_eq!(refusal.factory, AD_HOC_FACTORY);
        let rendered = refusal.to_string();
        assert!(
            rendered.contains("56"),
            "the refusal names the chain: {rendered}"
        );
        assert!(
            rendered.contains(&AD_HOC_FACTORY.to_string()),
            "the refusal names the factory: {rendered}"
        );

        // A preset factory address on a chain the JSON does not ship it on is
        // the same case: unshipped is unshipped.
        let v3 = resolve_v3_init_hash(BSC, UNISWAP_V2_MAINNET_FACTORY);
        assert_eq!(
            v3,
            InitHashProvenance::AssumedMainnet(UNISWAP_V3_MAINNET_INIT_HASH)
        );
        assert!(admit_init_hash(BSC, UNISWAP_V2_MAINNET_FACTORY, v3).is_err());
    }

    /// The measured mainnet chain and the offline placeholder both admit the
    /// default — as an ASSUMED value, never as a verified record.
    #[test]
    fn the_ad_hoc_path_admits_the_assumed_default_on_mainnet_and_the_placeholder() {
        for chain_id in [MAINNET_CHAIN_ID, PLACEHOLDER_CHAIN_ID] {
            let v2 = resolve_v2_init_hash(chain_id, AD_HOC_FACTORY);
            assert_eq!(
                v2,
                InitHashProvenance::AssumedMainnet(UNISWAP_V2_MAINNET_INIT_HASH),
                "chain {chain_id}"
            );
            assert!(!v2.is_verified(), "chain {chain_id}");
            assert_eq!(
                admit_init_hash(chain_id, AD_HOC_FACTORY, v2),
                Ok(UNISWAP_V2_MAINNET_INIT_HASH),
                "chain {chain_id}"
            );

            let v3 = resolve_v3_init_hash(chain_id, AD_HOC_FACTORY);
            assert_eq!(
                v3,
                InitHashProvenance::AssumedMainnet(UNISWAP_V3_MAINNET_INIT_HASH),
                "chain {chain_id}"
            );
            assert!(!v3.is_verified(), "chain {chain_id}");
            assert_eq!(
                admit_init_hash(chain_id, AD_HOC_FACTORY, v3),
                Ok(UNISWAP_V3_MAINNET_INIT_HASH),
                "chain {chain_id}"
            );
        }

        // The refusal names what IS admitted, so an operator reading it knows
        // where the default belongs.
        let rendered = admit_init_hash(
            BSC,
            AD_HOC_FACTORY,
            resolve_v2_init_hash(BSC, AD_HOC_FACTORY),
        )
        .expect_err("a real foreign chain refuses")
        .to_string();
        assert!(
            rendered.contains(&MAINNET_CHAIN_ID.to_string())
                && rendered.contains(&format!("chain-{PLACEHOLDER_CHAIN_ID}")),
            "the refusal names the admitted chains: {rendered}"
        );
    }

    /// A row with neither an init hash nor a clone implementation (Balancer) has
    /// no hash to admit anywhere — not even on the chains that admit the
    /// assumed default, nor on the chain its row names.
    #[test]
    fn a_no_create2_row_never_yields_a_guessed_hash() {
        // The row's own chain resolves it as the NoCreate2 shape it is...
        let provenance = resolve_v2_init_hash(MAINNET_CHAIN_ID, BALANCER_WEIGHTED_FACTORY);
        assert_eq!(provenance, InitHashProvenance::NoCreate2);
        assert_eq!(provenance.init_hash(), None);

        // ...and whatever a consumer's lookup answered (the row on its own
        // chain, an unshipped pair elsewhere), the CREATE2-completing boundary
        // refuses a no-address-derived provenance on EVERY chain.
        for chain_id in [MAINNET_CHAIN_ID, 8453, BSC, PLACEHOLDER_CHAIN_ID] {
            assert!(
                admit_init_hash(
                    chain_id,
                    BALANCER_WEIGHTED_FACTORY,
                    InitHashProvenance::NoCreate2
                )
                .is_err(),
                "chain {chain_id} must refuse a no-CREATE2 provenance"
            );
        }
    }

    /// An EIP-1167 clone row IS a CREATE2 deployment: its measured init code
    /// hash is the clone bytecode pinned to its verified implementation, so
    /// resolution answers `Record` and admission lets it through on EVERY chain
    /// — including real foreign networks, because the value is measured.
    #[test]
    fn an_eip1167_clone_row_resolves_its_clone_init_code_hash() {
        for (chain_id, factory, implementation) in [
            (
                8453,
                AERODROME_V2_BASE_FACTORY,
                AERODROME_V2_POOL_IMPLEMENTATION,
            ),
            (
                8453,
                AERODROME_V3_BASE_FACTORY,
                AERODROME_V3_POOL_IMPLEMENTATION,
            ),
        ] {
            let expected = create2::eip1167_init_code_hash(implementation);
            let where_ = format!("chain {chain_id} factory {factory}");

            assert_eq!(
                resolve_v2_init_hash(chain_id, factory),
                InitHashProvenance::Record(expected),
                "{where_}"
            );
            assert_eq!(
                resolve_v3_init_hash(chain_id, factory),
                InitHashProvenance::Record(expected),
                "{where_}"
            );
            assert!(
                resolve_v2_init_hash(chain_id, factory).is_verified(),
                "a clone row's hash is measured, not assumed: {where_}"
            );

            // Measured → admitted on every chain, the row's own included.
            for admitted_on in [chain_id, BSC, MAINNET_CHAIN_ID, PLACEHOLDER_CHAIN_ID] {
                assert_eq!(
                    admit_init_hash(
                        admitted_on,
                        factory,
                        resolve_v2_init_hash(chain_id, factory)
                    ),
                    Ok(expected),
                    "{where_} on chain {admitted_on}"
                );
            }
        }
    }

    /// Every shipped clone row resolves through its `implementation_address`,
    /// and no other row shape does.
    #[test]
    fn every_shipped_clone_row_resolves_through_its_implementation() {
        let mut checked = 0;
        for record in records() {
            let Some(implementation) = record.implementation_address else {
                continue;
            };
            let where_ = format!("chain {} factory {}", record.chain_id, record.factory);
            let expected = create2::eip1167_init_code_hash(implementation);
            assert_eq!(
                resolve_v2_init_hash(record.chain_id, record.factory),
                InitHashProvenance::Record(expected),
                "{where_}"
            );
            assert_eq!(
                resolve_v3_init_hash(record.chain_id, record.factory),
                InitHashProvenance::Record(expected),
                "{where_}"
            );
            assert_eq!(
                record.init_hash, None,
                "a clone row sets no init_hash: {where_}"
            );
            checked += 1;
        }
        assert_eq!(
            checked, 2,
            "the shipped clone rows are the two Aerodrome ones"
        );
    }

    /// Every shipped row carrying an init hash resolves to exactly that hash,
    /// on its own chain, as a verified record.
    #[test]
    fn every_shipped_create2_row_resolves_as_its_record() {
        let mut checked = 0;
        for record in records() {
            let Some(init_hash) = record.init_hash else {
                continue;
            };
            let where_ = format!("chain {} factory {}", record.chain_id, record.factory);
            assert_eq!(
                resolve_v2_init_hash(record.chain_id, record.factory),
                InitHashProvenance::Record(init_hash),
                "{where_}"
            );
            assert_eq!(
                resolve_v3_init_hash(record.chain_id, record.factory),
                InitHashProvenance::Record(init_hash),
                "{where_}"
            );
            assert_eq!(
                admit_init_hash(
                    record.chain_id,
                    record.factory,
                    InitHashProvenance::Record(init_hash)
                ),
                Ok(init_hash),
                "{where_}"
            );
            checked += 1;
        }
        assert!(
            checked >= 15,
            "expected the shipped CREATE2 rows to be walked, walked {checked}"
        );
    }

    /// Resolution's answer, reduced to its kind for a matrix row.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Provenance {
        Record,
        AssumedMainnet,
        NoCreate2,
    }

    fn provenance_kind(provenance: InitHashProvenance) -> Provenance {
        match provenance {
            InitHashProvenance::Record(_) => Provenance::Record,
            InitHashProvenance::AssumedMainnet(_) => Provenance::AssumedMainnet,
            InitHashProvenance::NoCreate2 => Provenance::NoCreate2,
        }
    }

    /// What the CREATE2 guard did with a deliberately-wrong declared address:
    /// [`VerifyVerdict::Enforced`] means it compared (and refused the nonsense
    /// address), [`VerifyVerdict::Skipped`] means the address passed untouched.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum VerifyVerdict {
        Enforced,
        Skipped,
    }

    /// One cell of the resolve / verify / admission agreement matrix.
    struct AgreementCell {
        label: &'static str,
        chain_id: u64,
        factory: Address,
        provenance: Provenance,
        verify: VerifyVerdict,
        admitted: bool,
    }

    /// The address no CREATE2 derivation can produce.
    const NONSENSE_ADDRESS: Address = address!("0000000000000000000000000000000000000001");

    fn v2_verdict(chain_id: u64, factory: Address) -> VerifyVerdict {
        if verify_v2_pool_address(chain_id, factory, NONSENSE_ADDRESS, WETH, DAI).is_ok() {
            VerifyVerdict::Skipped
        } else {
            VerifyVerdict::Enforced
        }
    }

    fn v3_verdict(chain_id: u64, factory: Address) -> VerifyVerdict {
        if verify_v3_pool_address(chain_id, factory, NONSENSE_ADDRESS, WETH, DAI, 500).is_ok() {
            VerifyVerdict::Skipped
        } else {
            VerifyVerdict::Enforced
        }
    }

    /// Assert every cell of the agreement matrix: resolution's provenance, the
    /// guard's verdict, and the boundary's admission are one policy.
    fn assert_agreement(
        cells: &[AgreementCell],
        resolve: fn(u64, Address) -> InitHashProvenance,
        verdict: fn(u64, Address) -> VerifyVerdict,
    ) {
        for cell in cells {
            let provenance = resolve(cell.chain_id, cell.factory);
            assert_eq!(
                provenance_kind(provenance),
                cell.provenance,
                "{}: resolution answered {provenance:?}",
                cell.label
            );
            assert_eq!(
                verdict(cell.chain_id, cell.factory),
                cell.verify,
                "{}: the CREATE2 guard's verdict",
                cell.label
            );

            let admitted = admit_init_hash(cell.chain_id, cell.factory, provenance);
            assert_eq!(
                admitted.is_ok(),
                cell.admitted,
                "{}: admission answered {admitted:?}",
                cell.label
            );

            // The agreement invariant: this guard compares exactly the rows
            // whose address is derived with the standard salt, i.e. measured
            // (a record) and not an EIP-1167 clone — the clone's own guard owns
            // the clone rows.
            assert_eq!(
                provenance.is_verified()
                    && implementation_address(cell.chain_id, cell.factory).is_none(),
                cell.verify == VerifyVerdict::Enforced,
                "{}: resolution and verification disagree",
                cell.label
            );

            match admitted {
                Ok(init_hash) => {
                    assert_eq!(
                        Some(init_hash),
                        provenance.init_hash(),
                        "{}: an admitted value is the resolved one",
                        cell.label
                    );
                    assert!(
                        provenance.is_verified() || admits_assumed_default(cell.chain_id),
                        "{}: an unverified hash crossed the boundary off the admitted chains",
                        cell.label
                    );
                }
                Err(refusal) => assert_eq!(
                    (refusal.chain_id, refusal.factory),
                    (cell.chain_id, cell.factory),
                    "{}: the refusal names the cell it refused",
                    cell.label
                ),
            }
        }
    }

    /// The table-driven cross-check: (row-with-hash | eip1167-clone-row |
    /// unshipped) x (mainnet | the offline placeholder | real foreign chains),
    /// for both CREATE2 families. The chain-0 and chain-56 columns can only be
    /// `unshipped` — the JSON ships no row on either — so the `row` categories
    /// are read on Base, the shipped non-mainnet network. The no-CREATE2 shape
    /// (a row with neither an init hash nor a clone implementation, i.e.
    /// Balancer) is exercised by
    /// [`a_no_create2_row_never_yields_a_guessed_hash`], because its rows are
    /// keyed to chain 1 and the standard-salt guard skips them either way.
    #[test]
    fn v2_resolve_verify_and_admission_agree_on_one_policy() {
        let cells = [
            AgreementCell {
                label: "v2 row-with-hash / chain 1",
                chain_id: 1,
                factory: UNISWAP_V2_MAINNET_FACTORY,
                provenance: Provenance::Record,
                verify: VerifyVerdict::Enforced,
                admitted: true,
            },
            AgreementCell {
                label: "v2 row-without-hash / chain 1",
                chain_id: 1,
                factory: BALANCER_WEIGHTED_FACTORY,
                provenance: Provenance::NoCreate2,
                verify: VerifyVerdict::Skipped,
                admitted: false,
            },
            AgreementCell {
                label: "v2 unshipped / chain 1",
                chain_id: 1,
                factory: AD_HOC_FACTORY,
                provenance: Provenance::AssumedMainnet,
                verify: VerifyVerdict::Skipped,
                admitted: true,
            },
            AgreementCell {
                label: "v2 row-with-hash / chain 8453",
                chain_id: 8453,
                factory: UNISWAP_V2_BASE_FACTORY,
                provenance: Provenance::Record,
                verify: VerifyVerdict::Enforced,
                admitted: true,
            },
            AgreementCell {
                // An EIP-1167 clone row: measured (Record) and admitted, but
                // the standard-salt guard skips it — its address derives from
                // the implementation, which the clone guard checks.
                label: "v2 eip1167-clone / chain 8453",
                chain_id: 8453,
                factory: AERODROME_V2_BASE_FACTORY,
                provenance: Provenance::Record,
                verify: VerifyVerdict::Skipped,
                admitted: true,
            },
            AgreementCell {
                label: "v2 unshipped / chain 0 (placeholder)",
                chain_id: PLACEHOLDER_CHAIN_ID,
                factory: AD_HOC_FACTORY,
                provenance: Provenance::AssumedMainnet,
                verify: VerifyVerdict::Skipped,
                admitted: true,
            },
            AgreementCell {
                label: "v2 unshipped / chain 56",
                chain_id: BSC,
                factory: AD_HOC_FACTORY,
                provenance: Provenance::AssumedMainnet,
                verify: VerifyVerdict::Skipped,
                admitted: false,
            },
        ];
        assert_agreement(&cells, resolve_v2_init_hash, v2_verdict);
    }

    /// The V2 table above, over the V3 family: the salt differs (tokens + fee)
    /// but the policy is the same one. The table's no-CREATE2 rows are protocol
    /// rows, not family rows, so the chain-1 row serves both.
    #[test]
    fn v3_resolve_verify_and_admission_agree_on_one_policy() {
        let cells = [
            AgreementCell {
                label: "v3 row-with-hash / chain 1",
                chain_id: 1,
                factory: UNISWAP_V3_MAINNET_FACTORY,
                provenance: Provenance::Record,
                verify: VerifyVerdict::Enforced,
                admitted: true,
            },
            AgreementCell {
                label: "v3 row-without-hash / chain 1",
                chain_id: 1,
                factory: BALANCER_WEIGHTED_FACTORY,
                provenance: Provenance::NoCreate2,
                verify: VerifyVerdict::Skipped,
                admitted: false,
            },
            AgreementCell {
                label: "v3 unshipped / chain 1",
                chain_id: 1,
                factory: AD_HOC_FACTORY,
                provenance: Provenance::AssumedMainnet,
                verify: VerifyVerdict::Skipped,
                admitted: true,
            },
            AgreementCell {
                label: "v3 row-with-hash / chain 8453",
                chain_id: 8453,
                factory: UNISWAP_V3_BASE_FACTORY,
                provenance: Provenance::Record,
                verify: VerifyVerdict::Enforced,
                admitted: true,
            },
            AgreementCell {
                // The V3 clone row, as above.
                label: "v3 eip1167-clone / chain 8453",
                chain_id: 8453,
                factory: AERODROME_V3_BASE_FACTORY,
                provenance: Provenance::Record,
                verify: VerifyVerdict::Skipped,
                admitted: true,
            },
            AgreementCell {
                label: "v3 unshipped / chain 0 (placeholder)",
                chain_id: PLACEHOLDER_CHAIN_ID,
                factory: AD_HOC_FACTORY,
                provenance: Provenance::AssumedMainnet,
                verify: VerifyVerdict::Skipped,
                admitted: true,
            },
            AgreementCell {
                label: "v3 unshipped / chain 56",
                chain_id: BSC,
                factory: AD_HOC_FACTORY,
                provenance: Provenance::AssumedMainnet,
                verify: VerifyVerdict::Skipped,
                admitted: false,
            },
        ];
        assert_agreement(&cells, resolve_v3_init_hash, v3_verdict);
    }
}
