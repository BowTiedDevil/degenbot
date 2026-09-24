//! all-V2 any-N chain shape (block 1 of the derive_plan dispatcher).

use super::super::{fits_i128, mechanics, HopFacts};
use crate::composers::ComposerInputs;
use crate::encoders::{AddressTable, SENTINEL_SELF};
use crate::grammar_ledger::FundingSource;
use crate::grammar_plan::{Plan, PlanStep};

#[expect(clippy::needless_return)]
pub(crate) fn derive(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let n = facts.len();
    let optimal_input = inputs.optimal_input;
    if !fits_i128(optimal_input) {
        return None;
    }
    let closing = facts[n - 1].out_currency;
    let weth = inputs.weth_address;

    let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
    let pool_idx: Vec<u8> = facts
        .iter()
        .map(|f| at.add(f.pool_address).ok())
        .collect::<Option<Vec<_>>>()?;
    let forward_idx = at.add(facts[0].out_currency).ok()?;
    let closing_idx = at.add(closing).ok()?;

    let plan: Plan = if inputs.opts.funding == FundingSource::SelfFund {
        let mut steps: Plan = vec![
            PlanStep::SelfFund {
                currency: closing,
                amount: optimal_input,
            },
            PlanStep::Erc20Transfer {
                token_idx: closing_idx,
                token_addr: closing,
                recipient_idx: pool_idx[0],
                amount: optimal_input,
                seeds_pool: Some(facts[0].pool_address),
                repays_flash: None,
            },
        ];
        for i in 0..n {
            let terminal = i == n - 1;
            steps.push(mechanics::v2_swap(
                &mut at,
                &facts[i],
                inputs.hop_outputs[i],
                if terminal {
                    SENTINEL_SELF
                } else {
                    pool_idx[i + 1]
                },
                if terminal {
                    None
                } else {
                    Some(facts[i + 1].pool_address)
                },
                false,
            )?);
        }
        steps
    } else {
        let mut callback: Plan = vec![PlanStep::Erc20Transfer {
            token_idx: forward_idx,
            token_addr: facts[0].out_currency,
            recipient_idx: pool_idx[1],
            amount: inputs.hop_outputs[0],
            seeds_pool: Some(facts[1].pool_address),
            repays_flash: None,
        }];
        for i in 1..n {
            let terminal = i == n - 1;
            callback.push(mechanics::v2_swap(
                &mut at,
                &facts[i],
                inputs.hop_outputs[i],
                if terminal {
                    SENTINEL_SELF
                } else {
                    pool_idx[i + 1]
                },
                if terminal {
                    None
                } else {
                    Some(facts[i + 1].pool_address)
                },
                false,
            )?);
        }
        callback.push(PlanStep::Erc20Transfer {
            token_idx: closing_idx,
            token_addr: closing,
            recipient_idx: pool_idx[0],
            amount: optimal_input,
            seeds_pool: None,
            repays_flash: Some(facts[0].pool_address),
        });
        vec![mechanics::v2_flash(
            &mut at,
            &facts[0],
            inputs.hop_outputs[0],
            closing,
            optimal_input,
            callback,
        )?]
    };
    return Some((plan, at));
}
