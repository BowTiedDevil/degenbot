//! Pool-builder construction orchestration (task `F2R2OC`, epic `Z5CNPB`).
//!
//! Part 1 of the builder-choreography port: the orchestrated
//! encode→call→decode choreography that the Python `builders/` used to drive
//! through `PyBotIo` moves **core-side** as free async functions over a
//! [`ConstructionIo`] handle (the atomic 7-RPC + 12-DB surface from
//! `construction_io`). The `PyO3` wrapper re-points its public methods to
//! `block_on` these functions; no `pyo3` in this module (the no-pyo3-in-cores
//! invariant, enforced by `just check-no-pyo3-in-cores`).
//!
//! Decision D-C scopes this first move to the **V2/V3/V4 + ERC-20 + tick**
//! choreography — exactly what the MEV `PoolBuilder` (task `3FVZF4`) needs.
//! Curve / Balancer / Camelot wrappers stay on the temporary `PyBotIo` path and
//! are absorbed with the builder-follow-up (task `SSSXG6`).
//!
//! Every encode/decode comes from [`degenbot_rpc::abi`], shared with
//! `AlloyTickBootstrapRpc` (the standalone-`cargo add degenbot` consumer), so
//! the choreography stays byte-identical across the ``PyO3`` adapter and the
//! pure-Rust path. Where no `abi` helper exists (factory/token0/token1/fee/
//! tickSpacing/erc20 name/symbol/decimals), the 4-byte selector is built from
//! `keccak256(signature)`.

use alloy::dyn_abi::{DynSolType, DynSolValue};
use alloy::primitives::{keccak256, Address, I256, U128, U256};
use degenbot_core::errors::ProviderError;
use degenbot_rpc::abi;

use crate::bot_core::construction_io::ConstructionIo;

/// Compute a 4-byte Solidity function selector (`keccak256(signature)[..4]`).
#[must_use]
pub fn selector(signature: &[u8]) -> [u8; 4] {
    let hash = keccak256(signature);
    let mut s = [0u8; 4];
    s.copy_from_slice(&hash[..4]);
    s
}

/// I-`eth_call` a choreographed read, returning the raw return bytes.
///
/// Wraps [`ConstructionIo::rpc.call`] so the encode→call→decode wrappers share
/// one call site (mirrors `PyBotIo::forward_call_to_provider`'s role, minus the
/// Python/provider fallback — the non-alloy fallback is dropped here per the
/// builder-choreography port).
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` failure.
pub async fn eth_call(
    io: &ConstructionIo,
    to: Address,
    data: Vec<u8>,
    block: Option<u64>,
) -> Result<Vec<u8>, ProviderError> {
    io.rpc
        .call(to, data.into(), block)
        .await
        .map(|b| b.to_vec())
}

/// Immutable-data shapes returned by the choreography (no `Py*` mirror).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2Immutable {
    pub factory: Address,
    pub token0: Address,
    pub token1: Address,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V3Immutable {
    pub factory: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
}

/// Fetch a no-argument address-returning read (`factory()`, `token0()`, …).
async fn fetch_address_returning(
    io: &ConstructionIo,
    signature: &[u8],
    to: Address,
    block: Option<u64>,
) -> Result<Address, ProviderError> {
    let calldata = selector(signature);
    let bytes = eth_call(io, to, calldata.to_vec(), block).await?;
    if bytes.len() < 32 {
        return Err(ProviderError::DecodingError {
            message: "address-returning call returned <32 bytes".into(),
        });
    }
    Ok(Address::from_slice(&bytes[12..32]))
}

/// Fetch a no-argument unsigned-int read (`fee()` as `uint24`, …). Right-aligns
/// the value into its ABI word; decodes via `DynSolType`.
async fn fetch_no_arg_uint(
    io: &ConstructionIo,
    signature: &[u8],
    to: Address,
    block: Option<u64>,
    ty: &DynSolType,
) -> Result<U256, ProviderError> {
    let calldata = selector(signature);
    let bytes = eth_call(io, to, calldata.to_vec(), block).await?;
    match ty.abi_decode(&bytes) {
        Ok(DynSolValue::Uint(n, _)) => Ok(n),
        _ => Err(ProviderError::DecodingError {
            message: "invalid uint decode for no-arg read".into(),
        }),
    }
}

/// Fetch a no-argument signed-int read (`tickSpacing()` as `int24`, …).
async fn fetch_no_arg_int(
    io: &ConstructionIo,
    signature: &[u8],
    to: Address,
    block: Option<u64>,
    ty: &DynSolType,
) -> Result<I256, ProviderError> {
    let calldata = selector(signature);
    let bytes = eth_call(io, to, calldata.to_vec(), block).await?;
    match ty.abi_decode(&bytes) {
        Ok(DynSolValue::Int(n, _)) => Ok(n),
        _ => Err(ProviderError::DecodingError {
            message: "invalid int decode for no-arg read".into(),
        }),
    }
}

/// Fetch a V3-style pool's immutable data — `factory()`, `token0()`,
/// `token1()`, `fee()`, `tickSpacing()`. Mirrors the DB-miss fallback block of
/// `v3_pool_builder.py::V3PoolBuilder.build` (ADR-005 slice 14f).
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` failure or a decode failure.
pub async fn fetch_v3_immutable_data(
    io: &ConstructionIo,
    address: Address,
    block: Option<u64>,
) -> Result<V3Immutable, ProviderError> {
    let factory = fetch_address_returning(io, b"factory()", address, block).await?;
    let token0 = fetch_address_returning(io, b"token0()", address, block).await?;
    let token1 = fetch_address_returning(io, b"token1()", address, block).await?;
    let fee = fetch_no_arg_uint(io, b"fee()", address, block, &DynSolType::Uint(24)).await?;
    let tick_spacing =
        fetch_no_arg_int(io, b"tickSpacing()", address, block, &DynSolType::Int(24)).await?;
    Ok(V3Immutable {
        factory,
        token0,
        token1,
        fee: fee.to::<u32>(),
        tick_spacing: tick_spacing.try_into().unwrap_or(0),
    })
}

/// Fetch a pool's `factory()` address (ADR-005 slice 14b).
///
/// Mirrors `type_resolution.py::fetch_factory_from_chain` — encode `factory()`,
/// `eth_call`, decode the right-aligned 20-byte `address`, EIP-55 checksum.
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` failure or a short result.
pub async fn fetch_factory_address(
    io: &ConstructionIo,
    address: Address,
    block: Option<u64>,
) -> Result<Address, ProviderError> {
    fetch_address_returning(io, b"factory()", address, block).await
}

/// Fetch a V2-style pool's immutable data — `factory()`, `token0()`,
/// `token1()`. Mirrors `v2_builder_base.py::_fetch_v2_common_data`'s fallback
/// (ADR-005 slice 14e).
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` failure.
pub async fn fetch_v2_immutable_data(
    io: &ConstructionIo,
    address: Address,
    block: Option<u64>,
) -> Result<V2Immutable, ProviderError> {
    let factory = fetch_address_returning(io, b"factory()", address, block).await?;
    let token0 = fetch_address_returning(io, b"token0()", address, block).await?;
    let token1 = fetch_address_returning(io, b"token1()", address, block).await?;
    Ok(V2Immutable {
        factory,
        token0,
        token1,
    })
}

/// Fetch a V2 pool's reserves via `getReserves()` (ADR-005 slice 14e).
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` failure or decode failure.
pub async fn fetch_v2_reserves(
    io: &ConstructionIo,
    address: Address,
    block: Option<u64>,
) -> Result<(U256, U256), ProviderError> {
    let bytes = eth_call(io, address, abi::encode_get_reserves(), block).await?;
    abi::decode_get_reserves(&bytes)
}

/// Aerodrome V2 common data read from the pool + factory (the ADR-005 slice
/// 14g choreography): the `stable()` flag (called on the pool — disambiguates
/// the shared volatile/stable factory) and the `getFee(address,bool)`
/// unidirectional fee (called on the factory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AerodromeV2Common {
    pub stable: bool,
    /// Unidirectional fee in basis points (Aerodrome fee denominator `10_000`).
    pub fee_bps: u64,
}

/// Fetch an Aerodrome V2 pool's `stable()` flag + `getFee()` fee.
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` or decode failure.
pub async fn fetch_aerodrome_stable_and_fee(
    io: &ConstructionIo,
    address: Address,
    factory: Address,
    block: Option<u64>,
) -> Result<AerodromeV2Common, ProviderError> {
    // Call 1: stable() on the pool — no-arg bool.
    let stable_bytes = eth_call(io, address, selector(b"stable()").to_vec(), block).await?;
    let Ok(DynSolValue::Bool(stable)) = DynSolType::Bool.abi_decode(&stable_bytes) else {
        return Err(ProviderError::DecodingError {
            message: "Aerodrome V2 stable() decode".to_owned(),
        });
    };
    // Call 2: getFee(address,bool) on the factory.
    let fee_bytes = eth_call(io, factory, abi::encode_get_fee(&address, stable), block).await?;
    let fee = abi::decode_uint256(&fee_bytes)?.to::<u64>();
    Ok(AerodromeV2Common {
        stable,
        fee_bps: fee,
    })
}

/// Fetch a V3 pool's `slot0()` + `liquidity()` (ADR-005 slice 14f).
///
/// Returns `(sqrt_price_x96, tick, liquidity)` — tick is `int24` sign-extended
/// into `I256`.
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` failure or decode failure.
pub async fn fetch_v3_slot0_liquidity(
    io: &ConstructionIo,
    address: Address,
    block: Option<u64>,
) -> Result<(U256, I256, U256), ProviderError> {
    let slot0_bytes = eth_call(io, address, abi::encode_slot0(), block).await?;
    let (sqrt, tick) = abi::decode_slot0(&slot0_bytes)?;
    let liq_bytes = eth_call(io, address, abi::encode_liquidity(), block).await?;
    let liquidity = abi::decode_liquidity(&liq_bytes)?;
    Ok((sqrt, tick, liquidity))
}

/// Fetch a V4 pool's `getSlot0(bytes32)` + `getLiquidity(bytes32)` on the
/// state-view contract (ADR-005 slice 14o).
///
/// Returns `(sqrt_price_x96, tick, protocol_fee, lp_fee, liquidity)`.
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` failure or decode failure.
pub async fn fetch_v4_slot0_liquidity(
    io: &ConstructionIo,
    state_view: Address,
    pool_id: [u8; 32],
    block: Option<u64>,
) -> Result<(U256, I256, U256, U256, U256), ProviderError> {
    let slot0_bytes = eth_call(io, state_view, abi::encode_get_slot0(&pool_id), block).await?;
    let (sqrt, tick, protocol_fee, lp_fee) = abi::decode_get_slot0(&slot0_bytes)?;
    let liq_bytes = eth_call(io, state_view, abi::encode_get_liquidity(&pool_id), block).await?;
    let liquidity = abi::decode_liquidity(&liq_bytes)?;
    Ok((sqrt, tick, protocol_fee, lp_fee, liquidity))
}

/// Fetch a V3 pool's tick bitmap word via `tickBitmap(int16)` (ADR-005 slice 14j).
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` failure or decode failure.
pub async fn fetch_tick_bitmap(
    io: &ConstructionIo,
    address: Address,
    word_position: i16,
    block: Option<u64>,
) -> Result<U256, ProviderError> {
    let bytes = eth_call(io, address, abi::encode_tick_bitmap(word_position), block).await?;
    abi::decode_tick_bitmap(&bytes)
}

/// Fetch a V3 pool's tick liquidity via `ticks(int24)` (ADR-005 slice 14j).
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` failure or decode failure.
pub async fn fetch_tick_data(
    io: &ConstructionIo,
    address: Address,
    tick: i32,
    block: Option<u64>,
) -> Result<(U128, I256), ProviderError> {
    let bytes = eth_call(io, address, abi::encode_tick_data(tick), block).await?;
    abi::decode_tick_data(&bytes)
}

/// Fetch a V4 pool's tick bitmap via `getTickBitmap(bytes32,int16)` (ADR-005 slice 14k).
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` failure or decode failure.
pub async fn fetch_v4_tick_bitmap(
    io: &ConstructionIo,
    state_view: Address,
    pool_id: [u8; 32],
    word_position: i16,
    block: Option<u64>,
) -> Result<U256, ProviderError> {
    let bytes = eth_call(
        io,
        state_view,
        abi::encode_v4_tick_bitmap(&pool_id, word_position),
        block,
    )
    .await?;
    abi::decode_v4_tick_bitmap(&bytes)
}

/// Fetch a V4 pool's tick liquidity via `getTickLiquidity(bytes32,int24)`
/// (ADR-005 slice 14k).
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` failure or decode failure.
pub async fn fetch_v4_tick_data(
    io: &ConstructionIo,
    state_view: Address,
    pool_id: [u8; 32],
    tick: i32,
    block: Option<u64>,
) -> Result<(U128, I256), ProviderError> {
    let bytes = eth_call(
        io,
        state_view,
        abi::encode_v4_tick_data(&pool_id, tick),
        block,
    )
    .await?;
    abi::decode_v4_tick_data(&bytes)
}

/// Fetch ERC-20 `name()` / `symbol()` / `decimals()` via three `eth_call`s
/// (ADR-005 slice 14c).
///
/// Returns `None` on any provider/decode error — mirrors the batched impl's
/// caller-side `except (Web3Exception, DecodingError)` contract, which falls
/// back to per-call `bytes32` alternate prototypes on the Python side.
///
/// # Errors
///
/// Returns a [`ProviderError`] only on a non-decode `eth_call` transport error;
/// decode/revert failures return `Ok(None)` (the caller's fallback contract).
pub async fn fetch_erc20_metadata(
    io: &ConstructionIo,
    address: Address,
) -> Result<Option<(String, String, u64)>, ProviderError> {
    let Ok(name_bytes) = eth_call(io, address, selector(b"name()").to_vec(), None).await else {
        return Ok(None);
    };
    let Ok(symbol_bytes) = eth_call(io, address, selector(b"symbol()").to_vec(), None).await else {
        return Ok(None);
    };
    let Ok(decimals_bytes) = eth_call(io, address, selector(b"decimals()").to_vec(), None).await
    else {
        return Ok(None);
    };
    let Ok(DynSolValue::String(name)) = DynSolType::String.abi_decode(&name_bytes) else {
        return Ok(None);
    };
    let Ok(DynSolValue::String(symbol)) = DynSolType::String.abi_decode(&symbol_bytes) else {
        return Ok(None);
    };
    let decimals = match DynSolType::Uint(256).abi_decode(&decimals_bytes) {
        Ok(DynSolValue::Uint(n, _)) => n.to::<u64>(),
        _ => return Ok(None),
    };
    Ok(Some((name, symbol, decimals)))
}

// ---------------------------------------------------------------------------
// Balancer V2 reads (the SSSXG6 buyer primitive layer)
// ---------------------------------------------------------------------------

/// The Balancer pool sub-type resolved by [`probe_balancer_type`] — mirrored
/// from `balancer_builder_base.py::_BalancerPoolType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalancerFamily {
    /// Has `getNormalizedWeights()` — a weighted pool.
    Weighted,
    /// Has `getAmplificationParameter()` (no weights) — a stable pool.
    Stable,
}

/// Fetch a Balancer pool's 32-byte `getPoolId()` identifier.
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` or decode failure.
pub async fn fetch_balancer_pool_id(
    io: &ConstructionIo,
    address: Address,
    block: Option<u64>,
) -> Result<[u8; 32], ProviderError> {
    let bytes = eth_call(io, address, abi::encode_get_pool_id(), block).await?;
    abi::decode_get_pool_id(&bytes)
}

/// Fetch a Balancer pool's tokens + balances from the singleton Vault via
/// `getPoolTokens(poolId)`.
///
/// Returns `(tokens, balances)` — the third Vault field (`lastChangeBlock`) is
/// dropped, matching the Python caller.
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` or decode failure.
pub async fn fetch_balancer_vault_tokens(
    io: &ConstructionIo,
    vault: Address,
    pool_id: &[u8; 32],
    block: Option<u64>,
) -> Result<(Vec<Address>, Vec<U256>), ProviderError> {
    let bytes = eth_call(io, vault, abi::encode_get_pool_tokens(pool_id), block).await?;
    abi::decode_get_pool_tokens(&bytes)
}

/// Fetch a Balancer pool's `getSwapFeePercentage()` as a `uint256`.
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` or decode failure.
pub async fn fetch_balancer_swap_fee(
    io: &ConstructionIo,
    address: Address,
    block: Option<u64>,
) -> Result<U256, ProviderError> {
    let bytes = eth_call(io, address, abi::encode_get_swap_fee(), block).await?;
    abi::decode_get_swap_fee(&bytes)
}

/// Fetch a Balancer pool's amplification parameter — the first `uint256 value`
/// word of the `getAmplificationParameter()` tuple.
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` or decode failure.
pub async fn fetch_balancer_amp(
    io: &ConstructionIo,
    address: Address,
    block: Option<u64>,
) -> Result<U256, ProviderError> {
    let bytes = eth_call(io, address, abi::encode_get_amp(), block).await?;
    abi::decode_get_amp(&bytes)
}

/// Fetch a Balancer weighted pool's `getNormalizedWeights()` array.
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` or decode failure.
pub async fn fetch_balancer_weights(
    io: &ConstructionIo,
    address: Address,
    block: Option<u64>,
) -> Result<Vec<U256>, ProviderError> {
    let bytes = eth_call(io, address, abi::encode_get_weights(), block).await?;
    abi::decode_get_weights(&bytes)
}

/// Fetch a Balancer pool's `getRateProviders()` address array.
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` or decode failure. Callers
/// mirror the Python `except (RpcError, AbiDecodeError): return []` — pools
/// that don't expose `getRateProviders` (`WeightedPool2Tokens` / `MetaStable`)
/// revert; the caller converts to an empty list.
pub async fn fetch_balancer_rate_providers(
    io: &ConstructionIo,
    address: Address,
    block: Option<u64>,
) -> Result<Vec<Address>, ProviderError> {
    let bytes = eth_call(io, address, abi::encode_get_rate_providers(), block).await?;
    abi::decode_get_rate_providers(&bytes)
}

/// Fetch a single Balancer rate provider's `getRate()` as a `uint256`.
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` or decode failure.
pub async fn fetch_balancer_rate(
    io: &ConstructionIo,
    provider: Address,
    block: Option<u64>,
) -> Result<U256, ProviderError> {
    let bytes = eth_call(io, provider, abi::encode_get_rate(), block).await?;
    abi::decode_get_rate(&bytes)
}

/// Probe a Balancer pool's sub-type: `getNormalizedWeights()` succeeds →
/// [`BalancerFamily::Weighted`]; else `getAmplificationParameter()` succeeds →
/// [`BalancerFamily::Stable`]; else an error (mirrors
/// `balancer_builder_base.py::_detect_pool_type` — Linear pools unsupported).
///
/// # Errors
///
/// Returns a [`ProviderError`] when neither probe responds.
pub async fn probe_balancer_type(
    io: &ConstructionIo,
    address: Address,
    block: Option<u64>,
) -> Result<BalancerFamily, ProviderError> {
    if eth_call(io, address, abi::encode_get_weights(), block)
        .await
        .is_ok()
    {
        return Ok(BalancerFamily::Weighted);
    }
    if eth_call(io, address, abi::encode_get_amp(), block)
        .await
        .is_ok()
    {
        return Ok(BalancerFamily::Stable);
    }
    Err(ProviderError::DecodingError {
        message: format!(
            "Cannot determine Balancer pool type for {address}: neither \
             getNormalizedWeights() nor getAmplificationParameter() responded"
        ),
    })
}
