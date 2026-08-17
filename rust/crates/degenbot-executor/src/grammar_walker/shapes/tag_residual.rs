//! Tag-driven residual shape (block 6 fallthrough).

use super::super::{fits_i128, mechanics, HopFacts, Repay};
use crate::composers::ComposerInputs;
use crate::encoders::{AddressTable, SENTINEL_WETH};
use crate::grammar_plan::{Plan, PlanStep};
use crate::grammar_shape::v4_scaffold_table;

pub(crate) fn derive(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    // ── Enclosure partition from the `Repay` tags (single-V4-middle shape).
    let mut selfrefund: Option<usize> = None;
    let mut offstream: Option<usize> = None;
    let mut netzero: Option<usize> = None;
    for (i, f) in facts.iter().enumerate() {
        match f.repay {
            Repay::SelfRefund => {
                debug_assert!(selfrefund.is_none(), "one SelfRefund hop per shape");
                selfrefund = Some(i);
            }
            Repay::Offstream => {
                debug_assert!(offstream.is_none(), "one Offstream hop per shape");
                offstream = Some(i);
            }
            Repay::NetZero => {
                debug_assert!(netzero.is_none(), "one NetZero hop per shape");
                netzero = Some(i);
            }
        }
    }
    let (li, mi, ti) = (selfrefund?, netzero?, offstream?);
    let (lf, mf, tf) = (&facts[li], &facts[mi], &facts[ti]);

    // ── Guards (mirror the hand-authored producer's decline partition).
    let optimal_input = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_i128(optimal_input) {
        return None;
    }
    let mid_in = *inputs.consumed_inputs.get(mi)?;
    let term_in = *inputs.consumed_inputs.get(ti)?;
    if !fits_i128(mid_in) || !fits_i128(term_in) {
        return None;
    }

    // ── AddressTable order must mirror the golden bytes: pm, leading pool,
    // terminal pool, leading-forward, mid-forward, mid c0, mid c1.
    let mut at = v4_scaffold_table(inputs);
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let leading_pool = at.add(lf.pool_address).ok()?;
    let term_pool = at.add(tf.pool_address).ok()?;
    let forward_lead = at.add(lf.out_currency).ok()?;
    let _ = at.add(mf.out_currency).ok()?; // order-preserving; v4_take re-resolves
    let _ = at.add(mf.currency0_address).ok()?; // order-preserving; v4_swap re-resolves
    let _ = at.add(mf.currency1_address).ok()?; // order-preserving; v4_swap re-resolves
    let weth = inputs.weth_address;
    let out_lead = *inputs.hop_outputs.get(li)?;
    let out_mid = *inputs.hop_outputs.get(mi)?;
    let out_term = *inputs.hop_outputs.get(ti)?;

    // ── The V4 middle ledger (from the `NetZero` facts): settle the leading
    // hop's PM forward, swap, take to the terminal (repaying its flash), net.
    // All built through the V4 mechanics fns — the reference's V4 middle is
    // facts-driven like its V3 hops (T3). `forward_mid` (the V4 out_currency
    // index) is re-resolved idempotently by `v4_take_compact`.
    let v4_inner: Plan = vec![
        mechanics::v4_settle(lf.out_currency, mid_in),
        mechanics::v4_swap(&mut at, mf, mid_in, out_mid)?,
        mechanics::v4_take_compact(&mut at, mf, term_pool, term_in, Some(tf.pool_address))?,
        mechanics::v4_settle_all(),
    ];

    // ── DERIVED ENCLOSURE: the `Offstream` terminal is the OUTERMOST flash;
    // the `SelfRefund` leading is INNER (WETH self-refund then the V4 unlock);
    // the `NetZero` V4 lives inside the unlock. The leading `out_dest`
    // (PoolManager) seeds the PM ledger.
    let leading_callback: Vec<PlanStep> = vec![
        PlanStep::Erc20Transfer {
            token_idx: SENTINEL_WETH,
            token_addr: weth,
            recipient_idx: leading_pool,
            amount: optimal_input,
            seeds_pool: None,
            repays_flash: Some(lf.pool_address),
        },
        mechanics::v4_unlock(v4_inner, pm_idx),
    ];
    let leading_flash = mechanics::v3_flash(
        &mut at,
        lf,
        out_lead,
        optimal_input,
        false,
        leading_callback,
    )?;
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::V4Sync {
            currency_idx: forward_lead,
            currency_addr: lf.out_currency,
        },
        mechanics::v3_flash(&mut at, tf, out_term, term_in, false, vec![leading_flash])?,
    ];
    Some((plan, at))
}
