//! CL family projections (V3 + V4) — two SELF-CONTAINED entries.
//!
//! GUARDRAIL (load-bearing, do not fuse): `project_v3` and `project_v4` share
//! only this file, the thin `ResolvedHop::V3/V4` wrap, and the nonce return.
//! The `build_int_v*_sequence` builders in `degenbot-pools` own the three
//! load-bearing differences — fee convention (V3 `gamma = 1e6 − lp_fee` vs V4
//! combined `swapFee`), current-tick drain framing (V3 leading hop vs V4
//! `base_liquidity` fold), and per-range net-sign direction (V4 only).

use degenbot_solvers::mixed::{MixedPoolRef, ResolvedHop};

use super::super::BotState;
use super::MissingHopReason;

/// V3 projection: read pool state + identity off `core` (ADR-003) and build
/// the integer tick-range sequence the CL solver consumes lock-free.
pub(crate) fn project_v3(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    let pool_state = core
        .get_v3_pool(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingState)?;
    let identity = core
        .get_v3_identity(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingIdentity)?;
    let int_seq = pool_state
        .build_int_v3_sequence(
            identity.tick_spacing,
            identity.fee,
            pool_ref.zero_for_one,
            // T47PPB: 24 = the active-set walk feed depth. The
            // enumeration-era value was 10 (tuple cap); the walk
            // has no tuple cap, so depth is bounded by data
            // availability (the range cache stores 24).
            24,
        )
        .ok_or(MissingHopReason::SequenceUnavailable)?;

    Ok((ResolvedHop::V3 { int_seq }, pool_state.state_nonce))
}

/// V4 projection: identical CL math as V3 (BotState-owned, ADR-003), with
/// V4's own fee/protocol-fee convention handled inside
/// `build_int_v4_sequence`.
pub(crate) fn project_v4(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    let pool_state = core
        .get_v4_pool(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingState)?;
    let identity = core
        .get_v4_identity(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingIdentity)?;
    let int_seq = pool_state
        .build_int_v4_sequence(
            identity.pool_key.tick_spacing,
            identity.pool_key.fee,
            pool_ref.zero_for_one,
            // T47PPB: 24 = the active-set walk feed depth (twin of
            // the V3 site above).
            24,
        )
        .ok_or(MissingHopReason::SequenceUnavailable)?;

    // AV42C7-debug: dump V4 solver intermediates for the
    // closed-form vs on-chain divergence hunt. Conservative
    // default ON (`crate::bot_core::bot_env_flag_default_on`);
    // set `DEGENBOT_DEBUG_V4_SOLVE=0` to disable. grep the log
    // for the failing pool_id (from the [sim-fixture] dump) to
    // localize the over-prediction to drain/coverage/range.
    if crate::bot_core::bot_env_flag_default_on("DEGENBOT_DEBUG_V4_SOLVE") {
        let pid_hex = alloy::hex::encode(identity.pool_id);
        let drain: i128 = if pool_ref.zero_for_one {
            pool_state
                .tick_data
                .get(&pool_state.tick)
                .map_or(0, |info| {
                    let bytes = info.liquidity_net.to_be_bytes::<32>();
                    let low: [u8; 16] = bytes[16..32].try_into().unwrap_or([0u8; 16]);
                    i128::from_be_bytes(low)
                })
        } else {
            0
        };
        tracing::debug!(
            pool_manager = ?identity.pool_manager,
            pool_id = %pid_hex,
            zero_for_one = %pool_ref.zero_for_one,
            tick = pool_state.tick,
            liquidity = pool_state.liquidity,
            sqrt_price_x96 = %pool_state.sqrt_price_x96,
            protocol_fee = pool_state.protocol_fee,
            coverage = ?pool_state.coverage,
            n_ranges = int_seq.ranges.len(),
            drain = %drain,
            "[debug-v4-solve] pool details"
        );
    }

    Ok((ResolvedHop::V4 { int_seq }, pool_state.state_nonce))
}
