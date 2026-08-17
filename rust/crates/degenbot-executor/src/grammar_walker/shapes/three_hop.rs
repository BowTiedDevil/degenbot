//! 3-hop mega-block (block 4: no Offstream).

use super::super::{fits_i128, mechanics, HopFacts};
use crate::composers::{resolve_axes, ComposerInputs, CurrencyBridge, NATIVE_CURRENCY_ADDRESS};
use crate::encoders::{AddressTable, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH};
use crate::grammar_ledger::Prot;
use crate::grammar_plan::{Plan, PlanStep, V4BatchSwap};
use crate::grammar_shape::{
    native_capture_declines, v4_bridge_steps, v4_scaffold_table, v4_terminal_capture_steps,
};

#[expect(clippy::too_many_lines, clippy::similar_names, clippy::needless_return)]
pub(crate) fn derive(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    return if facts[0].prot == Prot::V3 && facts[1].prot == Prot::V3 && facts[2].prot == Prot::V3 {
        // ── v3v3v3: 3-deep nested V3 flashes. Hop2 outermost (SELF),
        // hop1 middle (recipient=hop2 pool, rpr=true), hop0 innermost
        // (auto_repay=true, recipient=hop1 pool, rpr=true).
        let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
        let optimal = inputs.optimal_input;
        if inputs.hop_outputs.contains(&0) || !fits_i128(optimal) {
            return None;
        }
        let b_in = *inputs.consumed_inputs.get(1)?;
        let c_in = *inputs.consumed_inputs.get(2)?;
        if !fits_i128(b_in) || !fits_i128(c_in) {
            return None;
        }
        let (out_a, out_b, out_c) = (
            inputs.hop_outputs[0],
            inputs.hop_outputs[1],
            inputs.hop_outputs[2],
        );
        let weth = inputs.weth_address;
        let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
        let _ = at.add(fa.pool_address).ok()?;
        let b_idx = at.add(fb.pool_address).ok()?;
        let c_idx = at.add(fc.pool_address).ok()?;
        let inner_a = mechanics::v3_flash_to(
            &mut at,
            fa,
            out_a,
            optimal,
            true,
            b_idx,
            Some(fb.pool_address),
            true,
            vec![],
        )?;
        let inner_b = mechanics::v3_flash_to(
            &mut at,
            fb,
            out_b,
            b_in,
            false,
            c_idx,
            Some(fc.pool_address),
            true,
            vec![inner_a],
        )?;
        let outer = mechanics::v3_flash_to(
            &mut at,
            fc,
            out_c,
            c_in,
            false,
            SENTINEL_SELF,
            None,
            false,
            vec![inner_b],
        )?;
        return Some((
            vec![
                PlanStep::SelfFund {
                    currency: weth,
                    amount: optimal,
                },
                outer,
            ],
            at,
        ));
    }
    // ── v3v3v2: hop1 outermost (recipient=V2 pool, rpr=false), hop0
    // inner (recipient=hop1 pool, rpr=true) with V2SwapCalc terminal +
    // WETH self-repay callback.
    else if facts[0].prot == Prot::V3 && facts[1].prot == Prot::V3 && facts[2].prot == Prot::V2 {
        let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
        let optimal = inputs.optimal_input;
        if inputs.hop_outputs.contains(&0) || !fits_i128(optimal) {
            return None;
        }
        let b_in = *inputs.consumed_inputs.get(1)?;
        if !fits_i128(b_in) {
            return None;
        }
        let (out_a, out_b, out_c) = (
            inputs.hop_outputs[0],
            inputs.hop_outputs[1],
            inputs.hop_outputs[2],
        );
        let weth = inputs.weth_address;
        let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
        let v2c = at.add(fc.pool_address).ok()?;
        let v3a = at.add(fa.pool_address).ok()?;
        let v3b = at.add(fb.pool_address).ok()?;
        let term = mechanics::v2_swap(&mut at, fc, out_c, SENTINEL_SELF, None, false)?;
        let a_inner_cb: Vec<PlanStep> = vec![
            term,
            PlanStep::Erc20Transfer {
                token_idx: SENTINEL_WETH,
                token_addr: weth,
                recipient_idx: v3a,
                amount: optimal,
                seeds_pool: None,
                repays_flash: Some(fa.pool_address),
            },
        ];
        let inner_a = mechanics::v3_flash_to(
            &mut at,
            fa,
            out_a,
            optimal,
            false,
            v3b,
            Some(fb.pool_address),
            true,
            a_inner_cb,
        )?;
        let outer_b = mechanics::v3_flash_to(
            &mut at,
            fb,
            out_b,
            b_in,
            false,
            v2c,
            Some(fc.pool_address),
            false,
            vec![inner_a],
        )?;
        return Some((
            vec![
                PlanStep::SelfFund {
                    currency: weth,
                    amount: optimal,
                },
                outer_b,
            ],
            at,
        ));
    }
    // ── v3v2v3: hop2 outermost (SELF, rpr=false), hop0 inner
    // (recipient=V2 pool, rpr=false) with V2SwapCalc to terminal
    // (repays V3c) + WETH self-repay callback.
    else if facts[0].prot == Prot::V3 && facts[1].prot == Prot::V2 && facts[2].prot == Prot::V3 {
        let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
        let optimal = inputs.optimal_input;
        if inputs.hop_outputs.contains(&0) || !fits_i128(optimal) {
            return None;
        }
        let c_in = *inputs.consumed_inputs.get(2)?;
        if !fits_i128(c_in) {
            return None;
        }
        let (out_a, out_b, out_c) = (
            inputs.hop_outputs[0],
            inputs.hop_outputs[1],
            inputs.hop_outputs[2],
        );
        let weth = inputs.weth_address;
        let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
        let v2b = at.add(fb.pool_address).ok()?;
        let v3a = at.add(fa.pool_address).ok()?;
        let v3c = at.add(fc.pool_address).ok()?;
        let term = mechanics::v2_swap(&mut at, fb, out_b, v3c, Some(fc.pool_address), true)?;
        let a_inner_cb: Vec<PlanStep> = vec![
            term,
            PlanStep::Erc20Transfer {
                token_idx: SENTINEL_WETH,
                token_addr: weth,
                recipient_idx: v3a,
                amount: optimal,
                seeds_pool: None,
                repays_flash: Some(fa.pool_address),
            },
        ];
        let inner_a = mechanics::v3_flash_to(
            &mut at,
            fa,
            out_a,
            optimal,
            false,
            v2b,
            Some(fb.pool_address),
            false,
            a_inner_cb,
        )?;
        let outer_c = mechanics::v3_flash_to(
            &mut at,
            fc,
            out_c,
            c_in,
            false,
            SENTINEL_SELF,
            None,
            false,
            vec![inner_a],
        )?;
        return Some((
            vec![
                PlanStep::SelfFund {
                    currency: weth,
                    amount: optimal,
                },
                outer_c,
            ],
            at,
        ));
    }
    // ── v3v2v2: hop0 outermost (recipient=V2 pool, rpr=false) whose
    // callback is a V2SwapCalc chain to SELF + WETH self-repay.
    else if facts[0].prot == Prot::V3 && facts[1].prot == Prot::V2 && facts[2].prot == Prot::V2 {
        let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
        let optimal = inputs.optimal_input;
        if inputs.hop_outputs.contains(&0) || !fits_i128(optimal) {
            return None;
        }
        let (out_a, out_b, out_c) = (
            inputs.hop_outputs[0],
            inputs.hop_outputs[1],
            inputs.hop_outputs[2],
        );
        let weth = inputs.weth_address;
        let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
        let v2b = at.add(fb.pool_address).ok()?;
        let v2c = at.add(fc.pool_address).ok()?;
        let v3a = at.add(fa.pool_address).ok()?;
        let b = mechanics::v2_swap(&mut at, fb, out_b, v2c, Some(fc.pool_address), false)?;
        let c = mechanics::v2_swap(&mut at, fc, out_c, SENTINEL_SELF, None, false)?;
        let a_inner_cb: Vec<PlanStep> = vec![
            b,
            c,
            PlanStep::Erc20Transfer {
                token_idx: SENTINEL_WETH,
                token_addr: weth,
                recipient_idx: v3a,
                amount: optimal,
                seeds_pool: None,
                repays_flash: Some(fa.pool_address),
            },
        ];
        let outer = mechanics::v3_flash_to(
            &mut at,
            fa,
            out_a,
            optimal,
            false,
            v2b,
            Some(fb.pool_address),
            false,
            a_inner_cb,
        )?;
        return Some((
            vec![
                PlanStep::SelfFund {
                    currency: weth,
                    amount: optimal,
                },
                outer,
            ],
            at,
        ));
    }
    // ── v2v2v3: hop2 outermost (SELF, rpr=false) whose callback is a
    // WETH prefund + V2SwapCalc chain (hop0→1, hop1 repays V3c).
    else if facts[0].prot == Prot::V2 && facts[1].prot == Prot::V2 && facts[2].prot == Prot::V3 {
        let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
        let optimal = inputs.optimal_input;
        if inputs.hop_outputs.contains(&0) || !fits_i128(optimal) {
            return None;
        }
        let c_in = *inputs.consumed_inputs.get(2)?;
        if !fits_i128(c_in) {
            return None;
        }
        let (out_a, out_b, out_c) = (
            inputs.hop_outputs[0],
            inputs.hop_outputs[1],
            inputs.hop_outputs[2],
        );
        let weth = inputs.weth_address;
        let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
        let v2a = at.add(fa.pool_address).ok()?;
        let v2b = at.add(fb.pool_address).ok()?;
        let v3c = at.add(fc.pool_address).ok()?;
        let b_repays = mechanics::v2_swap(&mut at, fb, out_b, v3c, Some(fc.pool_address), true)?;
        let a_swap = mechanics::v2_swap(&mut at, fa, out_a, v2b, Some(fb.pool_address), false)?;
        let c_cb: Vec<PlanStep> = vec![
            PlanStep::Erc20Transfer {
                token_idx: SENTINEL_WETH,
                token_addr: weth,
                recipient_idx: v2a,
                amount: optimal,
                seeds_pool: Some(fa.pool_address),
                repays_flash: None,
            },
            a_swap,
            b_repays,
        ];
        let outer_c = mechanics::v3_flash_to(
            &mut at,
            fc,
            out_c,
            c_in,
            false,
            SENTINEL_SELF,
            None,
            false,
            c_cb,
        )?;
        return Some((
            vec![
                PlanStep::SelfFund {
                    currency: weth,
                    amount: optimal,
                },
                outer_c,
            ],
            at,
        ));
    }
    // ── v2v3v3: hop2 outermost (SELF, rpr=false) wrapping hop1
    // (recipient=hop2 pool, rpr=true) whose callback is WETH prefund +
    // V2SwapDirect (hop0 repays hop1 flash).
    else if facts[0].prot == Prot::V2 && facts[1].prot == Prot::V3 && facts[2].prot == Prot::V3 {
        let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
        let optimal = inputs.optimal_input;
        if inputs.hop_outputs.contains(&0) || !fits_i128(optimal) {
            return None;
        }
        let b_in = *inputs.consumed_inputs.get(1)?;
        let c_in = *inputs.consumed_inputs.get(2)?;
        if !fits_i128(b_in) || !fits_i128(c_in) {
            return None;
        }
        let (out_a, out_b, out_c) = (
            inputs.hop_outputs[0],
            inputs.hop_outputs[1],
            inputs.hop_outputs[2],
        );
        let weth = inputs.weth_address;
        let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
        let v2a = at.add(fa.pool_address).ok()?;
        let v3b = at.add(fb.pool_address).ok()?;
        let v3c = at.add(fc.pool_address).ok()?;
        let direct = mechanics::v2_swap_direct(
            &mut at,
            fa,
            out_a,
            fa.out_currency,
            v3b,
            Some(fb.pool_address),
            true,
        )?;
        let b_cb: Vec<PlanStep> = vec![
            PlanStep::Erc20Transfer {
                token_idx: SENTINEL_WETH,
                token_addr: weth,
                recipient_idx: v2a,
                amount: optimal,
                seeds_pool: Some(fa.pool_address),
                repays_flash: None,
            },
            direct,
        ];
        let inner_b = mechanics::v3_flash_to(
            &mut at,
            fb,
            out_b,
            b_in,
            false,
            v3c,
            Some(fc.pool_address),
            true,
            b_cb,
        )?;
        let outer_c = mechanics::v3_flash_to(
            &mut at,
            fc,
            out_c,
            c_in,
            false,
            SENTINEL_SELF,
            None,
            false,
            vec![inner_b],
        )?;
        return Some((
            vec![
                PlanStep::SelfFund {
                    currency: weth,
                    amount: optimal,
                },
                outer_c,
            ],
            at,
        ));
    }
    // ── v2v3v2: hop2 (V2 flash) outermost (SELF) wrapping hop1
    // (recipient=hop2 pool, rpr=true) whose callback is WETH prefund +
    // V2SwapDirect (hop0 repays hop1 flash).
    else if facts[0].prot == Prot::V2 && facts[1].prot == Prot::V3 && facts[2].prot == Prot::V2 {
        let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
        let optimal = inputs.optimal_input;
        if inputs.hop_outputs.contains(&0) || !fits_i128(optimal) {
            return None;
        }
        let b_in = *inputs.consumed_inputs.get(1)?;
        let c_in = *inputs.consumed_inputs.get(2)?;
        if !fits_i128(b_in) || !fits_i128(c_in) {
            return None;
        }
        let (out_a, out_b, out_c) = (
            inputs.hop_outputs[0],
            inputs.hop_outputs[1],
            inputs.hop_outputs[2],
        );
        let weth = inputs.weth_address;
        let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
        let _ = at.add(fa.out_currency).ok()?;
        let v2a = at.add(fa.pool_address).ok()?;
        let v2c = at.add(fc.pool_address).ok()?;
        let v3b = at.add(fb.pool_address).ok()?;
        let direct = mechanics::v2_swap_direct(
            &mut at,
            fa,
            out_a,
            fa.out_currency,
            v3b,
            Some(fb.pool_address),
            true,
        )?;
        let b_cb: Vec<PlanStep> = vec![
            PlanStep::Erc20Transfer {
                token_idx: SENTINEL_WETH,
                token_addr: weth,
                recipient_idx: v2a,
                amount: optimal,
                seeds_pool: Some(fa.pool_address),
                repays_flash: None,
            },
            direct,
        ];
        let inner_b = mechanics::v3_flash_to(
            &mut at,
            fb,
            out_b,
            b_in,
            false,
            v2c,
            Some(fc.pool_address),
            true,
            b_cb,
        )?;
        let outer_c = mechanics::v2_flash(&mut at, fc, out_c, fc.in_currency, c_in, vec![inner_b])?;
        return Some((
            vec![
                PlanStep::SelfFund {
                    currency: weth,
                    amount: optimal,
                },
                outer_c,
            ],
            at,
        ));
    } else {
        if facts.len() != 3 {
            return None;
        }
        if facts[0].prot == Prot::V4 && facts[1].prot == Prot::V4 && facts[2].prot == Prot::V4 {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if out_a == 0 || out_b == 0 || out_c == 0 {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let a_swap_in = *inputs.consumed_inputs.first()?;
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(a_swap_in) || !fits_i128(b_swap_in) || !fits_i128(c_swap_in) {
                return None;
            }
            let mid_a_out = fa.out_currency;
            let in_currency_a = fa.in_currency;
            let mid_b_out = fb.out_currency;
            let mid_b_in = fb.in_currency;
            let output_c = fc.out_currency;
            let mid_c_in = fc.in_currency;
            let weth = inputs.weth_address;
            let capture = resolve_axes(inputs.opts).1;
            if native_capture_declines(capture, output_c, weth) {
                return None;
            }
            let bridge_ab = CurrencyBridge::at_boundary(mid_a_out, mid_b_in);
            let bridge_bc = CurrencyBridge::at_boundary(mid_b_out, mid_c_in);
            let any_gap = bridge_ab.needs_bridge() || bridge_bc.needs_bridge();

            let mut at = v4_scaffold_table(inputs);
            let c0_a = at.add(fa.currency0_address).ok()?;
            let c1_a = at.add(fa.currency1_address).ok()?;
            let c0_b = at.add(fb.currency0_address).ok()?;
            let c1_b = at.add(fb.currency1_address).ok()?;
            let c0_c = at.add(fc.currency0_address).ok()?;
            let c1_c = at.add(fc.currency1_address).ok()?;
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;

            let fee_a = fa.swap_fee;
            let ts_a = fa.tick_spacing;
            let fee_b = fb.swap_fee;
            let ts_b = fb.tick_spacing;
            let fee_c = fc.swap_fee;
            let ts_c = fc.tick_spacing;

            let profit = out_c.saturating_sub(optimal_input);
            let terminal_idx = if output_c == weth {
                SENTINEL_WETH
            } else if output_c == NATIVE_CURRENCY_ADDRESS {
                SENTINEL_NATIVE
            } else {
                at.add(output_c).ok()?
            };

            let v4swap_a = PlanStep::V4Swap {
                c0_idx: c0_a,
                c1_idx: c1_a,
                fee: fee_a,
                tick_spacing: ts_a,
                hooks_idx: SENTINEL_NATIVE,
                zfo: fa.zfo,
                amount: a_swap_in,
                in_currency: in_currency_a,
                in_amount: a_swap_in,
                out_currency: mid_a_out,
                out_amount: out_a,
            };
            let v4swap_b = PlanStep::V4Swap {
                c0_idx: c0_b,
                c1_idx: c1_b,
                fee: fee_b,
                tick_spacing: ts_b,
                hooks_idx: SENTINEL_NATIVE,
                zfo: fb.zfo,
                amount: b_swap_in,
                in_currency: mid_b_in,
                in_amount: b_swap_in,
                out_currency: mid_b_out,
                out_amount: out_b,
            };
            let v4swap_c = PlanStep::V4Swap {
                c0_idx: c0_c,
                c1_idx: c1_c,
                fee: fee_c,
                tick_spacing: ts_c,
                hooks_idx: SENTINEL_NATIVE,
                zfo: fc.zfo,
                amount: c_swap_in,
                in_currency: mid_c_in,
                in_amount: c_swap_in,
                out_currency: output_c,
                out_amount: out_c,
            };

            let mut inner: Plan = if inputs.opts.use_v4_batch && !any_gap {
                let mut steps = vec![PlanStep::V4Batch {
                    entries: vec![
                        V4BatchSwap {
                            c0_idx: c0_a,
                            c1_idx: c1_a,
                            fee: fee_a,
                            tick_spacing: ts_a,
                            hooks_idx: SENTINEL_NATIVE,
                            zfo: fa.zfo,
                            amount: a_swap_in,
                            in_currency: in_currency_a,
                            in_amount: a_swap_in,
                            out_currency: mid_a_out,
                            out_amount: out_a,
                        },
                        V4BatchSwap {
                            c0_idx: c0_b,
                            c1_idx: c1_b,
                            fee: fee_b,
                            tick_spacing: ts_b,
                            hooks_idx: SENTINEL_NATIVE,
                            zfo: fb.zfo,
                            amount: b_swap_in,
                            in_currency: mid_b_in,
                            in_amount: b_swap_in,
                            out_currency: mid_b_out,
                            out_amount: out_b,
                        },
                        V4BatchSwap {
                            c0_idx: c0_c,
                            c1_idx: c1_c,
                            fee: fee_c,
                            tick_spacing: ts_c,
                            hooks_idx: SENTINEL_NATIVE,
                            zfo: fc.zfo,
                            amount: c_swap_in,
                            in_currency: mid_c_in,
                            in_amount: c_swap_in,
                            out_currency: output_c,
                            out_amount: out_c,
                        },
                    ],
                }];
                if output_c != NATIVE_CURRENCY_ADDRESS && output_c != weth {
                    steps.push(PlanStep::V4TakeDelta {
                        currency_idx: terminal_idx,
                        currency_addr: output_c,
                        recipient_idx: SENTINEL_SELF,
                        seeds_pool: None,
                    });
                }
                steps
            } else {
                let mut steps = vec![v4swap_a];
                if bridge_ab.needs_bridge() {
                    let (bridge_steps, settle_idx, settle_currency) =
                        v4_bridge_steps(bridge_ab, weth, out_a);
                    steps.extend(bridge_steps);
                    steps.push(v4swap_b);
                    steps.push(PlanStep::V4SettleDelta {
                        currency_idx: settle_idx,
                        currency_addr: settle_currency,
                    });
                } else {
                    steps.push(v4swap_b);
                }
                if bridge_bc.needs_bridge() {
                    let (bridge_steps, settle_idx, settle_currency) =
                        v4_bridge_steps(bridge_bc, weth, out_b);
                    steps.extend(bridge_steps);
                    steps.push(v4swap_c);
                    steps.push(PlanStep::V4SettleDelta {
                        currency_idx: settle_idx,
                        currency_addr: settle_currency,
                    });
                } else {
                    steps.push(v4swap_c);
                }
                steps
            };
            inner.append(&mut v4_terminal_capture_steps(
                output_c,
                terminal_idx,
                capture,
                inputs.opts.use_v4_batch,
                any_gap,
                profit,
                weth,
            ));
            inner.push(PlanStep::V4SettleAll);

            let plan: Plan = vec![PlanStep::V4Unlock {
                inner,
                pool_manager_idx: pm_idx,
            }];
            Some((plan, at))
        } else if facts[0].prot == Prot::V4
            && facts[1].prot == Prot::V2
            && facts[2].prot == Prot::V2
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let b_out = inputs.hop_outputs[1];
            let c_out = inputs.hop_outputs[2];
            if out_a == 0 || inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let forward_a_cur = fa.out_currency;
            let in_currency_a = fa.in_currency;
            let weth = inputs.weth_address;
            if in_currency_a != weth {
                return None;
            }
            let mut at = v4_scaffold_table(inputs);
            let forward_a = at.add(forward_a_cur).ok()?;
            let fee_a = fa.swap_fee;
            let ts_a = fa.tick_spacing;
            let c0_a = at.add(fa.currency0_address).ok()?;
            let c1_a = at.add(fa.currency1_address).ok()?;
            let v2b = at.add(fb.pool_address).ok()?;
            let v2c = at.add(fc.pool_address).ok()?;
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;

            let b_step = mechanics::v2_swap(&mut at, fb, b_out, v2c, Some(fc.pool_address), false)?;
            let c_step = mechanics::v2_swap(&mut at, fc, c_out, SENTINEL_SELF, None, false)?;
            let inner: Plan = vec![
                PlanStep::V4Swap {
                    c0_idx: c0_a,
                    c1_idx: c1_a,
                    fee: fee_a,
                    tick_spacing: ts_a,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fa.zfo,
                    amount: optimal_input,
                    in_currency: in_currency_a,
                    in_amount: optimal_input,
                    out_currency: forward_a_cur,
                    out_amount: out_a,
                },
                PlanStep::V4TakeCompact {
                    currency_idx: forward_a,
                    currency_addr: forward_a_cur,
                    recipient_idx: v2b,
                    amount: out_a,
                    seeds_pool: Some(fb.pool_address),
                    repays_flash: None,
                },
                b_step,
                c_step,
                PlanStep::V4SettleDelta {
                    currency_idx: SENTINEL_WETH,
                    currency_addr: weth,
                },
            ];
            let plan: Plan = vec![PlanStep::V4Unlock {
                inner,
                pool_manager_idx: pm_idx,
            }];
            Some((plan, at))
        } else if facts[0].prot == Prot::V4
            && facts[1].prot == Prot::V2
            && facts[2].prot == Prot::V4
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let b_out = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if out_a == 0 || inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(c_swap_in) {
                return None;
            }
            let forward_a_cur = fa.out_currency;
            let in_currency_a = fa.in_currency;
            let output_c = fc.out_currency;
            let in_currency_c = fc.in_currency;
            let b_forward_cur = fb.out_currency;
            let mut at = v4_scaffold_table(inputs);
            let forward_a = at.add(forward_a_cur).ok()?;
            let _ = at.add(b_forward_cur).ok()?;
            let fee_a = fa.swap_fee;
            let ts_a = fa.tick_spacing;
            let fee_c = fc.swap_fee;
            let ts_c = fc.tick_spacing;
            let c0_a = at.add(fa.currency0_address).ok()?;
            let c1_a = at.add(fa.currency1_address).ok()?;
            let c0_c = at.add(fc.currency0_address).ok()?;
            let c1_c = at.add(fc.currency1_address).ok()?;
            let v2b = at.add(fb.pool_address).ok()?;
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;

            let b_step = mechanics::v2_swap(&mut at, fb, b_out, SENTINEL_SELF, None, false)?;
            let inner: Plan = vec![
                PlanStep::V4Swap {
                    c0_idx: c0_a,
                    c1_idx: c1_a,
                    fee: fee_a,
                    tick_spacing: ts_a,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fa.zfo,
                    amount: optimal_input,
                    in_currency: in_currency_a,
                    in_amount: optimal_input,
                    out_currency: forward_a_cur,
                    out_amount: out_a,
                },
                PlanStep::V4TakeCompact {
                    currency_idx: forward_a,
                    currency_addr: forward_a_cur,
                    recipient_idx: v2b,
                    amount: out_a,
                    seeds_pool: Some(fb.pool_address),
                    repays_flash: None,
                },
                b_step,
                PlanStep::V4Swap {
                    c0_idx: c0_c,
                    c1_idx: c1_c,
                    fee: fee_c,
                    tick_spacing: ts_c,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fc.zfo,
                    amount: c_swap_in,
                    in_currency: in_currency_c,
                    in_amount: c_swap_in,
                    out_currency: output_c,
                    out_amount: out_c,
                },
                PlanStep::V4SettleAll,
            ];
            let plan: Plan = vec![PlanStep::V4Unlock {
                inner,
                pool_manager_idx: pm_idx,
            }];
            Some((plan, at))
        } else if facts[0].prot == Prot::V4
            && facts[1].prot == Prot::V3
            && facts[2].prot == Prot::V3
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(b_swap_in) || !fits_i128(c_swap_in) {
                return None;
            }
            let forward_a_cur = fa.out_currency;
            let in_currency_a = fa.in_currency;
            let mut at = v4_scaffold_table(inputs);
            let _ = at.add(inputs.pool_manager_address).ok()?;
            let v3b = at.add(fb.pool_address).ok()?;
            let v3c = at.add(fc.pool_address).ok()?;
            let forward_a = at.add(forward_a_cur).ok()?;
            let fee_a = fa.swap_fee;
            let ts_a = fa.tick_spacing;
            let c0_a = at.add(fa.currency0_address).ok()?;
            let c1_a = at.add(fa.currency1_address).ok()?;
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let weth = inputs.weth_address;

            let inner_take = PlanStep::V4TakeCompact {
                currency_idx: forward_a,
                currency_addr: forward_a_cur,
                recipient_idx: v3b,
                amount: out_a,
                seeds_pool: None,
                repays_flash: Some(fb.pool_address),
            };
            let inner_b = mechanics::v3_flash_to(
                &mut at,
                fb,
                out_b,
                b_swap_in,
                false,
                v3c,
                Some(fc.pool_address),
                true,
                vec![inner_take],
            )?;
            let outer_c = mechanics::v3_flash_to(
                &mut at,
                fc,
                out_c,
                c_swap_in,
                false,
                SENTINEL_SELF,
                None,
                false,
                vec![inner_b],
            )?;
            let inner: Plan = vec![
                PlanStep::V4Swap {
                    c0_idx: c0_a,
                    c1_idx: c1_a,
                    fee: fee_a,
                    tick_spacing: ts_a,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fa.zfo,
                    amount: optimal_input,
                    in_currency: in_currency_a,
                    in_amount: optimal_input,
                    out_currency: forward_a_cur,
                    out_amount: out_a,
                },
                outer_c,
                PlanStep::V4SettleDelta {
                    currency_idx: SENTINEL_WETH,
                    currency_addr: weth,
                },
            ];
            let plan: Plan = vec![PlanStep::V4Unlock {
                inner,
                pool_manager_idx: pm_idx,
            }];
            Some((plan, at))
        } else if facts[0].prot == Prot::V4
            && facts[1].prot == Prot::V3
            && facts[2].prot == Prot::V4
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(b_swap_in) || !fits_i128(c_swap_in) {
                return None;
            }
            let forward_a_cur = fa.out_currency;
            let in_currency_a = fa.in_currency;
            let fwd_b = fb.out_currency;
            let output_c = fc.out_currency;
            let in_currency_c = fc.in_currency;
            let mut at = v4_scaffold_table(inputs);
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let v3b = at.add(fb.pool_address).ok()?;
            let forward_a = at.add(forward_a_cur).ok()?;
            let forward_b = at.add(fwd_b).ok()?;
            let fee_a = fa.swap_fee;
            let ts_a = fa.tick_spacing;
            let fee_c = fc.swap_fee;
            let ts_c = fc.tick_spacing;
            let c0_a = at.add(fa.currency0_address).ok()?;
            let c1_a = at.add(fa.currency1_address).ok()?;
            let c0_c = at.add(fc.currency0_address).ok()?;
            let c1_c = at.add(fc.currency1_address).ok()?;

            let take = PlanStep::V4TakeCompact {
                currency_idx: forward_a,
                currency_addr: forward_a_cur,
                recipient_idx: v3b,
                amount: out_a,
                seeds_pool: None,
                repays_flash: Some(fb.pool_address),
            };
            let flash_b = mechanics::v3_flash_to(
                &mut at,
                fb,
                out_b,
                b_swap_in,
                false,
                pm_idx,
                None,
                false,
                vec![take],
            )?;
            let inner: Plan = vec![
                PlanStep::V4Swap {
                    c0_idx: c0_a,
                    c1_idx: c1_a,
                    fee: fee_a,
                    tick_spacing: ts_a,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fa.zfo,
                    amount: optimal_input,
                    in_currency: in_currency_a,
                    in_amount: optimal_input,
                    out_currency: forward_a_cur,
                    out_amount: out_a,
                },
                PlanStep::V4Sync {
                    currency_idx: forward_b,
                    currency_addr: fwd_b,
                },
                flash_b,
                PlanStep::V4Settle {
                    currency_addr: fwd_b,
                    amount: out_b,
                },
                PlanStep::V4Swap {
                    c0_idx: c0_c,
                    c1_idx: c1_c,
                    fee: fee_c,
                    tick_spacing: ts_c,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fc.zfo,
                    amount: c_swap_in,
                    in_currency: in_currency_c,
                    in_amount: c_swap_in,
                    out_currency: output_c,
                    out_amount: out_c,
                },
                PlanStep::V4SettleAll,
            ];
            let plan: Plan = vec![PlanStep::V4Unlock {
                inner,
                pool_manager_idx: pm_idx,
            }];
            Some((plan, at))
        } else if facts[0].prot == Prot::V4
            && facts[1].prot == Prot::V4
            && facts[2].prot == Prot::V2
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let a_swap_in = *inputs.consumed_inputs.first()?;
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            if !fits_i128(a_swap_in) || !fits_i128(b_swap_in) {
                return None;
            }
            let forward_a_cur = fa.out_currency;
            let in_currency_a = fa.in_currency;
            let forward_b_cur = fb.out_currency;
            let in_currency_b = fb.in_currency;
            let mut at = v4_scaffold_table(inputs);
            let forward_b = at.add(forward_b_cur).ok()?;
            let v2c = at.add(fc.pool_address).ok()?;
            let fee_a = fa.swap_fee;
            let ts_a = fa.tick_spacing;
            let fee_b = fb.swap_fee;
            let ts_b = fb.tick_spacing;
            let c0_a = at.add(fa.currency0_address).ok()?;
            let c1_a = at.add(fa.currency1_address).ok()?;
            let c0_b = at.add(fb.currency0_address).ok()?;
            let c1_b = at.add(fb.currency1_address).ok()?;
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;

            let v2step = mechanics::v2_swap(&mut at, fc, out_c, SENTINEL_SELF, None, false)?;
            let inner: Plan = vec![
                PlanStep::V4Swap {
                    c0_idx: c0_a,
                    c1_idx: c1_a,
                    fee: fee_a,
                    tick_spacing: ts_a,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fa.zfo,
                    amount: a_swap_in,
                    in_currency: in_currency_a,
                    in_amount: a_swap_in,
                    out_currency: forward_a_cur,
                    out_amount: out_a,
                },
                PlanStep::V4Swap {
                    c0_idx: c0_b,
                    c1_idx: c1_b,
                    fee: fee_b,
                    tick_spacing: ts_b,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fb.zfo,
                    amount: b_swap_in,
                    in_currency: in_currency_b,
                    in_amount: b_swap_in,
                    out_currency: forward_b_cur,
                    out_amount: out_b,
                },
                PlanStep::V4TakeCompact {
                    currency_idx: forward_b,
                    currency_addr: forward_b_cur,
                    recipient_idx: v2c,
                    amount: out_b,
                    seeds_pool: Some(fc.pool_address),
                    repays_flash: None,
                },
                v2step,
                PlanStep::V4SettleAll,
            ];
            let plan: Plan = vec![PlanStep::V4Unlock {
                inner,
                pool_manager_idx: pm_idx,
            }];
            Some((plan, at))
        } else if facts[0].prot == Prot::V4
            && facts[1].prot == Prot::V4
            && facts[2].prot == Prot::V3
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let a_swap_in = *inputs.consumed_inputs.first()?;
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(a_swap_in) || !fits_i128(b_swap_in) || !fits_i128(c_swap_in) {
                return None;
            }
            let forward_a_cur = fa.out_currency;
            let in_currency_a = fa.in_currency;
            let forward_b_cur = fb.out_currency;
            let in_currency_b = fb.in_currency;
            let out_currency_c = fc.out_currency;
            let mut at = v4_scaffold_table(inputs);
            let forward_b = at.add(forward_b_cur).ok()?;
            let v3c = at.add(fc.pool_address).ok()?;
            let fee_a = fa.swap_fee;
            let ts_a = fa.tick_spacing;
            let fee_b = fb.swap_fee;
            let ts_b = fb.tick_spacing;
            let fee_c = fc.swap_fee;
            let c0_a = at.add(fa.currency0_address).ok()?;
            let c1_a = at.add(fa.currency1_address).ok()?;
            let c0_b = at.add(fb.currency0_address).ok()?;
            let c1_b = at.add(fb.currency1_address).ok()?;
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;

            let take = PlanStep::V4TakeCompact {
                currency_idx: forward_b,
                currency_addr: forward_b_cur,
                recipient_idx: v3c,
                amount: c_swap_in,
                seeds_pool: None,
                repays_flash: Some(fc.pool_address),
            };
            let flash_c = PlanStep::FlashSwap {
                pool_idx: v3c,
                pool_addr: fc.pool_address,
                protocol: Prot::V3,
                zfo: fc.zfo,
                fee: fee_c,
                out_currency: out_currency_c,
                out_amount: out_c,
                in_currency: forward_b_cur,
                in_amount: c_swap_in,
                recipient_idx: SENTINEL_SELF,
                recipient_pool_addr: None,
                recipient_pool_repays: false,
                auto_repay: false,
                callback: vec![take],
            };
            let inner: Plan = vec![
                PlanStep::V4Swap {
                    c0_idx: c0_a,
                    c1_idx: c1_a,
                    fee: fee_a,
                    tick_spacing: ts_a,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fa.zfo,
                    amount: a_swap_in,
                    in_currency: in_currency_a,
                    in_amount: a_swap_in,
                    out_currency: forward_a_cur,
                    out_amount: out_a,
                },
                PlanStep::V4Swap {
                    c0_idx: c0_b,
                    c1_idx: c1_b,
                    fee: fee_b,
                    tick_spacing: ts_b,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fb.zfo,
                    amount: b_swap_in,
                    in_currency: in_currency_b,
                    in_amount: b_swap_in,
                    out_currency: forward_b_cur,
                    out_amount: out_b,
                },
                flash_c,
                PlanStep::V4SettleAll,
            ];
            let plan: Plan = vec![PlanStep::V4Unlock {
                inner,
                pool_manager_idx: pm_idx,
            }];
            Some((plan, at))
        } else if facts[0].prot == Prot::V4
            && facts[1].prot == Prot::V2
            && facts[2].prot == Prot::V3
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(c_swap_in) {
                return None;
            }
            let forward_a_cur = fa.out_currency;
            let in_currency_a = fa.in_currency;
            if in_currency_a != inputs.weth_address {
                return None;
            }
            let mut at = v4_scaffold_table(inputs);
            let v3c = at.add(fc.pool_address).ok()?;
            let forward_a = at.add(forward_a_cur).ok()?;
            let _ = at.add(fb.out_currency).ok()?;
            let fee_a = fa.swap_fee;
            let ts_a = fa.tick_spacing;
            let c0_a = at.add(fa.currency0_address).ok()?;
            let c1_a = at.add(fa.currency1_address).ok()?;
            let v2b = at.add(fb.pool_address).ok()?;
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let weth = inputs.weth_address;

            let b_step = mechanics::v2_swap(&mut at, fb, out_b, v3c, Some(fc.pool_address), true)?;
            let v4_inner: Plan = vec![
                PlanStep::V4Swap {
                    c0_idx: c0_a,
                    c1_idx: c1_a,
                    fee: fee_a,
                    tick_spacing: ts_a,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fa.zfo,
                    amount: optimal_input,
                    in_currency: in_currency_a,
                    in_amount: optimal_input,
                    out_currency: forward_a_cur,
                    out_amount: out_a,
                },
                PlanStep::V4TakeCompact {
                    currency_idx: forward_a,
                    currency_addr: forward_a_cur,
                    recipient_idx: v2b,
                    amount: out_a,
                    seeds_pool: Some(fb.pool_address),
                    repays_flash: None,
                },
                b_step,
                PlanStep::V4SettleDelta {
                    currency_idx: SENTINEL_WETH,
                    currency_addr: weth,
                },
            ];
            let plan: Plan = vec![mechanics::v3_flash_to(
                &mut at,
                fc,
                out_c,
                c_swap_in,
                false,
                SENTINEL_SELF,
                None,
                false,
                vec![PlanStep::V4Unlock {
                    inner: v4_inner,
                    pool_manager_idx: pm_idx,
                }],
            )?];
            Some((plan, at))
        } else if facts[0].prot == Prot::V4
            && facts[1].prot == Prot::V3
            && facts[2].prot == Prot::V2
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            if !fits_i128(b_swap_in) {
                return None;
            }
            let forward_a_cur = fa.out_currency;
            let in_currency_a = fa.in_currency;
            let fwd_b = fb.out_currency;
            let mut at = v4_scaffold_table(inputs);
            let v3b = at.add(fb.pool_address).ok()?;
            let forward_a = at.add(forward_a_cur).ok()?;
            let _ = at.add(fwd_b).ok()?;
            let fee_a = fa.swap_fee;
            let ts_a = fa.tick_spacing;
            let c0_a = at.add(fa.currency0_address).ok()?;
            let c1_a = at.add(fa.currency1_address).ok()?;
            let v2c = at.add(fc.pool_address).ok()?;
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let weth = inputs.weth_address;

            let take = PlanStep::V4TakeCompact {
                currency_idx: forward_a,
                currency_addr: forward_a_cur,
                recipient_idx: v3b,
                amount: out_a,
                seeds_pool: None,
                repays_flash: Some(fb.pool_address),
            };
            let v2step = mechanics::v2_swap(&mut at, fc, out_c, SENTINEL_SELF, None, false)?;
            let flash_b = PlanStep::FlashSwap {
                pool_idx: v3b,
                pool_addr: fb.pool_address,
                protocol: Prot::V3,
                zfo: fb.zfo,
                fee: fb.swap_fee,
                out_currency: fwd_b,
                out_amount: out_b,
                in_currency: forward_a_cur,
                in_amount: b_swap_in,
                recipient_idx: v2c,
                recipient_pool_addr: Some(fc.pool_address),
                recipient_pool_repays: false,
                auto_repay: false,
                callback: vec![take, v2step],
            };
            let inner: Plan = vec![
                PlanStep::V4Swap {
                    c0_idx: c0_a,
                    c1_idx: c1_a,
                    fee: fee_a,
                    tick_spacing: ts_a,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fa.zfo,
                    amount: optimal_input,
                    in_currency: in_currency_a,
                    in_amount: optimal_input,
                    out_currency: forward_a_cur,
                    out_amount: out_a,
                },
                flash_b,
                PlanStep::V4SettleDelta {
                    currency_idx: SENTINEL_WETH,
                    currency_addr: weth,
                },
            ];
            let plan: Plan = vec![PlanStep::V4Unlock {
                inner,
                pool_manager_idx: pm_idx,
            }];
            Some((plan, at))
        } else if facts[0].prot == Prot::V2
            && facts[1].prot == Prot::V2
            && facts[2].prot == Prot::V4
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(c_swap_in) {
                return None;
            }
            let fwd_b = fb.out_currency;
            let output_c = fc.out_currency;
            let in_currency_c = fc.in_currency;
            let mut at = v4_scaffold_table(inputs);
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let v2a = at.add(fa.pool_address).ok()?;
            let v2b = at.add(fb.pool_address).ok()?;
            let fee_c = fc.swap_fee;
            let ts_c = fc.tick_spacing;
            let c0 = at.add(fc.currency0_address).ok()?;
            let c1 = at.add(fc.currency1_address).ok()?;
            let forward_b = at.add(fwd_b).ok()?;
            let weth = inputs.weth_address;

            let a_step = mechanics::v2_swap(&mut at, fa, out_a, v2b, Some(fb.pool_address), false)?;
            let b_step = mechanics::v2_swap(&mut at, fb, c_swap_in, pm_idx, None, false)?;
            let inner: Plan = vec![
                PlanStep::V4Sync {
                    currency_idx: forward_b,
                    currency_addr: fwd_b,
                },
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v2a,
                    amount: optimal_input,
                    seeds_pool: Some(fa.pool_address),
                    repays_flash: None,
                },
                a_step,
                b_step,
                PlanStep::V4Settle {
                    currency_addr: fwd_b,
                    amount: c_swap_in,
                },
                PlanStep::V4Swap {
                    c0_idx: c0,
                    c1_idx: c1,
                    fee: fee_c,
                    tick_spacing: ts_c,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fc.zfo,
                    amount: c_swap_in,
                    in_currency: in_currency_c,
                    in_amount: c_swap_in,
                    out_currency: output_c,
                    out_amount: out_c,
                },
                PlanStep::V4SettleAll,
            ];
            let plan: Plan = vec![
                PlanStep::SelfFund {
                    currency: weth,
                    amount: optimal_input,
                },
                PlanStep::V4Unlock {
                    inner,
                    pool_manager_idx: pm_idx,
                },
            ];
            Some((plan, at))
        } else if facts[0].prot == Prot::V2
            && facts[1].prot == Prot::V4
            && facts[2].prot == Prot::V4
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(b_swap_in) || !fits_i128(c_swap_in) {
                return None;
            }
            let forward_a_cur = fa.out_currency;
            let forward_b_cur = fb.out_currency;
            let in_currency_b = fb.in_currency;
            let output_c = fc.out_currency;
            let in_currency_c = fc.in_currency;
            let mut at = v4_scaffold_table(inputs);
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let forward_a = at.add(forward_a_cur).ok()?;
            let fee_b = fb.swap_fee;
            let ts_b = fb.tick_spacing;
            let fee_c = fc.swap_fee;
            let ts_c = fc.tick_spacing;
            let c0_b = at.add(fb.currency0_address).ok()?;
            let c1_b = at.add(fb.currency1_address).ok()?;
            let c0_c = at.add(fc.currency0_address).ok()?;
            let c1_c = at.add(fc.currency1_address).ok()?;
            let v2a = at.add(fa.pool_address).ok()?;
            let weth = inputs.weth_address;

            let a_step = mechanics::v2_swap(&mut at, fa, b_swap_in, pm_idx, None, false)?;
            let inner: Plan = vec![
                PlanStep::V4Sync {
                    currency_idx: forward_a,
                    currency_addr: forward_a_cur,
                },
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v2a,
                    amount: optimal_input,
                    seeds_pool: Some(fa.pool_address),
                    repays_flash: None,
                },
                a_step,
                PlanStep::V4Settle {
                    currency_addr: forward_a_cur,
                    amount: b_swap_in,
                },
                PlanStep::V4Swap {
                    c0_idx: c0_b,
                    c1_idx: c1_b,
                    fee: fee_b,
                    tick_spacing: ts_b,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fb.zfo,
                    amount: b_swap_in,
                    in_currency: in_currency_b,
                    in_amount: b_swap_in,
                    out_currency: forward_b_cur,
                    out_amount: out_b,
                },
                PlanStep::V4Swap {
                    c0_idx: c0_c,
                    c1_idx: c1_c,
                    fee: fee_c,
                    tick_spacing: ts_c,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fc.zfo,
                    amount: c_swap_in,
                    in_currency: in_currency_c,
                    in_amount: c_swap_in,
                    out_currency: output_c,
                    out_amount: out_c,
                },
                PlanStep::V4SettleAll,
            ];
            let plan: Plan = vec![
                PlanStep::SelfFund {
                    currency: weth,
                    amount: optimal_input,
                },
                PlanStep::V4Unlock {
                    inner,
                    pool_manager_idx: pm_idx,
                },
            ];
            Some((plan, at))
        } else if facts[0].prot == Prot::V2
            && facts[1].prot == Prot::V3
            && facts[2].prot == Prot::V4
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(b_swap_in) || !fits_i128(c_swap_in) {
                return None;
            }
            let fwd_b = fb.out_currency;
            let fwd_a = fa.out_currency;
            let output_c = fc.out_currency;
            let in_currency_c = fc.in_currency;
            let mut at = v4_scaffold_table(inputs);
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let v2a = at.add(fa.pool_address).ok()?;
            let v3b = at.add(fb.pool_address).ok()?;
            let fee_c = fc.swap_fee;
            let ts_c = fc.tick_spacing;
            let c0 = at.add(fc.currency0_address).ok()?;
            let c1 = at.add(fc.currency1_address).ok()?;
            let forward_b = at.add(fwd_b).ok()?;
            let weth = inputs.weth_address;

            let v4_inner: Plan = vec![
                PlanStep::V4Settle {
                    currency_addr: fwd_b,
                    amount: c_swap_in,
                },
                PlanStep::V4Swap {
                    c0_idx: c0,
                    c1_idx: c1,
                    fee: fee_c,
                    tick_spacing: ts_c,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fc.zfo,
                    amount: c_swap_in,
                    in_currency: in_currency_c,
                    in_amount: c_swap_in,
                    out_currency: output_c,
                    out_amount: out_c,
                },
                PlanStep::V4TakeCompact {
                    currency_idx: SENTINEL_WETH,
                    currency_addr: weth,
                    recipient_idx: v2a,
                    amount: optimal_input,
                    seeds_pool: Some(fa.pool_address),
                    repays_flash: None,
                },
                PlanStep::V4TakeCompact {
                    currency_idx: SENTINEL_WETH,
                    currency_addr: weth,
                    recipient_idx: SENTINEL_SELF,
                    amount: out_c - optimal_input,
                    seeds_pool: None,
                    repays_flash: None,
                },
                PlanStep::V4Sync {
                    currency_idx: SENTINEL_WETH,
                    currency_addr: weth,
                },
                PlanStep::V4SettleAll,
            ];
            let cb: Plan = vec![
                PlanStep::V4Unlock {
                    inner: v4_inner,
                    pool_manager_idx: pm_idx,
                },
                mechanics::v2_swap_direct(
                    &mut at,
                    fa,
                    out_a,
                    fwd_a,
                    v3b,
                    Some(fb.pool_address),
                    true,
                )?,
            ];
            let plan: Plan = vec![
                PlanStep::V4Sync {
                    currency_idx: forward_b,
                    currency_addr: fwd_b,
                },
                mechanics::v3_flash_to(
                    &mut at, fb, out_b, b_swap_in, false, pm_idx, None, false, cb,
                )?,
            ];
            Some((plan, at))
        } else if facts[0].prot == Prot::V3
            && facts[1].prot == Prot::V2
            && facts[2].prot == Prot::V4
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(b_swap_in) || !fits_i128(c_swap_in) {
                return None;
            }
            let fwd_b = fb.out_currency;
            let in_b = fb.in_currency;
            let output_c = fc.out_currency;
            let in_currency_c = fc.in_currency;
            let mut at = v4_scaffold_table(inputs);
            let _ = at.add(inputs.pool_manager_address).ok()?;
            let v3a = at.add(fa.pool_address).ok()?;
            let v2b = at.add(fb.pool_address).ok()?;
            let forward_b = at.add(fwd_b).ok()?;
            let fee_c = fc.swap_fee;
            let ts_c = fc.tick_spacing;
            let c0 = at.add(fc.currency0_address).ok()?;
            let c1 = at.add(fc.currency1_address).ok()?;
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let weth = inputs.weth_address;

            let v4_inner: Plan = vec![
                PlanStep::V4Swap {
                    c0_idx: c0,
                    c1_idx: c1,
                    fee: fee_c,
                    tick_spacing: ts_c,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fc.zfo,
                    amount: c_swap_in,
                    in_currency: in_currency_c,
                    in_amount: c_swap_in,
                    out_currency: output_c,
                    out_amount: out_c,
                },
                PlanStep::V4TakeCompact {
                    currency_idx: SENTINEL_WETH,
                    currency_addr: weth,
                    recipient_idx: SENTINEL_SELF,
                    amount: out_c,
                    seeds_pool: None,
                    repays_flash: None,
                },
                PlanStep::V4SettleDelta {
                    currency_idx: forward_b,
                    currency_addr: fwd_b,
                },
            ];
            let v2_flash = PlanStep::FlashSwap {
                pool_idx: v2b,
                pool_addr: fb.pool_address,
                protocol: Prot::V2,
                zfo: fb.zfo,
                fee: fb.swap_fee,
                out_currency: fwd_b,
                out_amount: out_b,
                in_currency: in_b,
                in_amount: b_swap_in,
                recipient_idx: SENTINEL_SELF,
                recipient_pool_addr: None,
                recipient_pool_repays: false,
                auto_repay: true,
                callback: vec![PlanStep::V4Unlock {
                    inner: v4_inner,
                    pool_manager_idx: pm_idx,
                }],
            };
            let a_cb: Plan = vec![
                v2_flash,
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v3a,
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
                mechanics::v3_flash_to(
                    &mut at,
                    fa,
                    out_a,
                    optimal_input,
                    false,
                    v2b,
                    None,
                    false,
                    a_cb,
                )?,
            ];
            Some((plan, at))
        } else if facts[0].prot == Prot::V3
            && facts[1].prot == Prot::V3
            && facts[2].prot == Prot::V4
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(b_swap_in) || !fits_i128(c_swap_in) {
                return None;
            }
            let fwd_b = fb.out_currency;
            let output_c = fc.out_currency;
            let in_currency_c = fc.in_currency;
            let mut at = v4_scaffold_table(inputs);
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let v3a = at.add(fa.pool_address).ok()?;
            let v3b = at.add(fb.pool_address).ok()?;
            let forward_b = at.add(fwd_b).ok()?;
            let fee_c = fc.swap_fee;
            let ts_c = fc.tick_spacing;
            let c0 = at.add(fc.currency0_address).ok()?;
            let c1 = at.add(fc.currency1_address).ok()?;
            let weth = inputs.weth_address;

            let v4_inner: Plan = vec![
                PlanStep::V4Settle {
                    currency_addr: fwd_b,
                    amount: c_swap_in,
                },
                PlanStep::V4Swap {
                    c0_idx: c0,
                    c1_idx: c1,
                    fee: fee_c,
                    tick_spacing: ts_c,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fc.zfo,
                    amount: c_swap_in,
                    in_currency: in_currency_c,
                    in_amount: c_swap_in,
                    out_currency: output_c,
                    out_amount: out_c,
                },
                PlanStep::V4TakeCompact {
                    currency_idx: SENTINEL_WETH,
                    currency_addr: weth,
                    recipient_idx: v3a,
                    amount: optimal_input,
                    seeds_pool: None,
                    repays_flash: Some(fa.pool_address),
                },
                PlanStep::V4SettleAll,
            ];
            let unlock = PlanStep::V4Unlock {
                inner: v4_inner,
                pool_manager_idx: pm_idx,
            };
            let inner_a = mechanics::v3_flash_to(
                &mut at,
                fa,
                out_a,
                optimal_input,
                false,
                v3b,
                Some(fb.pool_address),
                true,
                vec![unlock],
            )?;
            let plan: Plan = vec![
                PlanStep::V4Sync {
                    currency_idx: forward_b,
                    currency_addr: fwd_b,
                },
                mechanics::v3_flash_to(
                    &mut at,
                    fb,
                    out_b,
                    b_swap_in,
                    false,
                    pm_idx,
                    None,
                    false,
                    vec![inner_a],
                )?,
            ];
            Some((plan, at))
        } else if facts[0].prot == Prot::V2
            && facts[1].prot == Prot::V4
            && facts[2].prot == Prot::V2
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(b_swap_in) || !fits_i128(c_swap_in) {
                return None;
            }
            let forward_a_cur = fa.out_currency;
            let forward_b_cur = fb.out_currency;
            let in_currency_b = fb.in_currency;
            let in_c = fc.in_currency;
            let mut at = v4_scaffold_table(inputs);
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let forward_a = at.add(forward_a_cur).ok()?;
            let forward_b = at.add(forward_b_cur).ok()?;
            let v2a = at.add(fa.pool_address).ok()?;
            let v2c = at.add(fc.pool_address).ok()?;
            let fee_b = fb.swap_fee;
            let ts_b = fb.tick_spacing;
            let c0_b = at.add(fb.currency0_address).ok()?;
            let c1_b = at.add(fb.currency1_address).ok()?;
            let weth = inputs.weth_address;

            let a_step = mechanics::v2_swap(&mut at, fa, b_swap_in, pm_idx, None, false)?;
            let v4_inner: Plan = vec![
                PlanStep::V4Sync {
                    currency_idx: forward_a,
                    currency_addr: forward_a_cur,
                },
                a_step,
                PlanStep::V4Settle {
                    currency_addr: forward_a_cur,
                    amount: b_swap_in,
                },
                PlanStep::V4Swap {
                    c0_idx: c0_b,
                    c1_idx: c1_b,
                    fee: fee_b,
                    tick_spacing: ts_b,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fb.zfo,
                    amount: b_swap_in,
                    in_currency: in_currency_b,
                    in_amount: b_swap_in,
                    out_currency: forward_b_cur,
                    out_amount: out_b,
                },
                PlanStep::V4TakeCompact {
                    currency_idx: forward_b,
                    currency_addr: forward_b_cur,
                    recipient_idx: v2c,
                    amount: out_b,
                    seeds_pool: None,
                    repays_flash: Some(fc.pool_address),
                },
                PlanStep::V4SettleDelta {
                    currency_idx: forward_a,
                    currency_addr: forward_a_cur,
                },
            ];
            let c_cb: Plan = vec![
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v2a,
                    amount: optimal_input,
                    seeds_pool: Some(fa.pool_address),
                    repays_flash: None,
                },
                PlanStep::V4Unlock {
                    inner: v4_inner,
                    pool_manager_idx: pm_idx,
                },
            ];
            let plan: Plan = vec![
                PlanStep::SelfFund {
                    currency: weth,
                    amount: optimal_input,
                },
                mechanics::v2_flash(&mut at, fc, out_c, in_c, c_swap_in, c_cb)?,
            ];
            Some((plan, at))
        } else if facts[0].prot == Prot::V2
            && facts[1].prot == Prot::V4
            && facts[2].prot == Prot::V3
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let v4_swap_in = *inputs.consumed_inputs.get(1)?;
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(v4_swap_in) || !fits_i128(c_swap_in) {
                return None;
            }
            let forward_a_cur = fa.out_currency;
            let forward_b_cur = fb.out_currency;
            let in_currency_b = fb.in_currency;
            let mut at = v4_scaffold_table(inputs);
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let forward_a = at.add(forward_a_cur).ok()?;
            let forward_b = at.add(forward_b_cur).ok()?;
            let v2a = at.add(fa.pool_address).ok()?;
            let v3c = at.add(fc.pool_address).ok()?;
            let fee_b = fb.swap_fee;
            let ts_b = fb.tick_spacing;
            let c0_b = at.add(fb.currency0_address).ok()?;
            let c1_b = at.add(fb.currency1_address).ok()?;
            let weth = inputs.weth_address;

            let a_step = mechanics::v2_swap(&mut at, fa, v4_swap_in, pm_idx, None, false)?;
            let v4_inner: Plan = vec![
                PlanStep::V4Sync {
                    currency_idx: forward_a,
                    currency_addr: forward_a_cur,
                },
                a_step,
                PlanStep::V4Settle {
                    currency_addr: forward_a_cur,
                    amount: v4_swap_in,
                },
                PlanStep::V4Swap {
                    c0_idx: c0_b,
                    c1_idx: c1_b,
                    fee: fee_b,
                    tick_spacing: ts_b,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fb.zfo,
                    amount: v4_swap_in,
                    in_currency: in_currency_b,
                    in_amount: v4_swap_in,
                    out_currency: forward_b_cur,
                    out_amount: out_b,
                },
                PlanStep::V4TakeCompact {
                    currency_idx: forward_b,
                    currency_addr: forward_b_cur,
                    recipient_idx: v3c,
                    amount: c_swap_in,
                    seeds_pool: None,
                    repays_flash: Some(fc.pool_address),
                },
                PlanStep::V4SettleDelta {
                    currency_idx: forward_a,
                    currency_addr: forward_a_cur,
                },
            ];
            let c_cb: Plan = vec![
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v2a,
                    amount: optimal_input,
                    seeds_pool: Some(fa.pool_address),
                    repays_flash: None,
                },
                PlanStep::V4Unlock {
                    inner: v4_inner,
                    pool_manager_idx: pm_idx,
                },
            ];
            let plan: Plan = vec![
                PlanStep::SelfFund {
                    currency: weth,
                    amount: optimal_input,
                },
                mechanics::v3_flash_to(
                    &mut at,
                    fc,
                    out_c,
                    c_swap_in,
                    false,
                    SENTINEL_SELF,
                    None,
                    false,
                    c_cb,
                )?,
            ];
            Some((plan, at))
        } else if facts[0].prot == Prot::V3
            && facts[1].prot == Prot::V4
            && facts[2].prot == Prot::V2
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            if !fits_i128(b_swap_in) {
                return None;
            }
            let fwd_a = fa.out_currency;
            let forward_b_cur = fb.out_currency;
            let in_currency_b = fb.in_currency;
            let mut at = v4_scaffold_table(inputs);
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let v3a = at.add(fa.pool_address).ok()?;
            let v2c = at.add(fc.pool_address).ok()?;
            let forward_a = at.add(fwd_a).ok()?;
            let forward_b = at.add(forward_b_cur).ok()?;
            let fee_b = fb.swap_fee;
            let ts_b = fb.tick_spacing;
            let c0_b = at.add(fb.currency0_address).ok()?;
            let c1_b = at.add(fb.currency1_address).ok()?;
            let weth = inputs.weth_address;

            let v4_inner: Plan = vec![
                PlanStep::V4Settle {
                    currency_addr: fwd_a,
                    amount: b_swap_in,
                },
                PlanStep::V4Swap {
                    c0_idx: c0_b,
                    c1_idx: c1_b,
                    fee: fee_b,
                    tick_spacing: ts_b,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fb.zfo,
                    amount: b_swap_in,
                    in_currency: in_currency_b,
                    in_amount: b_swap_in,
                    out_currency: forward_b_cur,
                    out_amount: out_b,
                },
                PlanStep::V4TakeDelta {
                    currency_idx: forward_b,
                    currency_addr: forward_b_cur,
                    recipient_idx: v2c,
                    seeds_pool: Some(fc.pool_address),
                },
                PlanStep::V4SettleAll,
            ];
            let c_step = mechanics::v2_swap(&mut at, fc, out_c, SENTINEL_SELF, None, false)?;
            let cb: Plan = vec![
                PlanStep::V4Unlock {
                    inner: v4_inner,
                    pool_manager_idx: pm_idx,
                },
                c_step,
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v3a,
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
                PlanStep::V4Sync {
                    currency_idx: forward_a,
                    currency_addr: fwd_a,
                },
                mechanics::v3_flash_to(
                    &mut at,
                    fa,
                    out_a,
                    optimal_input,
                    false,
                    pm_idx,
                    None,
                    false,
                    cb,
                )?,
            ];
            Some((plan, at))
        } else if facts[0].prot == Prot::V3
            && facts[1].prot == Prot::V4
            && facts[2].prot == Prot::V4
        {
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let optimal_input = inputs.optimal_input;
            let out_a = inputs.hop_outputs[0];
            let out_b = inputs.hop_outputs[1];
            let out_c = inputs.hop_outputs[2];
            if inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(b_swap_in) || !fits_i128(c_swap_in) {
                return None;
            }
            let fwd_a = fa.out_currency;
            let forward_b_cur = fb.out_currency;
            let in_currency_b = fb.in_currency;
            let output_c = fc.out_currency;
            let in_currency_c = fc.in_currency;
            let mut at = v4_scaffold_table(inputs);
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let v3a = at.add(fa.pool_address).ok()?;
            let forward_a = at.add(fwd_a).ok()?;
            let fee_b = fb.swap_fee;
            let ts_b = fb.tick_spacing;
            let fee_c = fc.swap_fee;
            let ts_c = fc.tick_spacing;
            let c0_b = at.add(fb.currency0_address).ok()?;
            let c1_b = at.add(fb.currency1_address).ok()?;
            let c0_c = at.add(fc.currency0_address).ok()?;
            let c1_c = at.add(fc.currency1_address).ok()?;
            let weth = inputs.weth_address;

            let v4_inner: Plan = vec![
                PlanStep::V4Settle {
                    currency_addr: fwd_a,
                    amount: b_swap_in,
                },
                PlanStep::V4Swap {
                    c0_idx: c0_b,
                    c1_idx: c1_b,
                    fee: fee_b,
                    tick_spacing: ts_b,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fb.zfo,
                    amount: b_swap_in,
                    in_currency: in_currency_b,
                    in_amount: b_swap_in,
                    out_currency: forward_b_cur,
                    out_amount: out_b,
                },
                PlanStep::V4Swap {
                    c0_idx: c0_c,
                    c1_idx: c1_c,
                    fee: fee_c,
                    tick_spacing: ts_c,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: fc.zfo,
                    amount: c_swap_in,
                    in_currency: in_currency_c,
                    in_amount: c_swap_in,
                    out_currency: output_c,
                    out_amount: out_c,
                },
                PlanStep::V4TakeDelta {
                    currency_idx: SENTINEL_WETH,
                    currency_addr: weth,
                    recipient_idx: SENTINEL_SELF,
                    seeds_pool: None,
                },
                PlanStep::V4SettleAll,
            ];
            let cb: Plan = vec![
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v3a,
                    amount: optimal_input,
                    seeds_pool: None,
                    repays_flash: Some(fa.pool_address),
                },
                PlanStep::V4Unlock {
                    inner: v4_inner,
                    pool_manager_idx: pm_idx,
                },
            ];
            let plan: Plan = vec![
                PlanStep::SelfFund {
                    currency: weth,
                    amount: optimal_input,
                },
                PlanStep::V4Sync {
                    currency_idx: forward_a,
                    currency_addr: fwd_a,
                },
                mechanics::v3_flash_to(
                    &mut at,
                    fa,
                    out_a,
                    optimal_input,
                    false,
                    pm_idx,
                    None,
                    false,
                    cb,
                )?,
            ];
            Some((plan, at))
        } else {
            None
        }
    };
}
