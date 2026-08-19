//! Curve stableswap projection.

use alloy::primitives::U256;

use degenbot_solvers::mixed::{CurveStableswapHopState, MixedPoolRef, ResolvedHop};

use super::super::BotState;
use super::MissingHopReason;

/// Curve-stableswap projection: rate-adjusted XP + the pairwise (0/1) variant
/// bytes, all read off `core` (ADR-003).
pub(crate) fn project_curve(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    let id = core
        .get_curve_identity(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingIdentity)?;
    let state = core
        .get_curve_pool(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingState)?;
    if id.tokens.len() < 2 {
        return Err(MissingHopReason::TooFewTokens); // Can't form a pairwise hop
    }
    let (raw_idx_in, raw_idx_out) = if pool_ref.zero_for_one {
        (0, 1)
    } else {
        (1, 0)
    };
    // Curve constants
    let precision = U256::from(10u64).pow(U256::from(18u64));
    let fee_denom = U256::from(10u64).pow(U256::from(10u64));
    let a_precision = U256::from(id.a_precision);
    let amp = U256::from(id.a_coefficient).saturating_mul(a_precision);
    let n_coins = U256::from(id.tokens.len() as u64);
    // Build rate-adjusted XP: xp[i] = balances[i] * rate_multipliers[i] / PRECISION
    let xp: Vec<U256> = state
        .balances
        .iter()
        .zip(id.rate_multipliers.iter())
        .map(|(b, rm)| b.saturating_mul(*rm) / precision)
        .collect();
    if raw_idx_in >= xp.len() || raw_idx_out >= xp.len() {
        return Err(MissingHopReason::OutOfRange);
    }
    let y_variant = degenbot_curve_math::stableswap::YVariant::try_from_u8(id.y_variant)
        .ok_or(MissingHopReason::UnknownVariant)?;
    let d_variant = degenbot_curve_math::stableswap::DVariant::try_from_u8(id.d_variant)
        .ok_or(MissingHopReason::UnknownVariant)?;
    Ok((
        ResolvedHop::CurveStableswap {
            state: CurveStableswapHopState {
                amp,
                a_precision,
                xp,
                token_index_in: raw_idx_in,
                token_index_out: raw_idx_out,
                n_coins,
                fee: U256::from(id.fee),
                fee_denom,
                precision,
                rate_multiplier_in: id.rate_multipliers[raw_idx_in],
                rate_multiplier_out: id.rate_multipliers[raw_idx_out],
                y_variant,
                d_variant,
            },
        },
        state.state_nonce,
    ))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::super::MissingHopReason;
    use super::project_curve;
    use crate::bot_core::{BotState, RegisterCurvePoolParams};
    use alloy::primitives::{Address, U256};
    use degenbot_solvers::mixed::{HopType, MixedPoolRef, ResolvedHop};

    // -----------------------------------------------------------------
    // Per-family projection tests: `project_curve`
    // crosses the seam directly — rate-adjusted XP + pairwise (0/1) variant
    // decode included.
    // -----------------------------------------------------------------

    fn cr_ref(pool_key: u64, zero_for_one: bool) -> MixedPoolRef {
        MixedPoolRef {
            hop_type: HopType::CurveStableswap,
            pool_key,
            zero_for_one,
        }
    }

    fn register_curve(
        core: &mut BotState,
        addr_byte: u8,
        balance0: u128,
        balance1: u128,
        y_variant: u8,
        d_variant: u8,
    ) -> u64 {
        let one_e18 = U256::from(10u64).pow(U256::from(18u64));
        let precision = one_e18;
        core.register_curve_pool(&RegisterCurvePoolParams {
            address: Address::from([addr_byte; 20]),
            tokens: vec![Address::repeat_byte(0x01), Address::repeat_byte(0x02)],
            a_coefficient: 10,
            a_precision: 100,
            fee: 4_000_000, // 0.04% of 1e10
            admin_fee: 0,
            rate_multipliers: vec![precision, precision], // identity rates
            balances: vec![
                U256::from(balance0) * one_e18,
                U256::from(balance1) * one_e18,
            ],
            update_block: 0,
            swap_style: 0,         // STANDARD
            lending_rate_style: 0, // NONE
            d_variant,
            y_variant,
            yd_variant: 1,
            base_pool: None,
            initial_a_coefficient: None,
            future_a_coefficient: None,
            initial_a_coefficient_time: None,
            future_a_coefficient_time: None,
            create_timestamp: None,
            fee_gamma: None,
            mid_fee: None,
            offpeg_fee_multiplier: None,
            out_fee: None,
            gamma: None,
            lp_token: None,
            use_lending: vec![false, false],
            precision_multipliers: vec![precision, precision],
            tokens_underlying: None,
            metapool_rate_style: 0,
            metapool_underlying_style: 0,
            data_provider: None,
        })
    }

    #[test]
    fn project_curve_builds_rate_adjusted_xp_hop() {
        let mut core = BotState::new();
        let cr_id = register_curve(&mut core, 0xd1, 1000, 2000, 1, 1);

        let (hop, _) = project_curve(&core, &cr_ref(cr_id, true)).unwrap();
        let ResolvedHop::CurveStableswap { state: s } = hop else {
            panic!("hop is not CurveStableswap");
        };
        let one_e18 = U256::from(10u64).pow(U256::from(18u64));
        // amp = a_coefficient * a_precision; identity rates -> xp == balances.
        assert_eq!(s.amp, U256::from(10u64) * U256::from(100u64));
        assert_eq!(s.a_precision, U256::from(100u64));
        assert_eq!(
            s.xp,
            vec![U256::from(1000u64) * one_e18, U256::from(2000u64) * one_e18]
        );
        assert_eq!(s.token_index_in, 0);
        assert_eq!(s.token_index_out, 1);
        assert_eq!(s.n_coins, U256::from(2u64));
        assert_eq!(s.fee, U256::from(4_000_000u64));
        assert_eq!(s.fee_denom, U256::from(10u64).pow(U256::from(10u64)));

        // Orientation flips the pairwise indices and the rate multipliers.
        let (hop, nonce) = project_curve(&core, &cr_ref(cr_id, false)).unwrap();
        let ResolvedHop::CurveStableswap { state: s } = hop else {
            panic!("hop is not CurveStableswap");
        };
        assert_eq!(s.token_index_in, 1);
        assert_eq!(s.token_index_out, 0);
        assert_eq!(s.rate_multiplier_in, one_e18);
        assert_eq!(
            nonce,
            core.get_curve_pool(cr_id).expect("state").state_nonce
        );
    }

    #[test]
    fn project_curve_unknown_variant_byte_is_unknown_variant() {
        let mut core = BotState::new();
        let cr_id = register_curve(&mut core, 0xd2, 1000, 2000, 9, 1);
        let reason = project_curve(&core, &cr_ref(cr_id, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::UnknownVariant);
    }

    #[test]
    fn project_curve_reason_qa() {
        // QA of the reachable failure -> variant mapping.
        // Unregistered pool: identity first -> MissingIdentity.
        // Not constructible via the registration API (documented so the QA
        // stays honest): TooFewTokens (token vector shape validated at
        // registration), OutOfRange (the index bounds track xp's length for
        // a well-formed pool). Both remain the defensive mappings for
        // malformed or future state.
        let core = BotState::new();
        let reason = project_curve(&core, &cr_ref(777_777, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::MissingIdentity);
    }
}
