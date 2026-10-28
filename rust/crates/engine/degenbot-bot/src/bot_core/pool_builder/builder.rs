//! Pool construction orchestration .
//!
//! Part 2 of the builder port: the probe-dispatch-assemble turn a bare
//! on-chain `(chain_id, address)` into a core structural pool
//! identity+state, reading every construction-tier contract value through the
//! one call step ([`ContractRead`]) over a [`ConstructionIo`]. This module is
//! the single source for constructing V2
//! pools end-to-end with zero Python; the DEX-variant/fees/deployer/init_hash
//! come from the built-in [`degenbot_uniswap::dex_identity`] presets (keyed by
//! factory, with a per-pool `stableSwap()`/`stable()` read call to disambiguate
//! the shared Camelot/Aerodrome volatile-vs-stable factory).
//!
//! No `pyo3` in this module (the no-pyo3-in-cores invariant). A standalone
//! `cargo add degenbot` consumer reaches [`build_v2`] via the `degenbot`
//! umbrella (Tier-0 slice in `examples/standalone_consumer.rs`).

use hashbrown::HashMap;
use std::sync::Arc;

use alloy::dyn_abi::{DynSolType, DynSolValue};
use alloy::primitives::{Address, B256, U256};
use degenbot_core::address_utils::address_to_checksum_string;
use degenbot_core::errors::ProviderError;
use degenbot_db::error::DbError;
use degenbot_db::snapshot::TickMapDb;
use degenbot_pools::resolve_curve_strategy_discriminants;
use degenbot_pools::state::spec_bounds;
use degenbot_pools::CurveDataProvider;
use degenbot_pools::RegisterAerodromeV2PoolParams;
use degenbot_pools::RegisterBalancerStablePoolParams;
use degenbot_pools::RegisterBalancerWeightedPoolParams;
use degenbot_pools::RegisterCurvePoolParams;
use degenbot_pools::RegisterV2PoolParams;
use degenbot_pools::SpecRefusal;
use degenbot_pools::TickWordKey;
use degenbot_pools::{
    BalanceVectorVariant, ConcentratedLiquidityVariant, Identity, ReservePairVariant,
};
use degenbot_pools::{ConcentratedLiquiditySlotLayout, RegisterV3PoolParams};
use degenbot_pools::{RegisterV4PoolParams, V4PoolKey};
use degenbot_rpc::abi::{self, ContractRead};
use degenbot_rpc::multicall3::{self, MulticallResult, MULTICALL3_ADDRESS};
use degenbot_rpc::tick_bootstrap::{fetch_tick_word_via, UnreadableBitmapPolicy};
use degenbot_uniswap::deployments;
use degenbot_uniswap::dex_identity::{self, DexIdentity, DexVariant};

use super::curve_choreography;
use super::parity;
use crate::bot_core::construction_io::{ConstructionIo, PacedRead};
use crate::bot_core::curve_data_provider_impl::RpcCurveDataProvider;
use degenbot_pools::PoolTickCoverage;
use degenbot_pools::TickInfo;
use degenbot_substrate::planning::{TickMapPoolIdentity, TickMapSeed};
use degenbot_substrate::tick_assembly::{
    chain_arm, reconcile_tracked_rows, resolve_tick_map_arm, seed_row_words, TickMapAssemblyError,
};

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

/// Typed builder failure.
#[derive(Debug, thiserror::Error)]
pub enum PoolBuilderError {
    #[error("RPC/decode error: {0}")]
    Rpc(#[from] ProviderError),
    #[error("unknown factory {factory} — no built-in DEX variant preset")]
    UnknownVariant { factory: Address },
    #[error("unknown pool identity at {address}: no identity selector answered")]
    UnknownPoolIdentity { address: Address },
    #[error("out of spec ({0})")]
    Spec(SpecRefusal),
    #[error("decode failure: {message}")]
    Decoding { message: String },
    #[error("{0}")]
    Identity(Box<parity::IdentityDivergence>),
    #[error("DB read failed: {0}")]
    Db(#[from] DbError),
    #[error("V4 identity incomplete: {message}")]
    MissingIdentity { message: String },
    /// A Tracked Db snapshot failed the intake reconciliation against CHAIN
    /// truth: its tick rows disagree with the word bitmaps read from chain.
    /// Registration is rejected with a typed error; the message names the
    /// conflict.
    #[error("tick map assembly failure: {0}")]
    TickAssembly(#[from] degenbot_substrate::tick_assembly::TickMapAssemblyError),
}

/// Sentinels returned when an ERC-20 metadata field cannot be resolved,
/// mirroring `erc20.py::UNKNOWN_NAME/UNKNOWN_SYMBOL/UNKNOWN_DECIMALS` (the
/// canonical Python display constants) so the core twin is byte-identical.
const UNKNOWN_NAME: &str = "Unknown Token";
const UNKNOWN_SYMBOL: &str = "UNKNOWN";
const UNKNOWN_DECIMALS: u8 = 18;

/// Fetch ERC-20 `name()` / `symbol()` / `decimals()` in three reads.
///
/// `None` when any of the three reads fails to answer or fails to decode. That
/// silence is this read's whole policy: what a field it could not read becomes
/// is [`build_erc20_metadata`]'s decision (its alternate-prototype fallback),
/// not this read's.
pub async fn fetch_erc20_metadata(
    read: &impl ContractRead,
    address: &Address,
) -> Option<(String, String, u64)> {
    let name = fetch_metadata_string(read, b"name()", address).await?;
    let symbol = fetch_metadata_string(read, b"symbol()", address).await?;
    let decimals_bytes = read
        .call(*address, abi::selector(b"decimals()").to_vec().into(), None)
        .await
        .ok()?;
    let decimals = match DynSolType::Uint(256).abi_decode(&decimals_bytes) {
        Ok(DynSolValue::Uint(n, _)) => n.to::<u64>(),
        _ => return None,
    };
    Some((name, symbol, decimals))
}

/// One token-metadata field decoded as a Solidity `string`; a transport or
/// decode failure is `None` so the caller can try the alternate prototype
/// (`NAME()` / `SYMBOL()`) or fall back to the UNKNOWN sentinel.
async fn fetch_metadata_string(
    read: &impl ContractRead,
    signature: &[u8],
    address: &Address,
) -> Option<String> {
    let bytes = read
        .call(*address, abi::selector(signature).to_vec().into(), None)
        .await
        .ok()?;
    match DynSolType::String.abi_decode(&bytes) {
        Ok(DynSolValue::String(value)) => Some(value),
        _ => None,
    }
}

/// Decode a Multicall3 sub-call's return data as a Solidity `string`
/// (`name()` / `symbol()`). A reverted sub-call or non-string return is `None`
/// (the caller's per-field alternate-prototype fallback contract).
fn decode_metadata_string(res: &MulticallResult) -> Option<String> {
    if !res.success {
        return None;
    }
    match DynSolType::String.abi_decode(&res.return_data) {
        Ok(DynSolValue::String(s)) => Some(s),
        _ => None,
    }
}

/// Decode a Multicall3 sub-call's return data as a `uint256` (`decimals()`);
/// `None` on revert or decode failure.
fn decode_metadata_decimals(res: &MulticallResult) -> Option<u64> {
    if !res.success {
        return None;
    }
    match DynSolType::Uint(256).abi_decode(&res.return_data) {
        Ok(DynSolValue::Uint(n, _)) => Some(n.to::<u64>()),
        _ => None,
    }
}

/// Fetch ERC-20 `name()` / `symbol()` / `decimals()` for MANY tokens in ONE
/// Multicall3 `aggregate3` call, degrading to per-token [`fetch_erc20_metadata`]
/// when the multicall itself fails.
///
/// Returns one `Option<(name, symbol, decimals)>` per input address, in order. A
/// token whose sub-call reverted or failed to decode is `None`, matching the
/// single-token path's fallback contract. The chunking and the degradation are
/// this read's policy; the call step and the `aggregate3` encode/decode are
/// shared (`degenbot_rpc::multicall3`).
pub async fn fetch_erc20_metadata_batch(
    read: &impl ContractRead,
    addresses: &[Address],
) -> Vec<Option<(String, String, u64)>> {
    if addresses.is_empty() {
        return Vec::new();
    }

    // 3 sub-calls per token: name(), symbol(), decimals().
    let mut calls: Vec<(Address, alloy::primitives::Bytes)> =
        Vec::with_capacity(addresses.len() * 3);
    for addr in addresses {
        calls.push((*addr, abi::selector(b"name()").to_vec().into()));
        calls.push((*addr, abi::selector(b"symbol()").to_vec().into()));
        calls.push((*addr, abi::selector(b"decimals()").to_vec().into()));
    }

    let batch = async {
        let calldata = multicall3::encode_aggregate3(&calls)?;
        let data = read.call(MULTICALL3_ADDRESS, calldata, None).await?;
        multicall3::decode_aggregate3_results(&data, calls.len())
    }
    .await;

    let Ok(results) = batch else {
        // Multicall3 not deployed / unreachable / malformed return: degrade to
        // the per-token path, preserving the pre-batch behaviour exactly.
        let mut out = Vec::with_capacity(addresses.len());
        for addr in addresses {
            out.push(fetch_erc20_metadata(read, addr).await);
        }
        return out;
    };

    let mut out = Vec::with_capacity(addresses.len());
    for chunk in results.chunks(3) {
        if chunk.len() < 3 {
            out.push(None);
            continue;
        }
        let name = decode_metadata_string(&chunk[0]);
        let symbol = decode_metadata_string(&chunk[1]);
        let decimals = decode_metadata_decimals(&chunk[2]);
        if let (Some(n), Some(s), Some(d)) = (name, symbol, decimals) {
            out.push(Some((n, s, d)));
        } else {
            out.push(None);
        }
    }
    out
}

/// Resolve ERC-20 token metadata DB-first, then on-chain, with the
/// alternate-prototype + UNKNOWN fallbacks — the core twin of
/// `Erc20Builder.build` steps 3–5.
///
/// Order:
/// 1. `io.fetch_erc20_token(chain_id, address)` — prefer the persisted row's
///    `name`/`symbol`/`decimals` when present.
/// 2. If any field is missing, guard that a contract is deployed
///    (`get_code` non-empty), then try the batched `fetch_erc20_metadata`
///    (all-or-nothing) and backfill missing fields.
/// 3. Any field still missing falls back to the alternate prototype reads
///    (`name()/NAME()`, `symbol()/SYMBOL()`, `decimals()/DECIMALS()`), else the
///    UNKNOWN sentinels.
/// 4. When the DB row existed but was fully blank, write the resolved values
///    back best-effort.
///
/// Returns `(name, symbol, decimals)`. This is a pure read fn over
/// [`ConstructionIo`] (no `BotState`) so a standalone `cargo add degenbot`
/// consumer can reach it via the umbrella; it does NOT itself register the
/// token — the `PyBot.build_erc20_token` seam calls this then
/// `BotState::register_token`.
///
/// # Errors
///
/// Returns [`PoolBuilderError::Rpc`] on an RPC failure, [`PoolBuilderError::Db`]
/// on a DB read failure, and [`PoolBuilderError::Decoding`] when no contract is
/// deployed at `address`. A metadata write-back failure is ignored.
///
/// # Panics
///
/// Never panics.
pub async fn build_erc20_metadata(
    io: &ConstructionIo,
    chain_id: i64,
    address: Address,
    block: Option<u64>,
) -> Result<(String, String, u8), PoolBuilderError> {
    // 1. DB-first.
    let db_row = io.fetch_erc20_token(chain_id, address).await?;
    let mut name = db_row.as_ref().and_then(|r| r.name.clone());
    let mut symbol = db_row.as_ref().and_then(|r| r.symbol.clone());
    let mut decimals = db_row
        .as_ref()
        .and_then(|r| r.decimals)
        .map(|dec| u8::try_from(dec).unwrap_or(UNKNOWN_DECIMALS));

    // All fields present in the DB — nothing more to resolve.
    if let (Some(n), Some(s), Some(d)) = (&name, &symbol, &decimals) {
        return Ok((n.clone(), s.clone(), *d));
    }

    // 2. Contract-present guard, then the batched all-or-nothing read.
    let code = io.get_code(address, block).await?;
    if code.is_empty() {
        return Err(PoolBuilderError::Decoding {
            message: "No contract deployed at this address".to_owned(),
        });
    }
    if let Some((n, s, d)) = fetch_erc20_metadata(io, &address).await {
        if name.is_none() {
            name = Some(n);
        }
        if symbol.is_none() {
            symbol = Some(s);
        }
        if decimals.is_none() {
            decimals = Some(u8::try_from(d).unwrap_or(UNKNOWN_DECIMALS));
        }
    }

    // 3. Per-field alternate-prototype fallback for anything still missing.
    if name.is_none() {
        name = Some(fetch_field_string(io, address, &[b"name()", b"NAME()"], UNKNOWN_NAME).await);
    }
    if symbol.is_none() {
        symbol = Some(
            fetch_field_string(io, address, &[b"symbol()", b"SYMBOL()"], UNKNOWN_SYMBOL).await,
        );
    }
    if decimals.is_none() {
        decimals = Some(fetch_field_decimals(io, address, &[b"decimals()", b"DECIMALS()"]).await);
    }
    let name = name.unwrap_or_else(|| UNKNOWN_NAME.to_string());
    let symbol = symbol.unwrap_or_else(|| UNKNOWN_SYMBOL.to_string());
    let decimals = decimals.unwrap_or(UNKNOWN_DECIMALS);

    // 4. Write back when the DB row existed but was fully blank.
    if db_row.is_some_and(|r| r.name.is_none() && r.symbol.is_none() && r.decimals.is_none()) {
        let addr_hex = address_to_checksum_string(&address);
        let _ = io
            .update_erc20_token_metadata(
                chain_id,
                &addr_hex,
                Some(&name),
                Some(&symbol),
                Some(i64::from(decimals)),
            )
            .await;
    }

    Ok((name, symbol, decimals))
}

/// Read a string field trying each prototype in order (e.g. `name()` then
/// `NAME()`), returning the FIRST that decodes, else `UNKNOWN_NAME`.
async fn fetch_field_string(
    io: &ConstructionIo,
    address: Address,
    prototypes: &[&[u8]],
    fallback: &str,
) -> String {
    for p in prototypes {
        if let Ok(s) = abi::fetch_string_field(io, p, &address, None).await {
            return s;
        }
    }
    fallback.to_string()
}

/// Read `decimals()` (or an alternate prototype) as a `uint256`, validated to
/// `u8`; else `UNKNOWN_DECIMALS`.
async fn fetch_field_decimals(io: &ConstructionIo, address: Address, prototypes: &[&[u8]]) -> u8 {
    for p in prototypes {
        if let Ok(v) = abi::fetch_uint256_field(io, p, &address, None).await {
            if let Ok(dec) = u8::try_from(v.to::<u64>()) {
                return dec;
            }
        }
    }
    UNKNOWN_DECIMALS
}

// ---------------------------------------------------------------------------
// Pool-identity and fee reads
//
// The multi-read assemblies a builder runs before it can name a pool. Each is a
// fixed sequence of single reads over the one call step — no fallback, no
// swallow: a pool that cannot answer fails its build instead of degrading into
// a shape the state machine would have to defend against.
// ---------------------------------------------------------------------------

/// A V2-style pool's immutable identity, read from the pool itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2Immutable {
    /// The factory that deployed the pool.
    pub factory: Address,
    /// The pool's first token.
    pub token0: Address,
    /// The pool's second token.
    pub token1: Address,
}

/// A V3-style pool's immutable identity plus its fee tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V3Immutable {
    /// The factory that deployed the pool.
    pub factory: Address,
    /// The pool's first token.
    pub token0: Address,
    /// The pool's second token.
    pub token1: Address,
    /// The pool's fee in hundredths of a bip.
    pub fee: u32,
    /// The pool's tick spacing.
    pub tick_spacing: i32,
}

/// Fetch a V2-style pool's `factory()` / `token0()` / `token1()`.
///
/// # Errors
///
/// Returns a [`ProviderError`] when any of the three reads fails.
pub async fn fetch_v2_immutable_data(
    read: &impl ContractRead,
    address: &Address,
    block: Option<u64>,
) -> Result<V2Immutable, ProviderError> {
    Ok(V2Immutable {
        factory: abi::fetch_pool_factory(read, address, block).await?,
        token0: abi::fetch_pool_token0(read, address, block).await?,
        token1: abi::fetch_pool_token1(read, address, block).await?,
    })
}

/// Fetch a V3-style pool's `factory()` / `token0()` / `token1()` / `fee()` /
/// `tickSpacing()`.
///
/// # Errors
///
/// Returns a [`ProviderError`] when any of the five reads fails.
pub async fn fetch_v3_immutable_data(
    read: &impl ContractRead,
    address: &Address,
    block: Option<u64>,
) -> Result<V3Immutable, ProviderError> {
    let factory = abi::fetch_pool_factory(read, address, block).await?;
    let token0 = abi::fetch_pool_token0(read, address, block).await?;
    let token1 = abi::fetch_pool_token1(read, address, block).await?;
    let fee = abi::fetch_no_arg_uint(read, b"fee()", address, block, &DynSolType::Uint(24)).await?;
    let tick_spacing =
        abi::fetch_no_arg_int(read, b"tickSpacing()", address, block, &DynSolType::Int(24)).await?;
    Ok(V3Immutable {
        factory,
        token0,
        token1,
        fee: fee.to::<u32>(),
        tick_spacing: tick_spacing.try_into().unwrap_or(0),
    })
}

/// Camelot V2 pool fee configuration read from the pool (ADR-005).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CamelotState {
    /// The pool's `stableSwap()` flag.
    pub stable: bool,
    /// `FEE_DENOMINATOR()` return (a `uint256`).
    pub fee_denominator: U256,
    /// `token0FeePercent()` return.
    pub token0_fee_percent: u64,
    /// `token1FeePercent()` return.
    pub token1_fee_percent: u64,
}

/// Fetch a Camelot V2 pool's `stableSwap()` flag and fee configuration
/// (ADR-005). The four probes are independent reads over the one call step.
///
/// # Errors
///
/// Returns a [`ProviderError`] when any of the four reads fails.
pub async fn fetch_camelot_state(
    read: &impl ContractRead,
    address: &Address,
    block: Option<u64>,
) -> Result<CamelotState, ProviderError> {
    let uint = &DynSolType::Uint(256);
    Ok(CamelotState {
        stable: abi::fetch_no_arg_bool(read, b"stableSwap()", address, block).await?,
        fee_denominator: abi::fetch_no_arg_uint(read, b"FEE_DENOMINATOR()", address, block, uint)
            .await?,
        token0_fee_percent: abi::fetch_no_arg_uint(
            read,
            b"token0FeePercent()",
            address,
            block,
            uint,
        )
        .await?
        .to::<u64>(),
        token1_fee_percent: abi::fetch_no_arg_uint(
            read,
            b"token1FeePercent()",
            address,
            block,
            uint,
        )
        .await?
        .to::<u64>(),
    })
}

/// Aerodrome V2 common data: the `stable()` flag (read on the pool, which
/// disambiguates the shared volatile/stable factory) and the
/// `getFee(address,bool)` unidirectional fee (read on the factory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AerodromeV2Common {
    /// The pool's `stable()` flag.
    pub stable: bool,
    /// Unidirectional fee in basis points (Aerodrome fee denominator `10_000`).
    pub fee_bps: u64,
}

/// Fetch an Aerodrome V2 pool's `stable()` flag + `getFee()` fee.
///
/// # Errors
///
/// Returns a [`ProviderError`] when either read fails.
pub async fn fetch_aerodrome_stable_and_fee(
    read: &impl ContractRead,
    address: &Address,
    factory: &Address,
    block: Option<u64>,
) -> Result<AerodromeV2Common, ProviderError> {
    let stable = abi::fetch_no_arg_bool(read, b"stable()", address, block).await?;
    // The fee read takes the pool's own `stable()` answer, so the stable flag
    // must be known before it — the one data dependency in this assembly.
    let fee_bps = abi::fetch_aerodrome_fee(read, factory, address, stable, block)
        .await?
        .to::<u64>();
    Ok(AerodromeV2Common { stable, fee_bps })
}

/// Probe a pool contract to identify its family via the canonical read-call
/// probe order (mirrors `type_resolution.py::probe_pool_type`):
/// `slot0()` → V3, `getReserves()` → V2, `getPoolId()` → Balancer (weighted vs
/// stable by `getNormalizedWeights()`), `coins(0)` → Curve.
///
/// An EVM revert means the selector is absent: the next probe runs. When no
/// probe answers — every selector reverts, or `coins(0)` returns an
/// undecodable word — the contract has no recognized identity and this
/// returns [`PoolBuilderError::UnknownPoolIdentity`], never
/// [`PoolFamily::Curve`]: an unverified contract is not a Curve pool. A
/// provider failure that is NOT a revert (transport, timeout, rate limit,
/// malformed response) likewise propagates as an error — a broken RPC must
/// never silently degrade a pool to Curve.
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
    if probe_present(io, address, abi::encode_slot0(), block).await? {
        Ok(PoolFamily::V3)
    } else if probe_present(io, address, abi::encode_get_reserves(), block).await? {
        Ok(PoolFamily::V2)
    } else if probe_present(io, address, abi::encode_get_pool_id(), block).await? {
        Ok(
            if probe_present(io, address, abi::encode_get_weights(), block).await? {
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

/// `true` when the selector answered, `false` on an EVM revert (selector
/// absent), `Err` on any other provider failure (mirrors the benign-revert
/// rule in [`build_balancer_stable`]).
async fn probe_present(
    read: &impl ContractRead,
    address: Address,
    calldata: Vec<u8>,
    block: Option<u64>,
) -> Result<bool, ProviderError> {
    match read.call(address, calldata.into(), block).await {
        Ok(_) => Ok(true),
        Err(ProviderError::ExecutionReverted { .. }) => Ok(false),
        Err(e) => Err(e),
    }
}

/// `true` when `coins(0)` answers with a decodable address.
///
/// `coins(uint256)` is exposed by every supported Curve family
/// (stable/metapools/two/tricrypto) — it is the positive identity signal. A
/// revert means the selector is absent and a garbage return means the call
/// site is not a Curve coin getter: both are "not Curve". A provider failure
/// that is not a revert propagates.
///
/// `coins(0)` needs the index word in the calldata (the ABI decoder rejects a
/// bare selector), so this cannot reuse [`probe_present`].
async fn probe_curve(
    read: &impl ContractRead,
    address: Address,
    block: Option<u64>,
) -> Result<bool, ProviderError> {
    match read
        .call(address, abi::encode_curve_coins_uint(0).into(), block)
        .await
    {
        Ok(bytes) => Ok(abi::decode_curve_coins(&bytes).is_ok()),
        Err(ProviderError::ExecutionReverted { .. }) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Resolve the [`DexIdentity`] preset (variant + factory + deployer + `init_hash`
/// + default fees) for a pool whose `factory()` has been read.
///
/// Matches the factory against the built-in [`DexIdentity`] presets. The
/// volatile-vs-stable variants share a factory (Camelot, Aerodrome), so for
/// those the pool's own `stableSwap()` / `stable()` read call disambiguates —
/// the built-in "read the contract to pick the DEX variant" mechanism.
///
/// Returns `None` for an unknown factory (ad-hoc / unregistered deployment); the
/// caller decides whether to error or fall back.
///
/// # Errors
///
/// Returns a [`ProviderError`] only if the disambiguating stable flag read
/// reverts (the pool is a degenerate/partial contract).
pub async fn resolve_v2_dex(
    io: &ConstructionIo,
    address: Address,
    factory: Address,
    block: Option<u64>,
) -> Result<Option<DexIdentity>, ProviderError> {
    if factory == dex_identity::CAMELOT_V2_STABLE.factory {
        let stable = abi::fetch_no_arg_bool(io, b"stableSwap()", &address, block).await?;
        Ok(Some(if stable {
            dex_identity::CAMELOT_V2_STABLE
        } else {
            dex_identity::CAMELOT_V2_VOLATILE
        }))
    } else if factory == dex_identity::AERODROME_V2_STABLE.factory {
        let stable = abi::fetch_no_arg_bool(io, b"stable()", &address, block).await?;
        Ok(Some(if stable {
            dex_identity::AERODROME_V2_STABLE
        } else {
            dex_identity::AERODROME_V2_VOLATILE
        }))
    } else if factory == dex_identity::UNISWAP_V2.factory {
        Ok(Some(dex_identity::UNISWAP_V2))
    } else if factory == dex_identity::SUSHISWAP_V2.factory {
        Ok(Some(dex_identity::SUSHISWAP_V2))
    } else if factory == dex_identity::PANCAKESWAP_V2.factory {
        Ok(Some(dex_identity::PANCAKESWAP_V2))
    } else if factory == dex_identity::SWAPBASED_V2.factory {
        Ok(Some(dex_identity::SWAPBASED_V2))
    } else {
        Ok(None)
    }
}

/// Read Camelot's `FEE_DENOMINATOR()`, used to scale the solidly-stable math.
/// `None` when the read fails (non-Camelot-stable callers never ask).
async fn fetch_fee_denominator(
    read: &impl ContractRead,
    address: Address,
    block: Option<u64>,
) -> Result<Option<u64>, ProviderError> {
    Ok(abi::fetch_no_arg_uint(
        read,
        b"FEE_DENOMINATOR()",
        &address,
        block,
        &DynSolType::Uint(256),
    )
    .await
    .ok()
    .map(|denominator| denominator.to::<u64>()))
}

/// Assemble `build_v2` params for a V2-style constant-product pool.
///
/// Reads immutable data (`factory()`/`token0()`/`token1()`), `getReserves()`,
/// resolves the [`DexIdentity`] preset via the built-in factory match + stable
/// read, verifies the CREATE2 address, and resolves deployment
/// deployer/`init_hash` — producing a [`RegisterV2PoolParams`] ready for
/// `BotState::register_v2_pool` with no Python round-trip.
///
/// # Errors
///
/// Returns [`PoolBuilderError::UnknownVariant`] for an unregistered factory,
/// [`PoolBuilderError::Identity`] on a CREATE2 mismatch (when the factory ships
/// in the JSON), or an RPC/spec error.
pub async fn build_v2(
    chain_id: u64,
    address: Address,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RegisterV2PoolParams, PoolBuilderError> {
    let imm = fetch_v2_immutable_data(io, &address, block).await?;
    let (r0, r1) = abi::fetch_v2_reserves(io, &address, block).await?;
    let id = resolve_v2_dex(io, address, imm.factory, block)
        .await?
        .ok_or(PoolBuilderError::UnknownVariant {
            factory: imm.factory,
        })?;

    let reserve0 = spec_bounds::narrow_v2_reserve(r0, "reserve0")
        .map_err(|_| PoolBuilderError::Spec(SpecRefusal::V2Reserve))?;
    let reserve1 = spec_bounds::narrow_v2_reserve(r1, "reserve1")
        .map_err(|_| PoolBuilderError::Spec(SpecRefusal::V2Reserve))?;

    parity::verify_v2_create2(chain_id, imm.factory, address, imm.token0, imm.token1)
        .map_err(|divergence| PoolBuilderError::Identity(Box::new(divergence)))?;

    let deployer = deployments::resolve_deployer(chain_id, imm.factory);
    let init_hash: B256 = deployments::resolve_v2_init_hash(chain_id, imm.factory);

    let stable_swap = matches!(id.variant, DexVariant::CamelotV2Stable);
    let fee_denominator = if stable_swap {
        fetch_fee_denominator(io, address, block).await?
    } else {
        None
    };

    Ok(RegisterV2PoolParams {
        address,
        token0: imm.token0,
        token1: imm.token1,
        reserve0,
        reserve1,
        fee_token0: id.fee_token0,
        fee_token1: id.fee_token1,
        factory: imm.factory,
        deployer,
        init_hash,
        update_block: block.unwrap_or(0),
        variant: id.variant,
        stable_swap,
        fee_denominator,
    })
}

/// Build an Aerodrome V2 (constant-product, unidirectional-fee) pool: a bare
/// `(chain_id, address)` becomes
/// a [`RegisterAerodromeV2PoolParams`] ready for `BotState::register_aerodrome_pool`.
///
/// The Aerodrome volatile/stable pair shares one factory, so the two reads the
/// `getReserves` probe can't reach are: `stable()` on the pool (picks the
/// variant) and `getFee(address,bool)` on the factory (the unidirectional fee;
/// `10_000` = 100%). CREATE2 identity is verified against the JSON-sourced
/// EIP-1167 deployer + implementation (the Aerodrome parity gap).
///
/// # Errors
///
/// Returns [`PoolBuilderError::Rpc`] on an RPC/decode failure,
/// [`PoolBuilderError::Spec`] with [`SpecRefusal::V2Reserve`] on an out-of-u112
/// bounds reserve, or [`PoolBuilderError::Identity`] on an EIP-1167 address
/// mismatch.
pub async fn build_aerodrome_v2(
    chain_id: u64,
    address: Address,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RegisterAerodromeV2PoolParams, PoolBuilderError> {
    let imm = fetch_v2_immutable_data(io, &address, block).await?;
    let (r0, r1) = abi::fetch_v2_reserves(io, &address, block).await?;
    let common = fetch_aerodrome_stable_and_fee(io, &address, &imm.factory, block).await?;

    let id = if common.stable {
        dex_identity::AERODROME_V2_STABLE
    } else {
        dex_identity::AERODROME_V2_VOLATILE
    };

    parity::verify_aerodrome_v2_create2(
        chain_id,
        imm.factory,
        address,
        imm.token0,
        imm.token1,
        common.stable,
    )
    .map_err(|divergence| PoolBuilderError::Identity(Box::new(divergence)))?;

    let reserve0 = spec_bounds::narrow_v2_reserve(r0, "reserve0")
        .map_err(|_| PoolBuilderError::Spec(SpecRefusal::V2Reserve))?;
    let reserve1 = spec_bounds::narrow_v2_reserve(r1, "reserve1")
        .map_err(|_| PoolBuilderError::Spec(SpecRefusal::V2Reserve))?;

    // The Solidly stable invariant needs the `10**decimals` scale factors;
    // fetch the ERC-20 decimal counts for both tokens (unused by volatile
    // pools but always carried on the identity).
    let decimals0 = decimals_of(io, imm.token0, block)
        .await?
        .try_into()
        .map_err(|_| PoolBuilderError::Decoding {
            message: "token0 decimals overflow u8 while building Aerodrome V2 pool".into(),
        })?;
    let decimals1 = decimals_of(io, imm.token1, block)
        .await?
        .try_into()
        .map_err(|_| PoolBuilderError::Decoding {
            message: "token1 decimals overflow u8 while building Aerodrome V2 pool".into(),
        })?;

    Ok(RegisterAerodromeV2PoolParams {
        address,
        token0: imm.token0,
        token1: imm.token1,
        factory: imm.factory,
        variant: id.variant,
        stable: common.stable,
        fee: (common.fee_bps, 10_000),
        token0_decimals: decimals0,
        token1_decimals: decimals1,
        reserve0,
        reserve1,
        update_block: block.unwrap_or(0),
    })
}

// ---------------------------------------------------------------------------
// Balancer V2 constructors (builder follow-up: weighted + stable)
// ---------------------------------------------------------------------------

/// Balancer V2 pool-ID decoding (mirrors
/// `balancer_builder_base.py::decode_pool_id`). The 32-byte identifier packs
/// the pool contract address (bytes 0..20), `specialization` (uint16, 20..22)
/// and `nonce` (bytes 22..32).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedBalancerPoolId {
    /// The pool contract address (low 20 bytes of the identifier).
    pub pool_address: Address,
    /// Vault specialization (`0` General, `1` `MinimalSwapInfo`, `2` `TwoToken`).
    pub specialization: u16,
    /// Registration nonce.
    pub nonce: u64,
}

/// Decode a 32-byte Balancer V2 pool identifier.
#[must_use]
pub fn decode_balancer_pool_id(raw: &[u8; 32]) -> DecodedBalancerPoolId {
    DecodedBalancerPoolId {
        pool_address: Address::from_slice(&raw[0..20]),
        specialization: u16::from_be_bytes([raw[20], raw[21]]),
        nonce: u64::from_be_bytes([
            raw[22], raw[23], raw[24], raw[25], raw[26], raw[27], raw[28], raw[29],
        ]),
    }
}

/// Resolve a Balancer V2 stable pool's `StableMath` invariant version from the
/// pool's *shape*: a pool that lists its own address among the Vault tokens is a
/// `ComposableStablePool` (`bpt` present) and ships the monorepo `StableMath`
/// (always-roundDown `D_P`) — `1` (`INVARIANT_V1`); every non-composable stable
/// pool (`MetaStablePool`, the 2021 `StablePool`) ships the 2021 deployed
/// `StableMath` (roundUp `P_D`) — `2` (`INVARIANT_V2`).
///
/// The Vault specialization is NOT a family signal: it is derived from the
/// token count (`TwoToken` for 2-token pools), so the wstETH/WETH `MetaStablePool`
/// (`0x32296969…`, `specialization = 0x0002`) reads exactly like a 2-token
/// composable. Rate providers do not discriminate either — both families answer
/// `getRateProviders()` on mainnet. The BPT-in-tokens shape is the one signal
/// that is both family-exact (composable pools are the only pools registering
/// their own BPT as a Vault token) and already fetched by the builder, so it
/// costs no extra RPC.
#[must_use]
pub const fn derive_invariant_version(bpt_present: bool) -> u8 {
    if bpt_present {
        1
    } else {
        2
    }
}

/// The `2e18` constant (`0x1BC16D674EC80000`) that only appears in the
/// deployed bytecode of **V2** `WeightedPool` contracts (the `y == TWO`
/// fast-path in `powDown`/`powUp`). Absent from V1 `WeightedPool2Tokens` —
/// mirroring `detect_pow_version` in `balancer/pools.py`.
const POW_V2_TWO: [u8; 8] = [0x1B, 0xC1, 0x6D, 0x67, 0x4E, 0xC8, 0x00, 0x00];

/// Detect which `FixedPoint` library version a pool contract uses from its
/// deployed bytecode: V2 (`2`) if the `2e18` constant is present, else V1
/// (`1`). Returns the opaque `u8` discriminator `PowVersion` Rust stores.
#[must_use]
pub fn detect_pow_version(bytecode: &[u8]) -> u8 {
    if bytecode.windows(8).any(|w| w == POW_V2_TWO) {
        2
    } else {
        1
    }
}

/// Compute a single token's Balancer scaling factor `ONE * 10^(18 - decimals)`
/// (mirrors `scaling_helpers.py::_compute_scaling_factor`; `ONE = 1e18`).
///
/// # Errors
///
/// Returns [`PoolBuilderError::Decoding`] when the token reports more than 18
/// decimals (the `10^(18 - decimals)` scale cannot be represented), or
/// [`PoolBuilderError::Rpc`] on an RPC/decode failure.
#[expect(clippy::cast_possible_truncation)]
async fn compute_scaling_factor(
    io: &ConstructionIo,
    token: Address,
    block: Option<u64>,
) -> Result<U256, PoolBuilderError> {
    let decimals = decimals_of(io, token, block).await?;
    if decimals > 18 {
        return Err(PoolBuilderError::Decoding {
            message: format!("token {token} has {decimals} decimals (> 18, unsupported)"),
        });
    }
    let one = U256::from(1_000_000_000_000_000_000u128); // 1e18
    let pow = U256::from(10u64.pow(18 - decimals as u32));
    Ok(one * pow)
}

/// Read an ERC-20 token's `decimals()` (`uint8`, right-aligned in its ABI
/// word) over the one call step.
///
/// # Errors
///
/// Returns [`PoolBuilderError::Rpc`] on an `eth_call`/decode failure.
async fn decimals_of(
    read: &impl ContractRead,
    token: Address,
    block: Option<u64>,
) -> Result<u64, PoolBuilderError> {
    abi::fetch_uint256_field(read, b"decimals()", &token, block)
        .await
        .map(|decimals| decimals.to::<u64>())
        .map_err(PoolBuilderError::Rpc)
}

/// A Balancer token's scaling factor, with an unreadable `decimals()` classified
/// as the pool fact it is.
///
/// A token that answers `decimals()` with nothing (no runtime code — a token
/// self-destructed after its pool was registered — or an empty return) cannot be
/// scaled, so the pool cannot be built. That is [`SpecRefusal::NoCodeCoin`], not
/// an RPC fault, which keeps a broken pool out of the retry class.
///
/// # Errors
///
/// Returns [`PoolBuilderError::Spec`] with [`SpecRefusal::NoCodeCoin`] when the
/// `decimals()` answer does not decode, or whatever
/// [`compute_scaling_factor`] returns for a transport fault.
async fn balancer_scaling_factor(
    io: &ConstructionIo,
    token: Address,
    block: Option<u64>,
) -> Result<U256, PoolBuilderError> {
    compute_scaling_factor(io, token, block)
        .await
        .map_err(|error| match error {
            PoolBuilderError::Rpc(degenbot_core::errors::ProviderError::DecodingError {
                ..
            }) => PoolBuilderError::Spec(SpecRefusal::NoCodeCoin),
            other => other,
        })
}

/// The Balancer pool sub-type resolved by [`probe_balancer_type`] — the
/// semantic owner of the family vocabulary; the binding mirrors its two
/// discriminants as the Python `BalancerFamily` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalancerFamily {
    /// Has `getNormalizedWeights()` — a weighted pool.
    Weighted,
    /// Has `getAmplificationParameter()` (no weights) — a stable pool.
    Stable,
}

/// Find the Balancer V2 factory that owns `pool`, by asking each candidate
/// factory's `isPoolFromFactory(pool)`.
///
/// Balancer V2 exposes no enumerable pool registry (`getPoolCount` / `getPools`
/// / `getPoolAt` revert on every factory revision), and a deployed pool carries
/// no `factory()` getter — so membership is the only on-chain ownership answer.
/// A candidate that reverts or fails to decode is not the owner; a candidate
/// that does not own the pool reports `false`. That skip is this read's policy:
/// it is what "ask every candidate" means.
///
/// Returns `None` when no candidate owns the pool (a factory revision the
/// registry does not carry — e.g. the `MetaStablePool` factory).
pub async fn fetch_balancer_factory(
    read: &impl ContractRead,
    pool: &Address,
    candidates: &[Address],
    block: Option<u64>,
) -> Option<Address> {
    for factory in candidates {
        let Ok(owns) = abi::fetch_balancer_is_pool_from_factory(read, factory, pool, block).await
        else {
            continue;
        };
        if owns {
            return Some(*factory);
        }
    }
    None
}

/// Probe a Balancer pool's sub-type: `getNormalizedWeights()` answers →
/// [`BalancerFamily::Weighted`]; else `getAmplificationParameter()` answers →
/// [`BalancerFamily::Stable`]; else an error (linear pools unsupported).
///
/// # Errors
///
/// Returns a [`ProviderError`] when neither probe responds.
pub async fn probe_balancer_type(
    read: &impl ContractRead,
    address: &Address,
    block: Option<u64>,
) -> Result<BalancerFamily, ProviderError> {
    if abi::fetch_balancer_weights(read, address, block)
        .await
        .is_ok()
    {
        return Ok(BalancerFamily::Weighted);
    }
    if abi::fetch_balancer_amp(read, address, block).await.is_ok() {
        return Ok(BalancerFamily::Stable);
    }
    Err(ProviderError::DecodingError {
        message: format!(
            "Cannot determine Balancer pool type for {address}: neither \
             getNormalizedWeights() nor getAmplificationParameter() responded"
        ),
    })
}

/// Assemble `build_balancer_weighted` params for a Balancer V2 weighted pool
/// (the Python builder's Rust twin): reads the 32-byte `poolId`,
/// the Vault `getPoolTokens`, `getSwapFeePercentage`, `getNormalizedWeights`,
/// detects the `PowVersion` from the deployed bytecode, and computes the token
/// scaling factors from on-chain `decimals()` — producing a
/// [`RegisterBalancerWeightedPoolParams`] ready for
/// `BotState::register_balancer_weighted_pool` with no Python round-trip.
///
/// Mirrors `balancer_builder.py::_build_weighted` step-for-step (the token
/// ERC-20 companion objects are built Python-side off the handle, as with
/// `build_v2`).
///
/// # Errors
///
/// Returns [`PoolBuilderError::Rpc`] on an RPC/decode failure,
/// [`PoolBuilderError::Spec`] with [`SpecRefusal::BalancerBrokenPool`] for a
/// pool in [`BROKEN_BALANCER_V2_POOLS`] or
/// [`SpecRefusal::BalancerComposableWeighted`] for a pool whose own BPT is
/// among the Vault tokens, or [`PoolBuilderError::Decoding`] for a token
/// reporting more than 18 decimals.
pub async fn build_balancer_weighted(
    vault: Address,
    address: Address,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RegisterBalancerWeightedPoolParams, PoolBuilderError> {
    if BROKEN_BALANCER_V2_POOLS.contains(&address) {
        return Err(PoolBuilderError::Spec(SpecRefusal::BalancerBrokenPool));
    }

    let pool_id = abi::fetch_balancer_pool_id(io, &address, block).await?;
    let (tokens, balances) = abi::fetch_balancer_vault_tokens(io, &vault, &pool_id, block).await?;

    // A composable weighted pool (`ManagedPool`) registers its own BPT as a
    // Vault token and reports weights only for the underlying tokens, so the
    // weight vector is one short of the Vault token vector — the arity the
    // weighted state requires. Refuse the shape by name here, before the fee /
    // weight / decimals reads and before the registration arity check.
    if tokens.contains(&address) {
        return Err(PoolBuilderError::Spec(
            SpecRefusal::BalancerComposableWeighted,
        ));
    }

    let fee = abi::fetch_balancer_swap_fee(io, &address, block).await?;
    let weights = abi::fetch_balancer_weights(io, &address, block).await?;

    let bytecode = io.get_code(address, block).await?;
    let pow_version = detect_pow_version(&bytecode);

    let mut scaling_factors = Vec::with_capacity(tokens.len());
    for token in &tokens {
        scaling_factors.push(balancer_scaling_factor(io, *token, block).await?);
    }

    Ok(RegisterBalancerWeightedPoolParams {
        address,
        vault,
        pool_id,
        tokens,
        weights,
        scaling_factors,
        swap_fee: fee.to::<u128>(),
        pow_version,
        balances,
        update_block: block.unwrap_or(0),
    })
}

/// Assemble `build_balancer_stable` params for a Balancer V2 stable pool (the
/// Python builder's Rust twin): reads the `poolId`, Vault
/// `getPoolTokens`, `getSwapFeePercentage`, `getAmplificationParameter`; detects
/// the BPT index (token whose address matches the pool — `None` for
/// `MetaStablePools`); reads `getRateProviders` + per-provider `getRate()`
/// (empty when the pool doesn't expose them); computes rate-multiplied scaling
/// factors; and resolves the `invariant_version` from the pool's shape (BPT
/// among the Vault tokens → composable → V1, else V2; see
/// [`derive_invariant_version`]) with an optional override that always wins.
/// Returns a [`RegisterBalancerStablePoolParams`] for
/// `BotState::register_balancer_stable_pool`.
///
/// Mirrors `balancer_builder.py::_build_stable` step-for-step.
///
/// # Errors
///
/// Returns [`PoolBuilderError::Rpc`] on an RPC/decode failure,
/// [`PoolBuilderError::Spec`] with [`SpecRefusal::BalancerBrokenPool`] for a
/// pool in [`BROKEN_BALANCER_V2_POOLS`] or
/// [`SpecRefusal::BalancerRateProviderUnreadable`] for a pool whose rate
/// provider reverts, or [`PoolBuilderError::Decoding`] for a token reporting
/// more than 18 decimals.
pub async fn build_balancer_stable(
    vault: Address,
    address: Address,
    io: &ConstructionIo,
    block: Option<u64>,
    invariant_version_override: Option<u8>,
) -> Result<RegisterBalancerStablePoolParams, PoolBuilderError> {
    if BROKEN_BALANCER_V2_POOLS.contains(&address) {
        return Err(PoolBuilderError::Spec(SpecRefusal::BalancerBrokenPool));
    }

    let pool_id = abi::fetch_balancer_pool_id(io, &address, block).await?;
    let (tokens, balances) = abi::fetch_balancer_vault_tokens(io, &vault, &pool_id, block).await?;
    let fee = abi::fetch_balancer_swap_fee(io, &address, block).await?;
    let amp = abi::fetch_balancer_amp(io, &address, block).await?;

    let bpt_idx = tokens.iter().position(|t| *t == address);
    let invariant_version =
        invariant_version_override.unwrap_or(derive_invariant_version(bpt_idx.is_some()));
    let mut base_sf = Vec::with_capacity(tokens.len());
    for token in &tokens {
        base_sf.push(balancer_scaling_factor(io, *token, block).await?);
    }

    // getRateProviders reverts for WeightedPool2Tokens / MetaStablePools —
    // degrade to an empty list (mirrors the Python `except -> []`).
    let providers = match abi::fetch_balancer_rate_providers(io, &address, block).await {
        Ok(p) => p,
        Err(
            degenbot_core::errors::ProviderError::DecodingError { .. }
            | degenbot_core::errors::ProviderError::ExecutionReverted { .. },
        ) => Vec::new(),
        Err(e) => return Err(PoolBuilderError::Rpc(e)),
    };

    let one = U256::from(1_000_000_000_000_000_000u128); // 1e18
    let scaling_factors = if providers.is_empty() {
        base_sf
    } else {
        let mut scaled = Vec::with_capacity(tokens.len());
        for (bsf, provider) in base_sf.iter().zip(&providers) {
            let rate = if *provider == Address::ZERO {
                // Zero-address sentinel → rate of `ONE`.
                one
            } else {
                // A provider that reverts (a nested pool killed or in recovery
                // mode) or answers data that does not decode leaves the pool
                // unable to price its own tokens: a pool fact, not a transport
                // fault, so it refuses by name instead of surfacing as an RPC
                // error.
                match abi::fetch_balancer_rate(io, provider, block).await {
                    Ok(rate) => rate,
                    Err(
                        degenbot_core::errors::ProviderError::ExecutionReverted { .. }
                        | degenbot_core::errors::ProviderError::DecodingError { .. },
                    ) => {
                        return Err(PoolBuilderError::Spec(
                            SpecRefusal::BalancerRateProviderUnreadable,
                        ));
                    }
                    Err(e) => return Err(PoolBuilderError::Rpc(e)),
                }
            };
            scaled.push(bsf.saturating_mul(rate) / one);
        }
        scaled
    };

    Ok(RegisterBalancerStablePoolParams {
        address,
        vault,
        pool_id,
        tokens,
        amp: amp.to::<u128>(),
        scaling_factors,
        swap_fee: fee.to::<u128>(),
        bpt_idx,
        invariant_version,
        balances,
        update_block: block.unwrap_or(0),
        rate_provider: None,
    })
}

/// Assemble a V3 pool's tick map with **DB-first** coverage.
///
/// The Db arm routes through the shared
/// [`resolve_tick_map_arm`](degenbot_substrate::tick_assembly::resolve_tick_map_arm)
/// (so the `Tracked`/`Sparse` precedence, the Tracked intake reconciliation,
/// and the Db-empty semantics are identical to
/// `tick_assembly::assemble_v3_tick_map`), then runs the async-native Chain
/// transport only on a Db miss. This module's `bootstrap_tick_map` is the
/// builder's own Chain adapter: it drives the shared transport-generic loop
/// directly rather than parking a runtime worker on the synchronous
/// [`TickWordSource`] trait object. Both arms mint through the shared
/// [`chain_arm`](degenbot_substrate::tick_assembly::chain_arm) /
/// `resolve_tick_map_arm` seed vocabulary.
///
/// # Errors
///
/// Returns [`PoolBuilderError::Db`] on a DB read failure,
/// [`PoolBuilderError::TickAssembly`] on a chain/snapshot disagreement, or
/// [`PoolBuilderError::Rpc`] on a Chain-arm failure.
async fn assemble_db_or_chain_v3(
    db: Option<&dyn TickMapDb>,
    io: &ConstructionIo,
    address: Address,
    tick: i32,
    tick_spacing: i32,
    block: u64,
) -> Result<TickMapSeed, PoolBuilderError> {
    let db_arm = resolve_tick_map_arm::<PoolBuilderError, _>(
        &format!("{address}"),
        TickMapPoolIdentity::V3(address),
        tick_spacing,
        block,
        || match db {
            Some(db) => db
                .fetch_liquidity_map(address)
                .map_err(PoolBuilderError::Db),
            None => Ok(None),
        },
    )?;
    if let Some(seed) = db_arm {
        reconcile_db_seed_v3(io, address, tick_spacing, block, &seed).await?;
        return Ok(seed);
    }
    // DB miss -> Chain arm (Sparse).
    let key = TickWordKey::V3 {
        address,
        tick_spacing,
    };
    let (ticks, _bitmap) = bootstrap_tick_map(io, &key, tick, block)
        .await
        .map_err(PoolBuilderError::Rpc)?;
    Ok(chain_arm(Some(ticks)).into_seed(block, TickMapPoolIdentity::V3(address)))
}

/// V4 twin of [`assemble_db_or_chain_v3`]: the shared Db arm over
/// `TickMapDb.fetch_liquidity_map_v4`, then the async V4 Chain transport on a
/// miss. Same semantics — see [`assemble_db_or_chain_v3`].
#[expect(clippy::too_many_arguments)]
async fn assemble_db_or_chain_v4(
    db: Option<&dyn TickMapDb>,
    io: &ConstructionIo,
    pool_manager: Address,
    state_view: Address,
    pool_id: [u8; 32],
    tick: i32,
    tick_spacing: i32,
    block: u64,
) -> Result<TickMapSeed, PoolBuilderError> {
    let db_arm = resolve_tick_map_arm::<PoolBuilderError, _>(
        &alloy::hex::encode_prefixed(pool_id),
        TickMapPoolIdentity::V4 {
            manager: pool_manager,
            pool_id: B256::from(pool_id),
        },
        tick_spacing,
        block,
        || match db {
            Some(db) => db
                .fetch_liquidity_map_v4(pool_manager, B256::from(pool_id))
                .map_err(PoolBuilderError::Db),
            None => Ok(None),
        },
    )?;
    if let Some(seed) = db_arm {
        reconcile_db_seed_v4(io, state_view, pool_id, tick_spacing, block, &seed).await?;
        return Ok(seed);
    }
    // DB miss -> Chain arm (Sparse).
    let key = TickWordKey::V4 {
        state_view,
        pool_id,
        tick_spacing,
    };
    let (ticks, _bitmap) = bootstrap_tick_map(io, &key, tick, block)
        .await
        .map_err(PoolBuilderError::Rpc)?;
    Ok(chain_arm(Some(ticks)).into_seed(
        block,
        TickMapPoolIdentity::V4 {
            manager: pool_manager,
            pool_id: B256::from(pool_id),
        },
    ))
}

/// Reconcile a V3 Db (`Tracked`) seed against the chain word bitmaps read
/// through [`ConstructionIo`] at `block`. Every word the seed's rows occupy is
/// fetched from chain and compared: a `liquidity_gross > 0` row must have its
/// chain bit, and every chain bit needs a gross>0 row. `Sparse`/empty seeds
/// carry no complete-map claim and are returned untouched.
///
/// # Errors
///
/// [`PoolBuilderError::TickAssembly`] on a row/word disagreement, or
/// [`PoolBuilderError::Rpc`] when a word read fails.
async fn reconcile_db_seed_v3(
    io: &ConstructionIo,
    address: Address,
    tick_spacing: i32,
    block: u64,
    seed: &TickMapSeed,
) -> Result<(), PoolBuilderError> {
    if seed.coverage != PoolTickCoverage::Tracked || seed.ticks.is_empty() {
        return Ok(());
    }
    let mut chain_words = HashMap::new();
    for word in seed_row_words(seed, tick_spacing) {
        let bitmap = abi::fetch_tick_bitmap(
            &PacedRead::new(io),
            &address,
            tick_word_i16(word)?,
            Some(block),
        )
        .await?;
        chain_words.insert(word, bitmap);
    }
    reconcile_tracked_rows(&chain_words, &seed.ticks, tick_spacing)?;
    Ok(())
}

/// V4 twin of [`reconcile_db_seed_v3`], keyed by the `StateView` contract.
///
/// # Errors
///
/// [`PoolBuilderError::TickAssembly`] on a row/word disagreement, or
/// [`PoolBuilderError::Rpc`] when a word read fails.
async fn reconcile_db_seed_v4(
    io: &ConstructionIo,
    state_view: Address,
    pool_id: [u8; 32],
    tick_spacing: i32,
    block: u64,
    seed: &TickMapSeed,
) -> Result<(), PoolBuilderError> {
    if seed.coverage != PoolTickCoverage::Tracked || seed.ticks.is_empty() {
        return Ok(());
    }
    let mut chain_words = HashMap::new();
    for word in seed_row_words(seed, tick_spacing) {
        let bitmap = abi::fetch_v4_tick_bitmap(
            &PacedRead::new(io),
            &state_view,
            &pool_id,
            tick_word_i16(word)?,
            Some(block),
        )
        .await?;
        chain_words.insert(word, bitmap);
    }
    reconcile_tracked_rows(&chain_words, &seed.ticks, tick_spacing)?;
    Ok(())
}

/// Narrow a word position to the `int16` the bitmap ABIs take. Valid V3/V4
/// words fit; the failure arm is defensive.
fn tick_word_i16(word: i64) -> Result<i16, PoolBuilderError> {
    i16::try_from(word).map_err(|_| {
        PoolBuilderError::TickAssembly(TickMapAssemblyError::InconsistentTickMap {
            word,
            bit: 0,
            tick: i32::MIN,
            bitmap_bit: true,
            row_gross_positive: false,
        })
    })
}

/// Chain-arm single-word tick bootstrap over [`ConstructionIo`] — the Chain arm
/// of `assemble_{v3,v4}_tick_map`, addressed by the pool identity's
/// [`TickWordKey`] so the V3 (`tickBitmap`/`ticks`) and V4
/// (`getTickBitmap`/`getTickLiquidity`) arms share one bootstrap.
///
/// Coverage is always [`PoolTickCoverage::Sparse`] (only the single current
/// word is seeded), so the pool is **live immediately** (decision D4) and the
/// live-pump miss-detection backfills neighbour words during swap simulation.
pub(crate) async fn bootstrap_tick_map(
    io: &ConstructionIo,
    key: &TickWordKey,
    tick: i32,
    block: u64,
) -> Result<(HashMap<i32, TickInfo>, U256), ProviderError> {
    let (word, _) =
        degenbot_math::concentrated_liquidity::liquidity_mapping::get_tick_word_and_bit_position(
            tick,
            key.tick_spacing(),
        );
    #[expect(clippy::expect_used)] // invariant-guarded (documented)
    let word_i16 = i16::try_from(word).expect("a concentrated-liquidity tick word fits in int16");
    // The registration arm TOLERATES an unreadable bitmap (a provider that
    // can't serve `tickBitmap` degrades to an empty word → empty ticks →
    // Sparse, preserving the legacy builder's graceful fallback). The policy
    // lives in the shared loop; genuine transport/network and tick-read
    // failures still propagate, with full error fidelity.
    match fetch_tick_word_via(
        io,
        key,
        word_i16,
        block,
        UnreadableBitmapPolicy::TreatAsEmpty,
    )
    .await?
    {
        Some(fetched) => Ok((fetched.ticks, fetched.bitmap.unwrap_or(U256::ZERO))),
        None => Ok((HashMap::new(), U256::ZERO)),
    }
}

/// The Curve V1 pools known to be broken beyond repair: stale or disabled
/// deployments whose getters will not return (a `coins(i)`/`balances(i)`
/// that reverts, an empty `A()`, or a permanently wedged rate path). The
/// build refuses them up front with [`SpecRefusal::CurveBrokenPool`] rather
/// than dying mid-discovery on whichever read breaks first.
pub const BROKEN_CURVE_V1_POOLS: &[Address] = &[
    alloy::primitives::address!("110cc323ca53d622469EdD217387E2E6B33F1dF5"),
    alloy::primitives::address!("1F71f05CF491595652378Fe94B7820344A551B8E"),
    alloy::primitives::address!("28B0Cf1baFB707F2c6826d10caf6DD901a6540C5"),
    alloy::primitives::address!("3685646651FCcC80e7CCE7Ee24c5f47Ed9b434ac"),
    alloy::primitives::address!("4029f7DcBdF6059ed80DA6856526E7510D64fA21"),
    alloy::primitives::address!("69ACcb968B19a53790f43e57558F5E443A91aF22"),
    alloy::primitives::address!("84997FAFC913f1613F51Bb0E2b5854222900514B"),
    alloy::primitives::address!("9CA41a2DaB3CEE15308998868ca644e2e3be5C59"),
    alloy::primitives::address!("A77d09743F77052950C4eb4e6547E9665299BecD"),
    alloy::primitives::address!("D652c40fBb3f06d6B58Cb9aa9CFF063eE63d465D"),
];

/// The Balancer V2 pools known to be broken beyond repair: deployments whose
/// swaps are disabled at the contract level (BAL#327 `SWAPS_DISABLED`), so the
/// pool would register and then revert on every swap simulation. The build
/// refuses them up front with [`SpecRefusal::BalancerBrokenPool`], on both
/// Balancer arms, so a caller's family guess can never route around the
/// verdict.
pub const BROKEN_BALANCER_V2_POOLS: &[Address] = &[alloy::primitives::address!(
    "753BD6a5bF0b14ae7e5d2877e5cD6a3398aA2AAB"
)];

/// A revert on a REQUIRED read is the contract's own verdict that it does
/// not implement the call (a decommissioned V1 pool whose `A()`/`fee()`
/// reverts, a coin whose `decimals()` reverts) — a broken pool, refused with
/// the `Spec` taxonomy rather than masked as a transport `Rpc`.
fn required_read(err: ProviderError) -> PoolBuilderError {
    match err {
        ProviderError::ExecutionReverted { .. } => {
            PoolBuilderError::Spec(SpecRefusal::CurveRequiredReadReverted)
        }
        other => PoolBuilderError::Rpc(other),
    }
}

/// Assemble `build_curve_pool` params for a Curve `StableSwap` pool (the task
/// assembly twin of `CurvePoolBuilder.build`):
/// discovers coins/balances, fetches `A`/`fee`/`admin_fee`, detects A-ramping,
/// lending, crypto, `lp_token` + metapool (base pool + underlying coins),
/// resolves the strategy discriminants (T3), computes rate/precision
/// multipliers, and constructs the [`RpcCurveDataProvider`] (T4) — producing a
/// [`RegisterCurvePoolParams`] ready for `BotState::register_curve_pool` with
/// no Python round-trip.
///
/// ERC-20 / LP-token companion objects are built Python-side off the handle by
/// the driver (as with `build_v2`/`build_balancer_*`).
///
/// # Errors
///
/// Returns [`PoolBuilderError::Rpc`] on an RPC/decode failure, or
/// [`PoolBuilderError::Spec`] naming why the pool is unbuildable: a pool in
/// [`BROKEN_CURVE_V1_POOLS`] ([`SpecRefusal::CurveBrokenPool`]), fewer than 2
/// coins ([`SpecRefusal::CurveTooFewCoins`]), a `coins()`/`balances()` length
/// disagreement ([`SpecRefusal::CurveCoinsBalancesMismatch`]), a required getter
/// revert ([`SpecRefusal::CurveRequiredReadReverted`]), or a coin that cannot
/// serve an ERC-20 read ([`SpecRefusal::NoCodeCoin`] /
/// [`SpecRefusal::CurveCoinDecimalsReverted`]).
#[expect(clippy::too_many_lines)]
pub async fn build_curve_pool(
    address: Address,
    registry_addresses: &[Address],
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RegisterCurvePoolParams, PoolBuilderError> {
    if BROKEN_CURVE_V1_POOLS.contains(&address) {
        return Err(PoolBuilderError::Spec(SpecRefusal::CurveBrokenPool));
    }

    let coins = curve_choreography::discover_curve_coins(io, address, block).await;
    let params = curve_choreography::fetch_curve_pool_params(io, address, block)
        .await
        .map_err(required_read)?;
    let ramping = curve_choreography::detect_curve_a_ramping(io, address, block).await;
    let crypto = curve_choreography::detect_curve_crypto_params(io, address, block).await;
    let lp_token =
        curve_choreography::find_curve_lp_token(io, address, registry_addresses, block).await;
    let metapool = curve_choreography::detect_curve_metapool(
        io,
        address,
        &coins.token_addresses,
        registry_addresses,
        block,
    )
    .await;

    // Minimum-tokens guard (mirrors `BrokenPool`).
    if coins.token_addresses.len() < 2 {
        return Err(PoolBuilderError::Spec(SpecRefusal::CurveTooFewCoins));
    }

    // A pool that answers `coins(i)` but reverts on `balances(i)` leaves the
    // vectors mismatched, and `register_curve_pool` asserts N-consistency —
    // refuse with the same taxonomy rather than assembling params that panic
    // downstream.
    if coins.balances.len() != coins.token_addresses.len() {
        return Err(PoolBuilderError::Spec(
            SpecRefusal::CurveCoinsBalancesMismatch,
        ));
    }

    // Per-coin ERC20 decimals — needed for the rate/precision multipliers and
    // the lending detection (the driver owns the companions, so we fetch here).
    // A discovered coin with no runtime code (an old vyper pool's ETH
    // sentinel, or a junk `coins()` return) will never answer an ERC20 read —
    // refuse with the same `Spec` taxonomy instead of dying on the decode.
    let mut token_decimals = Vec::with_capacity(coins.token_addresses.len());
    for token in &coins.token_addresses {
        match fetch_erc20_decimals(io, *token, block).await {
            Ok(decimals) => token_decimals.push(decimals),
            Err(PoolBuilderError::Rpc(ProviderError::ExecutionReverted { .. })) => {
                return Err(PoolBuilderError::Spec(
                    SpecRefusal::CurveCoinDecimalsReverted,
                ));
            }
            Err(err) => return Err(err),
        }
    }
    let lending = curve_choreography::detect_curve_lending_tokens(
        io,
        &coins.token_addresses,
        &token_decimals,
        block,
    )
    .await;

    let strategies = resolve_curve_strategy_discriminants(address);

    let block_num = block.unwrap_or(0);
    let create_timestamp = io
        .get_block_timestamp(block_num)
        .await?
        // `None` only when the RPC double can't resolve the block.
        .unwrap_or(0);

    let one_e18 = U256::from(10u64).pow(U256::from(18u64));
    // Rust-owned derivation : the single source of truth shared
    // with the `degenbot._ffi.curve_math` surface. Lending overrides →
    // `pm * 10**PRECISION_DECIMALS`; else from token decimals.
    let (rate_multipliers, precision_multipliers) =
        degenbot_math::curve::derive_rate_and_precision_multipliers(
            &token_decimals,
            lending.precision_multipliers.as_deref(),
            degenbot_math::curve::PRECISION_DECIMALS,
        );

    // The provider's own rate/precision config (from the lending overrides, or
    // `[1]*n` / `[1e18]*n` defaults) — consumed by the oracle/yToken/cToken
    // styles.
    let default_pms = vec![U256::from(1u64); coins.token_addresses.len()];
    let provider_pms = lending
        .precision_multipliers
        .clone()
        .unwrap_or_else(|| default_pms.clone());
    let provider_rate_multipliers: Vec<U256> =
        provider_pms.iter().map(|pm| *pm * one_e18).collect();
    let provider = RpcCurveDataProvider::new(
        io.rpc.clone(),
        address,
        metapool.base_pool_address,
        coins.token_addresses.len(),
        strategies.lending_rate_style,
        coins.token_addresses.clone(),
        lending.use_lending.clone(),
        provider_pms,
        provider_rate_multipliers,
    );

    Ok(RegisterCurvePoolParams {
        address,
        tokens: coins.token_addresses.clone(),
        a_coefficient: params.a_coefficient,
        a_precision: 100,
        fee: params.fee,
        admin_fee: params.admin_fee,
        rate_multipliers,
        balances: coins.balances,
        update_block: block_num,
        swap_style: strategies.swap_style,
        lending_rate_style: strategies.lending_rate_style,
        d_variant: strategies.d_variant,
        y_variant: strategies.y_variant,
        yd_variant: strategies.yd_variant,
        base_pool: metapool.base_pool_address,
        initial_a_coefficient: ramping.initial_a,
        future_a_coefficient: ramping.future_a,
        initial_a_coefficient_time: ramping.initial_a_time,
        future_a_coefficient_time: ramping.future_a_time,
        create_timestamp: Some(create_timestamp),
        fee_gamma: crypto.fee_gamma,
        mid_fee: crypto.mid_fee,
        offpeg_fee_multiplier: crypto.offpeg_fee_multiplier,
        out_fee: crypto.out_fee,
        gamma: crypto.gamma,
        lp_token,
        use_lending: lending.use_lending,
        precision_multipliers,
        tokens_underlying: metapool.tokens_underlying,
        metapool_rate_style: strategies.metapool_rate_style,
        metapool_underlying_style: strategies.metapool_underlying_style,
        data_provider: Some(Arc::new(provider) as Arc<dyn CurveDataProvider>),
    })
}

/// Fetch a Curve pool coin's ERC20 `decimals()` as a `u8`.
///
/// # Errors
///
/// Returns [`PoolBuilderError::Rpc`] on an RPC/decode failure.
async fn fetch_erc20_decimals(
    read: &impl ContractRead,
    token: Address,
    block: Option<u64>,
) -> Result<u8, PoolBuilderError> {
    let calldata = abi::selector(b"decimals()");
    let bytes = read.call(token, calldata.to_vec().into(), block).await?;
    // An empty return is a coin that cannot serve an ERC20 read at all (an
    // old vyper pool's ETH sentinel has no code; a junk `coins()` return is
    // the same shape): the pool is refused, not decoded.
    if bytes.is_empty() {
        return Err(PoolBuilderError::Spec(SpecRefusal::NoCodeCoin));
    }
    let dec = abi::decode_uint256(&bytes)?;
    Ok(dec.to::<u8>())
}

/// Assemble `build_v3` params for a concentrated-liquidity (V3-style) pool.
///
/// Reads immutable data (`factory()`/`token0()`/`token1()`/`fee()`/
/// `tickSpacing()`), `slot0()` + `liquidity()`, seeds a Sparse tick map via the
/// Chain-arm bootstrap, verifies the CREATE2 address, and resolves deployment
/// deployer/`init_hash` — producing a [`RegisterV3PoolParams`] ready for
/// `BotState::register_v3_pool` with no Python round-trip.
///
/// # Errors
///
/// Returns [`PoolBuilderError::Identity`] on a CREATE2 mismatch (when the
/// factory ships in the JSON) or an RPC/decode error.
pub async fn build_v3(
    chain_id: u64,
    address: Address,
    db: Option<&dyn TickMapDb>,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RegisterV3PoolParams, PoolBuilderError> {
    let imm = fetch_v3_immutable_data(io, &address, block).await?;
    let (sqrt_price_x96, tick_i, liquidity) =
        abi::fetch_v3_slot0_liquidity(io, &address, block).await?;
    let tick = i32::try_from(tick_i).unwrap_or(0);
    let update_block = block.unwrap_or(0);

    let seed =
        assemble_db_or_chain_v3(db, io, address, tick, imm.tick_spacing, update_block).await?;
    let coverage = seed.coverage;
    let tick_data = seed.ticks;
    // Two-stamp rule (regression 8c50e0cd): the PRICE clock
    // (`update_block`) stays at the fresh caller-supplied head read, but the
    // LIQUIDITY clock (`tick_data_block`) of a DB-seeded (`Tracked`) pool must
    // be the block its DB liquidity map is EXACT at (`liquidity_update_block`),
    // not the head — otherwise the seed/post-drain verify compares stale seed
    // data against on-chain at head and false-positives on every tick that
    // moved in the `(liquidity_update_block, head]` window. Sparse (Chain-
    // seeded) pools keep the head clock. Falls back to `update_block` when the
    // DB block is unavailable/zero.
    let tick_data_block = if coverage == PoolTickCoverage::Tracked {
        db.and_then(|d| d.fetch_liquidity_update_block(address).ok().flatten())
            .and_then(|b| u64::try_from(b).ok())
            .filter(|b| *b > 0)
            .unwrap_or(update_block)
    } else {
        update_block
    };

    parity::verify_v3_create2(
        chain_id,
        imm.factory,
        address,
        imm.token0,
        imm.token1,
        imm.fee,
    )
    .map_err(|divergence| PoolBuilderError::Identity(Box::new(divergence)))?;

    let deployer = deployments::resolve_deployer(chain_id, imm.factory);
    let init_hash: B256 = deployments::resolve_v3_init_hash(chain_id, imm.factory);
    // Family-aware slot layout: the Pancake V3 fork's
    // divergent storage layout (liquidity@5, ticks@6) — resolved from the
    // deployment table; the FFI layer can override for non-JSON pools.
    let slot_layout = if deployments::is_pancakeswap_v3_factory(chain_id, imm.factory) {
        ConcentratedLiquiditySlotLayout::PancakeV3
    } else {
        ConcentratedLiquiditySlotLayout::UniswapV3
    };

    Ok(RegisterV3PoolParams {
        address,
        token0: imm.token0,
        token1: imm.token1,
        fee: imm.fee,
        tick_spacing: imm.tick_spacing,
        factory: imm.factory,
        sqrt_price_x96,
        liquidity: liquidity.to::<u128>(),
        tick,
        tick_data,
        update_block,
        tick_data_block: Some(tick_data_block),
        coverage,
        fetcher: None,
        deployer,
        init_hash,
        slot_layout,
        known_bitmap_words: Vec::new(),
    })
}

/// Caller-supplied V4 pool identity (mirrors the `register_v4_pool` argument
/// set — the core never reads these on-chain; hook flags are carried through
/// for caveat tagging, not for rejection). Bundled so `build_v4`
/// stays under `clippy::too_many_arguments`
/// (same convention as `RegisterV4PoolParams`).
#[derive(Debug, Clone, Copy)]
pub struct V4PoolBuildIdentity {
    /// The V4 `PoolManager` contract (DB key for the chain-arm tick-map read).
    pub pool_manager: Address,
    /// The `StateView` contract that exposes `getSlot0(bytes32)` /
    /// `getLiquidity(bytes32)` / `getTickBitmap` / `getTickLiquidity` for
    /// `pool_manager`'s pools (the live-scalar read target — distinct from
    /// `pool_manager`; `PoolManager` itself does NOT expose the state-view
    /// getters).
    pub state_view: Address,
    /// The pool's 32-byte key hash.
    pub pool_id: [u8; 32],
    /// `pool_key.currency0`.
    pub currency0: Address,
    /// `pool_key.currency1`.
    pub currency1: Address,
    /// `pool_key.fee` (`0x100000` = dynamic-fee flag, rejected at admission).
    pub fee: u32,
    /// `pool_key.tick_spacing`.
    pub tick_spacing: i32,
    /// `pool_key.hooks` — the REAL hook contract address. Carried in full (not
    /// reduced to its 16-bit flag mask) so the registered identity round-trips
    /// `pool_id = keccak(abi.encode(pool_key))` and the ADR-037 hook guards
    /// (`has_amount_modifying_hook`) read the true permissions. The flag mask
    /// is derived on demand via [`derive_hook_flags`].
    pub hook_address: Address,
}

/// The complete result of a V4 pool build: the registration params plus the
/// LP-fee pip decoded from the same head-stamped slot0 read.
///
/// ``lp_fee`` is exposed here (rather than bloat
/// [`RegisterV4PoolParams`] with a field the engine/solver does not consume)
/// so the Python companion can set its ``lp_fee`` override from the builder's
/// own slot0 read instead of issuing a second, redundant ``fetch_v4_slot0_liquidity``
/// round-trip. ``protocol_fee`` already rides inside
/// [`RegisterV4PoolParams::protocol_fee`] (the raw 24-bit word; the companion
/// splits it into zero-for-one / one-for-zero). Same head stamp, no second read.
#[derive(Debug)]
pub struct V4BuildResult {
    /// Registration params (incl. ``protocol_fee``) fed to the shared `BotState`.
    pub params: RegisterV4PoolParams,
    /// The LP-fee pip (``lp_fee``/1e6) decoded from the head-stamped slot0 word.
    pub lp_fee: u32,
}

/// Assemble `build_v4` params for a V4 pool identified by
/// [`V4PoolBuildIdentity`].
///
/// V4 pool identity is **caller-supplied** — the core never reads
/// `getToken0`/`getHooks`/`getHookFlags` on-chain (mirroring the existing
/// `register_v4_pool`, which takes `currency0`/`currency1`/`fee`/`tick_spacing`/
/// `hook_flags` as arguments; hook filtering is already applied via
/// `hook_flags`). This reads only the live scalars (`getSlot0` +
/// `getLiquidity` on the `pool_manager`) and seeds a Sparse tick map via the
/// Chain-arm bootstrap — reusing the existing StateView-backed choreography
/// with no new ABI.
///
/// # Errors
///
/// Returns a [`ProviderError`] on an RPC/decode failure.
pub async fn build_v4(
    id: V4PoolBuildIdentity,
    db: Option<&dyn TickMapDb>,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<V4BuildResult, PoolBuilderError> {
    let (sqrt_price_x96, tick_i, protocol_fee_u, lp_fee_u, liquidity_u) =
        abi::fetch_v4_slot0_liquidity(io, &id.state_view, &id.pool_id, block).await?;
    let protocol_fee = protocol_fee_u.to::<u32>();
    let lp_fee = lp_fee_u.to::<u32>();
    let tick = i32::try_from(tick_i).unwrap_or(0);
    let update_block = block.unwrap_or(0);

    let seed = assemble_db_or_chain_v4(
        db,
        io,
        id.pool_manager,
        id.state_view,
        id.pool_id,
        tick,
        id.tick_spacing,
        update_block,
    )
    .await?;
    let coverage = seed.coverage;
    let tick_data = seed.ticks;
    // Two-stamp rule (V4 twin of build_v3): a DB-seeded (`Tracked`) pool's
    // liquidity clock is its DB `liquidity_update_block`, not the head price
    // clock (regression 8c50e0cd).
    let tick_data_block = if coverage == PoolTickCoverage::Tracked {
        db.and_then(|d| {
            d.fetch_liquidity_update_block_v4(id.pool_manager, B256::from(id.pool_id))
                .ok()
                .flatten()
        })
        .and_then(|b| u64::try_from(b).ok())
        .filter(|b| *b > 0)
        .unwrap_or(update_block)
    } else {
        update_block
    };

    Ok(V4BuildResult {
        params: RegisterV4PoolParams {
            pool_manager: id.pool_manager,
            pool_id: id.pool_id,
            pool_key: V4PoolKey {
                currency0: id.currency0,
                currency1: id.currency1,
                fee: id.fee,
                tick_spacing: id.tick_spacing,
                // The REAL hook address (pool-ID mismatch regression:
                // a zeroed hooks breaks the keccak(pool_key) round-trip the
                // Python companion asserts and disables the ADR-037
                // has_amount_modifying_hook guards (both read the identity).
                hooks: id.hook_address,
            },
            state_view: Address::ZERO,
            hook_flags: derive_hook_flags(id.hook_address),
            protocol_fee,
            sqrt_price_x96,
            liquidity: liquidity_u.to::<u128>(),
            tick,
            tick_data,
            update_block,
            tick_data_block: Some(tick_data_block),
            coverage,
            fetcher: None,
            known_bitmap_words: Vec::new(),
        },
        lp_fee,
    })
}

/// Caller-supplied V4 identity **overrides** (the DB-resolution fallback).
/// Every field is optional because the core prefers the DB two-step
/// (manager → V4 row → per-FK tokens); when that is incomplete these fill the
/// gaps (all required for a pool not in the database). The word "overrides"
/// distinguishes this raw caller surface from the fully-resolved
/// [`V4PoolBuildIdentity`] that [`build_v4`] consumes.
#[derive(Debug, Clone, Copy, Default)]
pub struct V4PoolBuildOverrides {
    /// `pool_key.currency0` (unused when the DB two-step resolves it).
    pub currency0: Option<Address>,
    /// `pool_key.currency1` (unused when the DB two-step resolves it).
    pub currency1: Option<Address>,
    /// `pool_key.fee` (unused when the DB two-step resolves it).
    pub fee: Option<u32>,
    /// `pool_key.tick_spacing` (unused when the DB two-step resolves it).
    pub tick_spacing: Option<i32>,
    /// The hook contract address used to derive `hook_flags` (low 16 bits; the
    /// practical value is `ZERO` for the no-hook pools the solver admits).
    pub hook_address: Option<Address>,
    /// The `StateView` contract exposing the V4 getters for `pool_manager`
    /// (unused when the DB two-step resolves it from the manager row).
    pub state_view: Option<Address>,
}

/// Resolve the V4 identity **core-side**: the DB two-step
/// (manager → V4 row → per-FK token rows) first, else the caller-supplied
/// [`V4PoolBuildOverrides`]. Returns the resolved [`V4PoolBuildIdentity`].
///
/// (The Rust `build_v4` reads `liquidity_update_block` itself via
/// `TickMapDb::fetch_liquidity_update_block_v4` to stamp the liquidity
/// clock; the driver no longer needs it.)
///
/// DB errors and partial rows are treated as "no DB identity" (matching the
/// retired Python driver's `contextlib.suppress(Exception)`), falling through
/// to the overrides path. A [`PoolBuilderError::MissingIdentity`] is returned
/// when neither the DB two-step nor the overrides yield a complete identity.
///
/// # Errors
///
/// Returns [`PoolBuilderError::MissingIdentity`] when no DB identity is found
/// and one or more required overrides are absent.
///
/// # Panics
///
/// The `expect(\"checked present\")` calls on the override path are
/// unreachable-by-construction: the `missing` completeness check above returns
/// [`PoolBuilderError::MissingIdentity`] before any field is read.
#[expect(clippy::expect_used)] // all overrides verified present by the `missing` check (see # Panics)
pub async fn resolve_v4_identity(
    chain_id: u64,
    pool_manager: Address,
    pool_id: [u8; 32],
    overrides: &V4PoolBuildOverrides,
    io: &ConstructionIo,
) -> Result<V4PoolBuildIdentity, PoolBuilderError> {
    // DB two-step: manager → V4 row → per-FK token rows. Any error or partial
    // row is a skip (fall through to the override path), never a hard failure.
    let pool_hash_hex = format!("0x{}", alloy::hex::encode(pool_id));
    let db_identity: Option<V4PoolBuildIdentity> = (async {
        let manager_row = io
            .fetch_pool_manager(i64::try_from(chain_id).unwrap_or(0), pool_manager)
            .await
            .ok()??;
        let v4_row = io.fetch_v4_pool_by_pool_hash(&pool_hash_hex).await.ok()??;
        let token0 = io.fetch_token_by_id(v4_row.currency0_id).await.ok()??;
        let token1 = io.fetch_token_by_id(v4_row.currency1_id).await.ok()??;
        let state_view = manager_row.state_view.or(overrides.state_view);
        Some(V4PoolBuildIdentity {
            pool_manager,
            state_view: state_view?,
            pool_id,
            currency0: token0.address,
            currency1: token1.address,
            fee: u32::try_from(v4_row.fee_currency0).ok()?,
            tick_spacing: i32::try_from(v4_row.tick_spacing).ok()?,
            // The DB row carries the real hook contract address — keep it in
            // full so the registered pool key round-trips the pool_id hash
            // (pool-ID mismatch regression.
            hook_address: v4_row.hooks,
        })
    })
    .await;

    if let Some(identity) = db_identity {
        return Ok(identity);
    }

    // Override (kwargs) path — all required for a pool not in the database.
    let missing = [
        ("currency0", overrides.currency0.is_some()),
        ("currency1", overrides.currency1.is_some()),
        ("fee", overrides.fee.is_some()),
        ("tick_spacing", overrides.tick_spacing.is_some()),
        ("state_view", overrides.state_view.is_some()),
    ]
    .into_iter()
    .filter(|(_, present)| !present)
    .map(|(name, _)| name)
    .collect::<Vec<_>>()
    .join(", ");
    if !missing.is_empty() {
        return Err(PoolBuilderError::MissingIdentity {
            message: format!("pool not in the database; missing required overrides: {missing}"),
        });
    }

    #[expect(clippy::expect_used)] // overrides validated present above (documented)
    let (currency0, currency1) = order_currencies(
        overrides.currency0.expect("checked present"),
        overrides.currency1.expect("checked present"),
    );
    Ok(V4PoolBuildIdentity {
        pool_manager,
        state_view: overrides.state_view.expect("checked present"),
        pool_id,
        currency0,
        currency1,
        fee: overrides.fee.expect("checked present"),
        tick_spacing: overrides.tick_spacing.expect("checked present"),
        hook_address: overrides.hook_address.unwrap_or_default(),
    })
}

/// Derive the V4 `hook_flags` bitmask from a hook contract address.
/// `pub` because the `PyO3` seam derives the same mask from the stored
/// identity (already-registered fast-path echo + registration params) — the
/// address is the single source of truth; the mask is a derived quantity.
#[must_use]
pub fn derive_hook_flags(hook_address: Address) -> u16 {
    u16::from_be_bytes([hook_address[18], hook_address[19]])
}

/// Order two currency addresses into `(currency0, currency1)` by ascending
/// byte value (the `sorted(..., key=lambda t: t.lower())` the retired Python
/// driver applied to the caller-supplied token pair).
fn order_currencies(a: Address, b: Address) -> (Address, Address) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
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
