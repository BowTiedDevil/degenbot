//! Pool construction orchestration (task `3FVZF4`, epic `Z5CNPB`).
//!
//! Part 2 of the builder port: the probe-dispatch-assemble turn a bare
//! on-chain `(chain_id, address)` into a core structural pool
//! identity+state, using the T1 choreography (`choreography`) over a
//! [`ConstructionIo`]. This module is the single source for constructing V2
//! pools end-to-end with zero Python; the DEX-variant/fees/deployer/init_hash
//! come from the built-in [`degenbot_uniswap::dex_identity`] presets (keyed by
//! factory, with a per-pool `stableSwap()`/`stable()` read call to disambiguate
//! the shared Camelot/Aerodrome volatile-vs-stable factory).
//!
//! No `pyo3` in this module (the no-pyo3-in-cores invariant). A standalone
//! `cargo add degenbot` consumer reaches [`build_v2`] via the `degenbot`
//! umbrella (Tier-0 slice in `examples/standalone_consumer.rs`).

use std::collections::HashMap;
use std::sync::Arc;

use alloy::primitives::{Address, B256, U256};
use degenbot_core::address_utils::address_to_checksum_string;
use degenbot_core::errors::ProviderError;
use degenbot_db::error::DbError;
use degenbot_db::snapshot::TickMapDb;
use degenbot_pools::aerodrome_v2_state::RegisterAerodromeV2PoolParams;
use degenbot_pools::balancer_stable_state::RegisterBalancerStablePoolParams;
use degenbot_pools::balancer_weighted_state::RegisterBalancerWeightedPoolParams;
use degenbot_pools::curve_data_provider::CurveDataProvider;
use degenbot_pools::curve_state::RegisterCurvePoolParams;
use degenbot_pools::curve_strategies::resolve_curve_strategy_discriminants;
use degenbot_pools::spec_bounds;
use degenbot_pools::v2_state::RegisterV2PoolParams;
use degenbot_pools::v3_state::RegisterV3PoolParams;
use degenbot_pools::v4_state::{RegisterV4PoolParams, V4PoolKey};
use degenbot_rpc::abi;
use degenbot_uniswap::deployments;
use degenbot_uniswap::dex_identity::{self, DexIdentity, DexVariant};

use super::choreography::{self};
use super::curve_choreography;
use crate::bot_core::construction_io::ConstructionIo;
use crate::bot_core::curve_data_provider_impl::RpcCurveDataProvider;
use crate::bot_core::{PoolTickCoverage, TickInfo};

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

/// Typed builder failure.
#[derive(Debug, thiserror::Error)]
pub enum PoolBuilderError {
    #[error("RPC/decode error: {0}")]
    Rpc(#[from] ProviderError),
    #[error("unknown factory {factory} — no built-in DEX variant preset")]
    UnknownVariant { factory: Address },
    #[error("out-of-spec V2 reserve")]
    Spec,
    #[error("decode failure: {message}")]
    Decoding { message: String },
    #[error("CREATE2 address verification failed")]
    Create2,
    #[error("DB read failed: {0}")]
    Db(#[from] DbError),
    #[error("V4 identity incomplete: {message}")]
    MissingIdentity { message: String },
}

/// Sentinels returned when an ERC-20 metadata field cannot be resolved,
/// mirroring `erc20.py::UNKNOWN_NAME/UNKNOWN_SYMBOL/UNKNOWN_DECIMALS` (the
/// canonical Python display constants) so the core twin is byte-identical.
const UNKNOWN_NAME: &str = "Unknown Token";
const UNKNOWN_SYMBOL: &str = "UNKNOWN";
const UNKNOWN_DECIMALS: u8 = 18;

/// Resolve ERC-20 token metadata DB-first, then on-chain, with the
/// alternate-prototype + UNKNOWN fallbacks — the core twin of
/// `Erc20Builder.build` steps 3–5 (VK3YDM-S2).
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
/// Returns `(name, symbol, decimals)`. This is a pure choreography fn over
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
    if let Ok(Some((n, s, d))) = choreography::fetch_erc20_metadata(io, address).await {
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
        if let Ok(s) = choreography::fetch_erc20_string_field(io, address, p, None).await {
            return s;
        }
    }
    fallback.to_string()
}

/// Read `decimals()` (or an alternate prototype) as a `uint256`, validated to
/// `u8`; else `UNKNOWN_DECIMALS`.
async fn fetch_field_decimals(io: &ConstructionIo, address: Address, prototypes: &[&[u8]]) -> u8 {
    for p in prototypes {
        if let Ok(v) = choreography::fetch_erc20_uint_field(io, address, p, None).await {
            if let Ok(dec) = u8::try_from(v.to::<u64>()) {
                return dec;
            }
        }
    }
    UNKNOWN_DECIMALS
}

/// Probe a pool contract to identify its family via the canonical read-call
/// probe order (mirrors `type_resolution.py::probe_pool_type`):
/// `slot0()` → V3, `getReserves()` → V2, `getPoolId()` → Balancer (weighted vs
/// stable by `getNormalizedWeights()`), else Curve. Returns a
/// [`PoolFamily`]; never errors — a probe that reverts is treated as absent.
pub async fn probe_pool_type(
    io: &ConstructionIo,
    address: Address,
    block: Option<u64>,
) -> PoolFamily {
    if choreography::eth_call(
        io,
        address,
        choreography::selector(b"slot0()").to_vec(),
        block,
    )
    .await
    .is_ok()
    {
        PoolFamily::V3
    } else if choreography::eth_call(
        io,
        address,
        choreography::selector(b"getReserves()").to_vec(),
        block,
    )
    .await
    .is_ok()
    {
        PoolFamily::V2
    } else if choreography::eth_call(
        io,
        address,
        choreography::selector(b"getPoolId()").to_vec(),
        block,
    )
    .await
    .is_ok()
    {
        if choreography::eth_call(
            io,
            address,
            choreography::selector(b"getNormalizedWeights()").to_vec(),
            block,
        )
        .await
        .is_ok()
        {
            PoolFamily::BalancerWeighted
        } else {
            PoolFamily::BalancerStable
        }
    } else {
        PoolFamily::Curve
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
        let stable = fetch_no_arg_bool(io, address, b"stableSwap()", block).await?;
        Ok(Some(if stable {
            dex_identity::CAMELOT_V2_STABLE
        } else {
            dex_identity::CAMELOT_V2_VOLATILE
        }))
    } else if factory == dex_identity::AERODROME_V2_STABLE.factory {
        let stable = fetch_no_arg_bool(io, address, b"stable()", block).await?;
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

/// Read a no-argument `bool` from the pool (the stable/volatile disambiguator).
async fn fetch_no_arg_bool(
    io: &ConstructionIo,
    address: Address,
    signature: &[u8],
    block: Option<u64>,
) -> Result<bool, ProviderError> {
    use alloy::dyn_abi::{DynSolType, DynSolValue};
    let bytes = choreography::eth_call(
        io,
        address,
        choreography::selector(signature).to_vec(),
        block,
    )
    .await?;
    match DynSolType::Bool.abi_decode(&bytes) {
        Ok(DynSolValue::Bool(b)) => Ok(b),
        _ => Err(ProviderError::DecodingError {
            message: "invalid bool decode for no-arg read".into(),
        }),
    }
}

/// Read Camelot's `FEE_DENOMINATOR()` (uint), used to scale the solidly-stable
/// math. `None` when the read reverts (non-Camelot-stable callers never call).
async fn fetch_fee_denominator(
    io: &ConstructionIo,
    address: Address,
    block: Option<u64>,
) -> Result<Option<u64>, ProviderError> {
    let calldata = choreography::selector(b"FEE_DENOMINATOR()").to_vec();
    match choreography::eth_call(io, address, calldata, block).await {
        Ok(bytes) => {
            use alloy::dyn_abi::{DynSolType, DynSolValue};
            match DynSolType::Uint(256).abi_decode(&bytes) {
                Ok(DynSolValue::Uint(n, _)) => Ok(Some(n.to::<u64>())),
                _ => Ok(None),
            }
        }
        Err(_) => Ok(None),
    }
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
/// [`PoolBuilderError::Create2`] on a CREATE2 mismatch (when the factory ships
/// in the JSON), or an RPC/spec error.
pub async fn build_v2(
    chain_id: u64,
    address: Address,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RegisterV2PoolParams, PoolBuilderError> {
    let imm = choreography::fetch_v2_immutable_data(io, address, block).await?;
    let (r0, r1) = choreography::fetch_v2_reserves(io, address, block).await?;
    let id = resolve_v2_dex(io, address, imm.factory, block)
        .await?
        .ok_or(PoolBuilderError::UnknownVariant {
            factory: imm.factory,
        })?;

    let reserve0 =
        spec_bounds::narrow_v2_reserve(r0, "reserve0").map_err(|_| PoolBuilderError::Spec)?;
    let reserve1 =
        spec_bounds::narrow_v2_reserve(r1, "reserve1").map_err(|_| PoolBuilderError::Spec)?;

    deployments::verify_v2_pool_address(chain_id, imm.factory, address, imm.token0, imm.token1)
        .map_err(|_| PoolBuilderError::Create2)?;

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

/// Build an Aerodrome V2 (constant-product, unidirectional-fee) pool (the
/// ADR-005 slice 14g follow-up / SSD2XI): a bare `(chain_id, address)` becomes
/// a [`RegisterAerodromeV2PoolParams`] ready for `BotState::register_aerodrome_pool`.
///
/// The Aerodrome volatile/stable pair shares one factory, so the two reads the
/// `getReserves` probe can't reach are: `stable()` on the pool (picks the
/// variant) and `getFee(address,bool)` on the factory (the unidirectional fee;
/// `10_000` = 100%). CREATE2 identity is verified against the JSON-sourced
/// EIP-1167 deployer + implementation (S5SJXF/WLJD2Y — the JC6OFG parity gap).
///
/// # Errors
///
/// Returns [`PoolBuilderError::Rpc`] on an RPC/decode failure,
/// [`PoolBuilderError::Spec`] on an out-of-u112 bounds reserve, or
/// [`PoolBuilderError::Create2`] on an EIP-1167 address mismatch.
pub async fn build_aerodrome_v2(
    chain_id: u64,
    address: Address,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RegisterAerodromeV2PoolParams, PoolBuilderError> {
    let imm = choreography::fetch_v2_immutable_data(io, address, block).await?;
    let (r0, r1) = choreography::fetch_v2_reserves(io, address, block).await?;
    let common =
        choreography::fetch_aerodrome_stable_and_fee(io, address, imm.factory, block).await?;

    let id = if common.stable {
        dex_identity::AERODROME_V2_STABLE
    } else {
        dex_identity::AERODROME_V2_VOLATILE
    };

    deployments::verify_aerodrome_v2_pool_address(
        chain_id,
        imm.factory,
        address,
        imm.token0,
        imm.token1,
        common.stable,
    )
    .map_err(|_| PoolBuilderError::Create2)?;

    let reserve0 =
        spec_bounds::narrow_v2_reserve(r0, "reserve0").map_err(|_| PoolBuilderError::Spec)?;
    let reserve1 =
        spec_bounds::narrow_v2_reserve(r1, "reserve1").map_err(|_| PoolBuilderError::Spec)?;

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
// Balancer V2 constructors (the SSSXG6 builder-follow-up: weighted + stable)
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
/// Returns [`PoolBuilderError::Spec`] if the token's decimals cannot be read.
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
/// word) via the choreography primitive.
///
/// # Errors
///
/// Returns [`PoolBuilderError::Rpc`] on an `eth_call`/decode failure.
async fn decimals_of(
    io: &ConstructionIo,
    token: Address,
    block: Option<u64>,
) -> Result<u64, PoolBuilderError> {
    use degenbot_rpc::abi;
    let bytes = choreography::eth_call(
        io,
        token,
        choreography::selector(b"decimals()").to_vec(),
        block,
    )
    .await
    .map_err(PoolBuilderError::Rpc)?;
    let dec = abi::decode_uint256(&bytes).map_err(PoolBuilderError::Rpc)?;
    Ok(dec.to::<u64>())
}

/// Assemble `build_balancer_weighted` params for a Balancer V2 weighted pool
/// (the ADR-005 slice 12b builder twin, SSSXG6): reads the 32-byte `poolId`,
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
/// Returns [`PoolBuilderError::Rpc`] on an RPC/decode failure or
/// [`PoolBuilderError::Spec`] on an out-of-range scaling factor.
pub async fn build_balancer_weighted(
    vault: Address,
    address: Address,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RegisterBalancerWeightedPoolParams, PoolBuilderError> {
    let pool_id = choreography::fetch_balancer_pool_id(io, address, block).await?;
    let (tokens, balances) =
        choreography::fetch_balancer_vault_tokens(io, vault, &pool_id, block).await?;
    let fee = choreography::fetch_balancer_swap_fee(io, address, block).await?;
    let weights = choreography::fetch_balancer_weights(io, address, block).await?;

    let bytecode = io.get_code(address, block).await?;
    let pow_version = detect_pow_version(&bytecode);

    let mut scaling_factors = Vec::with_capacity(tokens.len());
    for token in &tokens {
        scaling_factors.push(compute_scaling_factor(io, *token, block).await?);
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
/// ADR-005 slice 12d builder twin, SSSXG6): reads the `poolId`, Vault
/// `getPoolTokens`, `getSwapFeePercentage`, `getAmplificationParameter`; detects
/// the BPT index (token whose address matches the pool — `None` for
/// `MetaStablePools`); reads `getRateProviders` + per-provider `getRate()`
/// (empty when the pool doesn't expose them); computes rate-multiplied scaling
/// factors; and resolves the `invariant_version` from the Vault specialization
/// (`MetaStable` `specialization=1` → V2, else V1) with an optional override.
/// Returns a [`RegisterBalancerStablePoolParams`] for
/// `BotState::register_balancer_stable_pool`.
///
/// Mirrors `balancer_builder.py::_build_stable` step-for-step.
///
/// # Errors
///
/// Returns [`PoolBuilderError::Rpc`] on an RPC/decode failure or
/// [`PoolBuilderError::Spec`] on an out-of-range scaling factor.
pub async fn build_balancer_stable(
    vault: Address,
    address: Address,
    io: &ConstructionIo,
    block: Option<u64>,
    invariant_version_override: Option<u8>,
) -> Result<RegisterBalancerStablePoolParams, PoolBuilderError> {
    let pool_id = choreography::fetch_balancer_pool_id(io, address, block).await?;
    let (tokens, balances) =
        choreography::fetch_balancer_vault_tokens(io, vault, &pool_id, block).await?;
    let fee = choreography::fetch_balancer_swap_fee(io, address, block).await?;
    let amp = choreography::fetch_balancer_amp(io, address, block).await?;

    let decoded = decode_balancer_pool_id(&pool_id);
    let bpt_idx = tokens.iter().position(|t| *t == address);
    let invariant_version = invariant_version_override.unwrap_or({
        // MetaStablePools use specialization=1 → INVARIANT_V2 (roundUp `P_D`);
        // else INVARIANT_V1 (always-roundDown `D_P`). Mirrors
        // `resolve_invariant_version`.
        if decoded.specialization == 1 {
            2
        } else {
            1
        }
    });
    let mut base_sf = Vec::with_capacity(tokens.len());
    for token in &tokens {
        base_sf.push(compute_scaling_factor(io, *token, block).await?);
    }

    // getRateProviders reverts for WeightedPool2Tokens / MetaStablePools —
    // degrade to an empty list (mirrors the Python `except -> []`).
    let providers = match choreography::fetch_balancer_rate_providers(io, address, block).await {
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
                choreography::fetch_balancer_rate(io, *provider, block).await?
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

/// Assemble a V3 pool's tick map with **DB-first** coverage (the cross-task
/// capture in task `4GQWZ4`): a `TickMapDb` hit (both `tick_bitmap` AND
/// `tick_data` populated) yields [`PoolTickCoverage::Tracked`]; a DB miss or
/// empty map falls back to the Chain-arm single-word bootstrap →
/// [`PoolTickCoverage::Sparse`]. Mirrors `tick_assembly::assemble_v3_tick_map`'s
/// `Db → Chain` precedence but is written async-native (the builder runs on the
/// async registration runtime, so it cannot `block_on` the sync assemble
/// helper — the nested-block_on deadlock class).
///
/// # Errors
///
/// Returns [`PoolBuilderError::Db`] on a DB read failure or
/// [`PoolBuilderError::Rpc`] on a Chain-arm failure.
async fn assemble_db_or_chain_v3(
    db: Option<&dyn TickMapDb>,
    io: &ConstructionIo,
    address: Address,
    tick: i32,
    tick_spacing: i32,
    block: u64,
) -> Result<(HashMap<i32, TickInfo>, PoolTickCoverage), PoolBuilderError> {
    if let Some(db) = db {
        if let Some(map) = db.fetch_liquidity_map(address)? {
            if let Some(hit) = crate::bot_core::tick_assembly::liquidity_map_to_tick_info(map) {
                return Ok(hit); // (ticks, Tracked)
            }
        }
    }
    // DB miss / empty → Chain arm (Sparse).
    let (ticks, _) = bootstrap_v3_tick_map(io, address, tick, tick_spacing, block).await?;
    Ok((ticks, PoolTickCoverage::Sparse))
}

/// V4 twin of [`assemble_db_or_chain_v3`]: `TickMapDb.fetch_liquidity_map_v4`
/// hit → [`PoolTickCoverage::Tracked`]; miss → Chain-arm → Sparse.
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
) -> Result<(HashMap<i32, TickInfo>, PoolTickCoverage), PoolBuilderError> {
    if let Some(db) = db {
        if let Some(map) = db.fetch_liquidity_map_v4(pool_manager, B256::from(pool_id))? {
            if let Some(hit) = crate::bot_core::tick_assembly::liquidity_map_to_tick_info(map) {
                return Ok(hit); // (ticks, Tracked)
            }
        }
    }
    // DB miss / empty → Chain arm (Sparse).
    let (ticks, _) =
        bootstrap_v4_tick_map(io, state_view, pool_id, tick, tick_spacing, block).await?;
    Ok((ticks, PoolTickCoverage::Sparse))
}

/// Chain-arm single-word tick bootstrap over [`ConstructionIo`] — the V3
/// portion of `assemble_v3_tick_map`'s Chain arm, inlined over the generic RPC
/// primitive so a bare `ConstructionIo` (no `TickBootstrapRpc`/db) can seed a
/// Sparse pool. Mirrors `AlloyTickBootstrapRpc::bootstrap_v3_tick_word` and
/// the Python Branch 3 loop verbatim: compute the word, read `tickBitmap`, on a
/// non-zero bitmap enumerate the 256 set bits and read each tick's
/// `ticks(int24)` liquidity.
///
/// Coverage is always [`PoolTickCoverage::Sparse`] (only the single current
/// word is seeded), so the pool is **live immediately** (decision D4) and the
/// live-pump miss-detection backfills neighbour words during swap simulation.
async fn bootstrap_v3_tick_map(
    io: &ConstructionIo,
    address: Address,
    tick: i32,
    tick_spacing: i32,
    block: u64,
) -> Result<(HashMap<i32, TickInfo>, PoolTickCoverage), ProviderError> {
    let (word, _) =
        degenbot_concentrated_liquidity_math::liquidity_mapping::get_tick_word_and_bit_position(
            tick,
            tick_spacing,
        );
    #[expect(clippy::expect_used)] // invariant-guarded (documented)
    let word_i16 = i16::try_from(word).expect("V3 tick word fits in int16");
    // Best-effort single-word probe for a Sparse pool: an unreadable word
    // (decode error on a provider that can't serve `tickBitmap`) degrades to
    // an empty word → empty ticks → Sparse, preserving the legacy builder's
    // graceful fallback (`tick_data=None, coverage=sparse`). Only the
    // mis-shaped-response decode error is tolerated; genuine transport/network
    // failures still propagate.
    let bitmap = match choreography::fetch_tick_bitmap(io, address, word_i16, Some(block)).await {
        Ok(b) => b,
        Err(degenbot_core::errors::ProviderError::DecodingError { .. }) => U256::ZERO,
        Err(e) => return Err(e),
    };

    let mut ticks = HashMap::new();
    for i in 0..=255u8 {
        if bitmap.bit(i.into()) {
            let active_tick = ((word << 8) + i32::from(i)) * tick_spacing;
            let (liquidity_gross, liquidity_net) =
                choreography::fetch_tick_data(io, address, active_tick, Some(block)).await?;
            ticks.insert(
                active_tick,
                TickInfo {
                    liquidity_gross,
                    liquidity_net,
                    block,
                },
            );
        }
    }

    Ok((ticks, PoolTickCoverage::Sparse))
}

/// Assemble `build_curve_pool` params for a Curve `StableSwap` pool (the task
/// `4TPB35`, epic `TV72EG`, assembly twin of `CurvePoolBuilder.build`):
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
/// Returns [`PoolBuilderError::Rpc`] on an RPC/decode failure or
/// [`PoolBuilderError::Spec`] when the pool exposes fewer than 2 coins
/// (mirrors the `BrokenPool` minimum-tokens guard).
#[expect(clippy::too_many_lines)] // data-dense assembly (mirrors the Python builder)
pub async fn build_curve_pool(
    address: Address,
    registry_addresses: &[Address],
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RegisterCurvePoolParams, PoolBuilderError> {
    let coins = curve_choreography::discover_curve_coins(io, address, block).await;
    let params = curve_choreography::fetch_curve_pool_params(io, address, block).await?;
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
        return Err(PoolBuilderError::Spec);
    }

    // Per-coin ERC20 decimals — needed for the rate/precision multipliers and
    // the lending detection (the driver owns the companions, so we fetch here).
    let mut token_decimals = Vec::with_capacity(coins.token_addresses.len());
    for token in &coins.token_addresses {
        token_decimals.push(fetch_erc20_decimals(io, *token, block).await?);
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
    // Mirror `_compute_rate_and_precision_multipliers`: lending overrides →
    // `pm * 10**PRECISION_DECIMALS`; else from token decimals.
    let (rate_multipliers, precision_multipliers) = match &lending.precision_multipliers {
        Some(pms) => (pms.iter().map(|pm| *pm * one_e18).collect(), pms.clone()),
        None => (
            token_decimals
                .iter()
                .map(|d| U256::from(10u64).pow(U256::from(36u32 - u32::from(*d))))
                .collect(),
            token_decimals
                .iter()
                .map(|d| U256::from(10u64).pow(U256::from(18u32 - u32::from(*d))))
                .collect(),
        ),
    };

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
    io: &ConstructionIo,
    token: Address,
    block: Option<u64>,
) -> Result<u8, PoolBuilderError> {
    let calldata = choreography::selector(b"decimals()");
    let bytes = io.call(token, calldata.into(), block).await?;
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
/// Returns [`PoolBuilderError::Create2`] on a CREATE2 mismatch (when the
/// factory ships in the JSON) or an RPC/decode error.
pub async fn build_v3(
    chain_id: u64,
    address: Address,
    db: Option<&dyn TickMapDb>,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RegisterV3PoolParams, PoolBuilderError> {
    let imm = choreography::fetch_v3_immutable_data(io, address, block).await?;
    let (sqrt_price_x96, tick_i, liquidity) =
        choreography::fetch_v3_slot0_liquidity(io, address, block).await?;
    let tick = i32::try_from(tick_i).unwrap_or(0);
    let update_block = block.unwrap_or(0);

    let (tick_data, coverage) =
        assemble_db_or_chain_v3(db, io, address, tick, imm.tick_spacing, update_block).await?;
    // Two-stamp OB7UNY (task 4TWM7C / regression 8c50e0cd): the PRICE clock
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

    deployments::verify_v3_pool_address(
        chain_id,
        imm.factory,
        address,
        imm.token0,
        imm.token1,
        imm.fee,
    )
    .map_err(|_| PoolBuilderError::Create2)?;

    let deployer = deployments::resolve_deployer(chain_id, imm.factory);
    let init_hash: B256 = deployments::resolve_v3_init_hash(chain_id, imm.factory);

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
    })
}

/// Chain-arm single-word tick bootstrap over [`ConstructionIo`] for a V4 pool
/// identified by (`pool_manager`, `pool_id`) — the V4 twin of
/// [`bootstrap_v3_tick_map`]. Reads the bitmap via `getTickBitmap(bytes32,
/// int16)` and each set tick's liquidity via `getTickLiquidity(bytes32,
/// int24)` on the `pool_manager` (which exposes the same state-view getters);
/// coverage is always [`PoolTickCoverage::Sparse`] so the pool is live
/// immediately (D4).
async fn bootstrap_v4_tick_map(
    io: &ConstructionIo,
    state_view: Address,
    pool_id: [u8; 32],
    tick: i32,
    tick_spacing: i32,
    block: u64,
) -> Result<(HashMap<i32, TickInfo>, PoolTickCoverage), ProviderError> {
    let (word, _) =
        degenbot_concentrated_liquidity_math::liquidity_mapping::get_tick_word_and_bit_position(
            tick,
            tick_spacing,
        );
    #[expect(clippy::expect_used)] // invariant-guarded (documented)
    let word_i16 = i16::try_from(word).expect("V4 tick word fits in int16");
    // Same best-effort single-word probe + graceful decode-error degradation as
    // the V3 arm (`bootstrap_v3_tick_map`): an unreadable word → empty/Sparse.
    let bitmap =
        match choreography::fetch_v4_tick_bitmap(io, state_view, pool_id, word_i16, Some(block))
            .await
        {
            Ok(b) => b,
            Err(degenbot_core::errors::ProviderError::DecodingError { .. }) => U256::ZERO,
            Err(e) => return Err(e),
        };

    let mut ticks = HashMap::new();
    for i in 0..=255u8 {
        if bitmap.bit(i.into()) {
            let active_tick = ((word << 8) + i32::from(i)) * tick_spacing;
            let (liquidity_gross, liquidity_net) =
                choreography::fetch_v4_tick_data(io, state_view, pool_id, active_tick, Some(block))
                    .await?;
            ticks.insert(
                active_tick,
                TickInfo {
                    liquidity_gross,
                    liquidity_net,
                    block,
                },
            );
        }
    }

    Ok((ticks, PoolTickCoverage::Sparse))
}

/// Caller-supplied V4 pool identity (mirrors the `register_v4_pool` argument
/// set — the core never reads these on-chain; hook filtering is already applied
/// via `hook_flags`). Bundled so `build_v4` stays under `clippy::too_many_arguments`
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
    /// Pre-decoded hook-flags bitmask (`& 0xCC != 0` rejected at admission).
    pub hook_flags: u16,
}

/// The complete result of a V4 pool build: the registration params plus the
/// LP-fee pip decoded from the same head-stamped slot0 read.
///
/// CDJEPJ-1: ``lp_fee`` is exposed here (rather than bloat
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
        choreography::fetch_v4_slot0_liquidity(io, id.state_view, id.pool_id, block).await?;
    let protocol_fee = protocol_fee_u.to::<u32>();
    let lp_fee = lp_fee_u.to::<u32>();
    let tick = i32::try_from(tick_i).unwrap_or(0);
    let update_block = block.unwrap_or(0);

    let (tick_data, coverage) = assemble_db_or_chain_v4(
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
    // Two-stamp OB7UNY (V4 twin of build_v3): a DB-seeded (`Tracked`) pool's
    // liquidity clock is its DB `liquidity_update_block`, not the head price
    // clock (task 4TWM7C / regression 8c50e0cd).
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
                hooks: Address::ZERO,
            },
            hook_flags: id.hook_flags,
            protocol_fee,
            sqrt_price_x96,
            liquidity: liquidity_u.to::<u128>(),
            tick,
            tick_data,
            update_block,
            tick_data_block: Some(tick_data_block),
            coverage,
            fetcher: None,
        },
        lp_fee,
    })
}

/// Caller-supplied V4 identity **overrides** (the DB-resolution fallback, Task
/// TF7RZB-S3). Every field is optional because the core prefers the DB two-step
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

/// Resolve the V4 identity **core-side** (TF7RZB-S3): the DB two-step
/// (manager → V4 row → per-FK token rows) first, else the caller-supplied
/// [`V4PoolBuildOverrides`]. Returns the resolved [`V4PoolBuildIdentity`].
///
/// (The DB `liquidity_update_block` plumbing was removed in task 4TWM7C's
/// cleanup — the Rust `build_v4` now reads it itself via
/// `TickMapDb::fetch_liquidity_update_block_v4` to stamp the liquidity clock;
/// the driver no longer needs it.)
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
            hook_flags: derive_hook_flags(v4_row.hooks),
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
        hook_flags: derive_hook_flags(overrides.hook_address.unwrap_or_default()),
    })
}

/// Derive the V4 `hook_flags` bitmask from a hook contract address. The
/// practical value is `0` (no-hook pools are the only ones the solver
/// admits); a real hook address contributes its low 16 bits, mirroring the
/// retired Python driver's `int(hook_address, 16)` with no overflow for
/// 20-byte addresses.
fn derive_hook_flags(hook_address: Address) -> u16 {
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
