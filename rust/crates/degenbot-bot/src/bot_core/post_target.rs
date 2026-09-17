//! Post-target state adjustment — the lean transform (task QZF6HQ).
//!
//! A backrun's opportunity is computed against POST-upstream-tx pool state.
//! The lean path applies the upstream swap to the pool's in-memory state
//! math (pure functions — no RPC, no canonical mutation), producing a
//! staged overlay the solver consults when evaluating the backrun
//! candidate. Canonical registry state is NEVER touched: the staged overlay
//! lives only inside the pipeline planning scope; the live-sim path
//! re-applies it per candidate.
//!
//! Protocol coverage:
//!
//! - v2: exact lean overlay. Constant-product fee math identical to the
//!   on-chain `getAmountOut` (via the same `IntHopState` the pool simulator
//!   uses) plus the reserve delta on the staged view:
//!   `reserve_in += amount_in`, `reserve_out -= amount_out`.
//! - v3/v4: exact-sim-only. Concentrated-liquidity post-swap state needs
//!   the full tick-crossing walk that lives in the simulator; a scalar
//!   overlay would not be honest (partial-convertible inputs, tick
//!   crossings). These families route to the exact path (`eth_callMany`,
//!   staged by the submission task) and this module returns
//!   `OverlayError::ExactSimOnly` — documented, never guessed.
//!
//! Semantics contract for the staged v2 overlay:
//!
//! - pure: no pool mutation, no clocks bumped, no reorg journal touched,
//! - the dispatcher stages an `OverlayV2` per pool id inside the candidate
//!   plan scope; the engine reads it when simulating the backrun's first
//!   hop through a pool the target tx moved.

use degenbot_math::v2::IntHopState;
use degenbot_pathfinding::PoolKind;

/// Fee parameters for a v2-family pool: `(gamma_numer, fee_denom)` — e.g.
/// `(997, 1000)` for a 0.3%-fee pair; Aerodrome volatile carries asymmetric
/// per-direction fees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2FeeParams {
    pub gamma_numer: u64,
    pub fee_denom: u64,
}

/// The exact post-upstream v2 pool view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayV2 {
    pub amount_out: u128,
    /// Reserves after the upstream swap (`token_in` side gains, `token_out`
    /// side loses; widths preserved as the canonical widened values).
    pub new_reserve_in: u128,
    pub new_reserve_out: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayError {
    /// v3/v4 lean overlays would be dishonest; the exact `eth_callMany` path
    /// covers them.
    ExactSimOnly,
    /// Upstream amount is zero or the swap math overflows the chain width
    /// (the chain would revert; a backrun on a reverting target never lands).
    NotComputable,
}

/// Exact post-upstream v2 view. Pure: returns the staged numbers, mutates
/// nothing. Reserves arrive as the widened swap-math values and overlay
/// outputs round-trip through `u128` (the canonical `uint112` storage class).
///
/// # Errors
///
/// - `NotComputable`: zero amount in; arithmetic overflow (the chain would
///   revert); output would reach the full reserve (not reachable via valid
///   v2 math — the fee shields it — but guarded as a safety net).
pub fn v2_post_target(
    reserve_in: u128,
    reserve_out: u128,
    fee: V2FeeParams,
    amount_in: u128,
) -> Result<OverlayV2, OverlayError> {
    if amount_in == 0 {
        return Err(OverlayError::NotComputable);
    }
    let hop = IntHopState::new(
        alloy::primitives::U256::from(reserve_in),
        alloy::primitives::U256::from(reserve_out),
        fee.gamma_numer,
        fee.fee_denom,
    );
    let out = hop
        .swap(alloy::primitives::U256::from(amount_in))
        .map_err(|_| OverlayError::NotComputable)?;
    let out_u128 = u128::try_from(out).map_err(|_| OverlayError::NotComputable)?;
    let new_reserve_in = reserve_in
        .checked_add(amount_in)
        .ok_or(OverlayError::NotComputable)?;
    let new_reserve_out = reserve_out
        .checked_sub(out_u128)
        .ok_or(OverlayError::NotComputable)?;
    if new_reserve_out == 0 {
        return Err(OverlayError::NotComputable);
    }
    Ok(OverlayV2 {
        amount_out: out_u128,
        new_reserve_in,
        new_reserve_out,
    })
}

/// Staged per-candidate plan: the pool ids the target tx moved plus their
/// exact post-views where the protocol admits a lean overlay.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostTargetPlan {
    /// pool id -> staged view (only pools with lean support present; v3/v4
    /// land in `exact_sim_pool_ids` instead).
    pub overlays: hashbrown::HashMap<u64, OverlayV2>,
    /// Upstream pools whose family needs the exact sim path.
    pub exact_sim_pool_ids: Vec<u64>,
}

impl PostTargetPlan {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stage the upstream leg for a pool.
    ///
    /// # Errors
    ///
    /// Propagates the per-family errors. v3/v4 (and any future non-lean
    /// family) register the pool id for the exact path and return
    /// `OverlayError::ExactSimOnly`.
    pub fn stage(
        &mut self,
        pool_id: u64,
        pool_kind: PoolKind,
        reserve_in: u128,
        reserve_out: u128,
        fee: V2FeeParams,
        amount_in: u128,
    ) -> Result<(), OverlayError> {
        if pool_kind == PoolKind::V2 {
            let view = v2_post_target(reserve_in, reserve_out, fee, amount_in)?;
            self.overlays.insert(pool_id, view);
            Ok(())
        } else {
            self.exact_sim_pool_ids.push(pool_id);
            Err(OverlayError::ExactSimOnly)
        }
    }
}
