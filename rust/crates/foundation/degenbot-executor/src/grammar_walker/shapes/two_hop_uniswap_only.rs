//! 2-hop no-NetZero shape (block 5: pure V2/V3).

use super::super::{fits_i128, mechanics, HopFacts};
use crate::composers::ComposerInputs;
use crate::encoders::{AddressTable, SENTINEL_SELF, SENTINEL_WETH};
use crate::grammar_ledger::{FundingSource, Prot};
use crate::grammar_plan::{Plan, PlanStep};

#[expect(clippy::too_many_lines, clippy::needless_return)]
pub(crate) fn derive(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    // ── 2-hop pure V2/V3 families (v2v3, v3v2, v3v3; ADR-031 D6).
    let (fa, fb) = (&facts[0], &facts[1]);
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    if forward_out == 0 {
        return None;
    }
    if !fits_i128(optimal_input) {
        return None;
    }
    let terminal_out = *inputs.hop_outputs.get(1)?;
    let weth = inputs.weth_address;
    let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
    let a_idx = at.add(fa.pool_address).ok()?;
    let b_idx = at.add(fb.pool_address).ok()?;
    if fa.prot == Prot::V2 {
        // ── v2v3: funding-branched (SelfFund / FlashFund).
        // ── 2-hop V2→V3 shape (folded from derive_2hop_v2v3; ADR-031 D6).
        let b_swap_in = *inputs.consumed_inputs.get(1)?;
        if !fits_i128(b_swap_in) {
            return None;
        }
        let plan: Plan = if inputs.opts.funding == FundingSource::SelfFund {
            vec![
                PlanStep::SelfFund {
                    currency: weth,
                    amount: optimal_input,
                },
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: a_idx,
                    amount: optimal_input,
                    seeds_pool: Some(fa.pool_address),
                    repays_flash: None,
                },
                mechanics::v2_swap(&mut at, fa, forward_out, SENTINEL_SELF, None, false)?,
                mechanics::v3_flash(&mut at, fb, terminal_out, b_swap_in, true, None, vec![])?,
            ]
        } else {
            let forward_idx = at.add(fa.out_currency).ok()?;
            let inner_v3 = mechanics::v3_flash(
                &mut at,
                fb,
                terminal_out,
                b_swap_in,
                false,
                None,
                vec![PlanStep::Erc20Transfer {
                    token_idx: forward_idx,
                    token_addr: fa.out_currency,
                    recipient_idx: b_idx,
                    amount: b_swap_in,
                    seeds_pool: None,
                    repays_flash: Some(fb.pool_address),
                }],
            )?;
            vec![mechanics::v2_flash(
                &mut at,
                fa,
                forward_out,
                weth,
                optimal_input,
                vec![
                    inner_v3,
                    PlanStep::Erc20Transfer {
                        token_idx: SENTINEL_WETH,
                        token_addr: weth,
                        recipient_idx: a_idx,
                        amount: optimal_input,
                        seeds_pool: None,
                        repays_flash: Some(fa.pool_address),
                    },
                ],
            )?]
        };
        return Some((plan, at));
    }
    // ── v3x (v3v2, v3v3): always SelfFund; terminal by fb.prot.
    // ── 2-hop V3-led shape (folded from derive_2hop_v3x; ADR-031 D6).
    let mut callback: Plan = vec![PlanStep::Erc20Transfer {
        token_idx: SENTINEL_WETH,
        token_addr: weth,
        recipient_idx: a_idx,
        amount: optimal_input,
        seeds_pool: None,
        repays_flash: Some(fa.pool_address),
    }];
    if fb.prot == Prot::V2 {
        let forward_idx = at.add(fa.out_currency).ok()?;
        callback.push(PlanStep::Erc20Transfer {
            token_idx: forward_idx,
            token_addr: fa.out_currency,
            recipient_idx: b_idx,
            amount: forward_out,
            seeds_pool: Some(fb.pool_address),
            repays_flash: None,
        });
        callback.push(mechanics::v2_swap(
            &mut at,
            fb,
            terminal_out,
            SENTINEL_SELF,
            None,
            false,
        )?);
    } else {
        let b_swap_in = *inputs.consumed_inputs.get(1)?;
        if !fits_i128(b_swap_in) {
            return None;
        }
        let terminal =
            mechanics::v3_flash(&mut at, fb, terminal_out, b_swap_in, true, None, vec![])?;
        callback.push(terminal);
    }
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        mechanics::v3_flash(
            &mut at,
            fa,
            forward_out,
            optimal_input,
            false,
            None,
            callback,
        )?,
    ];
    return Some((plan, at));
}
