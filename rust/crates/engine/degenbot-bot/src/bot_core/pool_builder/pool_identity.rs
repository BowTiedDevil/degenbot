//! Pool identity resolution — the ONE core home both consumers resolve
//! through (the pure-Rust bot and the Python driver's companion layer).
//!
//! A requested pool's identity has one resolution ladder and one structured
//! answer. This module OWNS every identity source and every identity read:
//!
//! - **DB-kind lookup** — the persisted `pools.kind` discriminator, mapped
//!   through the one kind grammar ([`identity_for_kind`]). A present kind the
//!   grammar cannot name is refused ([`PoolIdentityError::UnknownDbKind`]),
//!   never re-classified by falling through to the chain arms.
//! - **Registered species/factory matching** — the pool's `factory()` against
//!   the species manifest's per-chain factory rows
//!   ([`degenbot_db::species`]) and the shipped deployment records
//!   ([`degenbot_uniswap::deployments`]).
//! - **Factory reads** — the one `factory()` read, at the head block
//!   (identity is a deployment-time property). A revert or an empty answer is
//!   absence; a transport failure is an error, never "no factory" (which would
//!   let the fall-through arms classify).
//! - **Positive V3/V2/Balancer/Curve probes** ([`probe_pool_type`] and the
//!   Balancer sub-family probe) — a selector counts as answered only with
//!   returndata, so no artifact can name a family.
//! - **Balancer factory membership** ([`fetch_balancer_factory`]) — Balancer
//!   exposes no pool registry and a deployed pool carries no `factory()`, so
//!   `isPoolFromFactory` across the registered factory rows is the ownership
//!   answer.
//!
//! The precedence is DB -> registered species/factory -> positive probes, and
//! the discipline is positive-only: unknown and contradictory identities and
//! transport failures are typed refusals
//! ([`PoolIdentityError`]), never a family classification. The answer is
//! structured ([`ResolvedPoolIdentity`]: species, family, variant, factory,
//! persisted kind) — never a Python class name. The Python companion projects
//! this value for class selection; it keeps no ladder of its own.

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use degenbot_core::errors::ProviderError;
use degenbot_db::error::DbError;
use degenbot_db::species;
use degenbot_pools::{
    BalanceVectorVariant, ConcentratedLiquidityVariant, Identity, ReservePairVariant,
};
use degenbot_rpc::abi::{
    ContractRead, IBalancerPool, IBalancerPoolFactory, ICurvePool, ICurvePoolInt128, IPoolIdentity,
    IUniswapV2Pair, IUniswapV3Pool,
};
use degenbot_uniswap::deployments::{self, DeploymentRecord};
use degenbot_uniswap::dex_identity::DexName;

use super::builder::PoolBuilderError;
use crate::bot_core::construction_io::ConstructionIo;

/// The family a resolved pool identity belongs to — the pricing family the
/// construction dispatch and the Python companion's class selection key on.
///
/// The fork spelling is the [`ResolvedPoolIdentity::variant`], not a family:
/// `sushiswap` V3 is `ConcentratedLiquidity` + `variant = "sushiswap"` (the
/// kind grammar's one spelling of it, `"sushiswap_v3"`), and the multi-token
/// families carry their own variant (`"balancer_stable"` under
/// [`Self::Stableswap`], `"balancer_weighted"` under [`Self::Weighted`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolIdentityFamily {
    /// V2-style constant product (Aerodrome V2 rides its `variant`).
    ConstantProduct,
    /// V3-style concentrated liquidity.
    ConcentratedLiquidity,
    /// Curve `StableSwap`, and the Balancer stable family's variant arm.
    Stableswap,
    /// Balancer weighted.
    Weighted,
    /// A positively identified Balancer Vault pool whose shape no
    /// construction arm models (a linear pool, a boosted pool's linear
    /// constituent, a gyro/quant-amm shape). The family exists so the
    /// identity reaches the construction switchboard, which refuses it under
    /// its own name; no pool class and no registered pool ever belong to it.
    Unmodeled,
}

impl PoolIdentityFamily {
    /// The family's spelling in the shared identity vocabulary (the Python
    /// `PoolFamily` values).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConstantProduct => "constant_product",
            Self::ConcentratedLiquidity => "concentrated_liquidity",
            Self::Stableswap => "stableswap",
            Self::Weighted => "weighted",
            Self::Unmodeled => "unmodeled",
        }
    }
}

/// The ONE resolved pool identity — structured facts, no class names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPoolIdentity {
    /// The species-manifest spelling this identity's persisted kind names
    /// (`"uniswap_v3"`, `"sushiswap_v2"`, …), or `None` for a kind the
    /// manifest does not carry (the multi-token families, an open-set
    /// third-party fork).
    pub species: Option<&'static str>,
    /// The pricing family.
    pub family: PoolIdentityFamily,
    /// The fork spelling (`"sushiswap"`, `"aerodrome"`, `"balancer_stable"`,
    /// …); `None` is the canonical Uniswap identity.
    pub variant: Option<String>,
    /// The factory evidence this identity resolved with — the DB exchange
    /// row's factory, the `factory()` read, or the Balancer membership
    /// answer. `None` for the factory-less families (Curve; a Balancer
    /// revision no registered factory claims).
    pub factory: Option<Address>,
    /// The persisted `pools.kind` discriminator this identity carries.
    pub persisted_kind: String,
}

/// The typed refusal of one identity resolution — the vocabulary in which an
/// unresolvable identity is loud. An arm of this enum is NEVER a
/// classification: unknown and contradictory identities and transport
/// failures refuse, they do not name a family.
#[derive(Debug, thiserror::Error)]
pub enum PoolIdentityError {
    /// A present DB `pools.kind` the kind grammar cannot name. The DB arm
    /// OWNS the answer when a row is present: this refusal is where an
    /// unrecognized kind stops, never a fall-through to on-chain
    /// re-classification.
    #[error("unrecognized pool kind {kind:?} from the database")]
    UnknownDbKind {
        /// The persisted kind that could not be named.
        kind: String,
    },
    /// Nothing positive names this pool: no DB row, no `factory()` getter (or
    /// an unregistered factory), and no identity selector answered.
    #[error("unknown pool identity at {address}: no identity selector answered")]
    UnknownIdentity {
        /// The pool that answered nothing.
        address: Address,
    },
    /// A positively answered shape this resolver refuses to classify: a
    /// factory-less V2/V3 answer (every such deployment exposes `factory()`),
    /// so the answer is not a pool identity at all.
    #[error("unsupported pool identity at {address}: {detail}")]
    Unsupported {
        /// The pool that answered.
        address: Address,
        /// Why the answer is not a pool identity.
        detail: String,
    },
    /// Two identity sources name different facts. Refused, never classified
    /// by picking a winner.
    #[error("contradictory pool identity at {address}: {detail}")]
    Contradictory {
        /// The pool whose identity evidence disagrees.
        address: Address,
        /// The two disagreeing facts.
        detail: String,
    },
    /// A transport/RPC failure while resolving. A broken RPC must never
    /// degrade a pool to a family or to unknown.
    #[error("pool identity read failed: {0}")]
    Io(#[from] ProviderError),
    /// A database failure while resolving (the DB arm's read is authoritative
    /// when a row is present, so its failure refuses rather than falls
    /// through to the chain arms).
    #[error("pool identity DB read failed: {0}")]
    Db(#[from] DbError),
}

// ---------------------------------------------------------------------------
// The kind grammar — the ONE spelling home
// ---------------------------------------------------------------------------

/// The identity one persisted `pools.kind` discriminator carries — the exact
/// inverse of the `derive_kind` projection the Python descriptor persists.
///
/// The grammar is closed over the families this engine models and OPEN over
/// fork spellings: `<variant>_v2` is a constant-product fork, `<variant>_v3`
/// a concentrated-liquidity fork (the third-party open set), and the
/// canonical and multi-token kinds are spelled exactly. A kind outside the
/// grammar is not an identity — [`PoolIdentityError::UnknownDbKind`] names it.
/// The `factory` of the answer is always `None`: a kind is not a factory fact.
#[must_use]
pub fn identity_for_kind(kind: &str) -> Option<ResolvedPoolIdentity> {
    let (family, variant) = match kind {
        "uniswap_v2" => (PoolIdentityFamily::ConstantProduct, None),
        "uniswap_v3" => (PoolIdentityFamily::ConcentratedLiquidity, None),
        "stableswap" => (PoolIdentityFamily::Stableswap, None),
        "balancer_weighted" => (PoolIdentityFamily::Weighted, Some("balancer_weighted")),
        "balancer_stable" => (PoolIdentityFamily::Stableswap, Some("balancer_stable")),
        "balancer_unmodeled" => (PoolIdentityFamily::Unmodeled, Some("balancer_unmodeled")),
        other => {
            let (variant, family) = match other.strip_suffix("_v2") {
                Some(variant) => (variant, PoolIdentityFamily::ConstantProduct),
                None => (
                    other.strip_suffix("_v3")?,
                    PoolIdentityFamily::ConcentratedLiquidity,
                ),
            };
            if variant.is_empty() {
                return None;
            }
            (family, Some(variant))
        }
    };
    Some(ResolvedPoolIdentity {
        species: species_for_kind(kind),
        family,
        variant: variant.map(str::to_string),
        factory: None,
        persisted_kind: kind.to_string(),
    })
}

/// The species-manifest species whose `kind` is `kind`, when the manifest
/// carries it.
#[must_use]
pub fn species_for_kind(kind: &str) -> Option<&'static str> {
    species::manifest().get(kind).map(|s| s.kind.as_str())
}

/// The identity one shipped deployment record carries — family/variant from
/// the row's `pool_type` slug and DEX label, spelled through the kind
/// grammar. `None` for a row whose slug names no modeled family.
fn identity_from_deployment(rec: &DeploymentRecord) -> Option<ResolvedPoolIdentity> {
    let kind = deployment_kind(rec)?;
    identity_for_kind(&kind).map(|identity| ResolvedPoolIdentity {
        factory: Some(rec.factory),
        ..identity
    })
}

/// The persisted-kind spelling of one shipped deployment row: the `pool_type`
/// slug's family + the `name` label's DEX spelling (the V2 rows collapse to a
/// shared slug, so the label is the V2 fork discriminator).
fn deployment_kind(rec: &DeploymentRecord) -> Option<String> {
    let slug = rec.pool_type.as_deref()?;
    if slug == "balancer-weighted" {
        return Some("balancer_weighted".to_owned());
    }
    if slug == "balancer-stable" {
        return Some("balancer_stable".to_owned());
    }
    // The DEX spelling; `None` is the canonical Uniswap identity.
    let variant = match rec.dex {
        Some(DexName::Uniswap) | None => None,
        Some(dex) => Some(dex.as_str()),
    };
    if slug.ends_with("-v2") {
        return Some(variant.map_or_else(|| "uniswap_v2".to_owned(), |v| format!("{v}_v2")));
    }
    if slug.ends_with("-v3") {
        return Some(variant.map_or_else(|| "uniswap_v3".to_owned(), |v| format!("{v}_v3")));
    }
    None
}

/// The registered identity for `(chain_id, factory)`: the species manifest's
/// per-chain factory rows first (the registered species), then the shipped
/// deployment records (the Balancer factory rows the manifest does not carry).
/// `None` for an unregistered factory — the caller probes.
#[must_use]
pub fn registered_identity(chain_id: u64, factory: Address) -> Option<ResolvedPoolIdentity> {
    for entry in &species::manifest().species {
        let is_create2 = matches!(entry.family, species::Family::V2 | species::Family::V3);
        if is_create2 && entry.chains.get(&chain_id).and_then(|ids| ids.factory) == Some(factory) {
            return identity_for_kind(&entry.kind).map(|identity| ResolvedPoolIdentity {
                factory: Some(factory),
                ..identity
            });
        }
    }
    deployments::lookup(chain_id, factory).and_then(identity_from_deployment)
}

/// The registered Balancer factory candidates for the membership walk: the
/// shipped deployment rows' Balancer factories on `chain_id`, filtered to the
/// probed sub-family's rows (only the stable factory rows are asked about a
/// stable pool). Sorted for a deterministic walk order.
fn balancer_membership_candidates(chain_id: u64, family: BalancerFamily) -> Vec<Address> {
    let slug = match family {
        BalancerFamily::Weighted => "balancer-weighted",
        BalancerFamily::Stable => "balancer-stable",
    };
    let mut candidates: Vec<Address> = deployments::records()
        .filter(|rec| {
            rec.chain_id == chain_id
                && rec.dex == Some(DexName::Balancer)
                && rec.pool_type.as_deref() == Some(slug)
        })
        .map(|rec| rec.factory)
        .collect();
    candidates.sort_unstable();
    candidates
}

// ---------------------------------------------------------------------------
// The positive probes (the one probe home)
// ---------------------------------------------------------------------------

/// The on-chain family a `probe` resolves to (V4 is a separate
/// `(PoolManager, pool_id)` path, not a single-address probe).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolFamily {
    V2,
    V3,
    BalancerWeighted,
    BalancerStable,
    Curve,
}

impl PoolFamily {
    /// Project the pool taxonomy onto the registration-probe family (ADR-059
    /// D1).
    ///
    /// The probe station LAGS the taxonomy in two places, and the lag is
    /// explicit rather than silently mapped: an `AerodromeV2` pair registers
    /// through the dedicated `build_aerodrome_v2` path (its `stable`/fee reads
    /// have no home here), and a `UniswapV4` pool through the separate
    /// `(PoolManager, pool_id)` `build_v4` path — neither is reachable from a
    /// single-address selector probe, so both project to `None`.
    #[must_use]
    pub fn from_identity(identity: &Identity) -> Option<Self> {
        match identity {
            Identity::ReservePair { variant, .. } => match variant {
                ReservePairVariant::UniswapV2 => Some(Self::V2),
                ReservePairVariant::AerodromeV2 { .. } => None,
            },
            Identity::ConcentratedLiquidity { variant, .. } => match variant {
                ConcentratedLiquidityVariant::UniswapV3 => Some(Self::V3),
                ConcentratedLiquidityVariant::UniswapV4 => None,
            },
            Identity::BalanceVector { variant, .. } => match variant {
                BalanceVectorVariant::Curve => Some(Self::Curve),
                BalanceVectorVariant::BalancerWeighted => Some(Self::BalancerWeighted),
                BalanceVectorVariant::BalancerStable => Some(Self::BalancerStable),
            },
            // Binned liquidity has no single-address selector probe: its
            // LBPair identity is an LBFactory deployment the probe station
            // does not model, so it LAGS the taxonomy and projects to None.
            Identity::BinnedLiquidity { .. } => None,
        }
    }
}

/// The Balancer pool sub-family the probe resolves — the semantic owner of
/// the family vocabulary; the binding mirrors its two discriminants as the
/// Python `BalancerFamily` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalancerFamily {
    /// Has `getNormalizedWeights()` — a weighted pool.
    Weighted,
    /// Has `getAmplificationParameter()` (no weights) — a stable pool.
    Stable,
}

/// Classify a contract by which read-only identity selector answers: V3
/// (`slot0()`), V2 (`getReserves()`), Balancer (`getPoolId()` +
/// `getNormalizedWeights()`), then the typed Curve leg (`coins(0)`, either
/// supported coin-getter shape). A selector counts as answered only with
/// returndata — a revert or a successful empty return (the artifact a
/// non-reverting fallback, or an EOA's `eth_call`, produces) is absence — so
/// no artifact can name a family, and a non-Curve pool never pays a Curve
/// probe.
///
/// The `getPoolId` arm collapses the Balancer sub-family onto the stable arm
/// when neither `getNormalizedWeights()` nor `getAmplificationParameter()`
/// answers — the identity-ladder caller re-probes through
/// [`probe_balancer_type`] to keep the unmodeled shape under its own name.
///
/// An unverified contract is never a Curve pool: when neither `coins(0)` shape
/// returns a decodable address, the contract has no recognized identity and
/// this returns [`PoolBuilderError::UnknownPoolIdentity`], never
/// [`PoolFamily::Curve`]. A provider failure that is NOT a revert (transport,
/// timeout, rate limit, malformed response) likewise propagates as an error —
/// a broken RPC must never silently degrade a pool to a family or to unknown.
///
/// # Errors
///
/// [`PoolBuilderError::Rpc`] from the first probe that failed for a reason
/// other than [`ProviderError::ExecutionReverted`], or
/// [`PoolBuilderError::UnknownPoolIdentity`] when no probe answered.
pub async fn probe_pool_type(
    io: &ConstructionIo,
    address: Address,
    block: Option<u64>,
) -> Result<PoolFamily, PoolBuilderError> {
    if probe_present(
        io,
        address,
        IUniswapV3Pool::slot0Call {}.abi_encode(),
        block,
    )
    .await?
    {
        Ok(PoolFamily::V3)
    } else if probe_present(
        io,
        address,
        IUniswapV2Pair::getReservesCall {}.abi_encode(),
        block,
    )
    .await?
    {
        Ok(PoolFamily::V2)
    } else if probe_present(
        io,
        address,
        IBalancerPool::getPoolIdCall {}.abi_encode(),
        block,
    )
    .await?
    {
        Ok(
            if probe_present(
                io,
                address,
                IBalancerPool::getNormalizedWeightsCall {}.abi_encode(),
                block,
            )
            .await?
            {
                PoolFamily::BalancerWeighted
            } else {
                PoolFamily::BalancerStable
            },
        )
    } else if probe_curve(io, address, block).await? {
        Ok(PoolFamily::Curve)
    } else {
        Err(PoolBuilderError::UnknownPoolIdentity { address })
    }
}

/// `true` when the selector answered with returndata, `false` when it is
/// absent — an EVM revert, or a successful empty return (the artifact a
/// non-reverting fallback or an EOA's `eth_call` produces) — and `Err` on any
/// other provider failure (mirrors the benign-revert rule in
/// `build_balancer_stable`). Every raw selector this gates has a nonempty
/// valid answer, so empty returndata is never the selector answering.
async fn probe_present(
    read: &impl ContractRead,
    address: Address,
    calldata: Vec<u8>,
    block: Option<u64>,
) -> Result<bool, ProviderError> {
    match read.call(address, calldata.into(), block).await {
        Ok(returndata) => Ok(!returndata.is_empty()),
        Err(ProviderError::ExecutionReverted { .. }) => Ok(false),
        Err(e) => Err(e),
    }
}

/// `true` when a supported Curve coin getter answers `coins(0)` with a
/// decodable address.
///
/// The supported Curve interface carries two coin-getter shapes (the
/// `curve_choreography::CurvePrototype` dual the construction choreography
/// already builds against): `coins(uint256)`, and the legacy `coins(int128)`
/// of the old registry deployments — which is the *only* coin getter some of
/// them expose (mainnet Curve registry pool 4/49,
/// `0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27`, reverts on `coins(uint256)`).
/// Either answering is positive Curve identity, so the uint shape is tried
/// first and the legacy shape is the fallback on its absence.
///
/// Per shape, a revert means the selector is absent and an answer that does
/// not decode as an address means the call site is not a Curve coin getter:
/// both are "not Curve". A provider failure that is neither a revert nor a
/// decode failure propagates — the fallback runs only on the first shape's
/// absence, never around an RPC fault.
///
/// The typed read owns the ABI decode, so the garbage-return case arrives as
/// [`ProviderError::DecodingError`] instead of as bytes; an undecodable answer
/// is absence, never an RPC fault.
///
/// `coins(0)` needs the index word in the calldata (the ABI decoder rejects a
/// bare selector), so this cannot reuse [`probe_present`].
async fn probe_curve(
    read: &impl ContractRead,
    address: Address,
    block: Option<u64>,
) -> Result<bool, ProviderError> {
    match read
        .read(address, ICurvePool::coinsCall { i: U256::ZERO }, block)
        .await
    {
        Ok(_) => Ok(true),
        Err(ProviderError::ExecutionReverted { .. } | ProviderError::DecodingError { .. }) => {
            // The uint shape is absent: the legacy shape decides before the
            // identity is refused as unknown.
            match read
                .read(address, ICurvePoolInt128::coinsCall { i: 0i128 }, block)
                .await
            {
                Ok(_) => Ok(true),
                Err(
                    ProviderError::ExecutionReverted { .. } | ProviderError::DecodingError { .. },
                ) => Ok(false),
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

/// Probe a Balancer pool's sub-family: `getNormalizedWeights()` answers →
/// [`BalancerFamily::Weighted`]; else `getAmplificationParameter()` answers →
/// [`BalancerFamily::Stable`]; else an error (the shape is a Balancer pool no
/// construction arm models — a linear pool, a boosted pool's linear
/// constituent, a gyro/quant-amm shape).
///
/// A revert or an undecodable/empty answer is ABSENCE (the sub-family is
/// probed, not read); a transport failure propagates — a broken RPC must not
/// classify a pool as unmodeled.
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] when neither probe responds (the
/// unmodeled shape), or the transport failure itself.
pub async fn probe_balancer_type(
    read: &impl ContractRead,
    address: &Address,
    block: Option<u64>,
) -> Result<BalancerFamily, ProviderError> {
    let absent = |err: &ProviderError| {
        matches!(
            err,
            ProviderError::ExecutionReverted { .. } | ProviderError::DecodingError { .. }
        )
    };
    match read
        .read(*address, IBalancerPool::getNormalizedWeightsCall {}, block)
        .await
    {
        Ok(_) => return Ok(BalancerFamily::Weighted),
        Err(err) if absent(&err) => {}
        Err(err) => return Err(err),
    }
    match read
        .read(
            *address,
            IBalancerPool::getAmplificationParameterCall {},
            block,
        )
        .await
    {
        Ok(_) => Ok(BalancerFamily::Stable),
        Err(err) if absent(&err) => Err(ProviderError::DecodingError {
            message: format!(
                "Cannot determine Balancer pool type for {address}: neither \
                 getNormalizedWeights() nor getAmplificationParameter() responded"
            ),
        }),
        Err(err) => Err(err),
    }
}

/// Find the Balancer V2 factory that owns `pool`, by asking each candidate
/// factory's `isPoolFromFactory(pool)`.
///
/// Balancer V2 exposes no enumerable pool registry (`getPoolCount` / `getPools`
/// / `getPoolAt` revert on every factory revision), and a deployed pool carries
/// no `factory()` getter — so membership is the only on-chain ownership answer.
/// A candidate that reverts or fails to decode is not the owner; a candidate
/// that does not own the pool reports `false`. That skip is this read's
/// policy: it is what "ask every candidate" means. A transport failure is NOT
/// a skip — it propagates, so a broken RPC can never read as "no registered
/// factory owns this pool".
///
/// Returns `Ok(None)` when no candidate owns the pool (a factory revision the
/// registry does not carry — e.g. the `MetaStablePool` factory).
///
/// # Errors
///
/// Returns the transport failure when a candidate's membership read fails for
/// any reason other than a revert or a decode failure.
pub async fn fetch_balancer_factory(
    read: &impl ContractRead,
    pool: &Address,
    candidates: &[Address],
    block: Option<u64>,
) -> Result<Option<Address>, ProviderError> {
    for factory in candidates {
        let owns = match read
            .read(
                *factory,
                IBalancerPoolFactory::isPoolFromFactoryCall { pool: *pool },
                block,
            )
            .await
        {
            Ok(owns) => owns,
            Err(ProviderError::ExecutionReverted { .. } | ProviderError::DecodingError { .. }) => {
                continue
            }
            Err(err) => return Err(err),
        };
        if owns {
            return Ok(Some(*factory));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// The ladder
// ---------------------------------------------------------------------------

/// Resolve one pool's identity — the ONE entry both consumers resolve
/// through.
///
/// Precedence: DB (`pools.kind` + the exchange factory), then registered
/// species/factory (the species manifest and the shipped deployment rows),
/// then the positive chain probes (V3/V2/Balancer/Curve, with the Balancer
/// factory-membership walk). Every read runs at the head block: identity is a
/// deployment-time property.
///
/// # Errors
///
/// Returns [`PoolIdentityError`] — the typed refusals
/// ([`PoolIdentityError::UnknownDbKind`], [`PoolIdentityError::UnknownIdentity`],
/// [`PoolIdentityError::Unsupported`], [`PoolIdentityError::Contradictory`])
/// and the transport classes ([`PoolIdentityError::Io`],
/// [`PoolIdentityError::Db`]). None of the arms is ever a classification.
pub async fn resolve_pool_identity(
    chain_id: u64,
    address: Address,
    io: &ConstructionIo,
) -> Result<ResolvedPoolIdentity, PoolIdentityError> {
    // 1. DB identity — the `kind` column is the most direct signal, and a
    //    present row OWNS the answer. The fetches are typed: a missing row
    //    falls through to the chain arms, but a DB failure refuses (a
    //    transport failure must never let the chain classify).
    if let Some(row) = io
        .fetch_pool_row(chain_id_as_i64(chain_id), address)
        .await?
    {
        let mut identity =
            identity_for_kind(&row.kind).ok_or_else(|| PoolIdentityError::UnknownDbKind {
                kind: row.kind.clone(),
            })?;
        let factory = io.fetch_exchange(row.exchange_id).await?.map(|e| e.factory);
        if let Some(factory) = factory {
            // Contradiction guard: the persisted kind and the factory's
            // registered species are two identity facts about one pool. A
            // family-level disagreement refuses; a variant-level refinement
            // defers to the DB kind (the DB precedence arm's own answer).
            if let Some(registered) = registered_identity(chain_id, factory) {
                if registered.family != identity.family {
                    return Err(PoolIdentityError::Contradictory {
                        address,
                        detail: format!(
                            "the database kind {:?} is a {} pool but its factory {} is registered \
                             as {:?} ({})",
                            row.kind,
                            identity.family.as_str(),
                            factory,
                            registered.persisted_kind,
                            registered.family.as_str(),
                        ),
                    });
                }
            }
            identity.factory = Some(factory);
        }
        return Ok(identity);
    }

    // 2. The registered species/factory arm — ONE `factory()` read at head. A
    //    revert or an empty answer is absence (the Balancer/Curve case); a
    //    transport failure refuses instead of reading as "no factory".
    let factory = match io.read(address, IPoolIdentity::factoryCall {}, None).await {
        Ok(factory) => Some(factory),
        Err(ProviderError::ExecutionReverted { .. } | ProviderError::DecodingError { .. }) => None,
        Err(err) => return Err(PoolIdentityError::Io(err)),
    };
    if let Some(factory) = factory {
        if let Some(registered) = registered_identity(chain_id, factory) {
            return Ok(registered);
        }
        // An unregistered factory: the positive probes name the family, and
        // the factory evidence rides the answer.
        return probe_identity(chain_id, Some(factory), address, io).await;
    }

    // 3. No `factory()` getter: Balancer identity first (`getPoolId()` plus
    //    the sub-family probe and the factory-membership walk), then the
    //    factory-less rule — a positive Curve coin getter is the only
    //    factory-less pool identity (every V2/V3-family deployment exposes
    //    `factory()`, and a factory-less answer naming one is not a pool
    //    identity at all).
    if probe_present(
        io,
        address,
        IBalancerPool::getPoolIdCall {}.abi_encode(),
        None,
    )
    .await?
    {
        return balancer_identity(chain_id, address, None, io).await;
    }
    if probe_present(io, address, IUniswapV3Pool::slot0Call {}.abi_encode(), None).await? {
        return Err(PoolIdentityError::Unsupported {
            address,
            detail: "a factory-less concentrated-liquidity (slot0) answer is not a pool identity: \
                     every V3-family deployment exposes factory()"
                .to_owned(),
        });
    }
    if probe_present(
        io,
        address,
        IUniswapV2Pair::getReservesCall {}.abi_encode(),
        None,
    )
    .await?
    {
        return Err(PoolIdentityError::Unsupported {
            address,
            detail: "a factory-less constant-product (getReserves) answer is not a pool identity: \
                     every V2-family deployment exposes factory()"
                .to_owned(),
        });
    }
    if probe_curve(io, address, None).await? {
        return Ok(curve_identity(None));
    }
    Err(PoolIdentityError::UnknownIdentity { address })
}

/// The chain-id narrowing the DB seam's `i64` keys need.
fn chain_id_as_i64(chain_id: u64) -> i64 {
    i64::try_from(chain_id).unwrap_or(i64::MAX)
}

/// The positive-probe arm: V3 (`slot0`) → V2 (`getReserves`) → Balancer
/// (`getPoolId` + sub-family) → Curve (a supported `coins(0)` getter), first
/// positive answer wins. `factory` is the factory evidence to carry into the
/// answer (`None` on the factory-less path, where the Balancer arm's
/// membership walk is the only factory source).
async fn probe_identity(
    chain_id: u64,
    factory: Option<Address>,
    address: Address,
    io: &ConstructionIo,
) -> Result<ResolvedPoolIdentity, PoolIdentityError> {
    if probe_present(io, address, IUniswapV3Pool::slot0Call {}.abi_encode(), None).await? {
        return Ok(with_factory(identity_for_kind("uniswap_v3"), factory));
    }
    if probe_present(
        io,
        address,
        IUniswapV2Pair::getReservesCall {}.abi_encode(),
        None,
    )
    .await?
    {
        return Ok(with_factory(identity_for_kind("uniswap_v2"), factory));
    }
    if probe_present(
        io,
        address,
        IBalancerPool::getPoolIdCall {}.abi_encode(),
        None,
    )
    .await?
    {
        return balancer_identity(chain_id, address, factory, io).await;
    }
    if probe_curve(io, address, None).await? {
        return Ok(curve_identity(factory));
    }
    Err(PoolIdentityError::UnknownIdentity { address })
}

/// The Balancer identity arm: `getPoolId()` is already positive; the
/// sub-family probe (`getNormalizedWeights()` / `getAmplificationParameter()`)
/// names the family — or nothing does, which is a positively identified
/// Balancer pool of a shape no construction arm models, resolved under its own
/// name and refused there (never routed to another family's arm). A named
/// family claims its owning factory through the registered membership walk.
async fn balancer_identity(
    chain_id: u64,
    address: Address,
    factory: Option<Address>,
    io: &ConstructionIo,
) -> Result<ResolvedPoolIdentity, PoolIdentityError> {
    let (kind, sub_family) = match probe_balancer_type(io, &address, None).await {
        Ok(BalancerFamily::Weighted) => ("balancer_weighted", BalancerFamily::Weighted),
        Ok(BalancerFamily::Stable) => ("balancer_stable", BalancerFamily::Stable),
        // Neither sub-family probe answered: the unmodeled shape keeps its own
        // name, and reaches no factory claim (no membership walk).
        Err(ProviderError::DecodingError { .. }) => {
            return Ok(with_factory(
                identity_for_kind("balancer_unmodeled"),
                factory,
            ));
        }
        Err(err) => return Err(PoolIdentityError::Io(err)),
    };
    let candidates = balancer_membership_candidates(chain_id, sub_family);
    let owning = if candidates.is_empty() {
        None
    } else {
        fetch_balancer_factory(io, &address, &candidates, None).await?
    };
    if let Some(owning) = owning {
        // Contradiction guard: the membership row and the sub-family probe are
        // two identity facts about one pool.
        if let Some(registered) = registered_identity(chain_id, owning) {
            let probed = identity_for_kind(kind);
            if probed.as_ref().map(|p| (&p.family, &p.variant))
                != Some((&registered.family, &registered.variant))
            {
                return Err(PoolIdentityError::Contradictory {
                    address,
                    detail: format!(
                        "the pool answers {kind:?} but its owning factory {owning} is registered \
                         as {registered_kind:?}",
                        registered_kind = registered.persisted_kind,
                    ),
                });
            }
        }
    }
    Ok(with_factory(identity_for_kind(kind), owning.or(factory)))
}

/// The generic Curve identity (a supported `coins(0)` coin getter answered).
fn curve_identity(factory: Option<Address>) -> ResolvedPoolIdentity {
    with_factory(identity_for_kind("stableswap"), factory)
}

/// The kind-grammar identity, carrying the factory evidence.
fn with_factory(
    identity: Option<ResolvedPoolIdentity>,
    factory: Option<Address>,
) -> ResolvedPoolIdentity {
    #[expect(clippy::expect_used)] // every kind this module names is in the grammar
    let mut identity = identity.expect("kind grammar covers every spelling this module names");
    identity.factory = factory;
    identity
}

#[cfg(test)]
mod taxonomy_tests {
    //! ADR-059 D1: the registration-probe family is a projection of the pool
    //! taxonomy. The two legs the probe station cannot reach are pinned as
    //! `None` here, never silently mapped onto a neighbour.
    use super::PoolFamily;
    use degenbot_pools::{
        BalanceVectorVariant, BinnedLiquidityVariant, ConcentratedLiquidityVariant, Identity,
        ReservePairVariant,
    };

    fn reserve_pair(variant: ReservePairVariant) -> Identity {
        Identity::ReservePair { variant, dex: None }
    }

    fn concentrated_liquidity(variant: ConcentratedLiquidityVariant) -> Identity {
        Identity::ConcentratedLiquidity { variant, dex: None }
    }

    fn balance_vector(variant: BalanceVectorVariant) -> Identity {
        Identity::BalanceVector { variant, dex: None }
    }

    #[test]
    fn identity_projection_maps_only_the_probed_families() {
        assert_eq!(
            PoolFamily::from_identity(&reserve_pair(ReservePairVariant::UniswapV2)),
            Some(PoolFamily::V2),
        );
        assert_eq!(
            PoolFamily::from_identity(&reserve_pair(ReservePairVariant::AerodromeV2 {
                stable: true
            })),
            None,
        );
        assert_eq!(
            PoolFamily::from_identity(&reserve_pair(ReservePairVariant::AerodromeV2 {
                stable: false
            })),
            None,
        );
        assert_eq!(
            PoolFamily::from_identity(&concentrated_liquidity(
                ConcentratedLiquidityVariant::UniswapV3
            )),
            Some(PoolFamily::V3),
        );
        assert_eq!(
            PoolFamily::from_identity(&concentrated_liquidity(
                ConcentratedLiquidityVariant::UniswapV4
            )),
            None,
        );
        assert_eq!(
            PoolFamily::from_identity(&balance_vector(BalanceVectorVariant::Curve)),
            Some(PoolFamily::Curve),
        );
        assert_eq!(
            PoolFamily::from_identity(&balance_vector(BalanceVectorVariant::BalancerWeighted)),
            Some(PoolFamily::BalancerWeighted),
        );
        assert_eq!(
            PoolFamily::from_identity(&balance_vector(BalanceVectorVariant::BalancerStable)),
            Some(PoolFamily::BalancerStable),
        );
        assert_eq!(
            PoolFamily::from_identity(&Identity::BinnedLiquidity {
                variant: BinnedLiquidityVariant::Lfj,
                dex: None,
            }),
            None,
            "no single-address probe for binned liquidity: it must lag",
        );
    }
}
