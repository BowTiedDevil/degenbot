//! Diagnostic state snapshots for the mixed Uniswap engine.
//!
//! This module provides a read-only view of the engine's current pool state
//! for a registered path. It is intended for debugging simulation failures
//! and comparing engine state against on-chain state. All access is
//! synchronous and does not mutate engine state.

use alloy::dyn_abi::{DynSolType, DynSolValue};
use alloy::primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};

use super::{HopType, UniswapEngine};


/// A snapshot of a single pool's state, formatted for diagnostics.
///
/// Numeric values are stored as hex strings so the output is
/// JSON-serializable without custom `U256` serializers and is easy to
/// compare against RPC results.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "pool_family")]
#[allow(clippy::module_name_repetitions)]
pub enum DiagnosticPoolState {
    /// V2 constant-product pool state.
    V2 {
        address: String,
        /// Reserve of the input token, accounting for the path direction.
        reserve_in: String,
        /// Reserve of the output token, accounting for the path direction.
        reserve_out: String,
        /// Fee denominator (e.g. 1000 for 0.3%).
        fee_denom: String,
        /// Gamma numerator — retained fraction after fees (e.g. 997).
        gamma_numer: String,
    },
    /// V3 concentrated-liquidity pool state.
    V3 {
        address: String,
        token0: String,
        token1: String,
        fee: u32,
        tick_spacing: i32,
        sqrt_price_x96: String,
        tick: i32,
        liquidity: String,
    },
    /// V4 concentrated-liquidity pool state.
    V4 {
        pool_manager: String,
        pool_id: String,
        currency0: String,
        currency1: String,
        fee: u32,
        tick_spacing: i32,
        hook_flags: u16,
        hooks: String,
        sqrt_price_x96: String,
        tick: i32,
        liquidity: String,
    },
}

/// A single hop inside a diagnostic path snapshot.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosticHop {
    /// Zero-based position of the hop in the path.
    pub position: usize,
    /// Short hop-type tag: "V2", "V3", or "V4".
    pub hop_type: String,
    /// Whether the hop is zero-for-one in the path direction.
    pub zero_for_one: bool,
    /// Engine-owned state captured under the engine lock.
    pub engine_state: DiagnosticPoolState,
    /// On-chain state fetched after the lock was released (`None` until
    /// Slice 3 implements RPC comparison).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onchain_state: Option<DiagnosticPoolState>,
    /// Human-readable field-level differences between engine and chain.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diff: Vec<String>,
}

impl Default for DiagnosticHop {
    fn default() -> Self {
        Self {
            position: 0,
            hop_type: String::new(),
            zero_for_one: false,
            engine_state: DiagnosticPoolState::V2 {
                address: String::new(),
                reserve_in: String::new(),
                reserve_out: String::new(),
                fee_denom: String::new(),
                gamma_numer: String::new(),
            },
            onchain_state: None,
            diff: Vec::new(),
        }
    }
}

/// Diagnostic snapshot for a single registered mixed path.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiagnosticPathState {
    /// Rust path ID.
    pub path_id: u64,
    /// Path type string, e.g. "V2-V3".
    pub path_type: String,
    /// Block number associated with the engine results when the snapshot
    /// was taken. Falls back to `last_processed_block` if no results yet.
    pub solve_block: Option<u64>,
    /// Block number at which on-chain state was fetched, if a fetch was
    /// attempted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onchain_block: Option<u64>,
    /// The hop snapshots.
    pub hops: Vec<DiagnosticHop>,
    /// Call data that produced the revert, if captured from a sim failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_calldata: Option<String>,
    /// Revert selector or decoded reason, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert_info: Option<String>,
}

impl DiagnosticPathState {
    /// Create a new diagnostic snapshot for a path.
    #[must_use]
    pub fn new(path_id: u64, solve_block: Option<u64>) -> Self {
        Self {
            path_id,
            path_type: String::new(),
            solve_block,
            onchain_block: None,
            hops: Vec::new(),
            failed_calldata: None,
            revert_info: None,
        }
    }

    /// Fetch on-chain state for every hop and populate `onchain_state` and
    /// `diff`.
    ///
    /// This is a best-effort operation. If an individual hop's RPC call or
    /// decoding fails, the error is recorded in that hop's `diff` and the
    /// rest of the hops are still processed.
    ///
    /// # Errors
    ///
    /// Returns a provider error only if the underlying provider call fails in a
    /// way that prevents making any further calls (e.g., connection loss).
    pub async fn fetch_onchain(
        &mut self,
        provider: &crate::provider::AlloyProvider,
        state_view: Option<Address>,
    ) -> Result<(), crate::errors::ProviderError> {
        let block_number = self.solve_block;
        self.onchain_block = block_number;

        for hop in &mut self.hops {
            let result = match &hop.engine_state {
                DiagnosticPoolState::V2 { address, reserve_in, reserve_out, fee_denom, gamma_numer } => {
                    fetch_v2_onchain(provider, block_number, hop.zero_for_one, address, reserve_in, reserve_out, fee_denom, gamma_numer).await
                }
                DiagnosticPoolState::V3 { address, token0, token1, fee, tick_spacing, sqrt_price_x96, tick, liquidity } => {
                    fetch_v3_onchain(provider, block_number, address, token0, token1, *fee, *tick_spacing, sqrt_price_x96, *tick, liquidity).await
                }
                DiagnosticPoolState::V4 { pool_manager, pool_id, currency0, currency1, fee, tick_spacing, hook_flags, hooks, sqrt_price_x96, tick, liquidity } => {
                    if let Some(sv) = state_view {
                        fetch_v4_onchain(provider, block_number, sv, pool_manager, pool_id, currency0, currency1, *fee, *tick_spacing, *hook_flags, hooks, sqrt_price_x96, *tick, liquidity).await
                    } else {
                        Ok(FetchOutcome {
                            onchain_state: None,
                            diffs: vec!["V4 on-chain fetch skipped: no StateView address provided".to_string()],
                        })
                    }
                }
            };

            match result {
                Ok(outcome) => {
                    hop.onchain_state = outcome.onchain_state;
                    hop.diff.extend(outcome.diffs);
                }
                Err(e) => {
                    hop.diff.push(format!("on-chain fetch failed: {e}"));
                }
            }
        }

        Ok(())
    }
}

/// Result of fetching on-chain state for a single hop.
struct FetchOutcome {
    onchain_state: Option<DiagnosticPoolState>,
    diffs: Vec<String>,
}

impl FetchOutcome {
    fn new(onchain_state: DiagnosticPoolState) -> Self {
        Self {
            onchain_state: Some(onchain_state),
            diffs: Vec::new(),
        }
    }

    fn record_diff(&mut self, msg: String) {
        self.diffs.push(msg);
    }
}

// ---------------------------------------------------------------------------
// ABI helpers
// ---------------------------------------------------------------------------

/// Compute the first 4 bytes of `keccak256(signature)`.
fn fn_selector(signature: &str) -> [u8; 4] {
    let hash = alloy::primitives::keccak256(signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

/// Build calldata = selector + ABI-encoded params.
fn encode_call(selector: [u8; 4], params: &[DynSolValue]) -> Bytes {
    let mut out = Vec::with_capacity(4 + 32 * params.len());
    out.extend_from_slice(&selector);
    for param in params {
        out.extend_from_slice(&param.abi_encode());
    }
    Bytes::from(out)
}

/// Parse a `0x`-prefixed hex string into a `U256`.
fn parse_hex_u256(s: &str) -> Option<U256> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    U256::from_str_radix(s, 16).ok()
}

/// Parse a `0x`-prefixed hex string into a `u128`.
fn parse_hex_u128(s: &str) -> Option<u128> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    u128::from_str_radix(s, 16).ok()
}

/// Extract a `U256` from an ABI-decoded uint value with the expected bit width.
fn uint_value(value: &DynSolValue, expected_bits: usize) -> Option<U256> {
    let (v, bits) = value.as_uint()?;
    if bits == expected_bits || bits == 256 {
        Some(v)
    } else {
        None
    }
}

/// Extract an `i32` from an ABI-decoded int value with the expected bit width.
fn int_value_to_i32(value: &DynSolValue, expected_bits: usize) -> Option<i32> {
    let (v, bits) = value.as_int()?;
    if bits != expected_bits && bits != 256 {
        return None;
    }
    v.try_into().ok()
}

// ---------------------------------------------------------------------------
// Per-family on-chain fetch
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn fetch_v2_onchain(
    provider: &crate::provider::AlloyProvider,
    block_number: Option<u64>,
    zero_for_one: bool,
    address: &str,
    engine_reserve_in: &str,
    engine_reserve_out: &str,
    fee_denom: &str,
    gamma_numer: &str,
) -> Result<FetchOutcome, crate::errors::ProviderError> {
    let pool_address: Address = address.parse().map_err(|e| {
        crate::errors::ProviderError::Other {
            message: format!("invalid V2 pool address {address}: {e}"),
        }
    })?;

    let selector = fn_selector("getReserves()");
    let calldata = encode_call(selector, &[]);
    let raw = provider.eth_call(&pool_address, calldata, block_number).await?;

    let return_type = DynSolType::Tuple(vec![
        DynSolType::Uint(112),
        DynSolType::Uint(112),
        DynSolType::Uint(32),
    ]);
    let decoded = return_type.abi_decode(&raw).map_err(|e| {
        crate::errors::ProviderError::SerializationError {
            message: format!("failed to decode getReserves result: {e}"),
        }
    })?;

    let DynSolValue::Tuple(values) = decoded else {
        return Err(crate::errors::ProviderError::SerializationError {
            message: "getReserves result is not a tuple".to_string(),
        });
    };

    let reserve0 = uint_value(&values[0], 112).ok_or_else(|| crate::errors::ProviderError::SerializationError {
        message: "getReserves reserve0 decode mismatch".to_string(),
    })?;
    let reserve1 = uint_value(&values[1], 112).ok_or_else(|| crate::errors::ProviderError::SerializationError {
        message: "getReserves reserve1 decode mismatch".to_string(),
    })?;

    let (chain_reserve_in, chain_reserve_out) = if zero_for_one {
        (reserve0, reserve1)
    } else {
        (reserve1, reserve0)
    };

    let mut outcome = FetchOutcome::new(DiagnosticPoolState::V2 {
        address: address.to_string(),
        reserve_in: u256_to_hex(chain_reserve_in),
        reserve_out: u256_to_hex(chain_reserve_out),
        fee_denom: fee_denom.to_string(),
        gamma_numer: gamma_numer.to_string(),
    });

    let engine_r_in = parse_hex_u256(engine_reserve_in);
    let engine_r_out = parse_hex_u256(engine_reserve_out);

    if engine_r_in != Some(chain_reserve_in) {
        outcome.record_diff(format!(
            "reserve_in: engine={engine_reserve_in}, onchain={}",
            u256_to_hex(chain_reserve_in)
        ));
    }
    if engine_r_out != Some(chain_reserve_out) {
        outcome.record_diff(format!(
            "reserve_out: engine={engine_reserve_out}, onchain={}",
            u256_to_hex(chain_reserve_out)
        ));
    }

    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
async fn fetch_v3_onchain(
    provider: &crate::provider::AlloyProvider,
    block_number: Option<u64>,
    address: &str,
    token0: &str,
    token1: &str,
    fee: u32,
    tick_spacing: i32,
    engine_sqrt_price_x96: &str,
    engine_tick: i32,
    engine_liquidity: &str,
) -> Result<FetchOutcome, crate::errors::ProviderError> {
    let pool_address: Address = address.parse().map_err(|e| {
        crate::errors::ProviderError::Other {
            message: format!("invalid V3 pool address {address}: {e}"),
        }
    })?;

    // slot0() -> (sqrtPriceX96, tick, observationIndex, observationCardinality, observationCardinalityNext, feeProtocol, unlocked)
    let slot0_selector = fn_selector("slot0()");
    let raw_slot0 = provider
        .eth_call(&pool_address, encode_call(slot0_selector, &[]), block_number)
        .await?;
    let slot0_type = DynSolType::Tuple(vec![
        DynSolType::Uint(160),
        DynSolType::Int(24),
        DynSolType::Uint(16),
        DynSolType::Uint(16),
        DynSolType::Uint(16),
        DynSolType::Uint(8),
        DynSolType::Bool,
    ]);
    let decoded_slot0 = slot0_type.abi_decode(&raw_slot0).map_err(|e| {
        crate::errors::ProviderError::SerializationError {
            message: format!("failed to decode slot0 result: {e}"),
        }
    })?;
    let DynSolValue::Tuple(slot0_values) = decoded_slot0 else {
        return Err(crate::errors::ProviderError::SerializationError {
            message: "slot0 result is not a tuple".to_string(),
        });
    };

    let chain_sqrt_price_x96 = uint_value(&slot0_values[0], 160).ok_or_else(|| {
        crate::errors::ProviderError::SerializationError {
            message: "slot0 sqrtPriceX96 decode mismatch".to_string(),
        }
    })?;
    let chain_tick = int_value_to_i32(&slot0_values[1], 24).ok_or_else(|| {
        crate::errors::ProviderError::SerializationError {
            message: "slot0 tick decode mismatch".to_string(),
        }
    })?;

    // liquidity() -> uint128
    let liquidity_selector = fn_selector("liquidity()");
    let raw_liquidity = provider
        .eth_call(&pool_address, encode_call(liquidity_selector, &[]), block_number)
        .await?;
    let liquidity_type = DynSolType::Uint(128);
    let decoded_liquidity = liquidity_type.abi_decode(&raw_liquidity).map_err(|e| {
        crate::errors::ProviderError::SerializationError {
            message: format!("failed to decode liquidity result: {e}"),
        }
    })?;
    let chain_liquidity = uint_value(&decoded_liquidity, 128)
        .ok_or_else(|| crate::errors::ProviderError::SerializationError {
            message: "liquidity decode mismatch".to_string(),
        })?
        .to::<u128>();

    let mut outcome = FetchOutcome::new(DiagnosticPoolState::V3 {
        address: address.to_string(),
        token0: token0.to_string(),
        token1: token1.to_string(),
        fee,
        tick_spacing,
        sqrt_price_x96: u256_to_hex(chain_sqrt_price_x96),
        tick: chain_tick,
        liquidity: u128_to_hex(chain_liquidity),
    });

    if parse_hex_u256(engine_sqrt_price_x96) != Some(chain_sqrt_price_x96) {
        outcome.record_diff(format!(
            "sqrt_price_x96: engine={engine_sqrt_price_x96}, onchain={}",
            u256_to_hex(chain_sqrt_price_x96)
        ));
    }
    if engine_tick != chain_tick {
        outcome.record_diff(format!(
            "tick: engine={engine_tick}, onchain={chain_tick}"
        ));
    }
    if parse_hex_u128(engine_liquidity) != Some(chain_liquidity) {
        outcome.record_diff(format!(
            "liquidity: engine={engine_liquidity}, onchain={}",
            u128_to_hex(chain_liquidity)
        ));
    }

    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
async fn fetch_v4_onchain(
    provider: &crate::provider::AlloyProvider,
    block_number: Option<u64>,
    state_view: Address,
    pool_manager: &str,
    pool_id: &str,
    currency0: &str,
    currency1: &str,
    fee: u32,
    tick_spacing: i32,
    hook_flags: u16,
    hooks: &str,
    engine_sqrt_price_x96: &str,
    engine_tick: i32,
    engine_liquidity: &str,
) -> Result<FetchOutcome, crate::errors::ProviderError> {
    let pool_id_bytes = crate::hex_utils::decode_32byte_hex(pool_id).map_err(|e| {
        crate::errors::ProviderError::Other {
            message: format!("invalid V4 pool_id {pool_id}: {e}"),
        }
    })?;

    let pool_id_value = DynSolValue::FixedBytes(B256::from(pool_id_bytes), 32);

    // StateView.getSlot0(bytes32 poolId) -> (uint160 sqrtPriceX96, int24 tick, uint24 protocolFee, uint24 swapFee)
    let slot0_selector = fn_selector("getSlot0(bytes32)");
    let raw_slot0 = provider
        .eth_call(&state_view, encode_call(slot0_selector, std::slice::from_ref(&pool_id_value)), block_number)
        .await?;
    let slot0_type = DynSolType::Tuple(vec![
        DynSolType::Uint(160),
        DynSolType::Int(24),
        DynSolType::Uint(24),
        DynSolType::Uint(24),
    ]);
    let decoded_slot0 = slot0_type.abi_decode(&raw_slot0).map_err(|e| {
        crate::errors::ProviderError::SerializationError {
            message: format!("failed to decode StateView.getSlot0 result: {e}"),
        }
    })?;
    let DynSolValue::Tuple(slot0_values) = decoded_slot0 else {
        return Err(crate::errors::ProviderError::SerializationError {
            message: "StateView.getSlot0 result is not a tuple".to_string(),
        });
    };

    let chain_sqrt_price_x96 = uint_value(&slot0_values[0], 160).ok_or_else(|| {
        crate::errors::ProviderError::SerializationError {
            message: "getSlot0 sqrtPriceX96 decode mismatch".to_string(),
        }
    })?;
    let chain_tick = int_value_to_i32(&slot0_values[1], 24).ok_or_else(|| {
        crate::errors::ProviderError::SerializationError {
            message: "getSlot0 tick decode mismatch".to_string(),
        }
    })?;

    // StateView.getLiquidity(bytes32 poolId) -> uint128
    let liquidity_selector = fn_selector("getLiquidity(bytes32)");
    let raw_liquidity = provider
        .eth_call(&state_view, encode_call(liquidity_selector, &[pool_id_value]), block_number)
        .await?;
    let liquidity_type = DynSolType::Uint(128);
    let decoded_liquidity = liquidity_type.abi_decode(&raw_liquidity).map_err(|e| {
        crate::errors::ProviderError::SerializationError {
            message: format!("failed to decode StateView.getLiquidity result: {e}"),
        }
    })?;
    let chain_liquidity = uint_value(&decoded_liquidity, 128)
        .ok_or_else(|| crate::errors::ProviderError::SerializationError {
            message: "getLiquidity decode mismatch".to_string(),
        })?
        .to::<u128>();

    let mut outcome = FetchOutcome::new(DiagnosticPoolState::V4 {
        pool_manager: pool_manager.to_string(),
        pool_id: pool_id.to_string(),
        currency0: currency0.to_string(),
        currency1: currency1.to_string(),
        fee,
        tick_spacing,
        hook_flags,
        hooks: hooks.to_string(),
        sqrt_price_x96: u256_to_hex(chain_sqrt_price_x96),
        tick: chain_tick,
        liquidity: u128_to_hex(chain_liquidity),
    });

    if parse_hex_u256(engine_sqrt_price_x96) != Some(chain_sqrt_price_x96) {
        outcome.record_diff(format!(
            "sqrt_price_x96: engine={engine_sqrt_price_x96}, onchain={}",
            u256_to_hex(chain_sqrt_price_x96)
        ));
    }
    if engine_tick != chain_tick {
        outcome.record_diff(format!("tick: engine={engine_tick}, onchain={chain_tick}"));
    }
    if parse_hex_u128(engine_liquidity) != Some(chain_liquidity) {
        outcome.record_diff(format!(
            "liquidity: engine={engine_liquidity}, onchain={}",
            u128_to_hex(chain_liquidity)
        ));
    }

    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn fmt_addr(addr: Address) -> String {
    addr.to_checksum(None)
}

fn fmt_u256(value: U256) -> String {
    format!("0x{value:x}")
}

fn u256_to_hex(value: U256) -> String {
    format!("0x{value:x}")
}

fn u128_to_hex(value: u128) -> String {
    format!("0x{value:x}")
}

impl UniswapEngine {
    /// Snapshot the engine-owned state for every hop in `path_id`.
    ///
    /// This method acquires the engine lock only long enough to copy the
    /// immutable pool refs and the current scalar state from each sub-engine.
    /// No RPC calls are made here.
    ///
    /// Returns `None` if the path is not registered.
    #[must_use]
    pub fn diagnostic_path_state(&self, path_id: u64) -> Option<DiagnosticPathState> {
        let path = self.path_pools.get(&path_id)?;
        let solve_block = if self.results_block > 0 {
            Some(self.results_block)
        } else {
            self.last_processed_block
        };

        let mut snapshot = DiagnosticPathState::new(path_id, solve_block);

        let type_tags: Vec<&str> = path
            .pools
            .iter()
            .map(|r| match r.hop_type {
                HopType::V2 => "V2",
                HopType::V3 => "V3",
                HopType::V4 => "V4",
            })
            .collect();
        snapshot.path_type = type_tags.join("-");

        for (position, pool_ref) in path.pools.iter().enumerate() {
            let engine_state = match pool_ref.hop_type {
                HopType::V2 => self.v2_engine.diagnostic_pool_state(pool_ref.pool_key),
                HopType::V3 => self.v3_engine.diagnostic_pool_state(pool_ref.pool_key),
                HopType::V4 => self.v4_engine.diagnostic_pool_state(pool_ref.pool_key),
            };
            let Some(engine_state) = engine_state else {
                // Pool referenced by the path is missing from the sub-engine.
                // This is itself a diagnostic signal; record a placeholder
                // and continue so the rest of the hops are still visible.
                snapshot.hops.push(DiagnosticHop {
                    position,
                    hop_type: type_tags[position].to_string(),
                    zero_for_one: pool_ref.zero_for_one,
                    engine_state: DiagnosticPoolState::V2 {
                        address: "0x0000000000000000000000000000000000000000".to_string(),
                        reserve_in: "0x0".to_string(),
                        reserve_out: "0x0".to_string(),
                        fee_denom: "0x0".to_string(),
                        gamma_numer: "0x0".to_string(),
                    },
                    onchain_state: None,
                    diff: vec![format!("missing pool_key={} in engine", pool_ref.pool_key)],
                });
                continue;
            };

            snapshot.hops.push(DiagnosticHop {
                position,
                hop_type: type_tags[position].to_string(),
                zero_for_one: pool_ref.zero_for_one,
                engine_state,
                onchain_state: None,
                diff: Vec::new(),
            });
        }

        Some(snapshot)
    }
}

// ---------------------------------------------------------------------------
// Sub-engine accessors
// ---------------------------------------------------------------------------

impl crate::optimizers::v2_block_engine::V2BlockEngine {
    /// Return the diagnostic state for the oriented pool identified by
    /// `pool_key`.
    #[must_use]
    pub fn diagnostic_pool_state(&self, pool_key: u64) -> Option<DiagnosticPoolState> {
        let state = self.get_pool(pool_key)?;
        let address = self.pool_address_for_key(pool_key)?;
        Some(DiagnosticPoolState::V2 {
            address: fmt_addr(address),
            reserve_in: fmt_u256(state.reserve_in),
            reserve_out: fmt_u256(state.reserve_out),
            fee_denom: format!("0x{:x}", state.fee_denom),
            gamma_numer: format!("0x{:x}", state.gamma_numer),
        })
    }

    /// Find the contract address associated with a forward or reverse pool key.
    fn pool_address_for_key(&self, pool_key: u64) -> Option<Address> {
        self.pool_addresses()
            .iter()
            .find(|(_, (fwd, rev))| *fwd == pool_key || *rev == pool_key)
            .map(|(addr, _)| *addr)
    }
}

impl crate::optimizers::v3_block_engine::V3BlockEngine {
    /// Return the diagnostic state for the pool identified by `pool_key`.
    #[must_use]
    pub fn diagnostic_pool_state(&self, pool_key: u64) -> Option<DiagnosticPoolState> {
        let state = self.get_pool(pool_key)?;
        Some(DiagnosticPoolState::V3 {
            address: fmt_addr(state.address),
            token0: fmt_addr(state.token0),
            token1: fmt_addr(state.token1),
            fee: state.fee,
            tick_spacing: state.tick_spacing,
            sqrt_price_x96: fmt_u256(state.sqrt_price_x96),
            tick: state.tick,
            liquidity: format!("0x{:x}", state.liquidity),
        })
    }
}

impl crate::optimizers::v4_block_engine::V4BlockEngine {
    /// Return the diagnostic state for the pool identified by `pool_key`.
    #[must_use]
    pub fn diagnostic_pool_state(&self, pool_key: u64) -> Option<DiagnosticPoolState> {
        let state = self.get_pool(pool_key)?;
        Some(DiagnosticPoolState::V4 {
            pool_manager: fmt_addr(state.pool_manager),
            pool_id: format!("0x{}", alloy::hex::encode(state.pool_id)),
            currency0: state.pool_key.currency0.to_checksum(None),
            currency1: state.pool_key.currency1.to_checksum(None),
            fee: state.pool_key.fee,
            tick_spacing: state.pool_key.tick_spacing,
            hook_flags: 0, // V4PoolState does not store hook_flags directly
            hooks: state.pool_key.hooks.to_checksum(None),
            sqrt_price_x96: fmt_u256(state.sqrt_price_x96),
            tick: state.tick,
            liquidity: format!("0x{:x}", state.liquidity),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use alloy::primitives::{Address, U256};

    use crate::optimizers::uniswap_engine::{
        DiagnosticPathState, HopType, MixedPoolRef, PoolTickCoverage, UniswapEngine,
    };
    use crate::optimizers::v3_block_engine::RegisterV3PoolParams as V3Params;
    use crate::optimizers::v4_block_engine::{RegisterV4PoolParams as V4Params, V4PoolKey};

    fn usdc(amount: u64) -> U256 {
        U256::from(amount) * U256::from(10u64).pow(U256::from(6))
    }

    fn weth(amount: u64) -> U256 {
        U256::from(amount) * U256::from(10u64).pow(U256::from(18))
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn diagnostic_path_state_captures_mixed_hops() {
        let mut engine = UniswapEngine::new();

        // V2 pool
        let v2_fwd = engine.v2_engine().register_pool(
            Address::from([0x11u8; 20]),
            usdc(1_500_000),
            weth(800),
            997,
            1000,
        );

        // V3 pool
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(300),
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        let v3_key = engine.v3_engine().register_pool(V3Params {
            address: Address::from([0x22u8; 20]),
            token0: Address::from([0u8; 20]),
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            coverage: PoolTickCoverage::Tracked,
        });

        // V4 pool
        let mut v4_tick_data = HashMap::new();
        v4_tick_data.insert(
            10,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(400),
                liquidity_net: alloy::primitives::I256::try_from(200i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        let v4_fwd = engine
            .v4_engine()
            .register_pool(V4Params {
                pool_manager: Address::from([0x33u8; 20]),
                pool_id: [0x44u8; 32],
                pool_key: V4PoolKey {
                    currency0: Address::from([2u8; 20]),
                    currency1: Address::from([3u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
                liquidity: 2_000_000,
                tick: 0,
                tick_data: v4_tick_data,
                update_block: 0,
                coverage: PoolTickCoverage::Tracked,
            })
            .expect("V4 registration failed");

        // Mixed V2 -> V3 -> V4 path
        let path_id = engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: false,
            },
            MixedPoolRef {
                hop_type: HopType::V4,
                pool_key: v4_fwd,
                zero_for_one: true,
            },
        ]);

        let snapshot = engine
            .diagnostic_path_state(path_id)
            .expect("path should exist");

        assert_eq!(snapshot.path_id, path_id);
        assert_eq!(snapshot.path_type, "V2-V3-V4");
        assert_eq!(snapshot.hops.len(), 3);

        // V2 hop
        assert!(matches!(
            snapshot.hops[0].engine_state,
            super::DiagnosticPoolState::V2 { .. }
        ));

        // V3 hop
        if let super::DiagnosticPoolState::V3 { fee, .. } = snapshot.hops[1].engine_state {
            assert_eq!(fee, 3000);
        } else {
            panic!("expected V3 state");
        }

        // V4 hop
        if let super::DiagnosticPoolState::V4 { fee, .. } = snapshot.hops[2].engine_state {
            assert_eq!(fee, 500);
        } else {
            panic!("expected V4 state");
        }

        // Round-trip through JSON should succeed.
        let json = serde_json::to_string(&snapshot).expect("snapshot should serialize");
        let _parsed: DiagnosticPathState =
            serde_json::from_str(&json).expect("snapshot should deserialize");
    }

    #[test]
    fn diagnostic_path_state_returns_none_for_unknown_path() {
        let engine = UniswapEngine::new();
        assert!(engine.diagnostic_path_state(1234).is_none());
    }
}
