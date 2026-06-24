//! Diagnostic state snapshots for the mixed Uniswap engine.
//!
//! This module provides a read-only view of the engine's current pool state
//! for a registered path. It is intended for debugging simulation failures
//! and comparing engine state against on-chain state. All access is
//! synchronous and does not mutate engine state.

use alloy::dyn_abi::{DynSolType, DynSolValue};
use alloy::primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};

use super::{HopType, MixedPoolRef, UniswapEngine};

/// A single typed field-level divergence between the engine's view of a pool
/// and the on-chain snapshot (PCG2M3). The string rendering is the single
/// source of truth for the legacy human-readable `DiagnosticHop::diff` entry:
/// `to_diff_string()` reproduces the exact `"<field>: engine=<v>, onchain=<v>"`
/// shape callers and tests already depend on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDiff {
    /// Field name (e.g. `reserve_in`, `sqrt_price_x96`, `tick`, `liquidity`).
    pub field: String,
    /// Engine-side value, rendered exactly as it appeared in the diff string.
    pub engine: String,
    /// On-chain-side value, rendered exactly as it appeared in the diff string.
    pub onchain: String,
}

impl FieldDiff {
    /// Render the legacy `"<field>: engine=<engine>, onchain=<onchain>"` line.
    #[must_use]
    pub fn to_diff_string(&self) -> String {
        format!(
            "{}: engine={}, onchain={}",
            self.field, self.engine, self.onchain
        )
    }
}

/// `skip_serializing_if` helper: skip `drift` when it is `false` (the common
/// no-drift case) so the default JSON stays compact.
///
/// `serde`'s `skip_serializing_if` passes the field by reference, so this must
/// take `&bool` despite clippy wanting pass-by-value.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// Compute typed field-level differences between engine and on-chain pool
/// state (PCG2M3). Pure — no RPC. The single source of truth for `diff`:
/// `fetch_onchain` calls this and derives the `Vec<String>` `diff` lines via
/// `FieldDiff::to_diff_string`, so the two never drift.
///
/// Fields compared per family:
/// - V2: `reserve_in`, `reserve_out`
/// - V3/V4: `sqrt_price_x96`, `tick`, `liquidity` (identical for both CL
///   families — merged with `|`)
///
/// Only true divergences are returned (matching fields are omitted). A
/// cross-family mismatch (e.g. engine V2 vs onchain V3) is recorded as a
/// single `FieldDiff { field: "pool_family" }` so the analyzer sees non-empty
/// `field_drift` instead of silently empty diffs.
#[must_use]
pub fn compute_field_diffs(
    engine: &DiagnosticPoolState,
    onchain: &DiagnosticPoolState,
) -> Vec<FieldDiff> {
    let mut diffs = Vec::new();
    let mut push = |field: &str, eng: String, chain: String| {
        if eng != chain {
            diffs.push(FieldDiff {
                field: field.to_string(),
                engine: eng,
                onchain: chain,
            });
        }
    };
    match (engine, onchain) {
        (
            DiagnosticPoolState::V2 {
                reserve_in: e_rin,
                reserve_out: e_rout,
                ..
            },
            DiagnosticPoolState::V2 {
                reserve_in: c_rin,
                reserve_out: c_rout,
                ..
            },
        ) => {
            push("reserve_in", e_rin.clone(), c_rin.clone());
            push("reserve_out", e_rout.clone(), c_rout.clone());
        }
        (
            DiagnosticPoolState::V3 {
                sqrt_price_x96: e_sp,
                tick: e_tick,
                liquidity: e_liq,
                ..
            },
            DiagnosticPoolState::V3 {
                sqrt_price_x96: c_sp,
                tick: c_tick,
                liquidity: c_liq,
                ..
            },
        )
        | (
            DiagnosticPoolState::V4 {
                sqrt_price_x96: e_sp,
                tick: e_tick,
                liquidity: e_liq,
                ..
            },
            DiagnosticPoolState::V4 {
                sqrt_price_x96: c_sp,
                tick: c_tick,
                liquidity: c_liq,
                ..
            },
        ) => {
            push("sqrt_price_x96", e_sp.clone(), c_sp.clone());
            push("tick", e_tick.to_string(), c_tick.to_string());
            push("liquidity", e_liq.clone(), c_liq.clone());
        }
        (engine, onchain) => {
            push(
                "pool_family",
                engine.family_tag().to_string(),
                onchain.family_tag().to_string(),
            );
        }
    }
    diffs
}

/// Engine-vs-onchain re-compute of a single hop's output (PCG2M3 schema; fields
/// populated by the recompute tasks). All `None` until those tasks wire it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HopRecompute {
    /// The hop's consumed input used as the re-compute basis.
    pub amount_in: String,
    /// The output the solver reported for this hop.
    pub solver_out: String,
    /// Re-computed output from the ENGINE state. `None` until the recompute
    /// task runs (intent: identity for V3/V4 — same math the solver ran).
    pub expected_out_engine: Option<String>,
    /// Re-computed output from the ONCHAIN state. `None` until the recompute
    /// task runs (intent: drift detector for V3/V4; meaningful calc check for V2).
    pub expected_out_onchain: Option<String>,
    /// True iff the chosen recompute basis agrees with `solver_out`.
    pub matches_solver: Option<bool>,
}

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

impl DiagnosticPoolState {
    /// Short family tag used by `compute_field_diffs` for the cross-family
    /// mismatch case (e.g. engine V2 vs onchain V3).
    #[must_use]
    pub fn family_tag(&self) -> &'static str {
        match self {
            Self::V2 { .. } => "V2",
            Self::V3 { .. } => "V3",
            Self::V4 { .. } => "V4",
        }
    }
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
    /// True iff ANY field differs between `engine_state` and `onchain_state`
    /// (PCG2M3). `false` when no on-chain fetch ran or every fetched field
    /// matched.
    #[serde(default, skip_serializing_if = "is_false")]
    pub drift: bool,
    /// Typed field-level differences engine vs on-chain (PCG2M3). Empty when
    /// the fetch was skipped or every field matched. The `diff` strings are
    /// derived from these (`FieldDiff::to_diff_string`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_drift: Vec<FieldDiff>,
    /// Engine-vs-onchain hop-output recompute (PCG2M3 schema; populated by the
    /// recompute tasks). `None` until a recompute runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recompute: Option<HopRecompute>,
}

impl DiagnosticHop {
    /// Apply an on-chain fetch outcome to this hop (PCG2M3). Single source of
    /// truth: sets `onchain_state` + `field_drift` + `drift`, then DERIVES the
    /// human-readable `diff` lines from `field_drift` (via
    /// `FieldDiff::to_diff_string`) and appends any arbitrary `messages`
    /// (fetch-skip notes / RPC error strings) verbatim. Callers must NEVER push
    /// field-diff strings to `diff` directly — go through `field_drift` so the
    /// typed view and the human view never drift.
    pub(crate) fn apply_onchain_fetch(
        &mut self,
        onchain_state: Option<DiagnosticPoolState>,
        field_drifts: Vec<FieldDiff>,
        messages: Vec<String>,
    ) {
        self.onchain_state = onchain_state;
        self.drift = !field_drifts.is_empty();
        self.diff.extend(
            field_drifts
                .iter()
                .map(FieldDiff::to_diff_string)
                .chain(messages),
        );
        self.field_drift.extend(field_drifts);
    }
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
            drift: false,
            field_drift: Vec::new(),
            recompute: None,
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
    /// The solver's reported optimal input for this path (PCG2M3; threaded in
    /// from `simulate_one`). `None` until populated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimal_input: Option<String>,
    /// The solver's reported per-hop output amounts in path order (PCG2M3;
    /// threaded in from `simulate_one`). Empty until populated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hop_outputs: Vec<String>,
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
            optimal_input: None,
            hop_outputs: Vec::new(),
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
        provider: &degenbot_rpc::provider::AlloyProvider,
        state_view: Option<Address>,
    ) -> Result<(), degenbot_core::errors::ProviderError> {
        let block_number = self.solve_block;
        self.onchain_block = block_number;

        for hop in &mut self.hops {
            let result = fetch_hop_onchain(hop, provider, block_number, state_view).await;
            match result {
                Ok(outcome) => {
                    let field_drifts = match &outcome.onchain_state {
                        Some(oc) => compute_field_diffs(&hop.engine_state, oc),
                        None => Vec::new(),
                    };
                    hop.apply_onchain_fetch(
                        outcome.onchain_state,
                        field_drifts,
                        outcome.messages,
                    );
                }
                Err(e) => {
                    hop.diff.push(format!("on-chain fetch failed: {e}"));
                }
            }
        }

        Ok(())
    }
}

/// Dispatch a single hop's on-chain fetch by pool family (PCG2M3; extracted
/// from `fetch_onchain` to keep that method under the clippy line budget).
/// Returns the on-chain `FetchOutcome`; field drift is computed by the caller
/// via `compute_field_diffs` (single source of truth).
async fn fetch_hop_onchain(
    hop: &DiagnosticHop,
    provider: &degenbot_rpc::provider::AlloyProvider,
    block_number: Option<u64>,
    state_view: Option<Address>,
) -> Result<FetchOutcome, degenbot_core::errors::ProviderError> {
    match &hop.engine_state {
        DiagnosticPoolState::V2 {
            address,
            reserve_in: _,
            reserve_out: _,
            fee_denom,
            gamma_numer,
        } => {
            fetch_v2_onchain(
                provider,
                block_number,
                hop.zero_for_one,
                address,
                fee_denom,
                gamma_numer,
            )
            .await
        }
        DiagnosticPoolState::V3 {
            address,
            token0,
            token1,
            fee,
            tick_spacing,
            sqrt_price_x96: _,
            tick: _,
            liquidity: _,
        } => {
            fetch_v3_onchain(
                provider,
                block_number,
                address,
                token0,
                token1,
                *fee,
                *tick_spacing,
            )
            .await
        }
        DiagnosticPoolState::V4 {
            pool_manager,
            pool_id,
            currency0,
            currency1,
            fee,
            tick_spacing,
            hook_flags,
            hooks,
            sqrt_price_x96: _,
            tick: _,
            liquidity: _,
        } => {
            if let Some(sv) = state_view {
                fetch_v4_onchain(
                    provider,
                    block_number,
                    sv,
                    pool_manager,
                    pool_id,
                    currency0,
                    currency1,
                    *fee,
                    *tick_spacing,
                    *hook_flags,
                    hooks,
                )
                .await
            } else {
                Ok(FetchOutcome {
                    onchain_state: None,
                    messages: vec![
                        "V4 on-chain fetch skipped: no StateView address provided"
                            .to_string(),
                    ],
                })
            }
        }
    }
}

/// Result of fetching on-chain state for a single hop.
struct FetchOutcome {
    onchain_state: Option<DiagnosticPoolState>,
    /// Arbitrary fetch-time messages appended verbatim to `diff` (skip notes,
    /// RPC errors) — NOT field divergences. Field drift is computed separately
    /// by `compute_field_diffs` in `fetch_onchain` (single source of truth).
    messages: Vec<String>,
}

impl FetchOutcome {
    fn new(onchain_state: DiagnosticPoolState) -> Self {
        Self {
            onchain_state: Some(onchain_state),
            messages: Vec::new(),
        }
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
    provider: &degenbot_rpc::provider::AlloyProvider,
    block_number: Option<u64>,
    zero_for_one: bool,
    address: &str,
    fee_denom: &str,
    gamma_numer: &str,
) -> Result<FetchOutcome, degenbot_core::errors::ProviderError> {
    let pool_address: Address =
        address
            .parse()
            .map_err(|e| degenbot_core::errors::ProviderError::Other {
                message: format!("invalid V2 pool address {address}: {e}"),
            })?;

    let selector = fn_selector("getReserves()");
    let calldata = encode_call(selector, &[]);
    let raw = provider
        .eth_call(&pool_address, calldata, block_number)
        .await?;

    let return_type = DynSolType::Tuple(vec![
        DynSolType::Uint(112),
        DynSolType::Uint(112),
        DynSolType::Uint(32),
    ]);
    let decoded = return_type.abi_decode(&raw).map_err(|e| {
        degenbot_core::errors::ProviderError::SerializationError {
            message: format!("failed to decode getReserves result: {e}"),
        }
    })?;

    let DynSolValue::Tuple(values) = decoded else {
        return Err(degenbot_core::errors::ProviderError::SerializationError {
            message: "getReserves result is not a tuple".to_string(),
        });
    };

    let reserve0 = uint_value(&values[0], 112).ok_or_else(|| {
        degenbot_core::errors::ProviderError::SerializationError {
            message: "getReserves reserve0 decode mismatch".to_string(),
        }
    })?;
    let reserve1 = uint_value(&values[1], 112).ok_or_else(|| {
        degenbot_core::errors::ProviderError::SerializationError {
            message: "getReserves reserve1 decode mismatch".to_string(),
        }
    })?;

    let (chain_reserve_in, chain_reserve_out) = if zero_for_one {
        (reserve0, reserve1)
    } else {
        (reserve1, reserve0)
    };

    let outcome = FetchOutcome::new(DiagnosticPoolState::V2 {
        address: address.to_string(),
        reserve_in: u256_to_hex(chain_reserve_in),
        reserve_out: u256_to_hex(chain_reserve_out),
        fee_denom: fee_denom.to_string(),
        gamma_numer: gamma_numer.to_string(),
    });

    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
async fn fetch_v3_onchain(
    provider: &degenbot_rpc::provider::AlloyProvider,
    block_number: Option<u64>,
    address: &str,
    token0: &str,
    token1: &str,
    fee: u32,
    tick_spacing: i32,
) -> Result<FetchOutcome, degenbot_core::errors::ProviderError> {
    let pool_address: Address =
        address
            .parse()
            .map_err(|e| degenbot_core::errors::ProviderError::Other {
                message: format!("invalid V3 pool address {address}: {e}"),
            })?;

    // slot0() -> (sqrtPriceX96, tick, observationIndex, observationCardinality, observationCardinalityNext, feeProtocol, unlocked)
    let slot0_selector = fn_selector("slot0()");
    let raw_slot0 = provider
        .eth_call(
            &pool_address,
            encode_call(slot0_selector, &[]),
            block_number,
        )
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
        degenbot_core::errors::ProviderError::SerializationError {
            message: format!("failed to decode slot0 result: {e}"),
        }
    })?;
    let DynSolValue::Tuple(slot0_values) = decoded_slot0 else {
        return Err(degenbot_core::errors::ProviderError::SerializationError {
            message: "slot0 result is not a tuple".to_string(),
        });
    };

    let chain_sqrt_price_x96 = uint_value(&slot0_values[0], 160).ok_or_else(|| {
        degenbot_core::errors::ProviderError::SerializationError {
            message: "slot0 sqrtPriceX96 decode mismatch".to_string(),
        }
    })?;
    let chain_tick = int_value_to_i32(&slot0_values[1], 24).ok_or_else(|| {
        degenbot_core::errors::ProviderError::SerializationError {
            message: "slot0 tick decode mismatch".to_string(),
        }
    })?;

    // liquidity() -> uint128
    let liquidity_selector = fn_selector("liquidity()");
    let raw_liquidity = provider
        .eth_call(
            &pool_address,
            encode_call(liquidity_selector, &[]),
            block_number,
        )
        .await?;
    let liquidity_type = DynSolType::Uint(128);
    let decoded_liquidity = liquidity_type.abi_decode(&raw_liquidity).map_err(|e| {
        degenbot_core::errors::ProviderError::SerializationError {
            message: format!("failed to decode liquidity result: {e}"),
        }
    })?;
    let chain_liquidity = uint_value(&decoded_liquidity, 128)
        .ok_or_else(
            || degenbot_core::errors::ProviderError::SerializationError {
                message: "liquidity decode mismatch".to_string(),
            },
        )?
        .to::<u128>();

    let outcome = FetchOutcome::new(DiagnosticPoolState::V3 {
        address: address.to_string(),
        token0: token0.to_string(),
        token1: token1.to_string(),
        fee,
        tick_spacing,
        sqrt_price_x96: u256_to_hex(chain_sqrt_price_x96),
        tick: chain_tick,
        liquidity: u128_to_hex(chain_liquidity),
    });

    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
async fn fetch_v4_onchain(
    provider: &degenbot_rpc::provider::AlloyProvider,
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
) -> Result<FetchOutcome, degenbot_core::errors::ProviderError> {
    let pool_id_bytes = degenbot_core::hex_utils::decode_32byte_hex(pool_id).map_err(|e| {
        degenbot_core::errors::ProviderError::Other {
            message: format!("invalid V4 pool_id {pool_id}: {e}"),
        }
    })?;

    let pool_id_value = DynSolValue::FixedBytes(B256::from(pool_id_bytes), 32);

    // StateView.getSlot0(bytes32 poolId) -> (uint160 sqrtPriceX96, int24 tick, uint24 protocolFee, uint24 swapFee)
    let slot0_selector = fn_selector("getSlot0(bytes32)");
    let raw_slot0 = provider
        .eth_call(
            &state_view,
            encode_call(slot0_selector, std::slice::from_ref(&pool_id_value)),
            block_number,
        )
        .await?;
    let slot0_type = DynSolType::Tuple(vec![
        DynSolType::Uint(160),
        DynSolType::Int(24),
        DynSolType::Uint(24),
        DynSolType::Uint(24),
    ]);
    let decoded_slot0 = slot0_type.abi_decode(&raw_slot0).map_err(|e| {
        degenbot_core::errors::ProviderError::SerializationError {
            message: format!("failed to decode StateView.getSlot0 result: {e}"),
        }
    })?;
    let DynSolValue::Tuple(slot0_values) = decoded_slot0 else {
        return Err(degenbot_core::errors::ProviderError::SerializationError {
            message: "StateView.getSlot0 result is not a tuple".to_string(),
        });
    };

    let chain_sqrt_price_x96 = uint_value(&slot0_values[0], 160).ok_or_else(|| {
        degenbot_core::errors::ProviderError::SerializationError {
            message: "getSlot0 sqrtPriceX96 decode mismatch".to_string(),
        }
    })?;
    let chain_tick = int_value_to_i32(&slot0_values[1], 24).ok_or_else(|| {
        degenbot_core::errors::ProviderError::SerializationError {
            message: "getSlot0 tick decode mismatch".to_string(),
        }
    })?;

    // StateView.getLiquidity(bytes32 poolId) -> uint128
    let liquidity_selector = fn_selector("getLiquidity(bytes32)");
    let raw_liquidity = provider
        .eth_call(
            &state_view,
            encode_call(liquidity_selector, &[pool_id_value]),
            block_number,
        )
        .await?;
    let liquidity_type = DynSolType::Uint(128);
    let decoded_liquidity = liquidity_type.abi_decode(&raw_liquidity).map_err(|e| {
        degenbot_core::errors::ProviderError::SerializationError {
            message: format!("failed to decode StateView.getLiquidity result: {e}"),
        }
    })?;
    let chain_liquidity = uint_value(&decoded_liquidity, 128)
        .ok_or_else(
            || degenbot_core::errors::ProviderError::SerializationError {
                message: "getLiquidity decode mismatch".to_string(),
            },
        )?
        .to::<u128>();

    let outcome = FetchOutcome::new(DiagnosticPoolState::V4 {
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

        // ADR-003: V2 state lives in BotState. One core-lock window covers all
        // V2 lookups in this loop; V3/V4 state still reads the per-family
        // block engines (disjoint fields, immutable borrows coexist).
        let core = self.core.read();

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
            let engine_state = build_engine_pool_state(&core, pool_ref);
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
                    drift: false,
                    field_drift: Vec::new(),
                    recompute: None,
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
                drift: false,
                field_drift: Vec::new(),
                recompute: None,
            });
        }

        Some(snapshot)
    }
}

/// Build the per-hop [`DiagnosticPoolState`] from a locked [`BotState`] snapshot.
///
/// Returns `None` when the pool referenced by `pool_ref` is absent from the
/// sub-engine — the caller records a "missing pool" placeholder in that case
/// so the rest of the hops remain visible.
fn build_engine_pool_state(
    core: &crate::bot_core::BotState,
    pool_ref: &MixedPoolRef,
) -> Option<DiagnosticPoolState> {
    match pool_ref.hop_type {
        HopType::V2 => core.get_v2_pool_state(pool_ref.pool_key).map(|state| {
            let (reserve_in, reserve_out, gamma_numer, fee_denom) = if pool_ref.zero_for_one {
                (
                    state.reserve0,
                    state.reserve1,
                    state.fee_token0.0,
                    state.fee_token0.1,
                )
            } else {
                (
                    state.reserve1,
                    state.reserve0,
                    state.fee_token1.0,
                    state.fee_token1.1,
                )
            };
            DiagnosticPoolState::V2 {
                address: fmt_addr(state.address),
                reserve_in: fmt_u256(reserve_in),
                reserve_out: fmt_u256(reserve_out),
                fee_denom: format!("0x{fee_denom:x}"),
                gamma_numer: format!("0x{gamma_numer:x}"),
            }
        }),
        HopType::V3 => core
            .get_v3_pool(pool_ref.pool_key)
            .map(|state| DiagnosticPoolState::V3 {
                address: fmt_addr(state.address),
                token0: fmt_addr(state.token0),
                token1: fmt_addr(state.token1),
                fee: state.fee,
                tick_spacing: state.tick_spacing,
                sqrt_price_x96: fmt_u256(state.sqrt_price_x96),
                tick: state.tick,
                liquidity: format!("0x{:x}", state.liquidity),
            }),
        HopType::V4 => core
            .get_v4_pool(pool_ref.pool_key)
            .map(|state| DiagnosticPoolState::V4 {
                pool_manager: fmt_addr(state.pool_manager),
                pool_id: format!("0x{}", alloy::hex::encode(state.pool_id)),
                currency0: state.pool_key.currency0.to_checksum(None),
                currency1: state.pool_key.currency1.to_checksum(None),
                fee: state.pool_key.fee,
                tick_spacing: state.pool_key.tick_spacing,
                hook_flags: 0,
                hooks: state.pool_key.hooks.to_checksum(None),
                sqrt_price_x96: fmt_u256(state.sqrt_price_x96),
                tick: state.tick,
                liquidity: format!("0x{:x}", state.liquidity),
            }),
    }
}

// ---------------------------------------------------------------------------
// Sub-engine accessors (V3/V4 state now reads from BotState — ADR-003)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use alloy::primitives::{Address, U256};

    use crate::bot_core::RegisterV3PoolParams as V3Params;
    use crate::bot_core::{RegisterV4PoolParams as V4Params, V4PoolKey};
    use crate::solvers::uniswap_engine::{
        DiagnosticPathState, PoolHop, PoolTickCoverage, UniswapEngine,
    };

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
        let v2_fwd = engine.register_v2_pool(
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
                block: 0,
            },
        );
        let v3_key = engine.register_v3_pool(&V3Params {
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
                block: 0,
            },
        );
        let v4_fwd = engine
            .register_v4_pool(&V4Params {
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_fwd,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v3_key,
                    zero_for_one: false,
                },
                PoolHop {
                    pool_id: v4_fwd,
                    zero_for_one: true,
                },
            ])
            .unwrap();

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

    // -----------------------------------------------------------------
    // Structured typed drift fields (PCG2M3)
    // -----------------------------------------------------------------

    /// A V2 hop whose engine `reserve_in` / `reserve_out` both diverge from the
    /// on-chain snapshot yields TWO `FieldDiff` entries (`reserve_in`,
    /// `reserve_out`), `drift == true`, and the derived `diff` strings preserve
    /// the legacy `"<field>: engine=<v>, onchain=<v>"` shape exactly.
    #[test]
    fn field_diff_v2_divergent_reserves_sets_drift_and_field_drift() {
        let engine_state = super::DiagnosticPoolState::V2 {
            address: "0x0000000000000000000000000000000000001111".to_string(),
            reserve_in: "0x01".to_string(),
            reserve_out: "0x02".to_string(),
            fee_denom: "0x03e8".to_string(),
            gamma_numer: "0x03e5".to_string(),
        };
        let onchain_state = super::DiagnosticPoolState::V2 {
            address: "0x0000000000000000000000000000000000001111".to_string(),
            reserve_in: "0xff".to_string(),
            reserve_out: "0xee".to_string(),
            fee_denom: "0x03e8".to_string(),
            gamma_numer: "0x03e5".to_string(),
        };

        let diffs = super::compute_field_diffs(&engine_state, &onchain_state);
        assert_eq!(
            diffs.len(),
            2,
            "both reserves diverge → two field diffs"
        );
        assert_eq!(diffs[0].field, "reserve_in");
        assert_eq!(diffs[0].engine, "0x01");
        assert_eq!(diffs[0].onchain, "0xff");
        assert_eq!(diffs[1].field, "reserve_out");
        assert_eq!(diffs[1].engine, "0x02");
        assert_eq!(diffs[1].onchain, "0xee");

        // drift is exactly "field_drift is non-empty".
        let drift = !diffs.is_empty();
        assert!(drift);

        // Derived diff strings match the legacy format byte-for-byte.
        assert_eq!(
            diffs[0].to_diff_string(),
            "reserve_in: engine=0x01, onchain=0xff"
        );
        assert_eq!(
            diffs[1].to_diff_string(),
            "reserve_out: engine=0x02, onchain=0xee"
        );
    }

    /// A V2 hop whose reserves match the on-chain snapshot yields NO field
    /// diffs and `drift == false` (the matching-hop case).
    #[test]
    fn field_diff_v2_matching_reserves_no_drift() {
        let engine_state = super::DiagnosticPoolState::V2 {
            address: "0x0000000000000000000000000000000000001111".to_string(),
            reserve_in: "0x01".to_string(),
            reserve_out: "0x02".to_string(),
            fee_denom: "0x03e8".to_string(),
            gamma_numer: "0x03e5".to_string(),
        };
        let onchain_state = super::DiagnosticPoolState::V2 {
            address: "0x0000000000000000000000000000000000001111".to_string(),
            reserve_in: "0x01".to_string(),
            reserve_out: "0x02".to_string(),
            fee_denom: "0x03e8".to_string(),
            gamma_numer: "0x03e5".to_string(),
        };

        let diffs = super::compute_field_diffs(&engine_state, &onchain_state);
        assert!(diffs.is_empty(), "matching state → no field diffs");
        assert!(diffs.is_empty(), "drift == false");
    }

    /// `apply_onchain_fetch` is the single source of truth: it sets
    /// `onchain_state` + `field_drift` + `drift`, and DERIVES the `diff`
    /// strings from `field_drift` (via `FieldDiff::to_diff_string`) plus any
    /// arbitrary fetch-time `messages` (skip notes / RPC errors). Field-drift
    /// entries become `diff` lines in order; messages are appended verbatim.
    #[test]
    fn apply_onchain_fetch_derives_diff_from_field_drift() {
        let mut hop = super::DiagnosticHop {
            position: 0,
            hop_type: "V2".to_string(),
            zero_for_one: true,
            engine_state: super::DiagnosticPoolState::V2 {
                address: "0x0000000000000000000000000000000000001111".to_string(),
                reserve_in: "0x01".to_string(),
                reserve_out: "0x02".to_string(),
                fee_denom: "0x03e8".to_string(),
                gamma_numer: "0x03e5".to_string(),
            },
            onchain_state: None,
            diff: Vec::new(),
            drift: false,
            field_drift: Vec::new(),
            recompute: None,
        };
        let onchain = super::DiagnosticPoolState::V2 {
            address: "0x0000000000000000000000000000000000001111".to_string(),
            reserve_in: "0xff".to_string(),
            reserve_out: "0xee".to_string(),
            fee_denom: "0x03e8".to_string(),
            gamma_numer: "0x03e5".to_string(),
        };
        let field_drifts = vec![
            super::FieldDiff {
                field: "reserve_in".to_string(),
                engine: "0x01".to_string(),
                onchain: "0xff".to_string(),
            },
            super::FieldDiff {
                field: "reserve_out".to_string(),
                engine: "0x02".to_string(),
                onchain: "0xee".to_string(),
            },
        ];

        hop.apply_onchain_fetch(
            Some(onchain),
            field_drifts.clone(),
            vec!["V4 fetch skipped".to_string()],
        );

        assert!(matches!(hop.onchain_state, Some(super::DiagnosticPoolState::V2 { .. })));
        assert_eq!(hop.field_drift, field_drifts);
        assert!(hop.drift, "non-empty field_drift → drift == true");
        // diff derives from field_drift (in order) THEN appends messages verbatim.
        assert_eq!(
            hop.diff,
            vec![
                "reserve_in: engine=0x01, onchain=0xff",
                "reserve_out: engine=0x02, onchain=0xee",
                "V4 fetch skipped",
            ]
        );
    }

    /// `apply_onchain_fetch` with zero field drifts + zero messages leaves
    /// `drift == false` and `diff` empty (the clean-match path).
    #[test]
    fn apply_onchain_fetch_clean_match_no_drift() {
        let mut hop = super::DiagnosticHop::default();
        hop.apply_onchain_fetch(None::<super::DiagnosticPoolState>, Vec::new(), Vec::new());
        assert!(hop.onchain_state.is_none());
        assert!(hop.field_drift.is_empty());
        assert!(!hop.drift);
        assert!(hop.diff.is_empty());
    }
}
