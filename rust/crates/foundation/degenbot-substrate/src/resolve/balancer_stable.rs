//! Balancer stable-pool resolve row — the registry-lookup + solver-vocabulary
//! wrap seam (ADR-073 D2).
//!
//! The projection itself is the family read module's door —
//! `degenbot_pools::families::balancer_stable::capture_hop` — a pure
//! function of the family state slice (pairwise (0/1) hop over the
//! BPT-skipped, 18-decimal-upscaled balances with the pre-computed
//! `invariant`). This file keeps ONLY the state-owner seam: the registry
//! lookups, the refusal mapping, the nonce, and the one `ResolvedHop` wrap.

use degenbot_pools::families::balancer_stable as stable_read;
use degenbot_pools::solve::mixed::{BalancerStableHopState, MixedPoolRef, ResolvedHop};

use super::super::BotState;
use super::MissingHopReason;

/// Balancer-stable resolve row: read pool state + identity off `core`
/// (ADR-003), then the family read module's [`stable_read::capture_hop`] door
/// derives the BPT-skipped upscaled hop payload (invariant pre-computed
/// one-shot) the stable solve branch consumes. The row wraps the
/// solver-vocabulary hop (`ResolvedHop::BalancerStable`).
pub(crate) fn project_balancer_stable(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    let id = core
        .pool_slice::<degenbot_pools::balancer_stable_state::BalancerStablePoolIdentity>(
            pool_ref.pool_key,
        )
        .ok_or(MissingHopReason::MissingIdentity)?;
    let state = core
        .pool_slice::<degenbot_pools::balancer_stable_state::BalancerStablePoolState>(
            pool_ref.pool_key,
        )
        .ok_or(MissingHopReason::MissingState)?;
    let hop = stable_read::capture_hop(state, id, pool_ref.zero_for_one)
        .map_err(balancer_stable_refusal)?;
    Ok((
        ResolvedHop::BalancerStable {
            state: BalancerStableHopState {
                amp: hop.amp,
                balances: hop.balances,
                token_index_in: hop.token_index_in,
                token_index_out: hop.token_index_out,
                invariant: hop.invariant,
                swap_fee: hop.swap_fee,
                scaling_factor_in: hop.scaling_factor_in,
                scaling_factor_out: hop.scaling_factor_out,
            },
        },
        state.state_nonce,
    ))
}

/// Map the family read module's capture refusal onto the resolve
/// invalidation vocabulary — the row's one mapping site.
fn balancer_stable_refusal(refusal: stable_read::CaptureRefusal) -> MissingHopReason {
    match refusal {
        stable_read::CaptureRefusal::TooFewTokens => MissingHopReason::TooFewTokens,
        stable_read::CaptureRefusal::OutOfRange => MissingHopReason::OutOfRange,
        stable_read::CaptureRefusal::InvariantError => MissingHopReason::InvariantError,
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::super::MissingHopReason;
    use super::project_balancer_stable;
    use crate::{BotState, RegisterBalancerStablePoolParams};
    use alloy::primitives::{Address, U256};
    use degenbot_pools::solve::mixed::{HopType, MixedPoolRef, ResolvedHop};

    fn bs_ref(pool_key: u64, zero_for_one: bool) -> MixedPoolRef {
        MixedPoolRef {
            hop_type: HopType::BalancerStable,
            pool_key,
            zero_for_one,
        }
    }

    fn register_balancer_stable(
        core: &mut BotState,
        addr_byte: u8,
        balance0: u128,
        balance1: u128,
    ) -> u64 {
        let one_e18 = U256::from(10u64).pow(U256::from(18u64));
        core.register_balancer_stable_pool(&RegisterBalancerStablePoolParams {
            address: Address::from([addr_byte; 20]),
            vault: Address::repeat_byte(0xba),
            pool_id: [0u8; 32],
            tokens: vec![Address::repeat_byte(0x01), Address::repeat_byte(0x02)],
            // amp=200_000 = raw_amp(200) * AMP_PRECISION(1000).
            amp: 200_000,
            scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
            swap_fee: 10_000_000_000_000u128, // 0.01% of 1e18
            bpt_idx: None,
            invariant_version: 2,
            balances: vec![
                U256::from(balance0) * one_e18,
                U256::from(balance1) * one_e18,
            ],
            update_block: 0,
            rate_provider: None,
        })
    }

    #[test]
    fn project_balancer_stable_builds_hop_with_invariant() {
        let mut core = BotState::new();
        let bs_id = register_balancer_stable(&mut core, 0xe1, 1000, 2000);

        let (hop, _) = project_balancer_stable(&core, &bs_ref(bs_id, true)).unwrap();
        let ResolvedHop::BalancerStable { state: s } = hop else {
            panic!("hop is not BalancerStable");
        };
        let one_e18 = U256::from(10u64).pow(U256::from(18u64));
        assert_eq!(s.amp, U256::from(200_000u64));
        assert_eq!(s.token_index_in, 0);
        assert_eq!(s.token_index_out, 1);
        assert_eq!(
            s.balances,
            vec![U256::from(1000u64) * one_e18, U256::from(2000u64) * one_e18]
        );
        assert!(s.invariant > U256::ZERO, "invariant of positive balances");
        assert_eq!(s.swap_fee, U256::from(10_000_000_000_000u64));

        // Orientation flips the pairwise indices.
        let (hop, nonce) = project_balancer_stable(&core, &bs_ref(bs_id, false)).unwrap();
        let ResolvedHop::BalancerStable { state: s } = hop else {
            panic!("hop is not BalancerStable");
        };
        assert_eq!(s.token_index_in, 1);
        assert_eq!(s.token_index_out, 0);
        assert_eq!(
            nonce,
            core.pool_slice::<degenbot_pools::balancer_stable_state::BalancerStablePoolState>(
                bs_id
            )
            .expect("state")
            .state_nonce
        );
    }

    #[test]
    fn project_balancer_stable_reason_qa() {
        // QA of the reachable failure -> variant mapping.
        // Unregistered pool: identity is checked FIRST in this arm ->
        // MissingIdentity.
        // Not constructible via the registration API (documented here so the
        // QA stays honest): TooFewTokens (registration validates the vector
        // shapes), OutOfRange (the bpt-skip bounds track the upscaled length
        // for a well-formed pool), InvariantError (the invariant math does
        // not err on sane positive balances). Those three remain the
        // defensive mappings for malformed or future state.
        let core = BotState::new();
        let reason = project_balancer_stable(&core, &bs_ref(888_888, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::MissingIdentity);
    }
}
