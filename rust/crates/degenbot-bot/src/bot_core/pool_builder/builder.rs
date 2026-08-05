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

use alloy::primitives::{Address, B256, U256};
use degenbot_core::errors::ProviderError;
use degenbot_db::error::DbError;
use degenbot_db::snapshot::TickMapDb;
use degenbot_pools::aerodrome_v2_state::RegisterAerodromeV2PoolParams;
use degenbot_pools::spec_bounds;
use degenbot_pools::v2_state::RegisterV2PoolParams;
use degenbot_pools::v3_state::RegisterV3PoolParams;
use degenbot_pools::v4_state::{RegisterV4PoolParams, V4PoolKey};
use degenbot_uniswap::deployments;
use degenbot_uniswap::dex_identity::{self, DexIdentity, DexVariant};

use super::choreography::{self};
use crate::bot_core::construction_io::ConstructionIo;
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
    #[error("CREATE2 address verification failed")]
    Create2,
    #[error("DB read failed: {0}")]
    Db(#[from] DbError),
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

    Ok(RegisterAerodromeV2PoolParams {
        address,
        token0: imm.token0,
        token1: imm.token1,
        factory: imm.factory,
        variant: id.variant,
        stable: common.stable,
        fee: (common.fee_bps, 10_000),
        reserve0,
        reserve1,
        update_block: block.unwrap_or(0),
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
#[allow(clippy::too_many_arguments)]
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
    let (word, _) = degenbot_cl_math::cl_lib::liquidity_mapping::get_tick_word_and_bit_position(
        tick,
        tick_spacing,
    );
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
        tick_data_block: Some(update_block),
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
    let (word, _) = degenbot_cl_math::cl_lib::liquidity_mapping::get_tick_word_and_bit_position(
        tick,
        tick_spacing,
    );
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
) -> Result<RegisterV4PoolParams, PoolBuilderError> {
    let (sqrt_price_x96, tick_i, protocol_fee_u, _lp_fee_u, liquidity_u) =
        choreography::fetch_v4_slot0_liquidity(io, id.state_view, id.pool_id, block).await?;
    let protocol_fee = protocol_fee_u.to::<u32>();
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

    Ok(RegisterV4PoolParams {
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
        tick_data_block: Some(update_block),
        coverage,
        fetcher: None,
    })
}
