//! 2-hop V4-led shape (block 3: NetZero lead).

use super::super::{fits_i128, mechanics, HopFacts};
use crate::composers::{resolve_axes, ComposerInputs, CurrencyBridge, NATIVE_CURRENCY_ADDRESS};
use crate::encoders::{AddressTable, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH};
use crate::grammar_ledger::{ProfitCapture, Prot};
use crate::grammar_plan::{Plan, PlanStep, V4BatchSwap};
use crate::grammar_shape::{erc6909_batch_capture_declines, v4_scaffold_table};

#[expect(clippy::too_many_lines, clippy::needless_return)]
pub(crate) fn derive(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    return if facts[1].prot == Prot::V4 {
        // ── v4v4 body (unchanged) ──
        {
            let (fa, fb) = (&facts[0], &facts[1]);
            let optimal_input = inputs.optimal_input;
            let forward_out = inputs.hop_outputs[0];
            let b_out = inputs.hop_outputs[1];
            if forward_out == 0 || b_out == 0 {
                return None;
            }
            if !fits_i128(optimal_input) || !fits_i128(forward_out) {
                return None;
            }
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            if !fits_i128(b_swap_in) {
                return None;
            }
            let weth = inputs.weth_address;

            let mid_currency_a = fa.out_currency;
            let in_currency_a = fa.in_currency;
            let out_currency_b = fb.out_currency;
            let mid_currency_b = fb.in_currency;
            let bridge = CurrencyBridge::at_boundary(mid_currency_a, mid_currency_b);
            let currency_gap = bridge.needs_bridge();
            let out_currency_a = mid_currency_a;
            let capture = resolve_axes(inputs.opts).1;
            if capture == ProfitCapture::Native
                && out_currency_b != weth
                && out_currency_b != NATIVE_CURRENCY_ADDRESS
            {
                return None;
            }

            let mut at = v4_scaffold_table(inputs);
            let c0_a = at.add(fa.currency0_address).ok()?;
            let c1_a = at.add(fa.currency1_address).ok()?;
            let c0_b = at.add(fb.currency0_address).ok()?;
            let c1_b = at.add(fb.currency1_address).ok()?;
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let weth_idx = SENTINEL_WETH;

            let fee_a = fa.swap_fee;
            let ts_a = fa.tick_spacing;
            let fee_b = fb.swap_fee;
            let ts_b = fb.tick_spacing;

            let inner: Plan = if currency_gap {
                let (take_currency, settle_currency) = match bridge {
                    CurrencyBridge::Wrap => (NATIVE_CURRENCY_ADDRESS, weth),
                    CurrencyBridge::Unwrap => (weth, NATIVE_CURRENCY_ADDRESS),
                    CurrencyBridge::None => unreachable!("currency_gap implies a bridge"),
                };
                let take_idx = if take_currency == NATIVE_CURRENCY_ADDRESS {
                    SENTINEL_NATIVE
                } else {
                    weth_idx
                };
                let settle_idx = if settle_currency == NATIVE_CURRENCY_ADDRESS {
                    SENTINEL_NATIVE
                } else {
                    weth_idx
                };
                let out_idx = if fb.zfo { c1_b } else { c0_b };
                vec![
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
                        out_currency: out_currency_a,
                        out_amount: forward_out,
                    },
                    PlanStep::V4TakeCompact {
                        currency_idx: take_idx,
                        currency_addr: take_currency,
                        recipient_idx: SENTINEL_SELF,
                        amount: forward_out,
                        seeds_pool: None,
                        repays_flash: None,
                    },
                    match bridge {
                        CurrencyBridge::Wrap => PlanStep::WethDeposit {
                            weth_idx,
                            weth_addr: weth,
                            amount: forward_out,
                        },
                        CurrencyBridge::Unwrap => PlanStep::WethWithdraw {
                            weth_idx,
                            weth_addr: weth,
                            amount: forward_out,
                        },
                        CurrencyBridge::None => unreachable!(),
                    },
                    PlanStep::V4Swap {
                        c0_idx: c0_b,
                        c1_idx: c1_b,
                        fee: fee_b,
                        tick_spacing: ts_b,
                        hooks_idx: SENTINEL_NATIVE,
                        zfo: fb.zfo,
                        amount: b_swap_in,
                        in_currency: mid_currency_b,
                        in_amount: b_swap_in,
                        out_currency: out_currency_b,
                        out_amount: b_out,
                    },
                    PlanStep::V4SettleDelta {
                        currency_idx: settle_idx,
                        currency_addr: settle_currency,
                    },
                    PlanStep::V4TakeDelta {
                        currency_idx: out_idx,
                        currency_addr: out_currency_b,
                        recipient_idx: SENTINEL_SELF,
                        seeds_pool: None,
                    },
                    PlanStep::V4SettleAll,
                ]
            } else if erc6909_batch_capture_declines(
                capture,
                inputs.opts.use_v4_batch,
                out_currency_b,
                weth,
            ) {
                // SMOZG3: batch tail-settle + V4_MINT on the WETH terminal is
                // unexecutable on the current executor artifact (D0) — decline
                // until TGUZCT ships the composable artifact.
                return None;
            } else {
                let swaps: Vec<PlanStep> = if inputs.opts.use_v4_batch {
                    vec![PlanStep::V4Batch {
                        entries: vec![
                            V4BatchSwap {
                                c0_idx: c0_a,
                                c1_idx: c1_a,
                                fee: fee_a,
                                tick_spacing: ts_a,
                                hooks_idx: SENTINEL_NATIVE,
                                zfo: fa.zfo,
                                amount: optimal_input,
                                in_currency: in_currency_a,
                                in_amount: optimal_input,
                                out_currency: out_currency_a,
                                out_amount: forward_out,
                            },
                            V4BatchSwap {
                                c0_idx: c0_b,
                                c1_idx: c1_b,
                                fee: fee_b,
                                tick_spacing: ts_b,
                                hooks_idx: SENTINEL_NATIVE,
                                zfo: fb.zfo,
                                amount: b_swap_in,
                                in_currency: out_currency_a,
                                in_amount: b_swap_in,
                                out_currency: out_currency_b,
                                out_amount: b_out,
                            },
                        ],
                    }]
                } else {
                    vec![
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
                            out_currency: out_currency_a,
                            out_amount: forward_out,
                        },
                        PlanStep::V4Swap {
                            c0_idx: c0_b,
                            c1_idx: c1_b,
                            fee: fee_b,
                            tick_spacing: ts_b,
                            hooks_idx: SENTINEL_NATIVE,
                            zfo: fb.zfo,
                            amount: b_swap_in,
                            in_currency: out_currency_a,
                            in_amount: b_swap_in,
                            out_currency: out_currency_b,
                            out_amount: b_out,
                        },
                    ]
                };
                let out_idx = if fb.zfo { c1_b } else { c0_b };
                let capture_steps: Vec<PlanStep> =
                    if capture == ProfitCapture::Erc6909 && out_currency_b == weth {
                        let profit = b_out.saturating_sub(optimal_input);
                        if profit > 0 {
                            vec![PlanStep::V4Mint {
                                currency_idx: weth_idx,
                                currency_addr: weth,
                                recipient_idx: SENTINEL_SELF,
                                amount: profit,
                            }]
                        } else {
                            vec![]
                        }
                    } else if !inputs.opts.use_v4_batch
                        || (out_currency_b != NATIVE_CURRENCY_ADDRESS && out_currency_b != weth)
                    {
                        vec![PlanStep::V4TakeDelta {
                            currency_idx: out_idx,
                            currency_addr: out_currency_b,
                            recipient_idx: SENTINEL_SELF,
                            seeds_pool: None,
                        }]
                    } else {
                        vec![]
                    };
                let native_withdraw: Vec<PlanStep> =
                    if capture == ProfitCapture::Native && out_currency_b == weth {
                        vec![PlanStep::WethWithdraw {
                            weth_idx,
                            weth_addr: weth,
                            amount: b_out.saturating_sub(optimal_input),
                        }]
                    } else {
                        vec![]
                    };
                swaps
                    .into_iter()
                    .chain(capture_steps)
                    .chain(native_withdraw)
                    .chain(std::iter::once(PlanStep::V4SettleAll))
                    .collect()
            };
            let plan: Plan = vec![PlanStep::V4Unlock {
                inner,
                pool_manager_idx: pm_idx,
            }];
            Some((plan, at))
        }
    } else if facts[1].prot == Prot::V2 {
        // ── v4v2 body (unchanged) ──
        {
            // ── v4v2 (folded from derive_2hop_v4v2; ADR-031 D6)
            let (fa, fb) = (&facts[0], &facts[1]);
            let optimal_input = inputs.optimal_input;
            let forward_out = inputs.hop_outputs[0];
            let weth_out = inputs.hop_outputs[1];
            if forward_out == 0 || weth_out == 0 {
                return None;
            }
            if !fits_i128(optimal_input) || !fits_i128(forward_out) {
                return None;
            }
            let weth = inputs.weth_address;
            let out_currency_a = fa.out_currency;
            let in_currency_a = fa.in_currency;
            let v4_out_native = out_currency_a == NATIVE_CURRENCY_ADDRESS;
            let out_currency_b = fb.out_currency;
            if v4_out_native {
            } else if in_currency_a != weth && in_currency_a != NATIVE_CURRENCY_ADDRESS {
                return None;
            }
            let mut at = AddressTable::with_sentinels(
                Some(weth),
                Some(inputs.executor_address),
                Some(inputs.pool_manager_address),
            );
            let c0_a = at.add(fa.currency0_address).ok()?;
            let c1_a = at.add(fa.currency1_address).ok()?;
            let v2_idx = at.add(fb.pool_address).ok()?;
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let weth_idx = SENTINEL_WETH;
            let forward_idx = if fa.zfo { c1_a } else { c0_a };
            let fee_a = fa.swap_fee;
            let ts_a = fa.tick_spacing;
            let v4_in_native = in_currency_a == NATIVE_CURRENCY_ADDRESS;
            let input_idx = if fa.zfo { c0_a } else { c1_a };
            let mut inner: Plan = vec![PlanStep::V4Swap {
                c0_idx: c0_a,
                c1_idx: c1_a,
                fee: fee_a,
                tick_spacing: ts_a,
                hooks_idx: SENTINEL_NATIVE,
                zfo: fa.zfo,
                amount: optimal_input,
                in_currency: in_currency_a,
                in_amount: optimal_input,
                out_currency: out_currency_a,
                out_amount: forward_out,
            }];
            if v4_out_native {
                let v2 = mechanics::v2_swap(&mut at, fb, weth_out, SENTINEL_SELF, None, false)?;
                inner.extend([
                    PlanStep::V4TakeCompact {
                        currency_idx: SENTINEL_NATIVE,
                        currency_addr: NATIVE_CURRENCY_ADDRESS,
                        recipient_idx: SENTINEL_SELF,
                        amount: forward_out,
                        seeds_pool: None,
                        repays_flash: None,
                    },
                    PlanStep::WethDeposit {
                        weth_idx,
                        weth_addr: weth,
                        amount: forward_out,
                    },
                    PlanStep::Erc20Transfer {
                        token_idx: weth_idx,
                        token_addr: weth,
                        recipient_idx: v2_idx,
                        amount: forward_out,
                        seeds_pool: Some(fb.pool_address),
                        repays_flash: None,
                    },
                    v2,
                    PlanStep::V4SettleDelta {
                        currency_idx: input_idx,
                        currency_addr: in_currency_a,
                    },
                    PlanStep::V4SettleAll,
                ]);
            } else {
                let v2 = mechanics::v2_swap(&mut at, fb, weth_out, SENTINEL_SELF, None, false)?;
                inner.extend([
                    PlanStep::V4TakeCompact {
                        currency_idx: forward_idx,
                        currency_addr: out_currency_a,
                        recipient_idx: v2_idx,
                        amount: forward_out,
                        seeds_pool: Some(fb.pool_address),
                        repays_flash: None,
                    },
                    v2,
                    PlanStep::V4Sync {
                        currency_idx: weth_idx,
                        currency_addr: weth,
                    },
                    PlanStep::Erc20Transfer {
                        token_idx: weth_idx,
                        token_addr: weth,
                        recipient_idx: pm_idx,
                        amount: optimal_input,
                        seeds_pool: None,
                        repays_flash: None,
                    },
                    PlanStep::V4Settle {
                        currency_addr: weth,
                        amount: optimal_input,
                    },
                    PlanStep::V4SettleAll,
                ]);
                if v4_in_native {
                    let settle_all = inner.pop()?;
                    inner.pop();
                    inner.pop();
                    inner.pop();
                    inner.extend([
                        PlanStep::WethWithdraw {
                            weth_idx,
                            weth_addr: weth,
                            amount: optimal_input,
                        },
                        PlanStep::NativeTransfer {
                            amount: optimal_input,
                        },
                        PlanStep::V4SettleDelta {
                            currency_idx: SENTINEL_NATIVE,
                            currency_addr: NATIVE_CURRENCY_ADDRESS,
                        },
                        settle_all,
                    ]);
                }
                let _ = input_idx;
            }
            let plan: Plan = if out_currency_b == weth {
                vec![PlanStep::V4Unlock {
                    inner,
                    pool_manager_idx: pm_idx,
                }]
            } else {
                vec![
                    PlanStep::SelfFund {
                        currency: weth,
                        amount: optimal_input,
                    },
                    PlanStep::V4Unlock {
                        inner,
                        pool_manager_idx: pm_idx,
                    },
                ]
            };
            return Some((plan, at));
        }
    } else {
        // ── v4v3 body (unchanged) ──
        {
            let (fa, fb) = (&facts[0], &facts[1]);
            let optimal_input = inputs.optimal_input;
            let forward_out = inputs.hop_outputs[0];
            let weth_out = inputs.hop_outputs[1];
            if forward_out == 0 || weth_out == 0 {
                return None;
            }
            if !fits_i128(optimal_input) || !fits_i128(forward_out) {
                return None;
            }
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            if !fits_i128(b_swap_in) {
                return None;
            }
            let weth = inputs.weth_address;

            let out_currency_a = fa.out_currency;
            let in_currency_a = fa.in_currency;
            let v4_out_native = out_currency_a == NATIVE_CURRENCY_ADDRESS;
            let out_currency_b = fb.out_currency;
            if v4_out_native {
                let in_currency_b = fb.in_currency;
                if in_currency_b != weth {
                    return None;
                }
                if in_currency_a == NATIVE_CURRENCY_ADDRESS || in_currency_a == weth {
                    return None;
                }
            } else if in_currency_a != weth && in_currency_a != NATIVE_CURRENCY_ADDRESS {
                return None;
            }

            let mut at = AddressTable::with_sentinels(
                Some(weth),
                Some(inputs.executor_address),
                Some(inputs.pool_manager_address),
            );
            let c0_a = at.add(fa.currency0_address).ok()?;
            let c1_a = at.add(fa.currency1_address).ok()?;
            let v3_idx = at.add(fb.pool_address).ok()?;
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let weth_idx = SENTINEL_WETH;
            let forward_idx = if fa.zfo { c1_a } else { c0_a };

            let fee_a = fa.swap_fee;
            let ts_a = fa.tick_spacing;
            let v4_in_native = in_currency_a == NATIVE_CURRENCY_ADDRESS;
            let input_idx = if fa.zfo { c0_a } else { c1_a };

            let mut inner: Plan = vec![PlanStep::V4Swap {
                c0_idx: c0_a,
                c1_idx: c1_a,
                fee: fee_a,
                tick_spacing: ts_a,
                hooks_idx: SENTINEL_NATIVE,
                zfo: fa.zfo,
                amount: optimal_input,
                in_currency: in_currency_a,
                in_amount: optimal_input,
                out_currency: out_currency_a,
                out_amount: forward_out,
            }];
            if v4_out_native {
                inner.extend([
                    PlanStep::V4TakeCompact {
                        currency_idx: SENTINEL_NATIVE,
                        currency_addr: NATIVE_CURRENCY_ADDRESS,
                        recipient_idx: SENTINEL_SELF,
                        amount: forward_out,
                        seeds_pool: None,
                        repays_flash: None,
                    },
                    PlanStep::WethDeposit {
                        weth_idx,
                        weth_addr: weth,
                        amount: forward_out,
                    },
                    PlanStep::FlashSwap {
                        pool_idx: v3_idx,
                        pool_addr: fb.pool_address,
                        protocol: Prot::V3,
                        zfo: fb.zfo,
                        fee: fb.swap_fee,
                        out_currency: out_currency_b,
                        out_amount: weth_out,
                        in_currency: weth,
                        in_amount: b_swap_in,
                        recipient_idx: SENTINEL_SELF,
                        recipient_pool_addr: None,
                        recipient_pool_repays: false,
                        auto_repay: true,
                        callback: vec![],
                    },
                    PlanStep::V4SettleDelta {
                        currency_idx: input_idx,
                        currency_addr: in_currency_a,
                    },
                    PlanStep::V4SettleAll,
                ]);
            } else {
                inner.extend([
                    PlanStep::V4TakeCompact {
                        currency_idx: forward_idx,
                        currency_addr: out_currency_a,
                        recipient_idx: SENTINEL_SELF,
                        amount: forward_out,
                        seeds_pool: None,
                        repays_flash: None,
                    },
                    PlanStep::FlashSwap {
                        pool_idx: v3_idx,
                        pool_addr: fb.pool_address,
                        protocol: Prot::V3,
                        zfo: fb.zfo,
                        fee: fb.swap_fee,
                        out_currency: out_currency_b,
                        out_amount: weth_out,
                        in_currency: out_currency_a,
                        in_amount: b_swap_in,
                        recipient_idx: SENTINEL_SELF,
                        recipient_pool_addr: None,
                        recipient_pool_repays: false,
                        auto_repay: true,
                        callback: vec![],
                    },
                    PlanStep::V4SettleDelta {
                        currency_idx: weth_idx,
                        currency_addr: weth,
                    },
                    PlanStep::V4SettleAll,
                ]);
                if v4_in_native {
                    let settle_all = inner.pop()?;
                    inner.pop();
                    inner.extend([
                        PlanStep::WethWithdraw {
                            weth_idx,
                            weth_addr: weth,
                            amount: optimal_input,
                        },
                        PlanStep::NativeTransfer {
                            amount: optimal_input,
                        },
                        PlanStep::V4SettleDelta {
                            currency_idx: SENTINEL_NATIVE,
                            currency_addr: NATIVE_CURRENCY_ADDRESS,
                        },
                        settle_all,
                    ]);
                }
                let _ = input_idx;
            }
            let plan: Plan = if out_currency_b == weth {
                vec![PlanStep::V4Unlock {
                    inner,
                    pool_manager_idx: pm_idx,
                }]
            } else {
                vec![
                    PlanStep::SelfFund {
                        currency: weth,
                        amount: optimal_input,
                    },
                    PlanStep::V4Unlock {
                        inner,
                        pool_manager_idx: pm_idx,
                    },
                ]
            };
            Some((plan, at))
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
            "/src/grammar_walker/shapes/two_hop_v4_led.rs"
        ));
        let i = src.find("\n#[cfg(test)]").unwrap_or(src.len());
        &src[..i]
    }

    /// RED by design (T1 RKNRJO, epic 6SWFBS): 49 literal `PlanStep::` sites
    /// today. Goes GREEN when T4 (4FKIPB) walks the v4v2/v4v3/v4v4 arms onto
    /// `mechanics` + the shared capture/bridge helpers; then it stays put as
    /// the honesty invariant (D6 precedent: RED probe at f3b06397, honesty
    /// test kept at DDNEAB).
    #[test]
    fn two_hop_v4_led_arms_use_mechanics_not_planstep_literals() {
        let src = walk_source();
        let count = src.matches("PlanStep::").count();
        assert_eq!(
            count, 0,
            "two_hop_v4_led.rs still hand-builds {count} PlanStep sites; the walk must express them via mechanics:: + shared capture/bridge helpers"
        );
    }

    /// Per-file byte-identity pin: the exact current streams for every
    /// v4v2/v4v3/v4v4 family × amount-set × `EncodeOptions` combo, including
    /// the decline partition (the SMOZG3 erc6909×batch WETH-terminal declines
    /// stay declined — TGUZCT is still open).
    const FAMILIES: &[(&str, &[&str])] = &[
        ("V4_V2", &["V4", "V2"]),
        ("V4_V3", &["V4", "V3"]),
        ("V4_V4", &["V4", "V4"]),
    ];

    const GOLDEN: &[&str] = &[
        "V4_V2_base_0 3adae8a43d6f79ce",
        "V4_V2_base_1 3adae8a43d6f79ce",
        "V4_V2_base_2 ff9113c64ec696a9",
        "V4_V2_base_3 ff9113c64ec696a9",
        "V4_V2_batch_0 3adae8a43d6f79ce",
        "V4_V2_batch_1 3adae8a43d6f79ce",
        "V4_V2_batch_2 ff9113c64ec696a9",
        "V4_V2_batch_3 ff9113c64ec696a9",
        "V4_V2_erc6909_0 3adae8a43d6f79ce",
        "V4_V2_erc6909_1 3adae8a43d6f79ce",
        "V4_V2_erc6909_2 ff9113c64ec696a9",
        "V4_V2_erc6909_3 ff9113c64ec696a9",
        "V4_V3_base_0 518e53d8d63b481e",
        "V4_V3_base_1 e1abca545ee1b7d7",
        "V4_V3_base_2 684ebfe7c96d6ab6",
        "V4_V3_base_3 2a3e56a813e6561a",
        "V4_V3_batch_0 518e53d8d63b481e",
        "V4_V3_batch_1 e1abca545ee1b7d7",
        "V4_V3_batch_2 684ebfe7c96d6ab6",
        "V4_V3_batch_3 2a3e56a813e6561a",
        "V4_V3_erc6909_0 518e53d8d63b481e",
        "V4_V3_erc6909_1 e1abca545ee1b7d7",
        "V4_V3_erc6909_2 684ebfe7c96d6ab6",
        "V4_V3_erc6909_3 2a3e56a813e6561a",
        "V4_V4_base_0 51aa71ec610de3b5",
        "V4_V4_base_1 5a137aa3db157ffa",
        "V4_V4_base_2 ac79cd7b8e92e5cc",
        "V4_V4_base_3 124e035d9389cec8",
        "V4_V4_batch_0 677fc97e39900f55",
        "V4_V4_batch_1 70705488b1a36c9a",
        "V4_V4_batch_2 f07dd803d7b0ca7a",
        "V4_V4_batch_3 6a28c372bc84cf16",
        "V4_V4_erc6909_0 51aa71ec610de3b5",
        "V4_V4_erc6909_1 5a137aa3db157ffa",
        "V4_V4_erc6909_2 ac79cd7b8e92e5cc",
        "V4_V4_erc6909_3 124e035d9389cec8",
    ];

    #[test]
    fn two_hop_v4_led_streams_are_pinned() {
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
            "2-hop V4-led streams changed — T1 pin"
        );
    }
}
