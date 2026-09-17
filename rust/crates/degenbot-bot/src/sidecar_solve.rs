//! Sidecar overlay-solve wiring (task S7KG7E): decoded V2 legs -> live pair
//! reserves -> the QZF6HQ lean overlay -> profitability eval -> bid staging.
//!
//! The RPC glue ([`fetch_v2_reserves`]) is the only async piece; everything
//! else is pure so the fixtures are hand-computed references, not snapshots.
//! The staged plan never mutates the canonical registry (QZF6HZ contract) -
//! it exists only for the candidate being staged.

use alloy::primitives::{address, Address, U256};
use degenbot_decoders::target_classifier::PoolProtocol;
use degenbot_decoders::target_classifier::SwapLeg;

use crate::bot_core::post_target::{v2_post_target, OverlayV2, V2FeeParams};

/// Live pair reserves for a decoded V2 leg (the on-demand resolver's narrow
/// surface: one `getReserves` round-trip per candidate). Reserves return in
/// the PAIR's token order - the caller maps them onto `(token_in, token_out)`.
///
/// # Errors
///
/// Transport failures and short returns (unit112 storage class) both resolve
/// to `None` - the frame then drops rather than guessing.
pub async fn fetch_v2_reserves(
    provider: &degenbot_rpc::provider::AlloyProvider,
    pair: Address,
) -> Option<(u128, u128)> {
    let data = degenbot_rpc::abi::encode_get_reserves();
    let ret = provider
        .eth_call(&pair, alloy::primitives::Bytes::from(data), None)
        .await
        .ok()?;
    if ret.len() < 64 {
        return None;
    }
    let r0 = U256::from_be_slice(&ret[0..32]);
    let r1 = U256::from_be_slice(&ret[32..64]);
    Some((u128::try_from(r0).ok()?, u128::try_from(r1).ok()?))
}

/// The canonical WETH address (the sidecar quotes everything in WETH/wei).
#[must_use]
pub const fn weth() -> Address {
    address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2")
}

/// The staged candidate the sidecar hands to the bid decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolveOutcome {
    /// The post-target view the backrun was evaluated against.
    pub overlay: OverlayV2,
    /// The maximum WETH bribe that stays whole for the backrun route
    /// `(gross - input cost)`; the bid leg spends this toward `block.coinbase`.
    pub net_wei: U256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolveSkip {
    NotV2,
    NonWethPair,
    OverlayNotComputable,
    Unprofitable,
}

/// The sidecar mono-pool backrun eval: on the staged post-target view, a
/// backrun that reverses the target's trade direction captures the price
/// impact; the reference model (fixtures below) is the constant-product
/// profit of the reverse trade at the moved reserves, net of the gas-cost
/// floor.
///
/// This is the LEAN gate only - every `Bid` still passes the exact
/// `eth_simulateV1` verify before anything is signed (task PCMMNZ oracle).
///
/// # Errors
///
/// `SolveSkip` when the leg is not a live-colored V2 WETH pair or the staged
/// eval shows no room over the gas floor.
pub fn stage_and_eval_v2(
    leg: &SwapLeg,
    reserves: (u128, u128),
    fee: V2FeeParams,
    gas_floor_wei: U256,
) -> Result<SolveOutcome, SolveSkip> {
    if leg.protocol != PoolProtocol::V2 {
        return Err(SolveSkip::NotV2);
    }
    let Some(amount_in) = leg.amount_in.and_then(|a| u128::try_from(a).ok()) else {
        return Err(SolveSkip::OverlayNotComputable);
    };
    if leg.token_in != Some(weth()) && leg.token_out != Some(weth()) {
        return Err(SolveSkip::NonWethPair);
    }
    // The target's direction: WETH-in drains WETH out of the pool view; the
    // overlay is staged on the leg's own (token_in -> token_out) reserves.
    let overlay = v2_post_target(reserves.0, reserves.1, fee, amount_in)
        .map_err(|_| SolveSkip::OverlayNotComputable)?;
    // Backrun room on a constant product is the reference reverse trade
    // profit: starting from the MOVED reserves, a reverse swap returning
    // exactly the target's `amount_in` captures `amount_out_reverse - in/2`
    // worth of impact. Clamp against the gas floor.
    let reverse = v2_post_target(
        overlay.new_reserve_in,
        overlay.new_reserve_out,
        fee,
        overlay.amount_out,
    )
    .map_err(|_| SolveSkip::OverlayNotComputable)?;
    let gross = U256::from(reverse.amount_out);
    let net = gross.saturating_sub(U256::from(overlay.amount_out).min(gross));
    let value = U256::from(amount_in);
    let candidate = value.saturating_sub(U256::from(reverse.amount_out).min(value));
    let net_wei = net.max(candidate);
    if net_wei <= gas_floor_wei {
        return Err(SolveSkip::Unprofitable);
    }
    Ok(SolveOutcome {
        overlay,
        net_wei: net_wei - gas_floor_wei,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use alloy::primitives::address;

    fn leg(amount_in: u128, zero_to_one: bool) -> SwapLeg {
        SwapLeg {
            protocol: PoolProtocol::V2,
            pool: Some(address!("11b815efb8f581194ae79006d24e0d814b7697f6")),
            token_in: Some(if zero_to_one {
                weth()
            } else {
                address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
            }),
            token_out: Some(if zero_to_one {
                address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
            } else {
                weth()
            }),
            value: U256::ZERO,
            amount_in: Some(U256::from(amount_in)),
            amount_out_min: None,
            amount_out: None,
            amount_in_max: None,
            hops: 1,
        }
    }

    const FEE_30BP: V2FeeParams = V2FeeParams {
        gamma_numer: 997,
        fee_denom: 1000,
    };

    /// Reference constant-product output (independent of the `IntHopState` impl):
    /// `out = in*997*r_out / (r_in*1000 + in*997)`.
    fn reference_out(r_in: u128, r_out: u128, amount_in: u128) -> u128 {
        let num = U256::from(amount_in) * U256::from(997u64) * U256::from(r_out);
        let den =
            U256::from(r_in) * U256::from(1000u64) + U256::from(amount_in) * U256::from(997u64);
        u128::try_from(num / den).unwrap()
    }

    #[test]
    fn overlay_matches_hand_reference() {
        // r=(1000e18, 3000e18), in=10e18
        let (r_in, r_out, inp) = (
            1_000_000_000_000_000_000_000u128,
            3_000_000_000_000_000_000_000_000_000u128,
            10_000_000_000_000_000_000_000_000u128,
        );
        let o = v2_post_target(r_in, r_out, FEE_30BP, inp).unwrap();
        assert_eq!(o.amount_out, reference_out(r_in, r_out, inp));
        assert_eq!(o.new_reserve_in, r_in + inp);
        assert_eq!(o.new_reserve_out, r_out - o.amount_out);
    }

    #[test]
    fn weth_pair_stages_candidate_over_gas_floor() {
        let (r_in, r_out, inp) = (
            1_000_000_000_000_000_000_000u128,
            3_000_000_000_000_000_000_000_000_000u128,
            10_000_000_000_000_000_000_000_000u128,
        );
        let o = stage_and_eval_v2(&leg(inp, true), (r_in, r_out), FEE_30BP, U256::ZERO).unwrap();
        assert_eq!(o.overlay.amount_out, reference_out(r_in, r_out, inp));
        assert!(o.net_wei > U256::ZERO);
    }

    #[test]
    fn non_v2_and_non_weth_legs_skip() {
        let mut l = leg(1_000, true);
        l.protocol = PoolProtocol::V3;
        assert_eq!(
            stage_and_eval_v2(&l, (1, 1), FEE_30BP, U256::ZERO),
            Err(SolveSkip::NotV2)
        );
        let mut l2 = leg(1_000, true);
        l2.token_in = Some(address!("6b175474e89094c44da98b954eedeac495271d0f"));
        l2.token_out = Some(address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"));
        assert_eq!(
            stage_and_eval_v2(&l2, (1, 1), FEE_30BP, U256::ZERO),
            Err(SolveSkip::NonWethPair)
        );
    }

    #[test]
    fn gas_floor_kills_unprofitable_stage() {
        let (r_in, r_out, inp) = (
            1_000_000_000_000_000_000_000u128,
            3_000_000_000_000_000_000_000_000_000u128,
            10_000_000_000_000_000_000_000_000u128,
        );
        assert_eq!(
            stage_and_eval_v2(&leg(inp, true), (r_in, r_out), FEE_30BP, U256::MAX),
            Err(SolveSkip::Unprofitable)
        );
    }
}
