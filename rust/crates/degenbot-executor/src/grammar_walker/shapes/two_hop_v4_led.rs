//! 2-hop V4-led shape (block 3: NetZero lead).
//!
//! **T4 walk (6SWFBS / 4FKIPB).** The v4v4/v4v2/v4v4 per-family bodies are
//! walked onto the `mechanics` primitives + the shared V4-crossing helpers:
//! `v4_terminal_capture_steps` (the V4-tail capture: `V4Mint` on an erc6909
//! WETH terminal, the explicit `V4TakeDelta` for the non-batch / tok
//! terminal, `WethWithdraw` for the Native capture) and `v4_bridge_steps`
//! (the native↔WETH boundary take + deposit/withdraw + settle pairing). The
//! surviving shape logic is the v4v2/v4v3 tail swap (`v2_swap` /
//! auto-repay `v3_flash`, both mechanics) and each arm's
//! `v4_in_native` tail-dedebt swap (withdraw + native transfer + the
//! NATIVE `v4_settle_delta`). Table staging order stays byte-pinned — the
//! lead-pool re-add inside `v2_swap`/`v3_flash` must not precede the
//! sentinel-staged pair indices — so the T1 golden pins (below) hold without
//! a single edit.

use super::super::{fits_i128, mechanics, HopFacts};
use crate::composers::{resolve_axes, ComposerInputs, CurrencyBridge, NATIVE_CURRENCY_ADDRESS};
use crate::encoders::{AddressTable, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH};
use crate::grammar_ledger::{ProfitCapture, Prot};
use crate::grammar_plan::Plan;
use crate::grammar_shape::{v4_bridge_steps, v4_scaffold_table, v4_terminal_capture_steps};

pub(crate) fn derive(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts[1].prot == Prot::V4 {
        v4v4_arm(&facts[0], &facts[1], inputs)
    } else if facts[1].prot == Prot::V2 {
        v4v2_arm(&facts[0], &facts[1], inputs)
    } else {
        v4v3_arm(&facts[0], &facts[1], inputs)
    }
}

/// v4v4: V4 mid + V4 tail inside one unlock — no flash anywhere. The batch
/// path folds both swaps into one `V4Batch`; the capture/bridge tails come
/// from the shared helpers (the `any_gap` slot carries this arm's
/// explicit-take rule: a tok terminal always takes, a WETH/Native terminal
/// lets the batch auto-settle).
fn v4v4_arm(
    fa: &HopFacts,
    fb: &HopFacts,
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    let optimal = inputs.optimal_input;
    let fwd = inputs.hop_outputs[0];
    let b_out = inputs.hop_outputs[1];
    if fwd == 0 || b_out == 0 {
        return None;
    }
    if !fits_i128(optimal) || !fits_i128(fwd) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_i128(b_swap_in) {
        return None;
    }
    let weth = inputs.weth_address;
    let (out_a, out_b) = (fa.out_currency, fb.out_currency);
    let bridge = CurrencyBridge::at_boundary(out_a, fb.in_currency);
    let capture = resolve_axes(inputs.opts).1;
    if capture == ProfitCapture::Native && out_b != weth && out_b != NATIVE_CURRENCY_ADDRESS {
        return None;
    }

    let mut at = v4_scaffold_table(inputs);
    let c0_a = at.add(fa.currency0_address).ok()?;
    let c1_a = at.add(fa.currency1_address).ok()?;
    let c0_b = at.add(fb.currency0_address).ok()?;
    let c1_b = at.add(fb.currency1_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let out_idx = if fb.zfo { c1_b } else { c0_b };
    let profit = b_out.saturating_sub(optimal);

    let inner: Plan = if bridge.needs_bridge() {
        let (bridge_steps, settle_idx, settle_currency) = v4_bridge_steps(bridge, weth, fwd);
        let mut inner = vec![mechanics::v4_swap(&mut at, fa, optimal, fwd)?];
        inner.extend(bridge_steps);
        inner.push(mechanics::v4_swap(&mut at, fb, b_swap_in, b_out)?);
        inner.push(mechanics::v4_settle_delta(settle_idx, settle_currency));
        inner.push(mechanics::v4_take_delta(
            out_idx,
            out_b,
            SENTINEL_SELF,
            None,
        ));
        inner.push(mechanics::v4_settle_all());
        inner
    } else {
        // TGUZCT/SW42JA: the deployed artifact composes batch × erc6909 on a
        // WETH terminal — the open-weth batch variant (0x43) skips the WETH
        // tail-settle, so the trailing `v4_terminal_capture_steps` mint finds
        // the live delta. (The SMOZG3 pre-deployment decline is lifted.)
        let batch = inputs.opts.use_v4_batch;
        let open_weth = batch && capture == ProfitCapture::Erc6909 && out_b == weth;
        let explicit_take = out_b != NATIVE_CURRENCY_ADDRESS && out_b != weth;
        let steps: Plan = if batch {
            vec![mechanics::v4_batch(
                &mut at,
                fa,
                vec![
                    mechanics::v4_batch_entry(fa, c0_a, c1_a, optimal, fwd, fa.in_currency),
                    mechanics::v4_batch_entry(fb, c0_b, c1_b, b_swap_in, b_out, out_a),
                ],
                open_weth,
            )]
        } else {
            vec![
                mechanics::v4_swap(&mut at, fa, optimal, fwd)?,
                mechanics::v4_swap(&mut at, fb, b_swap_in, b_out)?,
            ]
        };
        let mut inner = steps;
        inner.extend(v4_terminal_capture_steps(
            out_b,
            out_idx,
            capture,
            batch,
            explicit_take,
            profit,
            weth,
        ));
        inner.push(mechanics::v4_settle_all());
        inner
    };
    let plan: Plan = vec![mechanics::v4_unlock(inner, pm_idx)];
    Some((plan, at))
}

/// v4v2: V4 lead, V2 tail — the tail swap is fed by the unlock's own take
/// (seeded into the V2 pool). The `v4_out_native` branch takes + converts
/// native, then hands WETH to the V2 pool; the erc20 branch takes straight
/// to the pool and dedebts the unlock's input ledger (`weth`, or the
/// native-bridge trio when the V4 swap drew native).
fn v4v2_arm(
    fa: &HopFacts,
    fb: &HopFacts,
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    let optimal = inputs.optimal_input;
    let fwd = inputs.hop_outputs[0];
    let tail_out = inputs.hop_outputs[1];
    if fwd == 0 || tail_out == 0 {
        return None;
    }
    if !fits_i128(optimal) || !fits_i128(fwd) {
        return None;
    }
    let weth = inputs.weth_address;
    let out_a = fa.out_currency;
    let in_a = fa.in_currency;
    let v4_out_native = out_a == NATIVE_CURRENCY_ADDRESS;
    let out_b = fb.out_currency;
    if !v4_out_native && in_a != weth && in_a != NATIVE_CURRENCY_ADDRESS {
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
    let input_idx = if fa.zfo { c0_a } else { c1_a };
    let v4_in_native = in_a == NATIVE_CURRENCY_ADDRESS;

    let v2 = mechanics::v2_swap(&mut at, fb, tail_out, SENTINEL_SELF, None, false)?;
    let mut inner: Plan = vec![mechanics::v4_swap(&mut at, fa, optimal, fwd)?];
    if v4_out_native {
        inner.push(mechanics::v4_take_compact_at(
            SENTINEL_NATIVE,
            NATIVE_CURRENCY_ADDRESS,
            SENTINEL_SELF,
            fwd,
            None,
            None,
        ));
        inner.push(mechanics::weth_deposit(weth_idx, weth, fwd));
        inner.push(mechanics::erc20_transfer(
            weth_idx,
            weth,
            v2_idx,
            fwd,
            Some(fb.pool_address),
            None,
        ));
        inner.push(v2);
        inner.push(mechanics::v4_settle_delta(input_idx, in_a));
    } else {
        inner.push(mechanics::v4_take_compact_at(
            forward_idx,
            out_a,
            v2_idx,
            fwd,
            Some(fb.pool_address),
            None,
        ));
        inner.push(v2);
        if v4_in_native {
            inner.push(mechanics::weth_withdraw(weth_idx, weth, optimal));
            inner.push(mechanics::native_transfer(optimal));
            inner.push(mechanics::v4_settle_delta(
                SENTINEL_NATIVE,
                NATIVE_CURRENCY_ADDRESS,
            ));
        } else {
            inner.push(mechanics::v4_sync(weth_idx, weth));
            inner.push(mechanics::erc20_transfer(
                weth_idx, weth, pm_idx, optimal, None, None,
            ));
            inner.push(mechanics::v4_settle(weth, optimal));
        }
    }
    inner.push(mechanics::v4_settle_all());

    let mut plan: Plan = Vec::new();
    if out_b != weth {
        plan.push(mechanics::self_fund(weth, optimal));
    }
    plan.push(mechanics::v4_unlock(inner, pm_idx));
    Some((plan, at))
}

/// v4v3: V4 lead, V3 tail — same enclosure as v4v2, but the tail is an
/// auto-repay `v3_flash` (the flash borrow covers the tail swap; no seed
/// transfer). The `v4_out_native` branch takes + converts native, then
/// flashes from WETH; the erc20 branch takes the forward to SELF and
/// flashes from the forward, dedebting `weth` — or the native-bridge trio
/// when the V4 swap drew native.
fn v4v3_arm(
    fa: &HopFacts,
    fb: &HopFacts,
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    let optimal = inputs.optimal_input;
    let fwd = inputs.hop_outputs[0];
    let tail_out = inputs.hop_outputs[1];
    if fwd == 0 || tail_out == 0 {
        return None;
    }
    if !fits_i128(optimal) || !fits_i128(fwd) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_i128(b_swap_in) {
        return None;
    }
    let weth = inputs.weth_address;
    let out_a = fa.out_currency;
    let in_a = fa.in_currency;
    let v4_out_native = out_a == NATIVE_CURRENCY_ADDRESS;
    let out_b = fb.out_currency;
    if v4_out_native {
        if fb.in_currency != weth {
            return None;
        }
        if in_a == NATIVE_CURRENCY_ADDRESS || in_a == weth {
            return None;
        }
    } else if in_a != weth && in_a != NATIVE_CURRENCY_ADDRESS {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let c0_a = at.add(fa.currency0_address).ok()?;
    let c1_a = at.add(fa.currency1_address).ok()?;
    at.add(fb.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let forward_idx = if fa.zfo { c1_a } else { c0_a };
    let input_idx = if fa.zfo { c0_a } else { c1_a };
    let v4_in_native = in_a == NATIVE_CURRENCY_ADDRESS;

    let flash = mechanics::v3_flash(
        &mut at,
        fb,
        tail_out,
        b_swap_in,
        true,
        Some((SENTINEL_SELF, None, false)),
        vec![],
    )?;
    let mut inner: Plan = vec![mechanics::v4_swap(&mut at, fa, optimal, fwd)?];
    if v4_out_native {
        inner.push(mechanics::v4_take_compact_at(
            SENTINEL_NATIVE,
            NATIVE_CURRENCY_ADDRESS,
            SENTINEL_SELF,
            fwd,
            None,
            None,
        ));
        inner.push(mechanics::weth_deposit(weth_idx, weth, fwd));
        inner.push(flash);
        inner.push(mechanics::v4_settle_delta(input_idx, in_a));
    } else {
        inner.push(mechanics::v4_take_compact_at(
            forward_idx,
            out_a,
            SENTINEL_SELF,
            fwd,
            None,
            None,
        ));
        inner.push(flash);
        if v4_in_native {
            inner.push(mechanics::weth_withdraw(weth_idx, weth, optimal));
            inner.push(mechanics::native_transfer(optimal));
            inner.push(mechanics::v4_settle_delta(
                SENTINEL_NATIVE,
                NATIVE_CURRENCY_ADDRESS,
            ));
        } else {
            inner.push(mechanics::v4_settle_delta(weth_idx, weth));
        }
    }
    inner.push(mechanics::v4_settle_all());

    let mut plan: Plan = Vec::new();
    if out_b != weth {
        plan.push(mechanics::self_fund(weth, optimal));
    }
    plan.push(mechanics::v4_unlock(inner, pm_idx));
    Some((plan, at))
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
    /// v4v2/v4v3/v4v4 family × amount-set × `EncodeOptions` combo. The
    /// SMOZG3 erc6909×batch WETH-terminal decline is lifted (TGUZCT/SW42JA);
    /// the flipped cell is pinned by the shape/matrix tests, not this table.
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
