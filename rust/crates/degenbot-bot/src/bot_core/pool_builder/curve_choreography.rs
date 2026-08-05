//! Curve `StableSwap` detection choreography over [`ConstructionIo`] (task
//! `4EBHRC`, epic `TV72EG`).
//!
//! The Curve detection pipeline that the Python `builders/curve_pool_builder.py`
//! drove through `PyBotIo` moves **core-side** here as free async functions over
//! a [`ConstructionIo`] handle — a standalone `cargo add degenbot` consumer
//! enumerates a Curve pool's coins/balances/params without Python. It mirrors
//! `curve/detection/*` (coin discovery, A-ramping, lending, crypto, `lp_token`,
//! metapool) + the `CurvePoolBuilder.build` pool-params fetch **byte-for-byte**:
//! uint256→int128 prototype fallback, zero-address stop, optional-revert
//! tolerance. No `pyo3` here (the no-pyo3-in-cores invariant).
//!
//! Every encode/decode comes from [`degenbot_rpc::abi`] (the shared
//! `sol!`-generated interfaces); the no-arg uint/address reads build their own
//! 4-byte selector and decode via the shared uint256/address decoders.
//!
//! Revert/decode tolerance mirrors the Python `try/except (RpcError,
//! AbiDecodeError, ValueError)` scoping exactly: a detection function returns
//! its "nothing found" default (empty/None) rather than erroring, except the
//! required pool-params fetch (`A`/`fee`/`admin_fee`) which a well-formed pool
//! always exposes and therefore propagates [`ProviderError`].

use std::collections::HashMap;

use alloy::primitives::{address, Address, U256};
use degenbot_core::errors::ProviderError;
use degenbot_rpc::abi;

use super::choreography::{eth_call, selector};
use crate::bot_core::construction_io::ConstructionIo;

/// Which coin/balance getter prototype a pool exposes. `coins(uint256)` is
/// tried first; older pools fall back to `coins(int128)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurvePrototype {
    /// `coins(uint256)` / `balances(uint256)`.
    Uint256,
    /// `coins(int128)` / `balances(int128)`.
    Int128,
}

/// Result of coin + balance enumeration (mirrors `CoinDiscoveryResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurveCoinDiscovery {
    /// Coin addresses in canonical Curve order.
    pub token_addresses: Vec<Address>,
    /// One balance per coin, aligned with `token_addresses`.
    pub balances: Vec<U256>,
    /// The detected coin getter prototype (`None` if no coins were found).
    pub coin_prototype: Option<CurvePrototype>,
    /// The detected balance getter prototype (`None` if no coins were found).
    pub balance_prototype: Option<CurvePrototype>,
}

/// Immutable pool parameters (`A`, `fee`, `admin_fee`) — required, not
/// optional (a well-formed Curve pool exposes all three).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurvePoolParams {
    /// Amplification coefficient (raw, as stored on-chain).
    pub a_coefficient: u128,
    /// Swap fee in `FEE_DENOMINATOR = 1e10` units.
    pub fee: u64,
    /// Admin fee share in `FEE_DENOMINATOR` units.
    pub admin_fee: u64,
}

/// A-coefficient ramping detection result (mirrors `ARampingResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurveARamping {
    /// Initial A at `initial_a_time`; `None` for non-ramping pools.
    pub initial_a: Option<u128>,
    /// Ramping start timestamp; `None` for non-ramping pools.
    pub initial_a_time: Option<u64>,
    /// Target A at `future_a_time`; `None` for non-ramping pools.
    pub future_a: Option<u128>,
    /// Ramping end timestamp; `None` for non-ramping pools.
    pub future_a_time: Option<u64>,
    /// `true` iff all four ramping values were fetched.
    pub has_ramping: bool,
}

/// Crypto-pool parameter detection result (mirrors `CryptoDetectionResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurveCryptoParams {
    /// `true` iff the pool reports `fee_gamma() > 0`.
    pub is_crypto: bool,
    /// Crypto `fee_gamma`; `None` for non-crypto pools.
    pub fee_gamma: Option<u64>,
    /// Crypto `mid_fee`; `None` for non-crypto pools.
    pub mid_fee: Option<u64>,
    /// Crypto `out_fee`; `None` for non-crypto pools.
    pub out_fee: Option<u64>,
    /// Crypto `gamma`; `None` for non-crypto pools.
    pub gamma: Option<u64>,
    /// `offpeg_fee_multiplier`; `None` if not exposed.
    pub offpeg_fee_multiplier: Option<u64>,
}

/// Lending-token detection result (mirrors `LendingDetectionResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurveLendingDetection {
    /// Per-coin `use_lending` flag (true for cToken/yToken-backed coins).
    pub use_lending: Vec<bool>,
    /// Per-coin precision multipliers (underlying-decimals-adjusted for
    /// lending coins); `None` if no lending tokens / no overrides needed.
    pub precision_multipliers: Option<Vec<U256>>,
}

/// Metapool detection result (mirrors `MetapoolDetectionResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurveMetapoolDetection {
    /// `true` iff the pool is a metapool.
    pub is_meta: bool,
    /// Base pool address for metapools; `None` for plain pools.
    pub base_pool_address: Option<Address>,
    /// Underlying coin addresses for metapools; `None` for plain pools.
    pub tokens_underlying: Option<Vec<Address>>,
}

/// 3Crv LP token — the fallback base-pool detection marker (a second-coin
/// match means the tripool is the base pool when `base_pool()` /
/// `get_base_pool()` are unavailable).
const THREE_CRV_LP_TOKEN: Address = address!("0x6c3F90f043a72FA612Cbac8115ee7e52bDE6E490");
/// 3pool (DAI/USDC/USDT) — the `tripool`, the canonical 3Crv base pool.
const THREE_CRV_POOL: Address = address!("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7");

/// Tolerance-aware encode→call→decode: returns `None` on revert OR decode
/// failure (mirrors Python `try/except (RpcError, AbiDecodeError, ValueError)`).
async fn call_opt<T>(
    io: &ConstructionIo,
    to: Address,
    data: Vec<u8>,
    block: Option<u64>,
    decode: fn(&[u8]) -> degenbot_core::errors::ProviderResult<T>,
) -> Option<T> {
    match eth_call(io, to, data, block).await {
        Ok(bytes) => decode(&bytes).ok(),
        Err(_) => None,
    }
}

/// Tolerance-aware no-argument `uint256` read (revert/decode → `None`).
async fn fetch_no_arg_uint_opt(
    io: &ConstructionIo,
    to: Address,
    signature: &[u8],
    block: Option<u64>,
) -> Option<U256> {
    call_opt(
        io,
        to,
        selector(signature).to_vec(),
        block,
        abi::decode_uint256,
    )
    .await
}

/// Tolerance-aware no-argument `address` read (revert/decode → `None`).
async fn fetch_no_arg_address_opt(
    io: &ConstructionIo,
    to: Address,
    signature: &[u8],
    block: Option<u64>,
) -> Option<Address> {
    call_opt(
        io,
        to,
        selector(signature).to_vec(),
        block,
        abi::decode_curve_coins,
    )
    .await
}

/// Enumerate a Curve pool's coins + balances (uint256 prototype first, int128
/// fallback, stops at the first zero address or revert). Mirrors
/// `curve/detection/coin_discovery.discover_coins`. Never errors — returns
/// whatever it found.
pub async fn discover_curve_coins(
    io: &ConstructionIo,
    pool: Address,
    block: Option<u64>,
) -> CurveCoinDiscovery {
    const MAX_COINS: u8 = 8;
    let mut token_addresses: Vec<Address> = Vec::new();
    let mut balances: Vec<U256> = Vec::new();
    let mut prototype: Option<CurvePrototype> = None;

    for i in 0..MAX_COINS {
        // Determine the getter prototype on the first successful coin read.
        if prototype.is_none() {
            match discover_curve_prototype(io, pool, i, block).await {
                Some(p) => prototype = Some(p),
                None => break,
            }
        }
        // `prototype` is Some here (we would have broken above otherwise).
        let Some(p) = prototype else { break };

        // Read the coin address at index `i` (zero address → stop).
        match read_curve_coin(io, pool, p, i, block).await {
            Some(addr) if !addr.is_zero() => token_addresses.push(addr),
            _ => break,
        }

        // Read the balance for the just-appended coin.
        match read_curve_balance(io, pool, p, i, block).await {
            Some(balance) => balances.push(balance),
            _ => break,
        }
    }

    CurveCoinDiscovery {
        token_addresses,
        balances,
        coin_prototype: prototype,
        balance_prototype: prototype,
    }
}

/// Determine which coin-getter prototype works by attempting a read of coin index
/// `i` under both the uint256 and int128 prototypes. Returns the first working
/// prototype, or `None` if neither returns a non-zero address.
async fn discover_curve_prototype(
    io: &ConstructionIo,
    pool: Address,
    i: u8,
    block: Option<u64>,
) -> Option<CurvePrototype> {
    if let Ok(bytes) = eth_call(io, pool, abi::encode_curve_coins_uint(i), block).await {
        if let Ok(addr) = abi::decode_curve_coins(&bytes) {
            if !addr.is_zero() {
                return Some(CurvePrototype::Uint256);
            }
        }
    }
    if let Ok(bytes) = eth_call(io, pool, abi::encode_curve_coins_int128(i), block).await {
        if let Ok(addr) = abi::decode_curve_coins(&bytes) {
            if !addr.is_zero() {
                return Some(CurvePrototype::Int128);
            }
        }
    }
    None
}

/// Read `coins(i)` under the given prototype; `None` on revert/decode failure.
async fn read_curve_coin(
    io: &ConstructionIo,
    pool: Address,
    prototype: CurvePrototype,
    i: u8,
    block: Option<u64>,
) -> Option<Address> {
    let calldata = match prototype {
        CurvePrototype::Uint256 => abi::encode_curve_coins_uint(i),
        CurvePrototype::Int128 => abi::encode_curve_coins_int128(i),
    };
    call_opt(io, pool, calldata, block, abi::decode_curve_coins).await
}

/// Read `balances(i)` under the given prototype; `None` on revert/decode failure.
async fn read_curve_balance(
    io: &ConstructionIo,
    pool: Address,
    prototype: CurvePrototype,
    i: u8,
    block: Option<u64>,
) -> Option<U256> {
    let calldata = match prototype {
        CurvePrototype::Uint256 => abi::encode_curve_balances_uint(i),
        CurvePrototype::Int128 => abi::encode_curve_balances_int128(i),
    };
    call_opt(io, pool, calldata, block, abi::decode_curve_balances).await
}

/// Fetch the immutable pool parameters `A()`, `fee()`, `admin_fee()`. Unlike
/// the optional detection reads this is **required** (mirrors Python
/// `_fetch_pool_params`, which propagates failures).
///
/// # Errors
///
/// Returns [`ProviderError`] from the underlying `eth_call` or a decode
/// failure.
pub async fn fetch_curve_pool_params(
    io: &ConstructionIo,
    pool: Address,
    block: Option<u64>,
) -> Result<CurvePoolParams, ProviderError> {
    let a = req_no_arg_uint(io, pool, b"A()", block).await?;
    let fee = req_no_arg_uint(io, pool, b"fee()", block).await?;
    let admin_fee = req_no_arg_uint(io, pool, b"admin_fee()", block).await?;
    Ok(CurvePoolParams {
        a_coefficient: a.to::<u128>(),
        fee: fee.to::<u64>(),
        admin_fee: admin_fee.to::<u64>(),
    })
}

/// Required (propagating) no-argument `uint256` read.
async fn req_no_arg_uint(
    io: &ConstructionIo,
    to: Address,
    signature: &[u8],
    block: Option<u64>,
) -> Result<U256, ProviderError> {
    let bytes = eth_call(io, to, selector(signature).to_vec(), block).await?;
    abi::decode_uint256(&bytes)
}

/// Fetch a Curve pool's token balances via `balances(uint256)` indexed
/// `0..count` (mirrors `CurvePoolBuilder.update`'s snapshot loop).
///
/// Uses the `uint256` argument prototype (modern pools), matching the Python
/// builder's primitive. Returns one `U256` per index.
///
/// # Errors
///
/// Returns a [`ProviderError`] on an `eth_call` or decode failure.
pub async fn fetch_curve_balances(
    io: &ConstructionIo,
    pool: Address,
    count: usize,
    block: Option<u64>,
) -> Result<Vec<U256>, ProviderError> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let index = u8::try_from(i).map_err(|_| ProviderError::EncodingError {
            message: format!("balances index {i} exceeds u8"),
        })?;
        let bytes = eth_call(io, pool, abi::encode_curve_balances_uint(index), block).await?;
        out.push(abi::decode_curve_balances(&bytes)?);
    }
    Ok(out)
}

/// Detect A-coefficient ramping params. If ANY of the four optional reads
/// reverts, the pool is treated as non-ramping (mirrors `detect_a_ramping`).
pub async fn detect_curve_a_ramping(
    io: &ConstructionIo,
    pool: Address,
    block: Option<u64>,
) -> CurveARamping {
    let initial_a = fetch_no_arg_uint_opt(io, pool, b"initial_A()", block).await;
    let initial_a_time = fetch_no_arg_uint_opt(io, pool, b"initial_A_time()", block).await;
    let future_a = fetch_no_arg_uint_opt(io, pool, b"future_A()", block).await;
    let future_a_time = fetch_no_arg_uint_opt(io, pool, b"future_A_time()", block).await;
    match (initial_a, initial_a_time, future_a, future_a_time) {
        (Some(ia), Some(iat), Some(fa), Some(fat)) => CurveARamping {
            initial_a: Some(ia.to::<u128>()),
            initial_a_time: Some(iat.to::<u64>()),
            future_a: Some(fa.to::<u128>()),
            future_a_time: Some(fat.to::<u64>()),
            has_ramping: true,
        },
        _ => CurveARamping {
            initial_a: None,
            initial_a_time: None,
            future_a: None,
            future_a_time: None,
            has_ramping: false,
        },
    }
}

/// Detect crypto-pool params (`fee_gamma() > 0` gates the crypto reads; each
/// sub-param is individually revert-tolerant; `offpeg_fee_multiplier()` is
/// fetched unconditionally). Mirrors `detect_crypto_params`.
pub async fn detect_curve_crypto_params(
    io: &ConstructionIo,
    pool: Address,
    block: Option<u64>,
) -> CurveCryptoParams {
    let fee_gamma = fetch_no_arg_uint_opt(io, pool, b"fee_gamma()", block).await;
    let (fee_gamma, mid_fee, out_fee, gamma) = match fee_gamma {
        Some(fg) if !fg.is_zero() => (
            Some(fg.to::<u64>()),
            fetch_no_arg_uint_opt(io, pool, b"mid_fee()", block)
                .await
                .map(|v| v.to::<u64>()),
            fetch_no_arg_uint_opt(io, pool, b"out_fee()", block)
                .await
                .map(|v| v.to::<u64>()),
            fetch_no_arg_uint_opt(io, pool, b"gamma()", block)
                .await
                .map(|v| v.to::<u64>()),
        ),
        _ => (None, None, None, None),
    };
    let offpeg_fee_multiplier = fetch_no_arg_uint_opt(io, pool, b"offpeg_fee_multiplier()", block)
        .await
        .map(|v| v.to::<u64>());
    CurveCryptoParams {
        is_crypto: fee_gamma.is_some(),
        fee_gamma,
        mid_fee,
        out_fee,
        gamma,
        offpeg_fee_multiplier,
    }
}

/// Detect lending tokens (cTokens via `isCToken()`/`underlying()`, yTokens via
/// `token()`) and compute underlying-decimals-adjusted precision multipliers.
/// Mirrors `detect_lending_tokens`.
///
/// `token_decimals` is the per-coin ERC20 `decimals()` (owned Python-side by
/// the driver, which builds the token companions); used for the default
/// precision multiplier `10**(18 - decimals)` for non-overridden coins.
pub async fn detect_curve_lending_tokens(
    io: &ConstructionIo,
    token_addresses: &[Address],
    token_decimals: &[u8],
    block: Option<u64>,
) -> CurveLendingDetection {
    let mut use_lending: Vec<bool> = Vec::with_capacity(token_addresses.len());
    let mut overrides: HashMap<usize, U256> = HashMap::new();

    for (idx, token_addr) in token_addresses.iter().enumerate() {
        let mut is_lending = false;

        // cToken probe: isCToken() → underlying() → underlying decimals().
        if let Some(is_c) = call_opt(
            io,
            *token_addr,
            abi::encode_lending_is_ctoken(),
            block,
            abi::decode_lending_is_ctoken,
        )
        .await
        {
            if is_c {
                is_lending = true;
                if let Some(underlying) = call_opt(
                    io,
                    *token_addr,
                    abi::encode_lending_underlying(),
                    block,
                    abi::decode_lending_underlying,
                )
                .await
                {
                    if let Some(underlying_dec) =
                        fetch_no_arg_uint_opt(io, underlying, b"decimals()", block).await
                    {
                        // Override to use the UNDERLYING decimals, not the
                        // wrapped token's.
                        overrides.insert(
                            idx,
                            U256::from(10u64).pow(U256::from(18 - underlying_dec.to::<u8>())),
                        );
                    }
                }
            }
        }

        // yToken probe: token() returning a non-zero underlying.
        if !is_lending {
            if let Some(underlying) = call_opt(
                io,
                *token_addr,
                abi::encode_lending_token(),
                block,
                abi::decode_lending_token,
            )
            .await
            {
                if !underlying.is_zero() {
                    is_lending = true;
                }
            }
        }

        use_lending.push(is_lending);
    }

    let has_lending = !overrides.is_empty() || use_lending.iter().any(|&b| b);
    let precision_multipliers = if has_lending {
        Some(
            (0..token_addresses.len())
                .map(|i| {
                    overrides.get(&i).copied().unwrap_or_else(|| {
                        U256::from(10u64).pow(U256::from(18 - token_decimals[i]))
                    })
                })
                .collect(),
        )
    } else {
        None
    };

    CurveLendingDetection {
        use_lending,
        precision_multipliers,
    }
}

/// Find the LP token address by probing the registry/factory addresses in
/// order. Returns `None` if no registry returns a non-zero address. Mirrors
/// `find_lp_token`.
pub async fn find_curve_lp_token(
    io: &ConstructionIo,
    pool: Address,
    registry_addresses: &[Address],
    block: Option<u64>,
) -> Option<Address> {
    for registry in registry_addresses {
        if let Some(lp) = call_opt(
            io,
            *registry,
            abi::encode_curve_get_lp_token(&pool),
            block,
            abi::decode_curve_get_lp_token,
        )
        .await
        {
            if !lp.is_zero() {
                return Some(lp);
            }
        }
    }
    None
}

/// Detect whether a pool is a metapool and resolve its base pool + underlying
/// coins, mirroring `detect_metapool` (`is_meta` per registry, base-pool address
/// resolution with 3Crv fallback, `address[8]` underlying-coins decode with
/// zero stop).
pub async fn detect_curve_metapool(
    io: &ConstructionIo,
    pool: Address,
    token_addresses: &[Address],
    registry_addresses: &[Address],
    block: Option<u64>,
) -> CurveMetapoolDetection {
    for registry in registry_addresses {
        let Some(is_meta) = call_opt(
            io,
            *registry,
            abi::encode_curve_is_meta(&pool),
            block,
            abi::decode_curve_is_meta,
        )
        .await
        else {
            // Revert/decode failure on is_meta → try the next registry.
            continue;
        };
        if !is_meta {
            continue;
        }

        let base_pool_address =
            resolve_curve_base_pool(io, pool, token_addresses, *registry, block).await;

        let tokens_underlying: Vec<Address> = match call_opt(
            io,
            *registry,
            abi::encode_curve_get_underlying_coins(&pool),
            block,
            abi::decode_curve_get_underlying_coins,
        )
        .await
        {
            // Filter out trailing zero addresses (the contract returns a
            // fixed `address[8]`).
            Some(arr) => arr.into_iter().take_while(|a| !a.is_zero()).collect(),
            None => continue,
        };

        return CurveMetapoolDetection {
            is_meta: true,
            base_pool_address,
            tokens_underlying: Some(tokens_underlying),
        };
    }

    CurveMetapoolDetection {
        is_meta: false,
        base_pool_address: None,
        tokens_underlying: None,
    }
}

/// Resolve the base pool address, trying `base_pool()` on the pool, then
/// `get_base_pool()` on the registry, then the 3Crv LP-token fallback. Each
/// probe is individually revert-tolerant. Mirrors `_resolve_base_pool_address`.
async fn resolve_curve_base_pool(
    io: &ConstructionIo,
    pool: Address,
    token_addresses: &[Address],
    registry: Address,
    block: Option<u64>,
) -> Option<Address> {
    if let Some(base_pool) = fetch_no_arg_address_opt(io, pool, b"base_pool()", block).await {
        return Some(base_pool);
    }
    if let Some(base_pool) = call_opt(
        io,
        registry,
        abi::encode_curve_get_base_pool(&pool),
        block,
        abi::decode_curve_get_base_pool,
    )
    .await
    {
        return Some(base_pool);
    }
    if token_addresses.len() >= 2 && token_addresses[1] == THREE_CRV_LP_TOKEN {
        return Some(THREE_CRV_POOL);
    }
    None
}
