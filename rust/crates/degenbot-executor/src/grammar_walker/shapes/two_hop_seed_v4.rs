//! 2-hop seed→V4 shape (block 2: SelfRefund lead, NetZero tail).
//!
//! **T3 walk (6SWFBS / CP6BNJ).** The six per-family literals (v2v4/v3v4 ×
//! native-out / native-in / erc20-out) are walked onto the `mechanics`
//! primitives + the single arm assembly below. v2v4 and v3v4 differ only in
//! the lead flash's protocol (V2 vs V3) and in v2v4-native-out's
//! `weth_deposit` (in-callback) standing in for the `self_fund` (plan head);
//! everything else is byte-identical across the pair. AddressTable staging
//! order stays byte-pinned (sentinels → lead pool → pm → c0 → c1 →
//! forward/tok currency) and the per-branch currency ladders stay verbatim,
//! so the T1 golden pins (below) still hold without a single edit.

use super::super::mechanics;
use super::super::{fits_i128, HopFacts};
use crate::composers::{ComposerInputs, NATIVE_CURRENCY_ADDRESS};
use crate::encoders::{AddressTable, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH};
use crate::grammar_ledger::Prot;
use crate::grammar_plan::Plan;

pub(crate) fn derive(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
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
    seed_v4_arm(fa, fb, inputs, forward_out, v4_out_amount, v4_swap_in)
}

#[expect(
    clippy::too_many_lines,
    reason = "One arm for six families (guards + table staging + inner/callback/plan-head sequences); splitting would move the branches into a parameter bag."
)]
fn seed_v4_arm(
    fa: &HopFacts,
    fb: &HopFacts,
    inputs: &ComposerInputs<'_>,
    forward_out: u128,
    v4_out_amount: u128,
    v4_swap_in: u128,
) -> Option<(Plan, AddressTable)> {
    let optimal = inputs.optimal_input;
    let weth = inputs.weth_address;
    let fwd = fa.out_currency;
    let (v4_in, v4_out) = (fb.in_currency, fb.out_currency);

    // Branch selection + the per-branch currency ladders (verbatim from the
    // pre-walk arms). `flash_in` is the lead flash's input currency;
    // `native_in` means the V4 swap draws native (in-callback WETH bridge,
    // no forward feed).
    let (flash_in, native_in) = if v4_out == NATIVE_CURRENCY_ADDRESS {
        if fa.in_currency != weth
            || fwd == NATIVE_CURRENCY_ADDRESS
            || v4_in != fwd
            || v4_in == NATIVE_CURRENCY_ADDRESS
        {
            return None;
        }
        (weth, false)
    } else if v4_in == NATIVE_CURRENCY_ADDRESS {
        let tok = fa.in_currency;
        if tok == weth || tok == NATIVE_CURRENCY_ADDRESS {
            return None;
        }
        (tok, true)
    } else {
        if fa.in_currency != weth || fwd == NATIVE_CURRENCY_ADDRESS || v4_in != fwd {
            return None;
        }
        (weth, false)
    };
    let extra_addr = if native_in { fa.in_currency } else { fwd };

    // Table staging — byte-pinned order (sentinels, lead pool, pm, c0, c1,
    // forward/tok currency). c0/c1 re-adds are no-ops for the table but must
    // precede the extra staging (they fail-closed when the pair is unstageable).
    let mut at = AddressTable::with_sentinels(
        Some(weth),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let lead_idx = at.add(fa.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    at.add(fb.currency0_address).ok()?;
    at.add(fb.currency1_address).ok()?;
    let extra_idx = at.add(extra_addr).ok()?;
    let weth_idx = SENTINEL_WETH;

    // V4-unlock inner: the forward-feed prelude (V4Sync + transfer + settle)
    // or the native bridge, then the swap core + take + settle-all.
    let mut inner: Plan = Vec::new();
    if native_in {
        inner.push(mechanics::v4_swap(&mut at, fb, v4_swap_in, v4_out_amount)?);
        inner.push(mechanics::native_transfer(v4_swap_in));
        inner.push(mechanics::v4_settle_delta(
            SENTINEL_NATIVE,
            NATIVE_CURRENCY_ADDRESS,
        ));
    } else {
        inner.push(mechanics::v4_sync(extra_idx, fwd));
        inner.push(mechanics::erc20_transfer(
            extra_idx,
            fwd,
            pm_idx,
            forward_out,
            None,
            None,
        ));
        inner.push(mechanics::v4_settle(fwd, forward_out));
        inner.push(mechanics::v4_swap(&mut at, fb, v4_swap_in, v4_out_amount)?);
    }
    inner.push(mechanics::v4_take_compact(
        &mut at,
        fb,
        SENTINEL_SELF,
        v4_out_amount,
        None,
    )?);
    inner.push(mechanics::v4_settle_all());

    // Seed-flash callback: the WETH head (native-in), the unlock, the
    // v2v4-native-out deposit, and the flash-repayment transfer (the seed's
    // borrowed flash input back to the lead pool).
    let mut callback: Plan = Vec::new();
    if native_in {
        callback.push(mechanics::weth_withdraw(weth_idx, weth, forward_out));
    }
    callback.push(mechanics::v4_unlock(inner, pm_idx));
    if !native_in && fa.prot == Prot::V2 && v4_out == NATIVE_CURRENCY_ADDRESS {
        callback.push(mechanics::weth_deposit(weth_idx, weth, v4_out_amount));
    }
    let (repay_tok_idx, repay_tok) = if native_in {
        (extra_idx, fa.in_currency)
    } else {
        (weth_idx, weth)
    };
    callback.push(mechanics::erc20_transfer(
        repay_tok_idx,
        repay_tok,
        lead_idx,
        optimal,
        None,
        Some(fa.pool_address),
    ));

    // Lead flash — the recipient is out_dest-derived (executor → SELF), so
    // the walk passes no explicit routing.
    let flash = if fa.prot == Prot::V3 {
        mechanics::v3_flash(&mut at, fa, forward_out, optimal, false, None, callback)?
    } else {
        mechanics::v2_flash(&mut at, fa, forward_out, flash_in, optimal, callback)?
    };

    // Plan head: self-fund the flash input — except v2v4-native-out (the
    // in-callback `weth_deposit` covers it) and a WETH-tailed arm (the
    // swap's WETH output recovers the executor's WETH, so no head funding).
    let self_fund = native_in
        || (v4_out == NATIVE_CURRENCY_ADDRESS && fa.prot == Prot::V3)
        || (v4_out != NATIVE_CURRENCY_ADDRESS && v4_out != weth);
    let mut plan: Plan = Vec::new();
    if self_fund {
        plan.push(mechanics::self_fund(flash_in, optimal));
    }
    plan.push(flash);
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
    /// for every v2v4/v3v4 family × amount-set × `EncodeOptions` combo. (The
    /// SMOZG3 erc6909×batch WETH-terminal decline is lifted — TGUZCT/SW42JA —
    /// and the flipped cell is pinned by the shape/matrix tests, not this
    /// table.)
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
