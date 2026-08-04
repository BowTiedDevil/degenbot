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

use alloy::primitives::{Address, B256};
use degenbot_core::errors::ProviderError;
use degenbot_pools::spec_bounds;
use degenbot_pools::v2_state::RegisterV2PoolParams;
use degenbot_uniswap::deployments;
use degenbot_uniswap::dex_identity::{self, DexIdentity, DexVariant};

use super::choreography::{self};
use crate::bot_core::construction_io::ConstructionIo;

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
    #[error("CREATE2 address verification failed")]
    Create2,
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
