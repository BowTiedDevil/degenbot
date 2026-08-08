//! Static ABI definitions for on-chain pool/token fetchers (B2).
//!
//! The `sol!` macro generates compile-time-verified selectors, calldata
//! encoders, and return-type decoders for the handful of read methods
//! `PyBotIo`'s fetchers issue. Centralising them here keeps the ABI knowledge
//! in the pyo3-free `degenbot-rpc` core — a standalone Rust consumer can use
//! the [`fetch_*`] helpers directly against an [`AlloyProvider`] without any
//! Python round-trip, and `PyBotIo` (in `degenbot-python`) re-exports the
//! `encode_*`/`decode_*` primitives so its own fallback dispatch path sources
//! selectors and return shapes from one place (no hand-rolled `keccak`
//! selector + byte-slice decode duplicated in the pyclass).
//!
//! ## Dispatch
//!
//! Each top-level [`fetch_*`] helper is a thin `encode → eth_call → decode`
//! pipeline over [`AlloyProvider::eth_call`], matching the established
//! `multicall3` precedent (encode/decode in core, a single `eth_call` at the
//! RPC boundary). The full alloy `CallBuilder` (`IPair::new(addr, provider).
//! getReserves().call().await`) is an equivalent path; the `eth_call` form is
//! used here because `AlloyProvider` already owns a hardened `eth_call` with
//! retry/backoff and revert-classification, so the fetchers inherit that
//! behaviour without re-plumbing provider construction.
//!
//! # Errors
//!
//! Every [`fetch_*`] returns [`ProviderError`]: `eth_call` failures surface
//! verbatim (reverts as [`ProviderError::ExecutionReverted`], transport errors
//! classified by the provider); return-data decode failures surface as
//! [`ProviderError::DecodingError`]. No swallowing — callers decide retry/policy.

use alloy::primitives::{Address, Bytes, I256, U128, U256};
use alloy::sol_types::SolCall;
use degenbot_core::errors::{ProviderError, ProviderResult};

use crate::provider::AlloyProvider;

alloy::sol! {
    /// ERC-20 token interface (read methods used by the fetchers).
    interface IERC20 {
        function balanceOf(address account) external view returns (uint256 balance);
        function allowance(address owner, address spender) external view returns (uint256 remaining);
        function totalSupply() external view returns (uint256 totalSupply);
    }

    /// Uniswap-V2-style pair interface.
    interface IUniswapV2Pair {
        function getReserves()
            external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    }

    /// Uniswap-V3-style pool interface.
    interface IUniswapV3Pool {
        function slot0() external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint8 feeProtocol,
            bool unlocked
        );
        function liquidity() external view returns (uint128 liquidity);
        function tickBitmap(int16 wordPosition) external view returns (uint256 word);
        function ticks(int24 tick) external view returns (
            uint128 liquidityGross,
            int128 liquidityNet,
            uint256 feeGrowthOutside0X128,
            uint256 feeGrowthOutside1X128,
            int56 tickCumulativeOutside,
            uint160 secondsPerLiquidityOutsideX128,
            uint32 secondsOutside,
            bool initialized
        );
    }

    /// Uniswap-V4 state-view interface.
    interface IUniswapV4StateView {
        function getSlot0(bytes32 poolId) external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint24 protocolFee,
            uint24 lpFee
        );
        function getLiquidity(bytes32 poolId) external view returns (uint128 liquidity);
        function getTickBitmap(bytes32 poolId, int16 wordPosition) external view returns (uint256 word);
        function getTickLiquidity(bytes32 poolId, int24 tick) external view returns (uint128 gross, int128 net);
    }

    /// Balancer V2 pool interface (the read methods the builder issues).
    interface IBalancerPool {
        function getPoolId() external view returns (bytes32);
        function getSwapFeePercentage() external view returns (uint256);
        function getAmplificationParameter() external view returns (uint256 value, bool isUpdating, uint256 precision);
        function getNormalizedWeights() external view returns (uint256[] memory);
        function getRateProviders() external view returns (address[] memory);
        function getRate() external view returns (uint256);
    }

    /// Balancer V2 singleton Vault interface (pool-token reads).
    interface IBalancerVault {
        function getPoolTokens(bytes32 poolId) external view returns (address[] memory tokens, uint256[] memory balances, uint256 lastChangeBlock);
    }

    /// Curve `StableSwap` pool read interface (all pool-state detection reads).
    /// The `uint256`-prototype coin/balance getters; the `int128` prototype is
    /// declared separately in `ICurveStableswapPoolInt128` so alloy's `sol!`
    /// does not need to disambiguate overloads.
    interface ICurvePool {
        function coins(uint256 i) external view returns (address);
        function balances(uint256 i) external view returns (uint256);
        function admin_balances(uint256 i) external view returns (uint256);
        function price_scale(uint256 k) external view returns (uint256);
        function A() external view returns (uint256);
        function fee() external view returns (uint256);
        function admin_fee() external view returns (uint256);
        function D() external view returns (uint256);
        function get_virtual_price() external view returns (uint256);
        function base_virtual_price() external view returns (uint256);
        function base_cache_updated() external view returns (uint256);
        function oracle_method() external view returns (uint256);
        function redemption_price_snap() external view returns (address);
        function snappedRedemptionPrice() external view returns (uint256);
        function initial_A() external view returns (uint256);
        function initial_A_time() external view returns (uint256);
        function future_A() external view returns (uint256);
        function future_A_time() external view returns (uint256);
        function fee_gamma() external view returns (uint256);
        function mid_fee() external view returns (uint256);
        function out_fee() external view returns (uint256);
        function gamma() external view returns (uint256);
        function offpeg_fee_multiplier() external view returns (uint256);
        function base_pool() external view returns (address);
    }

    /// Curve `StableSwap` coin/balance reads under the `int128` prototype
    /// (older pools). Kept as a separate interface so the overloaded
    /// `coins`/`balances` names land in distinct `SolCall` types.
    interface ICurvePoolInt128 {
        function coins(int128 i) external view returns (address);
        function balances(int128 i) external view returns (uint256);
    }

    /// Curve registry / factory read interface (LP token, metapool, coins).
    interface ICurveRegistry {
        function get_lp_token(address pool) external view returns (address);
        function is_meta(address pool) external view returns (bool);
        function get_underlying_coins(address pool) external view returns (address[8] memory);
        function get_base_pool(address pool) external view returns (address);
    }

    /// Lending-token detection reads (cToken / yToken probes).
    interface ILendingToken {
        function isCToken() external view returns (bool);
        function underlying() external view returns (address);
        function token() external view returns (address);
    }
}

// ---------------------------------------------------------------------------
// V2 reserves: getReserves() → (uint112, uint112, uint32)
// ---------------------------------------------------------------------------

/// Encode the `getReserves()` calldata (4-byte selector only).
#[must_use]
pub fn encode_get_reserves() -> Vec<u8> {
    IUniswapV2Pair::getReservesCall {}.abi_encode()
}

/// Decode `getReserves()` return data into `(reserve0, reserve1)` as `U256`s
/// (the third word — `blockTimestampLast` — is unused, mirroring the original
/// Python/V3 builder behaviour).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] if the bytes do not decode as the
/// `(uint112, uint112, uint32)` tuple.
pub fn decode_get_reserves(bytes: &[u8]) -> ProviderResult<(U256, U256)> {
    let r = IUniswapV2Pair::getReservesCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("getReserves decode: {e}"),
        }
    })?;
    Ok((U256::from(r.reserve0), U256::from(r.reserve1)))
}

// ---------------------------------------------------------------------------
// Balancer V2 reads (pool + Vault) — the SSSXG6 buyer primitive layer
// ---------------------------------------------------------------------------

/// Encode the `getPoolId()` calldata (4-byte selector only).
#[must_use]
pub fn encode_get_pool_id() -> Vec<u8> {
    IBalancerPool::getPoolIdCall {}.abi_encode()
}

/// Decode `getPoolId()` return data into the 32-byte pool identifier.
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] if the return data is shorter than
/// 32 bytes (the `bytes32` occupies one static word).
pub fn decode_get_pool_id(bytes: &[u8]) -> ProviderResult<[u8; 32]> {
    if bytes.len() < 32 {
        return Err(ProviderError::DecodingError {
            message: format!(
                "getPoolId decode: expected >= 32 bytes, got {}",
                bytes.len()
            ),
        });
    }
    let mut pool_id = [0u8; 32];
    pool_id.copy_from_slice(&bytes[0..32]);
    Ok(pool_id)
}

/// Encode the Vault `getPoolTokens(bytes32)` calldata.
#[must_use]
pub fn encode_get_pool_tokens(pool_id: &[u8; 32]) -> Vec<u8> {
    IBalancerVault::getPoolTokensCall {
        poolId: (*pool_id).into(),
    }
    .abi_encode()
}

/// Decode Vault `getPoolTokens(bytes32)` return data into `(tokens, balances)`
/// — the third field (`lastChangeBlock`) is dropped, mirroring
/// `balancer_builder_base.py::decode_vault_tokens`.
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] if the tuple does not decode.
pub fn decode_get_pool_tokens(bytes: &[u8]) -> ProviderResult<(Vec<Address>, Vec<U256>)> {
    let r = IBalancerVault::getPoolTokensCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("getPoolTokens decode: {e}"),
        }
    })?;
    Ok((r.tokens, r.balances))
}

/// Encode the `getSwapFeePercentage()` calldata (4-byte selector only).
#[must_use]
pub fn encode_get_swap_fee() -> Vec<u8> {
    IBalancerPool::getSwapFeePercentageCall {}.abi_encode()
}

/// Decode `getSwapFeePercentage()` return data (`uint256`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] if the bytes do not decode as a
/// `uint256`.
pub fn decode_get_swap_fee(bytes: &[u8]) -> ProviderResult<U256> {
    decode_uint256(bytes)
}

/// Encode the `getAmplificationParameter()` calldata (4-byte selector only).
#[must_use]
pub fn encode_get_amp() -> Vec<u8> {
    IBalancerPool::getAmplificationParameterCall {}.abi_encode()
}

/// Decode `getAmplificationParameter()` return data into the first word — the
/// `uint256 value` (the `bool isUpdating` and `uint256 precision` tail fields
/// are dropped, mirroring `balancer_builder_base.py::_fetch_amp`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] if the tuple does not decode.
pub fn decode_get_amp(bytes: &[u8]) -> ProviderResult<U256> {
    let r =
        IBalancerPool::getAmplificationParameterCall::abi_decode_returns(bytes).map_err(|e| {
            ProviderError::DecodingError {
                message: format!("getAmplificationParameter decode: {e}"),
            }
        })?;
    Ok(r.value)
}

/// Encode the `getNormalizedWeights()` calldata (4-byte selector only).
#[must_use]
pub fn encode_get_weights() -> Vec<u8> {
    IBalancerPool::getNormalizedWeightsCall {}.abi_encode()
}

/// Decode `getNormalizedWeights()` return data into a `uint256[]`.
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] if the array does not decode.
pub fn decode_get_weights(bytes: &[u8]) -> ProviderResult<Vec<U256>> {
    IBalancerPool::getNormalizedWeightsCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("getNormalizedWeights decode: {e}"),
        }
    })
}

/// Encode the `getRateProviders()` calldata (4-byte selector only).
#[must_use]
pub fn encode_get_rate_providers() -> Vec<u8> {
    IBalancerPool::getRateProvidersCall {}.abi_encode()
}

/// Decode `getRateProviders()` return data into an `address[]`.
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] if the array does not decode.
pub fn decode_get_rate_providers(bytes: &[u8]) -> ProviderResult<Vec<Address>> {
    IBalancerPool::getRateProvidersCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("getRateProviders decode: {e}"),
        }
    })
}

/// Encode the `getRate()` calldata (4-byte selector only).
#[must_use]
pub fn encode_get_rate() -> Vec<u8> {
    IBalancerPool::getRateCall {}.abi_encode()
}

/// Decode `getRate()` return data (`uint256`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] if the bytes do not decode as a
/// `uint256`.
pub fn decode_get_rate(bytes: &[u8]) -> ProviderResult<U256> {
    decode_uint256(bytes)
}

/// Fetch a V2-style pair's `getReserves()` and return `(reserve0, reserve1)`.
///
/// # Errors
///
/// Returns [`ProviderError`] from the underlying `eth_call` (revert surfaces
/// as [`ProviderError::ExecutionReverted`], transport errors classified by
/// the provider) or [`ProviderError::DecodingError`] if the return data does
/// not decode as the `(uint112, uint112, uint32)` tuple.
pub async fn fetch_v2_reserves(
    provider: &AlloyProvider,
    pool: &Address,
    block: Option<u64>,
) -> ProviderResult<(U256, U256)> {
    let calldata = Bytes::from(encode_get_reserves());
    let bytes = provider.eth_call(pool, calldata, block).await?;
    decode_get_reserves(&bytes)
}

// ---------------------------------------------------------------------------
// V3 slot0 + liquidity
// ---------------------------------------------------------------------------

/// Encode the `slot0()` calldata (no args).
#[must_use]
pub fn encode_slot0() -> Vec<u8> {
    IUniswapV3Pool::slot0Call {}.abi_encode()
}

/// Decode `slot0()` return data into `(sqrtPriceX96, tick)` — only the first
/// two packed fields are needed by the builder snapshot; the remaining five
/// are ignored (mirrors the Python `decode_slot0`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_slot0(bytes: &[u8]) -> ProviderResult<(U256, I256)> {
    let s = IUniswapV3Pool::slot0Call::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("slot0 decode: {e}"),
        }
    })?;
    Ok((U256::from(s.sqrtPriceX96), widen_int24(s.tick.as_i32())))
}

/// Encode the `liquidity()` calldata (no args).
#[must_use]
pub fn encode_liquidity() -> Vec<u8> {
    IUniswapV3Pool::liquidityCall {}.abi_encode()
}

/// Decode `liquidity()` return data (`uint128`) as a `U256`.
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_liquidity(bytes: &[u8]) -> ProviderResult<U256> {
    let l = IUniswapV3Pool::liquidityCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("liquidity decode: {e}"),
        }
    })?;
    Ok(U256::from(l))
}

/// Fetch a V3-style pool's `slot0()` + `liquidity()` and return
/// `(sqrtPriceX96, tick, liquidity)`.
///
/// # Errors
///
/// Returns [`ProviderError`] from either underlying `eth_call` or
/// [`ProviderError::DecodingError`] if a return value fails to decode.
pub async fn fetch_v3_slot0_liquidity(
    provider: &AlloyProvider,
    pool: &Address,
    block: Option<u64>,
) -> ProviderResult<(U256, I256, U256)> {
    let slot0_bytes = provider
        .eth_call(pool, Bytes::from(encode_slot0()), block)
        .await?;
    let (sqrt_price_x96, tick) = decode_slot0(&slot0_bytes)?;
    let liq_bytes = provider
        .eth_call(pool, Bytes::from(encode_liquidity()), block)
        .await?;
    let liquidity = decode_liquidity(&liq_bytes)?;
    Ok((sqrt_price_x96, tick, liquidity))
}

// ---------------------------------------------------------------------------
// V4 slot0 + liquidity (state-view contract, takes a bytes32 poolId)
// ---------------------------------------------------------------------------

/// Encode the `getSlot0(bytes32)` calldata.
#[must_use]
pub fn encode_get_slot0(pool_id: &[u8; 32]) -> Vec<u8> {
    IUniswapV4StateView::getSlot0Call {
        poolId: (*pool_id).into(),
    }
    .abi_encode()
}

/// Decode `getSlot0(bytes32)` return data into
/// `(sqrtPriceX96, tick, protocolFee, lpFee)`.
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_get_slot0(bytes: &[u8]) -> ProviderResult<(U256, I256, U256, U256)> {
    let s = IUniswapV4StateView::getSlot0Call::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("getSlot0 decode: {e}"),
        }
    })?;
    Ok((
        U256::from(s.sqrtPriceX96),
        widen_int24(s.tick.as_i32()),
        U256::from(s.protocolFee),
        U256::from(s.lpFee),
    ))
}

/// Encode the `getLiquidity(bytes32)` calldata.
#[must_use]
pub fn encode_get_liquidity(pool_id: &[u8; 32]) -> Vec<u8> {
    IUniswapV4StateView::getLiquidityCall {
        poolId: (*pool_id).into(),
    }
    .abi_encode()
}

/// Fetch a V4 pool's `getSlot0(bytes32)` + `getLiquidity(bytes32)` from the
/// state-view contract and return
/// `(sqrtPriceX96, tick, protocolFee, lpFee, liquidity)`.
///
/// # Errors
///
/// Returns [`ProviderError`] from either underlying `eth_call` or
/// [`ProviderError::DecodingError`] if a return value fails to decode.
pub async fn fetch_v4_slot0_liquidity(
    provider: &AlloyProvider,
    state_view: &Address,
    pool_id: &[u8; 32],
    block: Option<u64>,
) -> ProviderResult<(U256, I256, U256, U256, U256)> {
    let slot0_bytes = provider
        .eth_call(state_view, Bytes::from(encode_get_slot0(pool_id)), block)
        .await?;
    let (sqrt_price_x96, tick, protocol_fee, lp_fee) = decode_get_slot0(&slot0_bytes)?;
    let liq_bytes = provider
        .eth_call(
            state_view,
            Bytes::from(encode_get_liquidity(pool_id)),
            block,
        )
        .await?;
    let liquidity = decode_liquidity(&liq_bytes)?;
    Ok((sqrt_price_x96, tick, protocol_fee, lp_fee, liquidity))
}

// ---------------------------------------------------------------------------
// V3 tick-bitmap + tick-data (`tickBitmap(int16)`, `ticks(int24)`)
//
// Decoders are intentionally lenient (read the first 1 / 2 of the 32-byte
// return words, ignore the rest) — matches `PyBotIo::fetch_tick_bitmap` /
// `fetch_tick_data`'s existing byte-slice decode behaviour. The full V3
// `ticks(int24)` return tuple is 8 × 32 = 256 bytes (declared in `sol!` above
// for selector+callee parity) but only the first 2 words are needed here.
// ---------------------------------------------------------------------------

/// Encode the `tickBitmap(int16)` calldata.
#[must_use]
pub fn encode_tick_bitmap(word_position: i16) -> Vec<u8> {
    IUniswapV3Pool::tickBitmapCall {
        wordPosition: word_position,
    }
    .abi_encode()
}

/// Decode `tickBitmap(int16)` return data (`uint256`). Returns the full
/// `U256` word — the caller checks `.is_zero()` and enumerates set bits.
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] if `bytes` is shorter than 32
/// bytes.
pub fn decode_tick_bitmap(bytes: &[u8]) -> ProviderResult<U256> {
    if bytes.len() < 32 {
        return Err(ProviderError::DecodingError {
            message: "tickBitmap result < 32 bytes".into(),
        });
    }
    Ok(U256::from_be_slice(&bytes[0..32]))
}

/// Encode the `ticks(int24)` calldata.
///
/// # Panics
///
/// Panics if `tick` is outside the signed 24-bit range
/// (`[-2^23, 2^23 - 1]`). All valid V3 ticks fit; callers passing
/// user-supplied values should validate the range first.
#[must_use]
pub fn encode_tick_data(tick: i32) -> Vec<u8> {
    // V3 ticks are guaranteed within the signed 24-bit range (documented).
    #[expect(clippy::expect_used)]
    let tick24 = tick.try_into().expect("tick within int24 range");
    IUniswapV3Pool::ticksCall { tick: tick24 }.abi_encode()
}

/// Decode `ticks(int24)` return data into `(liquidity_gross, liquidity_net)`
/// — only the first two of the eight packed return fields. Both are
/// right-aligned in their 32-byte ABI words; `liquidity_gross` is
/// `uint128`, `liquidity_net` is `int128` (sign-extended in the lower 16
/// bytes).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] if `bytes` is shorter than 64
/// bytes or the sign-extension decoding fails.
pub fn decode_tick_data(bytes: &[u8]) -> ProviderResult<(U128, I256)> {
    if bytes.len() < 64 {
        return Err(ProviderError::DecodingError {
            message: "ticks result < 64 bytes".into(),
        });
    }
    // Word 0: liquidity_gross (uint128, right-aligned in 32-byte word).
    let gross = U128::from_be_slice(&bytes[16..32]);
    // Word 1: liquidity_net (int128, sign-extended in 32-byte word).
    let net =
        I256::try_from_be_slice(&bytes[32..64]).ok_or_else(|| ProviderError::DecodingError {
            message: "invalid ticks(int24) int128 decode".into(),
        })?;
    Ok((gross, net))
}

/// Fetch a V3 pool's `tickBitmap(int16)` from the chain and return it as a
/// `U256`. The caller enumerates set bits.
///
/// # Errors
///
/// Returns [`ProviderError`] from the underlying `eth_call` (revert surfaces
/// as [`ProviderError::ExecutionReverted`]) or
/// [`ProviderError::DecodingError`] if the return data fails to decode.
pub async fn fetch_tick_bitmap(
    provider: &AlloyProvider,
    pool: &Address,
    word_position: i16,
    block: Option<u64>,
) -> ProviderResult<U256> {
    let calldata = Bytes::from(encode_tick_bitmap(word_position));
    let bytes = provider.eth_call(pool, calldata, block).await?;
    decode_tick_bitmap(&bytes)
}

/// Fetch a V3 pool's `ticks(int24)` and return `(liquidity_gross,
/// liquidity_net)`.
///
/// # Errors
///
/// Returns [`ProviderError`] from the underlying `eth_call` or
/// [`ProviderError::DecodingError`] on decode failure.
pub async fn fetch_tick_data(
    provider: &AlloyProvider,
    pool: &Address,
    tick: i32,
    block: Option<u64>,
) -> ProviderResult<(U128, I256)> {
    let calldata = Bytes::from(encode_tick_data(tick));
    let bytes = provider.eth_call(pool, calldata, block).await?;
    decode_tick_data(&bytes)
}

// ---------------------------------------------------------------------------
// V4 tick-bitmap + tick-data (`getTickBitmap(bytes32,int16)`,
// `getTickLiquidity(bytes32,int24)`) on the state-view contract.
// ---------------------------------------------------------------------------

/// Encode the `getTickBitmap(bytes32,int16)` calldata.
#[must_use]
pub fn encode_v4_tick_bitmap(pool_id: &[u8; 32], word_position: i16) -> Vec<u8> {
    IUniswapV4StateView::getTickBitmapCall {
        poolId: (*pool_id).into(),
        wordPosition: word_position,
    }
    .abi_encode()
}

/// Encode the `getTickLiquidity(bytes32,int24)` calldata.
///
/// # Panics
///
/// Panics if `tick` is outside the signed 24-bit range
/// (`[-2^23, 2^23 - 1]`). All valid V4 ticks fit; callers passing
/// user-supplied values should validate the range first.
#[must_use]
pub fn encode_v4_tick_data(pool_id: &[u8; 32], tick: i32) -> Vec<u8> {
    // All valid V4 ticks fit the signed 24-bit range (documented).
    #[expect(clippy::expect_used)]
    let tick24 = tick.try_into().expect("tick within int24 range");
    IUniswapV4StateView::getTickLiquidityCall {
        poolId: (*pool_id).into(),
        tick: tick24,
    }
    .abi_encode()
}

/// Decode `getTickBitmap(bytes32,int16)` return data into the bitmap `U256`.
/// Same shape as [`decode_tick_bitmap`] (V4 layout is identical for the
/// single-word return).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] if `bytes` is shorter than 32
/// bytes.
pub fn decode_v4_tick_bitmap(bytes: &[u8]) -> ProviderResult<U256> {
    decode_tick_bitmap(bytes)
}

/// Decode `getTickLiquidity(bytes32,int24)` return data into
/// `(liquidity_gross, liquidity_net)`. Same byte layout as
/// [`decode_tick_data`] — V4 returns only `(uint128 gross, int128 net)`
/// (2 × 32-byte words); the decoder reuses the V3 path since the first 64
/// bytes are laid out identically.
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] if `bytes` is shorter than 64
/// bytes or the decode fails.
pub fn decode_v4_tick_data(bytes: &[u8]) -> ProviderResult<(U128, I256)> {
    decode_tick_data(bytes)
}

/// Fetch a V4 pool's `getTickBitmap(bytes32,int16)` from the state-view
/// contract and return it as a `U256`.
///
/// # Errors
///
/// Returns [`ProviderError`] from the underlying `eth_call` or
/// [`ProviderError::DecodingError`] on decode failure.
pub async fn fetch_v4_tick_bitmap(
    provider: &AlloyProvider,
    state_view: &Address,
    pool_id: &[u8; 32],
    word_position: i16,
    block: Option<u64>,
) -> ProviderResult<U256> {
    let calldata = Bytes::from(encode_v4_tick_bitmap(pool_id, word_position));
    let bytes = provider.eth_call(state_view, calldata, block).await?;
    decode_v4_tick_bitmap(&bytes)
}

/// Fetch a V4 pool's `getTickLiquidity(bytes32,int24)` and return
/// `(liquidity_gross, liquidity_net)`.
///
/// # Errors
///
/// Returns [`ProviderError`] from the underlying `eth_call` or
/// [`ProviderError::DecodingError`] on decode failure.
pub async fn fetch_v4_tick_data(
    provider: &AlloyProvider,
    state_view: &Address,
    pool_id: &[u8; 32],
    tick: i32,
    block: Option<u64>,
) -> ProviderResult<(U128, I256)> {
    let calldata = Bytes::from(encode_v4_tick_data(pool_id, tick));
    let bytes = provider.eth_call(state_view, calldata, block).await?;
    decode_v4_tick_data(&bytes)
}

// ---------------------------------------------------------------------------
// ERC-20: balanceOf / allowance / totalSupply
// ---------------------------------------------------------------------------

/// Encode the `balanceOf(address)` calldata.
#[must_use]
pub fn encode_balance_of(account: &Address) -> Vec<u8> {
    IERC20::balanceOfCall { account: *account }.abi_encode()
}

/// Encode the Aerodrome V2 `getFee(address,bool)` calldata.
///
/// Called on the factory with the pool's address + stable flag (the Aerodrome
/// volatile/stable pair shares one factory, so `getFee` returns the per-pool
/// unidirectional fee — the ADR-005 slice 14g choreography's second call).
#[must_use]
pub fn encode_get_fee(pool: &Address, stable: bool) -> Vec<u8> {
    let mut calldata = Vec::with_capacity(4 + 64);
    calldata.extend_from_slice(&0xcc_56_b2_c5u32.to_be_bytes()); // getFee(address,bool)
                                                                 // address: 12 zero bytes + 20-byte address (word 0).
    calldata.extend_from_slice(&[0u8; 12]);
    calldata.extend_from_slice(pool.as_slice());
    // bool: 31 zero bytes + 1 byte (word 1).
    calldata.extend_from_slice(&[0u8; 31]);
    calldata.push(u8::from(stable));
    calldata
}

/// Decode `balanceOf(address)` return data (`uint256`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_balance_of(bytes: &[u8]) -> ProviderResult<U256> {
    let r = IERC20::balanceOfCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("balanceOf decode: {e}"),
        }
    })?;
    Ok(r)
}

/// Decode a generic `uint256` ABI return (a single 32-byte big-endian word).
///
/// This is the generic form that `decode_balance_of` / `decode_total_supply` /
/// `decode_allowance` (and any other single-`uint256` return — e.g. Aave's
/// `scaledBalanceOf` / `getPreviousIndex`) reduce to. Provided as a first-class
/// entry point so callers probing a non-ERC20 `uint256` return do not have to
/// reach for a method-named decoder that would mislabel their call
/// (`decode_balance_of` for a `scaledBalanceOf` return is a semantic lie even
/// though the ABI shape is identical).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] if `bytes` is shorter than 32 bytes
/// or does not decode as a `uint256`.
pub fn decode_uint256(bytes: &[u8]) -> ProviderResult<U256> {
    if bytes.len() < 32 {
        return Err(ProviderError::DecodingError {
            message: format!("uint256 decode: expected >= 32 bytes, got {}", bytes.len()),
        });
    }
    let mut word = [0u8; 32];
    word.copy_from_slice(&bytes[0..32]);
    Ok(U256::from_be_bytes::<32>(word))
}

/// Fetch an ERC-20 `balanceOf(address)`.
///
/// # Errors
///
/// Returns [`ProviderError`] from the underlying `eth_call` or
/// [`ProviderError::DecodingError`] if the return data does not decode as a
/// `uint256`.
pub async fn fetch_token_balance(
    provider: &AlloyProvider,
    token: &Address,
    owner: &Address,
    block: Option<u64>,
) -> ProviderResult<U256> {
    let calldata = Bytes::from(encode_balance_of(owner));
    let bytes = provider.eth_call(token, calldata, block).await?;
    decode_balance_of(&bytes)
}

/// Encode the `allowance(address,address)` calldata.
#[must_use]
pub fn encode_allowance(owner: &Address, spender: &Address) -> Vec<u8> {
    IERC20::allowanceCall {
        owner: *owner,
        spender: *spender,
    }
    .abi_encode()
}

/// Decode `allowance(address,address)` return data (`uint256`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_allowance(bytes: &[u8]) -> ProviderResult<U256> {
    let r = IERC20::allowanceCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("allowance decode: {e}"),
        }
    })?;
    Ok(r)
}

/// Fetch an ERC-20 `allowance(address,address)`.
///
/// # Errors
///
/// Returns [`ProviderError`] from the underlying `eth_call` or
/// [`ProviderError::DecodingError`] if the return data does not decode as a
/// `uint256`.
pub async fn fetch_token_allowance(
    provider: &AlloyProvider,
    token: &Address,
    owner: &Address,
    spender: &Address,
    block: Option<u64>,
) -> ProviderResult<U256> {
    let calldata = Bytes::from(encode_allowance(owner, spender));
    let bytes = provider.eth_call(token, calldata, block).await?;
    decode_allowance(&bytes)
}

/// Encode the `totalSupply()` calldata (no args).
#[must_use]
pub fn encode_total_supply() -> Vec<u8> {
    IERC20::totalSupplyCall {}.abi_encode()
}

/// Decode `totalSupply()` return data (`uint256`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_total_supply(bytes: &[u8]) -> ProviderResult<U256> {
    let r = IERC20::totalSupplyCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("totalSupply decode: {e}"),
        }
    })?;
    Ok(r)
}

/// Fetch an ERC-20 `totalSupply()`.
///
/// # Errors
///
/// Returns [`ProviderError`] from the underlying `eth_call` or
/// [`ProviderError::DecodingError`] if the return data does not decode as a
/// `uint256`.
pub async fn fetch_token_total_supply(
    provider: &AlloyProvider,
    token: &Address,
    block: Option<u64>,
) -> ProviderResult<U256> {
    let calldata = Bytes::from(encode_total_supply());
    let bytes = provider.eth_call(token, calldata, block).await?;
    decode_total_supply(&bytes)
}

/// Encode `coins(uint256)` calldata for coin index `i` (uint256 prototype).
#[must_use]
pub fn encode_curve_coins_uint(i: u8) -> Vec<u8> {
    ICurvePool::coinsCall { i: U256::from(i) }.abi_encode()
}

/// Encode `coins(int128)` calldata for coin index `i` (int128 prototype).
#[must_use]
pub fn encode_curve_coins_int128(i: u8) -> Vec<u8> {
    ICurvePoolInt128::coinsCall { i: i128::from(i) }.abi_encode()
}

/// Decode `coins(...)` return data (single `address`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_curve_coins(bytes: &[u8]) -> ProviderResult<Address> {
    let r = ICurvePool::coinsCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("coins decode: {e}"),
        }
    })?;
    Ok(r)
}

/// Encode `balances(uint256)` calldata for coin index `i` (uint256 prototype).
#[must_use]
pub fn encode_curve_balances_uint(i: u8) -> Vec<u8> {
    ICurvePool::balancesCall { i: U256::from(i) }.abi_encode()
}

/// Encode `balances(int128)` calldata for coin index `i` (int128 prototype).
#[must_use]
pub fn encode_curve_balances_int128(i: u8) -> Vec<u8> {
    ICurvePoolInt128::balancesCall { i: i128::from(i) }.abi_encode()
}

/// Decode `balances(...)` return data (single `uint256`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_curve_balances(bytes: &[u8]) -> ProviderResult<U256> {
    let r = ICurvePool::balancesCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("balances decode: {e}"),
        }
    })?;
    Ok(r)
}

/// Encode `admin_balances(uint256)` calldata for coin index `i`.
#[must_use]
pub fn encode_curve_admin_balances(i: u8) -> Vec<u8> {
    ICurvePool::admin_balancesCall { i: U256::from(i) }.abi_encode()
}

/// Encode `price_scale(uint256)` calldata for price-scale index `k`.
#[must_use]
pub fn encode_curve_price_scale(k: u8) -> Vec<u8> {
    ICurvePool::price_scaleCall { k: U256::from(k) }.abi_encode()
}

/// Decode `redemption_price_snap()` return data (single `address`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_curve_redemption_price_snap(bytes: &[u8]) -> ProviderResult<Address> {
    let r = ICurvePool::redemption_price_snapCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("redemption_price_snap decode: {e}"),
        }
    })?;
    Ok(r)
}

/// Encode registry/`factory` `get_lp_token(address)` calldata for `pool`.
#[must_use]
pub fn encode_curve_get_lp_token(pool: &Address) -> Vec<u8> {
    ICurveRegistry::get_lp_tokenCall { pool: *pool }.abi_encode()
}

/// Decode `get_lp_token(address)` return data (single `address`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_curve_get_lp_token(bytes: &[u8]) -> ProviderResult<Address> {
    let r = ICurveRegistry::get_lp_tokenCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("get_lp_token decode: {e}"),
        }
    })?;
    Ok(r)
}

/// Encode registry/`factory` `is_meta(address)` calldata for `pool`.
#[must_use]
pub fn encode_curve_is_meta(pool: &Address) -> Vec<u8> {
    ICurveRegistry::is_metaCall { pool: *pool }.abi_encode()
}

/// Decode `is_meta(address)` return data (single `bool`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_curve_is_meta(bytes: &[u8]) -> ProviderResult<bool> {
    let r = ICurveRegistry::is_metaCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("is_meta decode: {e}"),
        }
    })?;
    Ok(r)
}

/// Encode registry `get_underlying_coins(address)` calldata for `pool`.
#[must_use]
pub fn encode_curve_get_underlying_coins(pool: &Address) -> Vec<u8> {
    ICurveRegistry::get_underlying_coinsCall { pool: *pool }.abi_encode()
}

/// Decode `get_underlying_coins(address)` return data (fixed `address[8]`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_curve_get_underlying_coins(bytes: &[u8]) -> ProviderResult<[Address; 8]> {
    let r = ICurveRegistry::get_underlying_coinsCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("get_underlying_coins decode: {e}"),
        }
    })?;
    Ok(r)
}

/// Encode registry/`factory` `get_base_pool(address)` calldata for `pool`.
#[must_use]
pub fn encode_curve_get_base_pool(pool: &Address) -> Vec<u8> {
    ICurveRegistry::get_base_poolCall { pool: *pool }.abi_encode()
}

/// Decode `get_base_pool(address)` return data (single `address`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_curve_get_base_pool(bytes: &[u8]) -> ProviderResult<Address> {
    let r = ICurveRegistry::get_base_poolCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("get_base_pool decode: {e}"),
        }
    })?;
    Ok(r)
}

/// Encode the lending-token `isCToken()` calldata (4-byte selector only).
#[must_use]
pub fn encode_lending_is_ctoken() -> Vec<u8> {
    ILendingToken::isCTokenCall {}.abi_encode()
}

/// Decode `isCToken()` return data (single `bool`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_lending_is_ctoken(bytes: &[u8]) -> ProviderResult<bool> {
    let r = ILendingToken::isCTokenCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("isCToken decode: {e}"),
        }
    })?;
    Ok(r)
}

/// Encode the lending-token `underlying()` calldata (4-byte selector only).
#[must_use]
pub fn encode_lending_underlying() -> Vec<u8> {
    ILendingToken::underlyingCall {}.abi_encode()
}

/// Decode `underlying()` return data (single `address`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_lending_underlying(bytes: &[u8]) -> ProviderResult<Address> {
    let r = ILendingToken::underlyingCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("underlying decode: {e}"),
        }
    })?;
    Ok(r)
}

/// Encode the lending-token `token()` calldata (4-byte selector only).
#[must_use]
pub fn encode_lending_token() -> Vec<u8> {
    ILendingToken::tokenCall {}.abi_encode()
}

/// Decode `token()` return data (single `address`).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] on decode failure.
pub fn decode_lending_token(bytes: &[u8]) -> ProviderResult<Address> {
    let r = ILendingToken::tokenCall::abi_decode_returns(bytes).map_err(|e| {
        ProviderError::DecodingError {
            message: format!("token decode: {e}"),
        }
    })?;
    Ok(r)
}

/// Widen a signed 24-bit `tick` value (already extracted as `i32`) into
/// `I256`, preserving sign. The value always fits in `I256`; `expect` is safe.
fn widen_int24(tick: i32) -> I256 {
    #[expect(clippy::expect_used)] // int24 -> i128 -> I256 always fits (documented)
    let widened = I256::try_from(i128::from(tick)).expect("int24 value always fits in I256");
    widened
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::dyn_abi::DynSolValue;
    use alloy::primitives::{address, I256, U128, U256};

    // Reference vectors computed independently with `eth_abi` +
    // `eth_utils.keccak` in a throwaway Python probe — a DIFFERENT ABI encoder
    // than alloy's `sol!`. They are the source of truth the sol!-generated
    // selectors/calldata must reproduce byte-for-byte, and the bytes the
    // `PyBotIo` hand-rolled fetchers produced before this refactor.

    /// `keccak256("getReserves()")[..4]` = `0x0902f1ac`.
    #[test]
    fn get_reserves_selector_is_0902f1ac() {
        let calldata = encode_get_reserves();
        assert_eq!(&calldata[0..4], &[0x09, 0x02, 0xf1, 0xac]);
        // No args: calldata is just the selector.
        assert_eq!(calldata.len(), 4);
    }

    /// `keccak256("slot0()")[..4]` = `0x1c681cc3`... actually confirmed below.
    #[test]
    fn slot0_selector_matches_reference() {
        let calldata = encode_slot0();
        // keccak256("slot0()")[..4]
        let expected = alloy::primitives::keccak256(b"slot0()");
        assert_eq!(&calldata[0..4], &expected[..4]);
    }

    /// `keccak256("liquidity()")[..4]`.
    #[test]
    fn liquidity_selector_matches_reference() {
        let calldata = encode_liquidity();
        let expected = alloy::primitives::keccak256(b"liquidity()");
        assert_eq!(&calldata[0..4], &expected[..4]);
    }

    /// `keccak256("balanceOf(address)")[..4]` = `0x70a08231`.
    #[test]
    fn balance_of_selector_is_70a08231() {
        let account = address!("2222222222222222222222222222222222222222");
        let calldata = encode_balance_of(&account);
        assert_eq!(&calldata[0..4], &[0x70, 0xa0, 0x82, 0x31]);
        // 4-byte selector + one 32-byte word (right-padded address).
        assert_eq!(calldata.len(), 36);
        // Address occupies the last 20 bytes of the word.
        assert_eq!(&calldata[16..36], &account.into_array()[..]);
    }

    /// `keccak256("allowance(address,address)")[..4]` = `0xdd62ed3e`.
    #[test]
    fn allowance_selector_is_dd62ed3e() {
        let owner = address!("1111111111111111111111111111111111111111");
        let spender = address!("2222222222222222222222222222222222222222");
        let calldata = encode_allowance(&owner, &spender);
        assert_eq!(&calldata[0..4], &[0xdd, 0x62, 0xed, 0x3e]);
        assert_eq!(calldata.len(), 68);
        assert_eq!(&calldata[16..36], &owner.into_array()[..]);
        assert_eq!(&calldata[48..68], &spender.into_array()[..]);
    }

    /// `keccak256("totalSupply()")[..4]` = `0x18160ddd`.
    #[test]
    fn total_supply_selector_is_18160ddd() {
        let calldata = encode_total_supply();
        assert_eq!(&calldata[0..4], &[0x18, 0x16, 0x0d, 0xdd]);
        assert_eq!(calldata.len(), 4);
    }

    /// `getSlot0(bytes32)` calldata: selector + the 32-byte poolId.
    #[test]
    fn get_slot0_calldata_is_selector_plus_pool_id() {
        let pool_id: [u8; 32] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20,
        ];
        let calldata = encode_get_slot0(&pool_id);
        let expected_sel = alloy::primitives::keccak256(b"getSlot0(bytes32)");
        assert_eq!(&calldata[0..4], &expected_sel[..4]);
        assert_eq!(calldata.len(), 36);
        assert_eq!(&calldata[4..36], &pool_id[..]);
    }

    /// `getLiquidity(bytes32)` calldata.
    #[test]
    fn get_liquidity_calldata_is_selector_plus_pool_id() {
        let pool_id: [u8; 32] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20,
        ];
        let calldata = encode_get_liquidity(&pool_id);
        let expected_sel = alloy::primitives::keccak256(b"getLiquidity(bytes32)");
        assert_eq!(&calldata[0..4], &expected_sel[..4]);
        assert_eq!(calldata.len(), 36);
        assert_eq!(&calldata[4..36], &pool_id[..]);
    }

    /// Decoding `getReserves()` return `(uint112 reserve0=0x2a, uint112
    /// reserve1=0x1, uint32=0)` yields the right pair.
    #[test]
    fn decode_get_reserves_roundtrip() {
        // ABI head encoding: 3 words, reserve0=42, reserve1=1, ts=0.
        let bytes = {
            let mut v = Vec::with_capacity(96);
            v.extend_from_slice(&U256::from(42u64).to_be_bytes::<32>());
            v.extend_from_slice(&U256::from(1u64).to_be_bytes::<32>());
            v.extend_from_slice(&[0u8; 32]);
            v
        };
        let (r0, r1) = decode_get_reserves(&bytes).unwrap();
        assert_eq!(r0, U256::from(42u64));
        assert_eq!(r1, U256::from(1u64));
    }

    /// Decoding `balanceOf()` return `uint256 = 0x..2a` (= 42).
    #[test]
    fn decode_balance_of_roundtrip() {
        let bytes = U256::from(42u64).to_be_bytes::<32>().to_vec();
        let r = decode_balance_of(&bytes).unwrap();
        assert_eq!(r, U256::from(42u64));
    }

    /// Decoding `slot0()` return: sqrtPriceX96 and tick in the first two words;
    /// remaining five fields ignored.
    #[test]
    fn decode_slot0_roundtrip() {
        let mut v = Vec::with_capacity(7 * 32);
        v.extend_from_slice(&U256::from(0xa1b2u64).to_be_bytes::<32>());
        v.extend_from_slice(&I256::try_from(-123i64).unwrap().to_be_bytes::<32>());
        // Remaining five fields (observationIndex, cardinality, cardinalityNext,
        // feeProtocol, unlocked) — zeros suffice.
        v.extend_from_slice(&[0u8; 5 * 32]);
        let (sqrt, tick) = decode_slot0(&v).unwrap();
        assert_eq!(sqrt, U256::from(0xa1b2u64));
        assert_eq!(tick, I256::try_from(-123i64).unwrap());
    }

    /// Decoding `getSlot0(bytes32)` return (4 fields: sqrtPriceX96, tick,
    /// protocolFee, lpFee).
    #[test]
    fn decode_get_slot0_roundtrip() {
        let mut v = Vec::with_capacity(4 * 32);
        v.extend_from_slice(&U256::from(0xdeadu64).to_be_bytes::<32>());
        v.extend_from_slice(&I256::try_from(7i64).unwrap().to_be_bytes::<32>());
        v.extend_from_slice(&U256::from(0x1000u64).to_be_bytes::<32>());
        v.extend_from_slice(&U256::from(0x2000u64).to_be_bytes::<32>());
        let (sqrt, tick, proto, lp) = decode_get_slot0(&v).unwrap();
        assert_eq!(sqrt, U256::from(0xdeadu64));
        assert_eq!(tick, I256::try_from(7i64).unwrap());
        assert_eq!(proto, U256::from(0x1000u64));
        assert_eq!(lp, U256::from(0x2000u64));
    }

    /// Decoding `liquidity()` return `uint128`.
    #[test]
    fn decode_liquidity_roundtrip() {
        let bytes = U256::from(1_000_000u64).to_be_bytes::<32>().to_vec();
        let r = decode_liquidity(&bytes).unwrap();
        assert_eq!(r, U256::from(1_000_000u64));
    }

    /// Short return data (fewer than the expected words) is a decode error, not a panic.
    #[test]
    fn decode_rejects_short_bytes() {
        assert!(decode_get_reserves(&[0u8; 8]).is_err());
        assert!(decode_slot0(&[0u8; 8]).is_err());
        assert!(decode_get_slot0(&[0u8; 8]).is_err());
        assert!(decode_balance_of(&[0u8; 8]).is_err());
        assert!(decode_allowance(&[0u8; 8]).is_err());
        assert!(decode_total_supply(&[0u8; 8]).is_err());
        assert!(decode_liquidity(&[0u8; 8]).is_err());
    }

    /// Decode functions round-trip encode→decode through `eth_abi`-shaped bytes
    /// for `allowance`/`totalSupply`.
    #[test]
    fn decode_allowance_and_total_supply_roundtrip() {
        let a = decode_allowance(&U256::from(99u64).to_be_bytes::<32>()).unwrap();
        assert_eq!(a, U256::from(99u64));
        let t = decode_total_supply(&U256::from(1_234_567u64).to_be_bytes::<32>()).unwrap();
        assert_eq!(t, U256::from(1_234_567u64));
    }

    // ── decode_uint256 — generic single-word decoder ──

    #[test]
    fn decode_uint256_decodes_32_byte_be_word() {
        // 0x00..2a (42) — independent of any method-named decoder.
        let mut buf = [0u8; 32];
        buf[31] = 0x2a;
        assert_eq!(decode_uint256(&buf).unwrap(), U256::from(42u64));
    }

    #[test]
    fn decode_uint256_max_word_round_trips() {
        let word = U256::MAX;
        let bytes = word.to_be_bytes::<32>();
        assert_eq!(decode_uint256(&bytes).unwrap(), U256::MAX);
    }

    #[test]
    fn decode_uint256_short_input_is_err() {
        assert!(decode_uint256(&[0u8; 16]).is_err());
        assert!(decode_uint256(&[]).is_err());
    }

    // ── tick_data / tick_bitmap (V3 + V4) — independent-oracle tests ──
    //
    // Migrated from `degenbot-pool-updater/src/verify.rs` (slice A, commit #1)
    // to fill the home's gap: `decode_tick_data` / `decode_tick_bitmap` /
    // `decode_v4_tick_data` / `decode_v4_tick_bitmap` had NO unit tests here
    // before this commit. The reference payloads are built with an
    // INDEPENDENT encoder path (`DynSolValue::abi_encode` of a hand-built
    // tuple) than the sol!-macro decoder under test — a different encoder than
    // alloy's `sol!`, stronger than a same-encoder roundtrip. This is the
    // independent-oracle discipline the pool-updater tests established; it now
    // guards the home decode every consumer (diagnostic, liquidity_verifier,
    // pool-updater) routes through.

    /// Independently ABI-encode a V3 `ticks()` return tuple — built with
    /// `DynSolValue::abi_encode`, a DIFFERENT encoder path than the
    /// `IUniswapV3Pool::ticksCall` sol! macro `decode_tick_data` uses.
    fn encode_ticks_return_ref(gross: u128, net: i128) -> Vec<u8> {
        let val = DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(gross), 128),
            DynSolValue::Int(I256::try_from(net).unwrap(), 128),
            DynSolValue::Uint(U256::ZERO, 256),
            DynSolValue::Uint(U256::ZERO, 256),
            DynSolValue::Int(I256::ZERO, 56),
            DynSolValue::Uint(U256::ZERO, 160),
            DynSolValue::Uint(U256::ZERO, 32),
            DynSolValue::Bool(false),
        ]);
        val.abi_encode()
    }

    /// Independently ABI-encode a `uint256` return (the `tickBitmap` shape).
    fn encode_uint256_return_ref(word: U256) -> Vec<u8> {
        DynSolValue::Uint(word, 256).abi_encode()
    }

    #[test]
    fn decode_tick_data_matches_ref_encoder() {
        let payload = encode_ticks_return_ref(500, -100);
        let (gross, net) = decode_tick_data(&payload).expect("decode");
        assert_eq!(gross, U128::from(500u64));
        assert_eq!(net, I256::try_from(-100i64).unwrap());
    }

    #[test]
    fn decode_tick_data_drained_tick_is_zeros() {
        // An uninitialized tick returns gross=0, net=0.
        let payload = encode_ticks_return_ref(0, 0);
        let (gross, net) = decode_tick_data(&payload).expect("decode");
        assert_eq!(gross, U128::ZERO);
        assert_eq!(net, I256::ZERO);
    }

    #[test]
    fn decode_tick_data_too_short_is_err() {
        assert!(decode_tick_data(&[0u8; 10]).is_err());
    }

    #[test]
    fn decode_tick_bitmap_matches_ref_encoder() {
        let word = U256::from_be_bytes::<32>([0x01; 32]);
        let payload = encode_uint256_return_ref(word);
        let decoded = decode_tick_bitmap(&payload).expect("decode");
        assert_eq!(decoded, word);
    }

    #[test]
    fn decode_tick_bitmap_too_short_is_err() {
        assert!(decode_tick_bitmap(&[0u8; 10]).is_err());
    }

    // V4 tick decoders share the V3 byte layout (`decode_v4_tick_data` /
    // `decode_v4_tick_bitmap` delegate to the V3 path). Re-assert the shape
    // against the same independent encoder so the V4 wrappers are not
    // untested wrapping.

    #[test]
    fn decode_v4_tick_data_matches_ref_encoder() {
        let payload = encode_ticks_return_ref(7, -7);
        let (gross, net) = decode_v4_tick_data(&payload).expect("decode");
        assert_eq!(gross, U128::from(7u64));
        assert_eq!(net, I256::try_from(-7i64).unwrap());
    }

    #[test]
    fn decode_v4_tick_bitmap_matches_ref_encoder() {
        let word = U256::from_be_bytes::<32>([0xab; 32]);
        let payload = encode_uint256_return_ref(word);
        let decoded = decode_v4_tick_bitmap(&payload).expect("decode");
        assert_eq!(decoded, word);
    }

    // ── encode: calldata sign-extension for tick args ──
    // `encode_tick_data` / `encode_tick_bitmap` take `i32` / `i16` and must
    // sign-extend into the 32-byte ABI word. Independently re-derive the
    // selectors (`keccak256("ticks(int24)") = 0xf30dba93`,
    // `keccak256("tickBitmap(int16)") = 0x5339c296`) so a spec bug in the
    // sol! ABI signature cannot silently pass encode + decode.

    #[test]
    fn encode_tick_data_positive_tick() {
        let cd = encode_tick_data(100);
        assert_eq!(cd.len(), 36);
        let expected_sel = alloy::primitives::keccak256(b"ticks(int24)");
        assert_eq!(&cd[..4], &expected_sel[..4]);
        // arg = 0x00..00 00 00 00 64 (100)
        assert!(cd[4..32].iter().all(|&b| b == 0));
        assert_eq!(&cd[32..36], &[0, 0, 0, 100]);
    }

    #[test]
    fn encode_tick_data_negative_tick_is_sign_extended() {
        let cd = encode_tick_data(-100);
        let expected_sel = alloy::primitives::keccak256(b"ticks(int24)");
        assert_eq!(&cd[..4], &expected_sel[..4]);
        assert!(cd[4..32].iter().all(|&b| b == 0xff), "high bytes 0xff");
        // low 4 bytes = -100 as i32 = 0xffffff9c
        assert_eq!(&cd[32..36], &[0xff, 0xff, 0xff, 0x9c]);
    }

    #[test]
    fn encode_tick_bitmap_sign_extends_negative_word() {
        let cd = encode_tick_bitmap(-1);
        let expected_sel = alloy::primitives::keccak256(b"tickBitmap(int16)");
        assert_eq!(&cd[..4], &expected_sel[..4]);
        assert!(cd[4..32].iter().all(|&b| b == 0xff));
        assert_eq!(
            U256::from_be_bytes::<32>(cd[4..36].try_into().unwrap()),
            U256::MAX
        );
    }

    #[test]
    fn encode_v4_tick_data_includes_pool_id_and_selector() {
        let pool_id: [u8; 32] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20,
        ];
        let cd = encode_v4_tick_data(&pool_id, 100);
        // selector(4) + pool_id(32) + int24 tick(32) = 68 bytes
        assert_eq!(cd.len(), 68);
        let expected_sel = alloy::primitives::keccak256(b"getTickLiquidity(bytes32,int24)");
        assert_eq!(&cd[..4], &expected_sel[..4]);
        assert_eq!(&cd[4..36], &pool_id[..]);
        // tick arg occupies the final 32-byte word; 100 sign-extended → bytes
        // [36..67] are 0x00, last byte [67] is 100.
        assert!(cd[36..67].iter().all(|&b| b == 0));
        assert_eq!(cd[67], 100);
    }

    #[test]
    fn encode_v4_tick_bitmap_includes_pool_id_and_selector() {
        let pool_id: [u8; 32] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20,
        ];
        let cd = encode_v4_tick_bitmap(&pool_id, -1);
        // selector(4) + pool_id(32) + int16 word(32) = 68 bytes
        assert_eq!(cd.len(), 68);
        let expected_sel = alloy::primitives::keccak256(b"getTickBitmap(bytes32,int16)");
        assert_eq!(&cd[..4], &expected_sel[..4]);
        assert_eq!(&cd[4..36], &pool_id[..]);
        // word -1 sign-extended: bytes [36..68) all 0xff
        assert!(cd[36..68].iter().all(|&b| b == 0xff));
    }
}
