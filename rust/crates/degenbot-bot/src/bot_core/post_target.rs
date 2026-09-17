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

use alloy::primitives::{U256, U512};
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

/// The exact post-upstream in-range v3 price view. The module doc's
/// v3 caveat (scalar overlays would be dishonest across tick crossings) is
/// honored two ways: (1) the caller only stages when the post-swap price
/// stays inside the admitted sparse ladder (it compares `sqrt_p_x96_max`
/// below), and (2) the composed candidate crosses the exact `eth_callMany`
/// bundle sim, which is the truth bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayV3 {
    /// Post-swap `sqrtPriceX96`.
    pub sqrt_p_x96: U256,
    /// The upstream swap's realized output (the token the router receives).
    pub amount_out: U256,
}

/// One-step in-range v3 swap from the router's exact-input (doc
/// how-to/searchers: `EXACT_INPUT_SINGLE` args carry `(tokenIn, tokenOut,
/// fee, amountIn, ...)`, and the feed wire provides the full calldata).
///
/// Math (constant L within the slot's range):
/// - zero-for-one (token0 in -> token1 out): `x = L*Q96/sqrtP` grows,
///   `1/sqrtP' = 1/sqrtP + net/L`, so `sqrtP' = L*sqrtP/(L + net*sqrtP)`;
///   `amountOut = L*(sqrtP - sqrtP')/Q96`.
/// - one-for-zero: `sqrtP' = sqrtP + net*Q96/L`;
///   `amountOut = L*Q96*(1/sqrtP - 1/sqrtP')` =
///   `L*Q96*delta/(sqrtP*sqrtP')`.
///
/// `fee` is the v3 tier numerator out of 1e6 (500 / 3000 / 10000).
/// Zero-net or overflow inputs round to `None` (the chain would revert).
#[must_use]
pub fn v3_exact_in_post_target(
    sqrt_p_x96: U256,
    liquidity: u128,
    fee: u32,
    amount_in: U256,
    zero_for_one: bool,
) -> Option<OverlayV3> {
    if amount_in.is_zero() || liquidity == 0 {
        return None;
    }
    let q96 = U256::from(1) << 96;
    let l = U256::from(liquidity);
    let net = (amount_in * U256::from(1_000_000u32 - fee)) / U256::from(1_000_000u32);
    if net.is_zero() {
        return None;
    }
    let l512 = U512::from(l);
    let p = U512::from(sqrt_p_x96);
    let net512 = U512::from(net);
    let q512 = U512::from(q96);
    if zero_for_one {
        // x = L*Q96/sqrtP grows by net -> sqrtP' = L*Q96/(x + net).
        let x = l512.checked_mul(q512)? / p;
        let num512 = l512.checked_mul(q512)?;
        let denom = x.checked_add(net512)?;
        let sq = num512 / denom;
        if sq > U512::from(U256::MAX) {
            return None;
        }
        let sqrt_new = sq.to::<U256>();
        if sqrt_new.is_zero() || sqrt_new >= sqrt_p_x96 {
            return None;
        }
        // out = L*(sqrtP - sqrtP')/Q96
        let out = (l512 * (p - U512::from(sqrt_new))) / q512;
        if out > U512::from(U256::MAX) {
            return None;
        }
        let amount_out = out.to::<U256>();
        Some(OverlayV3 {
            sqrt_p_x96: sqrt_new,
            amount_out,
        })
    } else {
        // sqrtP' = sqrtP + net*Q96/L
        let delta = net512.checked_mul(q512)? / l512;
        let sqrt_p_new_raw = p + delta;
        if sqrt_p_new_raw > U512::from(U256::MAX) {
            return None;
        }
        let sqrt_new = sqrt_p_new_raw.to::<U256>();
        // out = L*Q96*(1/sqrtP - 1/sqrtP') = L*Q96*(sqrtP' - sqrtP)/(sqrtP*sqrtP')
        let num = l512.checked_mul(q512)? * (sqrt_p_new_raw - p);
        let den = p.checked_mul(U512::from(sqrt_new))?;
        let out = num / den;
        if out > U512::from(U256::MAX) {
            return None;
        }
        let amount_out = out.to::<U256>();
        Some(OverlayV3 {
            sqrt_p_x96: sqrt_new,
            amount_out,
        })
    }
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

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "golden values: hand-verified")]
mod tests {
    use super::*;

    /// Symmetric pool (price 1): x = y = L when sqrtP = Q96. A 1000-wei
    /// 0.3%-fee swap moves 997 net and pays out 996.997 -> floor 996.
    #[test]
    fn zero_for_one_symmetric_pool_matches_hand_math() {
        let l = U256::from(1_000_000_000_000_000_000u64);
        let sqrt_p = U256::from(1) << 96;
        let v = v3_exact_in_post_target(
            sqrt_p,
            1_000_000_000_000_000_000u128,
            3000,
            U256::from(1000),
            true,
        )
        .unwrap();
        // sqrtP' = L*Q96/(L+997)
        assert_eq!(
            U512::from(v.sqrt_p_x96),
            (U512::from(l) * U512::from(sqrt_p)) / (U512::from(l) + U512::from(997u32))
        );
        assert!(v.sqrt_p_x96 < sqrt_p, "zero-for-one must lower the price");
        // The floor on sqrtP' runs WITH the payout direction here (a smaller
        // sqrtP' means a bigger delta), so the deterministic value is 997,
        // not the idealized 996.997. The mirror test floors the other way
        // and stays 996 -- that asymmetry is the documented rounding shape.
        assert_eq!(v.amount_out, U256::from(997u32));
    }

    #[test]
    fn one_for_zero_symmetric_is_mirror() {
        let l = 10_000_000_000_000_000_000u128;
        let sqrt_p = U256::from(1) << 96;
        let v = v3_exact_in_post_target(sqrt_p, l, 3000, U256::from(1000), false).unwrap();
        assert!(v.sqrt_p_x96 > sqrt_p, "one-for-zero must raise the price");
        assert_eq!(v.amount_out, U256::from(996u32));
    }

    #[test]
    fn fee_is_enforced() {
        let l = 10_000_000_000_000_000_000u128;
        let sqrt_p = U256::from(1) << 96;
        let no_fee = v3_exact_in_post_target(sqrt_p, l, 0, U256::from(10_000), true).unwrap();
        let fee = v3_exact_in_post_target(sqrt_p, l, 3000, U256::from(10_000), true).unwrap();
        assert!(
            no_fee.amount_out > fee.amount_out,
            "the fee tier must earn its keep"
        );
    }

    #[test]
    fn degenerate_inputs_are_none() {
        let sqrt_p = U256::from(1) << 96;
        assert!(v3_exact_in_post_target(sqrt_p, 0, 3000, U256::from(1000), true).is_none());
        assert!(v3_exact_in_post_target(sqrt_p, 10u128, 3000, U256::ZERO, true).is_none());
        // 1-wei swap through a 0.3% fee nets 0 -> unroutable.
        assert!(v3_exact_in_post_target(sqrt_p, 10u128, 3000, U256::from(1), false).is_none());
    }
}
