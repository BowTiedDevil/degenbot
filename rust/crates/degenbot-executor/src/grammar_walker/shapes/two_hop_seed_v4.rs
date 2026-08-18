//! 2-hop seed→V4 shape (block 2: SelfRefund lead, NetZero tail).

use super::super::{fits_i128, HopFacts};
use crate::composers::ComposerInputs;
use crate::composers::NATIVE_CURRENCY_ADDRESS;
use crate::encoders::{AddressTable, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH};
use crate::grammar_ledger::Prot;
use crate::grammar_plan::{Plan, PlanStep};

#[expect(clippy::too_many_lines, clippy::needless_return)]
pub(crate) fn derive(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    return if facts[0].prot == Prot::V3 {
        // ── v3v4 body (unchanged) ──
        {
            let (fa, fb) = (&facts[0], &facts[1]);
            let forward_out = inputs.hop_outputs[0];
            let v4_out_amount = inputs.hop_outputs[1];
            if forward_out == 0 || v4_out_amount == 0 {
                return None;
            }
            if !fits_i128(forward_out) || !fits_i128(v4_out_amount) {
                return None;
            }
            let v4_swap_in = *inputs.consumed_inputs.get(1)?;
            if !fits_i128(v4_swap_in) {
                return None;
            }
            let v4_in_currency = fb.in_currency;
            let v4_out_currency = fb.out_currency;
            if v4_out_currency == NATIVE_CURRENCY_ADDRESS {
                {
                    let optimal_input = inputs.optimal_input;
                    let forward_out = inputs.hop_outputs[0];
                    let v4_out_amount = inputs.hop_outputs[1];
                    let v4_swap_in = *inputs.consumed_inputs.get(1)?;
                    let weth = inputs.weth_address;
                    let forward_addr = fa.out_currency;
                    let v3_in_currency = fa.in_currency;
                    if v3_in_currency != weth
                        || forward_addr == NATIVE_CURRENCY_ADDRESS
                        || v4_in_currency != forward_addr
                        || v4_in_currency == NATIVE_CURRENCY_ADDRESS
                    {
                        return None;
                    }
                    let mut at = AddressTable::with_sentinels(
                        Some(weth),
                        Some(inputs.executor_address),
                        Some(inputs.pool_manager_address),
                    );
                    let v3_idx = at.add(fa.pool_address).ok()?;
                    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
                    let c0_b = at.add(fb.currency0_address).ok()?;
                    let c1_b = at.add(fb.currency1_address).ok()?;
                    let forward_idx = at.add(forward_addr).ok()?;
                    let weth_idx = SENTINEL_WETH;
                    let fee_b = fb.swap_fee;
                    let ts_b = fb.tick_spacing;
                    let v4_inner: Plan = vec![
                        PlanStep::V4Sync {
                            currency_idx: forward_idx,
                            currency_addr: forward_addr,
                        },
                        PlanStep::Erc20Transfer {
                            token_idx: forward_idx,
                            token_addr: forward_addr,
                            recipient_idx: pm_idx,
                            amount: forward_out,
                            seeds_pool: None,
                            repays_flash: None,
                        },
                        PlanStep::V4Settle {
                            currency_addr: forward_addr,
                            amount: forward_out,
                        },
                        PlanStep::V4Swap {
                            c0_idx: c0_b,
                            c1_idx: c1_b,
                            fee: fee_b,
                            tick_spacing: ts_b,
                            hooks_idx: SENTINEL_NATIVE,
                            zfo: fb.zfo,
                            amount: v4_swap_in,
                            in_currency: forward_addr,
                            in_amount: v4_swap_in,
                            out_currency: NATIVE_CURRENCY_ADDRESS,
                            out_amount: v4_out_amount,
                        },
                        PlanStep::V4TakeCompact {
                            currency_idx: SENTINEL_NATIVE,
                            currency_addr: NATIVE_CURRENCY_ADDRESS,
                            recipient_idx: SENTINEL_SELF,
                            amount: v4_out_amount,
                            seeds_pool: None,
                            repays_flash: None,
                        },
                        PlanStep::V4SettleAll,
                    ];
                    let callback: Plan = vec![
                        PlanStep::V4Unlock {
                            inner: v4_inner,
                            pool_manager_idx: pm_idx,
                        },
                        PlanStep::Erc20Transfer {
                            token_idx: weth_idx,
                            token_addr: weth,
                            recipient_idx: v3_idx,
                            amount: optimal_input,
                            seeds_pool: None,
                            repays_flash: Some(fa.pool_address),
                        },
                    ];
                    let plan: Plan = vec![
                        PlanStep::SelfFund {
                            currency: weth,
                            amount: optimal_input,
                        },
                        PlanStep::FlashSwap {
                            pool_idx: v3_idx,
                            pool_addr: fa.pool_address,
                            protocol: Prot::V3,
                            zfo: fa.zfo,
                            fee: fa.swap_fee,
                            out_currency: forward_addr,
                            out_amount: forward_out,
                            in_currency: weth,
                            in_amount: optimal_input,
                            recipient_idx: SENTINEL_SELF,
                            recipient_pool_addr: None,
                            recipient_pool_repays: false,
                            auto_repay: false,
                            callback,
                        },
                    ];
                    Some((plan, at))
                }
            } else if v4_in_currency == NATIVE_CURRENCY_ADDRESS {
                {
                    let optimal_input = inputs.optimal_input;
                    let forward_out = inputs.hop_outputs[0];
                    let v4_out_amount = inputs.hop_outputs[1];
                    let v4_swap_in = *inputs.consumed_inputs.get(1)?;
                    let weth = inputs.weth_address;
                    let tok = fa.in_currency;
                    if tok == weth || tok == NATIVE_CURRENCY_ADDRESS {
                        return None;
                    }
                    let mut at = AddressTable::with_sentinels(
                        Some(weth),
                        Some(inputs.executor_address),
                        Some(inputs.pool_manager_address),
                    );
                    let v3_idx = at.add(fa.pool_address).ok()?;
                    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
                    let c0_b = at.add(fb.currency0_address).ok()?;
                    let c1_b = at.add(fb.currency1_address).ok()?;
                    let tok_idx = at.add(tok).ok()?;
                    let weth_idx = SENTINEL_WETH;
                    let output_idx = if fb.zfo { c1_b } else { c0_b };
                    let fee_b = fb.swap_fee;
                    let ts_b = fb.tick_spacing;
                    let v4_inner: Plan = vec![
                        PlanStep::V4Swap {
                            c0_idx: c0_b,
                            c1_idx: c1_b,
                            fee: fee_b,
                            tick_spacing: ts_b,
                            hooks_idx: SENTINEL_NATIVE,
                            zfo: fb.zfo,
                            amount: v4_swap_in,
                            in_currency: NATIVE_CURRENCY_ADDRESS,
                            in_amount: v4_swap_in,
                            out_currency: v4_out_currency,
                            out_amount: v4_out_amount,
                        },
                        PlanStep::NativeTransfer { amount: v4_swap_in },
                        PlanStep::V4SettleDelta {
                            currency_idx: SENTINEL_NATIVE,
                            currency_addr: NATIVE_CURRENCY_ADDRESS,
                        },
                        PlanStep::V4TakeCompact {
                            currency_idx: output_idx,
                            currency_addr: v4_out_currency,
                            recipient_idx: SENTINEL_SELF,
                            amount: v4_out_amount,
                            seeds_pool: None,
                            repays_flash: None,
                        },
                        PlanStep::V4SettleAll,
                    ];
                    let callback: Plan = vec![
                        PlanStep::WethWithdraw {
                            weth_idx,
                            weth_addr: weth,
                            amount: forward_out,
                        },
                        PlanStep::V4Unlock {
                            inner: v4_inner,
                            pool_manager_idx: pm_idx,
                        },
                        PlanStep::Erc20Transfer {
                            token_idx: tok_idx,
                            token_addr: tok,
                            recipient_idx: v3_idx,
                            amount: optimal_input,
                            seeds_pool: None,
                            repays_flash: Some(fa.pool_address),
                        },
                    ];
                    let plan: Plan = vec![
                        PlanStep::SelfFund {
                            currency: tok,
                            amount: optimal_input,
                        },
                        PlanStep::FlashSwap {
                            pool_idx: v3_idx,
                            pool_addr: fa.pool_address,
                            protocol: Prot::V3,
                            zfo: fa.zfo,
                            fee: fa.swap_fee,
                            out_currency: weth,
                            out_amount: forward_out,
                            in_currency: tok,
                            in_amount: optimal_input,
                            recipient_idx: SENTINEL_SELF,
                            recipient_pool_addr: None,
                            recipient_pool_repays: false,
                            auto_repay: false,
                            callback,
                        },
                    ];
                    Some((plan, at))
                }
            } else {
                {
                    let optimal_input = inputs.optimal_input;
                    let forward_out = inputs.hop_outputs[0];
                    let v4_out_amount = inputs.hop_outputs[1];
                    let v4_swap_in = *inputs.consumed_inputs.get(1)?;
                    let weth = inputs.weth_address;
                    let forward_addr = fa.out_currency;
                    let v3_in_currency = fa.in_currency;
                    if v3_in_currency != weth
                        || forward_addr == NATIVE_CURRENCY_ADDRESS
                        || v4_in_currency != forward_addr
                    {
                        return None;
                    }
                    let mut at = AddressTable::with_sentinels(
                        Some(weth),
                        Some(inputs.executor_address),
                        Some(inputs.pool_manager_address),
                    );
                    let v3_idx = at.add(fa.pool_address).ok()?;
                    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
                    let c0_b = at.add(fb.currency0_address).ok()?;
                    let c1_b = at.add(fb.currency1_address).ok()?;
                    let forward_idx = at.add(forward_addr).ok()?;
                    let weth_idx = SENTINEL_WETH;
                    let output_idx = if fb.zfo { c1_b } else { c0_b };
                    let fee_b = fb.swap_fee;
                    let ts_b = fb.tick_spacing;
                    let v4_inner: Plan = vec![
                        PlanStep::V4Sync {
                            currency_idx: forward_idx,
                            currency_addr: forward_addr,
                        },
                        PlanStep::Erc20Transfer {
                            token_idx: forward_idx,
                            token_addr: forward_addr,
                            recipient_idx: pm_idx,
                            amount: forward_out,
                            seeds_pool: None,
                            repays_flash: None,
                        },
                        PlanStep::V4Settle {
                            currency_addr: forward_addr,
                            amount: forward_out,
                        },
                        PlanStep::V4Swap {
                            c0_idx: c0_b,
                            c1_idx: c1_b,
                            fee: fee_b,
                            tick_spacing: ts_b,
                            hooks_idx: SENTINEL_NATIVE,
                            zfo: fb.zfo,
                            amount: v4_swap_in,
                            in_currency: forward_addr,
                            in_amount: v4_swap_in,
                            out_currency: v4_out_currency,
                            out_amount: v4_out_amount,
                        },
                        PlanStep::V4TakeCompact {
                            currency_idx: output_idx,
                            currency_addr: v4_out_currency,
                            recipient_idx: SENTINEL_SELF,
                            amount: v4_out_amount,
                            seeds_pool: None,
                            repays_flash: None,
                        },
                        PlanStep::V4SettleAll,
                    ];
                    let callback: Plan = vec![
                        PlanStep::V4Unlock {
                            inner: v4_inner,
                            pool_manager_idx: pm_idx,
                        },
                        PlanStep::Erc20Transfer {
                            token_idx: weth_idx,
                            token_addr: weth,
                            recipient_idx: v3_idx,
                            amount: optimal_input,
                            seeds_pool: None,
                            repays_flash: Some(fa.pool_address),
                        },
                    ];
                    let flash = PlanStep::FlashSwap {
                        pool_idx: v3_idx,
                        pool_addr: fa.pool_address,
                        protocol: Prot::V3,
                        zfo: fa.zfo,
                        fee: fa.swap_fee,
                        out_currency: forward_addr,
                        out_amount: forward_out,
                        in_currency: weth,
                        in_amount: optimal_input,
                        recipient_idx: SENTINEL_SELF,
                        recipient_pool_addr: None,
                        recipient_pool_repays: false,
                        auto_repay: false,
                        callback,
                    };
                    let plan: Plan = if v4_out_currency == weth {
                        vec![flash]
                    } else {
                        vec![
                            PlanStep::SelfFund {
                                currency: weth,
                                amount: optimal_input,
                            },
                            flash,
                        ]
                    };
                    Some((plan, at))
                }
            }
        }
    } else {
        // ── v2v4 body (unchanged) ──
        {
            let (fa, fb) = (&facts[0], &facts[1]);
            let forward_out = inputs.hop_outputs[0];
            let v4_out_amount = inputs.hop_outputs[1];
            if forward_out == 0 || v4_out_amount == 0 {
                return None;
            }
            if !fits_i128(forward_out) || !fits_i128(v4_out_amount) {
                return None;
            }
            let v4_swap_in = *inputs.consumed_inputs.get(1)?;
            if !fits_i128(v4_swap_in) {
                return None;
            }
            let v4_in_currency = fb.in_currency;
            let v4_out_currency = fb.out_currency;
            if v4_out_currency == NATIVE_CURRENCY_ADDRESS {
                {
                    let forward_out = inputs.hop_outputs[0];
                    let v4_out_amount = inputs.hop_outputs[1];
                    let v4_swap_in = *inputs.consumed_inputs.get(1)?;
                    let weth = inputs.weth_address;
                    let optimal_input = inputs.optimal_input;
                    let forward_addr = fa.out_currency;
                    let v2_in_currency = fa.in_currency;
                    if v2_in_currency != weth
                        || forward_addr == NATIVE_CURRENCY_ADDRESS
                        || v4_in_currency != forward_addr
                        || v4_in_currency == NATIVE_CURRENCY_ADDRESS
                    {
                        return None;
                    }
                    let mut at = AddressTable::with_sentinels(
                        Some(weth),
                        Some(inputs.executor_address),
                        Some(inputs.pool_manager_address),
                    );
                    let v2_idx = at.add(fa.pool_address).ok()?;
                    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
                    let c0_b = at.add(fb.currency0_address).ok()?;
                    let c1_b = at.add(fb.currency1_address).ok()?;
                    let forward_idx = at.add(forward_addr).ok()?;
                    let weth_idx = SENTINEL_WETH;
                    let fee_b = fb.swap_fee;
                    let ts_b = fb.tick_spacing;
                    let v4_inner: Plan = vec![
                        PlanStep::V4Sync {
                            currency_idx: forward_idx,
                            currency_addr: forward_addr,
                        },
                        PlanStep::Erc20Transfer {
                            token_idx: forward_idx,
                            token_addr: forward_addr,
                            recipient_idx: pm_idx,
                            amount: forward_out,
                            seeds_pool: None,
                            repays_flash: None,
                        },
                        PlanStep::V4Settle {
                            currency_addr: forward_addr,
                            amount: forward_out,
                        },
                        PlanStep::V4Swap {
                            c0_idx: c0_b,
                            c1_idx: c1_b,
                            fee: fee_b,
                            tick_spacing: ts_b,
                            hooks_idx: SENTINEL_NATIVE,
                            zfo: fb.zfo,
                            amount: v4_swap_in,
                            in_currency: forward_addr,
                            in_amount: v4_swap_in,
                            out_currency: NATIVE_CURRENCY_ADDRESS,
                            out_amount: v4_out_amount,
                        },
                        PlanStep::V4TakeCompact {
                            currency_idx: SENTINEL_NATIVE,
                            currency_addr: NATIVE_CURRENCY_ADDRESS,
                            recipient_idx: SENTINEL_SELF,
                            amount: v4_out_amount,
                            seeds_pool: None,
                            repays_flash: None,
                        },
                        PlanStep::V4SettleAll,
                    ];
                    let callback: Plan = vec![
                        PlanStep::V4Unlock {
                            inner: v4_inner,
                            pool_manager_idx: pm_idx,
                        },
                        PlanStep::WethDeposit {
                            weth_idx,
                            weth_addr: weth,
                            amount: v4_out_amount,
                        },
                        PlanStep::Erc20Transfer {
                            token_idx: weth_idx,
                            token_addr: weth,
                            recipient_idx: v2_idx,
                            amount: optimal_input,
                            seeds_pool: None,
                            repays_flash: Some(fa.pool_address),
                        },
                    ];
                    let plan: Plan = vec![PlanStep::FlashSwap {
                        pool_idx: v2_idx,
                        pool_addr: fa.pool_address,
                        protocol: Prot::V2,
                        zfo: fa.zfo,
                        fee: fa.swap_fee,
                        out_currency: forward_addr,
                        out_amount: forward_out,
                        in_currency: weth,
                        in_amount: optimal_input,
                        recipient_idx: SENTINEL_SELF,
                        recipient_pool_addr: None,
                        recipient_pool_repays: false,
                        auto_repay: false,
                        callback,
                    }];
                    Some((plan, at))
                }
            } else if v4_in_currency == NATIVE_CURRENCY_ADDRESS {
                {
                    let forward_out = inputs.hop_outputs[0];
                    let v4_out_amount = inputs.hop_outputs[1];
                    let v4_swap_in = *inputs.consumed_inputs.get(1)?;
                    let weth = inputs.weth_address;
                    let optimal_input = inputs.optimal_input;
                    let tok = fa.in_currency;
                    if tok == weth || tok == NATIVE_CURRENCY_ADDRESS {
                        return None;
                    }
                    let mut at = AddressTable::with_sentinels(
                        Some(weth),
                        Some(inputs.executor_address),
                        Some(inputs.pool_manager_address),
                    );
                    let v2_idx = at.add(fa.pool_address).ok()?;
                    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
                    let c0_b = at.add(fb.currency0_address).ok()?;
                    let c1_b = at.add(fb.currency1_address).ok()?;
                    let tok_idx = at.add(tok).ok()?;
                    let weth_idx = SENTINEL_WETH;
                    let output_idx = if fb.zfo { c1_b } else { c0_b };
                    let fee_b = fb.swap_fee;
                    let ts_b = fb.tick_spacing;
                    let v4_inner: Plan = vec![
                        PlanStep::V4Swap {
                            c0_idx: c0_b,
                            c1_idx: c1_b,
                            fee: fee_b,
                            tick_spacing: ts_b,
                            hooks_idx: SENTINEL_NATIVE,
                            zfo: fb.zfo,
                            amount: v4_swap_in,
                            in_currency: NATIVE_CURRENCY_ADDRESS,
                            in_amount: v4_swap_in,
                            out_currency: v4_out_currency,
                            out_amount: v4_out_amount,
                        },
                        PlanStep::NativeTransfer { amount: v4_swap_in },
                        PlanStep::V4SettleDelta {
                            currency_idx: SENTINEL_NATIVE,
                            currency_addr: NATIVE_CURRENCY_ADDRESS,
                        },
                        PlanStep::V4TakeCompact {
                            currency_idx: output_idx,
                            currency_addr: v4_out_currency,
                            recipient_idx: SENTINEL_SELF,
                            amount: v4_out_amount,
                            seeds_pool: None,
                            repays_flash: None,
                        },
                        PlanStep::V4SettleAll,
                    ];
                    let callback: Plan = vec![
                        PlanStep::WethWithdraw {
                            weth_idx,
                            weth_addr: weth,
                            amount: forward_out,
                        },
                        PlanStep::V4Unlock {
                            inner: v4_inner,
                            pool_manager_idx: pm_idx,
                        },
                        PlanStep::Erc20Transfer {
                            token_idx: tok_idx,
                            token_addr: tok,
                            recipient_idx: v2_idx,
                            amount: optimal_input,
                            seeds_pool: None,
                            repays_flash: Some(fa.pool_address),
                        },
                    ];
                    let plan: Plan = vec![
                        PlanStep::SelfFund {
                            currency: tok,
                            amount: optimal_input,
                        },
                        PlanStep::FlashSwap {
                            pool_idx: v2_idx,
                            pool_addr: fa.pool_address,
                            protocol: Prot::V2,
                            zfo: fa.zfo,
                            fee: fa.swap_fee,
                            out_currency: weth,
                            out_amount: forward_out,
                            in_currency: tok,
                            in_amount: optimal_input,
                            recipient_idx: SENTINEL_SELF,
                            recipient_pool_addr: None,
                            recipient_pool_repays: false,
                            auto_repay: false,
                            callback,
                        },
                    ];
                    Some((plan, at))
                }
            } else {
                {
                    let forward_out = inputs.hop_outputs[0];
                    let v4_out_amount = inputs.hop_outputs[1];
                    let v4_swap_in = *inputs.consumed_inputs.get(1)?;
                    let weth = inputs.weth_address;
                    let optimal_input = inputs.optimal_input;
                    let forward_addr = fa.out_currency;
                    let v2_in_currency = fa.in_currency;
                    if v2_in_currency != weth
                        || forward_addr == NATIVE_CURRENCY_ADDRESS
                        || v4_in_currency != forward_addr
                    {
                        return None;
                    }
                    let mut at = AddressTable::with_sentinels(
                        Some(weth),
                        Some(inputs.executor_address),
                        Some(inputs.pool_manager_address),
                    );
                    let v2_idx = at.add(fa.pool_address).ok()?;
                    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
                    let c0_b = at.add(fb.currency0_address).ok()?;
                    let c1_b = at.add(fb.currency1_address).ok()?;
                    let forward_idx = at.add(forward_addr).ok()?;
                    let weth_idx = SENTINEL_WETH;
                    let output_idx = if fb.zfo { c1_b } else { c0_b };
                    let fee_b = fb.swap_fee;
                    let ts_b = fb.tick_spacing;
                    let v4_inner: Plan = vec![
                        PlanStep::V4Sync {
                            currency_idx: forward_idx,
                            currency_addr: forward_addr,
                        },
                        PlanStep::Erc20Transfer {
                            token_idx: forward_idx,
                            token_addr: forward_addr,
                            recipient_idx: pm_idx,
                            amount: forward_out,
                            seeds_pool: None,
                            repays_flash: None,
                        },
                        PlanStep::V4Settle {
                            currency_addr: forward_addr,
                            amount: forward_out,
                        },
                        PlanStep::V4Swap {
                            c0_idx: c0_b,
                            c1_idx: c1_b,
                            fee: fee_b,
                            tick_spacing: ts_b,
                            hooks_idx: SENTINEL_NATIVE,
                            zfo: fb.zfo,
                            amount: v4_swap_in,
                            in_currency: forward_addr,
                            in_amount: v4_swap_in,
                            out_currency: v4_out_currency,
                            out_amount: v4_out_amount,
                        },
                        PlanStep::V4TakeCompact {
                            currency_idx: output_idx,
                            currency_addr: v4_out_currency,
                            recipient_idx: SENTINEL_SELF,
                            amount: v4_out_amount,
                            seeds_pool: None,
                            repays_flash: None,
                        },
                        PlanStep::V4SettleAll,
                    ];
                    let callback: Plan = vec![
                        PlanStep::V4Unlock {
                            inner: v4_inner,
                            pool_manager_idx: pm_idx,
                        },
                        PlanStep::Erc20Transfer {
                            token_idx: weth_idx,
                            token_addr: weth,
                            recipient_idx: v2_idx,
                            amount: optimal_input,
                            seeds_pool: None,
                            repays_flash: Some(fa.pool_address),
                        },
                    ];
                    let flash = PlanStep::FlashSwap {
                        pool_idx: v2_idx,
                        pool_addr: fa.pool_address,
                        protocol: Prot::V2,
                        zfo: fa.zfo,
                        fee: fa.swap_fee,
                        out_currency: forward_addr,
                        out_amount: forward_out,
                        in_currency: weth,
                        in_amount: optimal_input,
                        recipient_idx: SENTINEL_SELF,
                        recipient_pool_addr: None,
                        recipient_pool_repays: false,
                        auto_repay: false,
                        callback,
                    };
                    let plan: Plan = if v4_out_currency == weth {
                        vec![flash]
                    } else {
                        vec![
                            PlanStep::SelfFund {
                                currency: weth,
                                amount: optimal_input,
                            },
                            flash,
                        ]
                    };
                    Some((plan, at))
                }
            }
        }
    };
}

#[cfg(test)]
mod walk_probe {
    #![expect(
        clippy::print_stdout,
        reason = "T1_CAPTURE diagnostic mode for golden-table capture"
    )]

    use super::super::test_support as tf;
    use crate::composers::PathInfo;

    /// The walk region: everything before the first test module, so the probe
    /// cannot count literals inside its own fixture.
    fn walk_source() -> &'static str {
        let src = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/grammar_walker/shapes/two_hop_seed_v4.rs"
        ));
        let i = src.find("\n#[cfg(test)]").unwrap_or(src.len());
        &src[..i]
    }

    /// RED by design (T1 RKNRJO, epic 6SWFBS): 60 literal `PlanStep::` sites
    /// today. Goes GREEN when T3 (CP6BNJ) walks the v2v4/v3v4 arms onto
    /// `mechanics` + the shared capture/bridge helpers; then it stays put as
    /// the honesty invariant that no per-family Plan bodies reappear (D6
    /// precedent: the RED probe committed at f3b06397, honesty test kept at
    /// DDNEAB).
    #[test]
    fn two_hop_seed_v4_arms_use_mechanics_not_planstep_literals() {
        let src = walk_source();
        let count = src.matches("PlanStep::").count();
        assert_eq!(
            count, 0,
            "two_hop_seed_v4.rs still hand-builds {count} PlanStep sites; the walk must express them via mechanics:: + shared capture/bridge helpers"
        );
    }

    /// Per-file byte-identity pin (T4 gate target): the exact current streams
    /// for every v2v4/v3v4 family × amount-set × `EncodeOptions` combo,
    /// including the decline partition (the SMOZG3 erc6909×batch WETH-terminal
    /// declines stay declined — TGUZCT is still open).
    const FAMILIES: &[(&str, &[&str])] = &[("V2_V4", &["V2", "V4"]), ("V3_V4", &["V3", "V4"])];

    const GOLDEN: &[&str] = &[
        "V2_V4_base_0 923364d759eb212d",
        "V2_V4_base_1 3a1b12cf9f0ec888",
        "V2_V4_base_2 34faecb7315436c5",
        "V2_V4_base_3 3de42bc24146d541",
        "V2_V4_batch_0 923364d759eb212d",
        "V2_V4_batch_1 3a1b12cf9f0ec888",
        "V2_V4_batch_2 34faecb7315436c5",
        "V2_V4_batch_3 3de42bc24146d541",
        "V2_V4_erc6909_0 923364d759eb212d",
        "V2_V4_erc6909_1 3a1b12cf9f0ec888",
        "V2_V4_erc6909_2 34faecb7315436c5",
        "V2_V4_erc6909_3 3de42bc24146d541",
        "V3_V4_base_0 91cede1cd9a55e3f",
        "V3_V4_base_1 e5f550676312ba0e",
        "V3_V4_base_2 14e64c847a39e6db",
        "V3_V4_base_3 8522cd96d6641187",
        "V3_V4_batch_0 91cede1cd9a55e3f",
        "V3_V4_batch_1 e5f550676312ba0e",
        "V3_V4_batch_2 14e64c847a39e6db",
        "V3_V4_batch_3 8522cd96d6641187",
        "V3_V4_erc6909_0 91cede1cd9a55e3f",
        "V3_V4_erc6909_1 e5f550676312ba0e",
        "V3_V4_erc6909_2 14e64c847a39e6db",
        "V3_V4_erc6909_3 8522cd96d6641187",
    ];

    #[test]
    fn two_hop_seed_v4_streams_are_pinned() {
        let mut lines = Vec::new();
        for (fam, combo) in FAMILIES {
            let path = PathInfo::new(tf::build_hops(combo));
            for (label, opt) in tf::opts() {
                for (ci, (optimal, out, consumed)) in tf::configs().iter().enumerate() {
                    lines.push(tf::entry_line(
                        fam,
                        path.clone(),
                        *optimal,
                        &out[..2],
                        &consumed[..2],
                        label,
                        ci,
                        opt,
                    ));
                }
            }
        }
        lines.sort();
        if std::env::var("T1_CAPTURE").is_ok() {
            for l in &lines {
                println!("{l}");
            }
        }
        assert_eq!(
            lines.join("\n"),
            GOLDEN.join("\n"),
            "2-hop seed-V4 streams changed — T1 pin"
        );
    }
}
