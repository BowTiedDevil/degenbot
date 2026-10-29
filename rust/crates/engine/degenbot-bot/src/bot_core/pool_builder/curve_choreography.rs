//! Curve `StableSwap` detection choreography over [`ConstructionIo`].
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
//! Every calldata shape and return decode comes from [`degenbot_rpc::abi`]: the
//! probes name the getter as a typed `sol!` call and cross
//! [`ContractRead::read`](degenbot_rpc::abi::ContractRead::read), which is the
//! same verb the standalone consumer uses.
//!
//! Revert/decode tolerance mirrors the Python `try/except (RpcError,
//! AbiDecodeError, ValueError)` scoping exactly: a detection function returns
//! its "nothing found" default (empty/None) rather than erroring, except the
//! required pool-params fetch (`A`/`fee`/`admin_fee`) which a well-formed pool
//! always exposes and therefore propagates [`ProviderError`].

use std::collections::HashMap;

use alloy::primitives::{address, Address, U256};
use alloy::sol_types::SolCall;
use degenbot_core::errors::ProviderError;
use degenbot_rpc::abi::{
    self, ContractRead, ICurvePool, ICurvePoolInt128, ICurveRegistry, ILendingToken,
};

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

/// Tolerance-aware read: returns `None` on revert OR decode failure (mirrors
/// Python `try/except (RpcError, AbiDecodeError, ValueError)`).
async fn read_opt<C: SolCall>(
    io: &impl ContractRead,
    to: Address,
    call: C,
    block: Option<u64>,
) -> Option<C::Return> {
    io.read(to, call, block).await.ok()
}

/// Tolerance-aware declared-shape word read (`decimals()` behind a prototype
/// list): revert/decode → `None`.
async fn declared_uint_opt(
    io: &impl ContractRead,
    to: Address,
    signature: &[u8],
    block: Option<u64>,
) -> Option<U256> {
    abi::fetch_uint256_field(io, signature, &to, block)
        .await
        .ok()
}

/// Enumerate a Curve pool's coins + balances (uint256 prototype first, int128
/// fallback, stops at the first zero address or revert). Mirrors
/// `curve/detection/coin_discovery.discover_coins`. Never errors — returns
/// whatever it found.
pub async fn discover_curve_coins(
    io: &impl ContractRead,
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
    io: &impl ContractRead,
    pool: Address,
    i: u8,
    block: Option<u64>,
) -> Option<CurvePrototype> {
    let uint_probe = read_opt(io, pool, ICurvePool::coinsCall { i: U256::from(i) }, block).await;
    if uint_probe.is_some_and(|addr| !addr.is_zero()) {
        return Some(CurvePrototype::Uint256);
    }
    let int128_probe = read_opt(
        io,
        pool,
        ICurvePoolInt128::coinsCall { i: i128::from(i) },
        block,
    )
    .await;
    if int128_probe.is_some_and(|addr| !addr.is_zero()) {
        return Some(CurvePrototype::Int128);
    }
    None
}

/// Read `coins(i)` under the given prototype; `None` on revert/decode failure.
async fn read_curve_coin(
    io: &impl ContractRead,
    pool: Address,
    prototype: CurvePrototype,
    i: u8,
    block: Option<u64>,
) -> Option<Address> {
    match prototype {
        CurvePrototype::Uint256 => {
            read_opt(io, pool, ICurvePool::coinsCall { i: U256::from(i) }, block).await
        }
        CurvePrototype::Int128 => {
            read_opt(
                io,
                pool,
                ICurvePoolInt128::coinsCall { i: i128::from(i) },
                block,
            )
            .await
        }
    }
}

/// Read `balances(i)` under the given prototype; `None` on revert/decode failure.
async fn read_curve_balance(
    io: &impl ContractRead,
    pool: Address,
    prototype: CurvePrototype,
    i: u8,
    block: Option<u64>,
) -> Option<U256> {
    match prototype {
        CurvePrototype::Uint256 => {
            read_opt(
                io,
                pool,
                ICurvePool::balancesCall { i: U256::from(i) },
                block,
            )
            .await
        }
        CurvePrototype::Int128 => {
            read_opt(
                io,
                pool,
                ICurvePoolInt128::balancesCall { i: i128::from(i) },
                block,
            )
            .await
        }
    }
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
    io: &impl ContractRead,
    pool: Address,
    block: Option<u64>,
) -> Result<CurvePoolParams, ProviderError> {
    let a = io.read(pool, ICurvePool::ACall {}, block).await?;
    let fee = io.read(pool, ICurvePool::feeCall {}, block).await?;
    let admin_fee = io.read(pool, ICurvePool::admin_feeCall {}, block).await?;
    Ok(CurvePoolParams {
        a_coefficient: a.to::<u128>(),
        fee: fee.to::<u64>(),
        admin_fee: admin_fee.to::<u64>(),
    })
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
    io: &impl ContractRead,
    pool: Address,
    count: usize,
    block: Option<u64>,
) -> Result<Vec<U256>, ProviderError> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let index = u8::try_from(i).map_err(|_| ProviderError::EncodingError {
            message: format!("balances index {i} exceeds u8"),
        })?;
        out.push(
            io.read(
                pool,
                ICurvePool::balancesCall {
                    i: U256::from(index),
                },
                block,
            )
            .await?,
        );
    }
    Ok(out)
}

/// Detect A-coefficient ramping params. If ANY of the four optional reads
/// reverts, the pool is treated as non-ramping (mirrors `detect_a_ramping`).
pub async fn detect_curve_a_ramping(
    io: &impl ContractRead,
    pool: Address,
    block: Option<u64>,
) -> CurveARamping {
    let initial_a = read_opt(io, pool, ICurvePool::initial_ACall {}, block).await;
    let initial_a_time = read_opt(io, pool, ICurvePool::initial_A_timeCall {}, block).await;
    let future_a = read_opt(io, pool, ICurvePool::future_ACall {}, block).await;
    let future_a_time = read_opt(io, pool, ICurvePool::future_A_timeCall {}, block).await;
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
    io: &impl ContractRead,
    pool: Address,
    block: Option<u64>,
) -> CurveCryptoParams {
    let fee_gamma = read_opt(io, pool, ICurvePool::fee_gammaCall {}, block).await;
    let (fee_gamma, mid_fee, out_fee, gamma) = match fee_gamma {
        Some(fg) if !fg.is_zero() => (
            Some(fg.to::<u64>()),
            read_opt(io, pool, ICurvePool::mid_feeCall {}, block)
                .await
                .map(|v| v.to::<u64>()),
            read_opt(io, pool, ICurvePool::out_feeCall {}, block)
                .await
                .map(|v| v.to::<u64>()),
            read_opt(io, pool, ICurvePool::gammaCall {}, block)
                .await
                .map(|v| v.to::<u64>()),
        ),
        _ => (None, None, None, None),
    };
    let offpeg_fee_multiplier = read_opt(io, pool, ICurvePool::offpeg_fee_multiplierCall {}, block)
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
    io: &impl ContractRead,
    token_addresses: &[Address],
    token_decimals: &[u8],
    block: Option<u64>,
) -> CurveLendingDetection {
    let mut use_lending: Vec<bool> = Vec::with_capacity(token_addresses.len());
    let mut overrides: HashMap<usize, U256> = HashMap::new();

    for (idx, token_addr) in token_addresses.iter().enumerate() {
        let mut is_lending = false;

        // cToken probe: isCToken() → underlying() → underlying decimals().
        if let Some(is_c) = read_opt(io, *token_addr, ILendingToken::isCTokenCall {}, block).await {
            if is_c {
                is_lending = true;
                if let Some(underlying) =
                    read_opt(io, *token_addr, ILendingToken::underlyingCall {}, block).await
                {
                    // `decimals()` is the generic metadata read (a word of a
                    // width the caller does not pin), so its selector comes
                    // from the signature text.
                    if let Some(underlying_dec) =
                        declared_uint_opt(io, underlying, b"decimals()", block).await
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
            if let Some(underlying) =
                read_opt(io, *token_addr, ILendingToken::tokenCall {}, block).await
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
    io: &impl ContractRead,
    pool: Address,
    registry_addresses: &[Address],
    block: Option<u64>,
) -> Option<Address> {
    for registry in registry_addresses {
        if let Some(lp) = read_opt(
            io,
            *registry,
            ICurveRegistry::get_lp_tokenCall { pool },
            block,
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
    io: &impl ContractRead,
    pool: Address,
    token_addresses: &[Address],
    registry_addresses: &[Address],
    block: Option<u64>,
) -> CurveMetapoolDetection {
    for registry in registry_addresses {
        let Some(is_meta) =
            read_opt(io, *registry, ICurveRegistry::is_metaCall { pool }, block).await
        else {
            // Revert/decode failure on is_meta → try the next registry.
            continue;
        };
        if !is_meta {
            continue;
        }

        let base_pool_address =
            resolve_curve_base_pool(io, pool, token_addresses, *registry, block).await;

        let tokens_underlying: Vec<Address> = match read_opt(
            io,
            *registry,
            ICurveRegistry::get_underlying_coinsCall { pool },
            block,
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
    io: &impl ContractRead,
    pool: Address,
    token_addresses: &[Address],
    registry: Address,
    block: Option<u64>,
) -> Option<Address> {
    if let Some(base_pool) = read_opt(io, pool, ICurvePool::base_poolCall {}, block).await {
        return Some(base_pool);
    }
    if let Some(base_pool) = read_opt(
        io,
        registry,
        ICurveRegistry::get_base_poolCall { pool },
        block,
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
