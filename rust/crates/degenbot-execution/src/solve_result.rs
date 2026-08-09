//! The solve-result **view contract** (ADR-025 D4).
//!
//! Today the per-hop solve amounts do not cross to Python on the clean path —
//! [`degenbot_solvers::mixed::SolvePathResult`] carries
//! `optimal_input` / `hop_outputs` / `consumed_inputs`, and
//! `degenbot_executor::composers::PathInfo` carries the hop descriptors, but
//! neither is exposed to Python as a typed view. This module owns the one
//! genuinely new surface: a symmetric, both-consumer view of the solved path.
//!
//! **Amounts are integer fixed-point u256, never floats** — decimal place
//! matters, so the projected view keeps them as `U256` integers.

use alloy::primitives::U256;

use degenbot_executor::composers::{HopInfo, PathInfo};
use degenbot_solvers::mixed::SolvePathResult;

/// Which engine family owns a hop — the descriptor's family tag.
///
/// Mirrors the hop mix already expressed by [`crate::gate`]'s per-family
/// adapters; a 4th family becomes one new enum route, not a fan-out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HopFamily {
    /// Uniswap V2 (constant-product).
    V2,
    /// Uniswap V3 (concentrated-liquidity).
    V3,
    /// Uniswap V4 (concentrated-liquidity, PoolManager settlement).
    V4,
}

/// A single hop's descriptor — the pool address + token/currency identities +
/// direction, projected from a [`HopInfo`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HopDescriptor {
    /// The engine family owning this hop.
    pub family: HopFamily,
    /// The swap pool (V2/V3) or the pool manager (V4 holds
    /// `pool_manager` + `pool_id`; the id string is exposed separately below).
    pub pool_address: alloy::primitives::Address,
    /// token0 / currency0 address (V4: `currency0`).
    pub token0: alloy::primitives::Address,
    /// token1 / currency1 address (V4: `currency1`).
    pub token1: alloy::primitives::Address,
    /// Swap direction: `true` = zero-for-one (token0→token1).
    pub zfo: bool,
    /// V4 only: the pool-id hex (0x-prefixed). `None` for V2/V3.
    pub v4_pool_id: Option<String>,
}

impl HopDescriptor {
    /// Project a hop from its [`HopInfo`] descriptor.
    #[must_use]
    pub fn from_hop_info(hop: &HopInfo) -> Self {
        match hop {
            HopInfo::V2(h) => Self {
                family: HopFamily::V2,
                pool_address: h.pool_address,
                token0: h.token0_address,
                token1: h.token1_address,
                zfo: h.zfo,
                v4_pool_id: None,
            },
            HopInfo::V3(h) => Self {
                family: HopFamily::V3,
                pool_address: h.pool_address,
                token0: h.token0_address,
                token1: h.token1_address,
                zfo: h.zfo,
                v4_pool_id: None,
            },
            HopInfo::V4(h) => Self {
                family: HopFamily::V4,
                pool_address: h.pool_manager_address,
                token0: h.currency0_address,
                token1: h.currency1_address,
                zfo: h.zfo,
                v4_pool_id: Some(h.pool_id_hex.clone()),
            },
        }
    }
}

/// The solved path's amounts + hop descriptors — the **solve-result view** the
/// Encode part of a strategy consumes (ADR-025 D4).
///
/// This is the seam's input, projected symmetrically: Rust consumers build it
/// directly from the two canonical types ([`SolvePathResult`] amounts +
/// [`PathInfo`] hop descriptors); Python consumers receive the same fields as
/// a typed view. Amounts stay integer fixed-point u256.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolveResult {
    /// The path id (`path_id`).
    pub path_id: u64,
    /// Number of hops in the path (`hop_count`).
    pub hop_count: usize,
    /// The flash input amount (`optimal_input`, uint256).
    pub optimal_input: U256,
    /// Per-hop output amounts (`hop_outputs`, uint256 integers; `[i]` = output
    /// after hop `i`).
    pub hop_outputs: Vec<U256>,
    /// Per-hop consumed input amounts (`consumed_inputs`, uint256 integers).
    /// The CL-clamp swap-in matters: for an over-fed CL hop this is reduced to
    /// `input_consumed − 1` (ADR-025 CL-clamp resolution).
    pub consumed_inputs: Vec<U256>,
    /// The net profit (`net_profit`, uint256) — `final_output −
    /// consumed_inputs[0]`.
    pub net_profit: U256,
    /// Per-hop descriptors (pool family + addresses + direction), projected
    /// from the path's [`HopInfo`]s.
    pub hop_descriptors: Vec<HopDescriptor>,
}

impl SolveResult {
    /// Project the view from the canonical solver intake — a
    /// [`SolvePathResult`] (amounts) + [`PathInfo`] (hop descriptors).
    #[must_use]
    pub fn from_solve_path(path_id: u64, result: &SolvePathResult, path: &PathInfo) -> Self {
        Self {
            path_id,
            hop_count: path.hops.len(),
            optimal_input: result.optimal_input,
            hop_outputs: result.hop_outputs.clone(),
            consumed_inputs: result.consumed_inputs.clone(),
            net_profit: result.profit,
            hop_descriptors: path.hops.iter().map(HopDescriptor::from_hop_info).collect(),
        }
    }
}
