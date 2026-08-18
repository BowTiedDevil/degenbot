//! 3-hop mega-block (block 4: no Offstream).

use super::super::{fits_i128, mechanics, HopFacts, RepayMechanism, SeedDelivery, TerminalForm};
use crate::composers::{resolve_axes, ComposerInputs, CurrencyBridge, NATIVE_CURRENCY_ADDRESS};
use crate::encoders::{AddressTable, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH};
use crate::grammar_ledger::Prot;
use crate::grammar_plan::{Plan, PlanStep, V4BatchSwap};
use crate::grammar_shape::{
    erc6909_batch_capture_declines, native_capture_declines, v4_bridge_steps, v4_scaffold_table,
    v4_terminal_capture_steps,
};

#[expect(clippy::too_many_lines, clippy::needless_return)]
pub(crate) fn derive(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    return if facts.len() == 3 && facts.iter().all(|f| matches!(f.prot, Prot::V2 | Prot::V3)) {
        // ── T6 cutover: the 7 V2/V3-only 3-hop families (v3v3v3, v3v3v2,
        // v3v2v3, v3v2v2, v2v2v3, v2v3v3, v2v3v2) are now derived by the
        // rule-driven walker (`rule_walk_v2v3`) — R1 (enclosure root) + R2
        // (repay-graph nest) + R3 (leaf + WETH-seed placement) + the
        // V2-flash upgrade — instead of 7 hand-written arms. Byte-identity
        // is pinned by `glopcn_bytepin` + the `degenbot-simulation` revm
        // matrix; the per-family golden-ordered AddressTable staging lives
        // in `stage_pools`.
        return rule_walk_v2v3(facts, inputs);
    } else {
        if facts.len() != 3 {
            return None;
        }
        if facts[0].prot == Prot::V4
            && facts
                .iter()
                .all(|f| matches!(f.prot, Prot::V2 | Prot::V3 | Prot::V4))
        {
            // ── T6b cutover: the 9 V4-led 3-hop families (v4v4v4,
            // v4v2v2, v4v2v4, v4v3v3, v4v3v4, v4v4v2, v4v4v3, v4v2v3,
            // v4v3v2 — hop0 == V4) are now derived by the rule-driven
            // walker (`rule_walk_v4_led`) — R1 (enclosure root: V4Unlock,
            // with the v4v2v3 outer-flash exception) + R2 (leg threading:
            // seeded V2 calcs + repay-graph flash nesting inside the
            // unlock) + R3 (finish = SettleAll when a downstream hop is V4
            // else SettleΔ(WETH); capture only for the pure-V4 family) —
            // instead of 9 hand-written arms. Byte-identity is pinned by
            // `glopcn_bytepin` + the `degenbot-simulation` revm matrix;
            // the per-family golden-ordered AddressTable staging lives
            // inside `rule_walk_v4_led`.
            return rule_walk_v4_led(facts, inputs);
        } else if facts[0].prot != Prot::V4
            && (facts[1].prot == Prot::V4 || facts[2].prot == Prot::V4)
            && !(facts[0].prot == Prot::V3 && facts[1].prot == Prot::V4)
        {
            // ── T6c group-C gate. The 7 families whose hop0 is V2/V3 AND
            // some hop is V4 (but not the v4-led block — hop0 != V4 — and not
            // the v3v4 terminal-form merge): v2v2v4, v2v4v4, v2v3v4, v3v2v4,
            // v3v3v4, v2v4v2, v2v4v3. They are now derived by the rule-driven
            // walker (`rule_walk_v2v3_v4_mixed`):
            //   R1 (enclosure root) — a V4Unlock is the root when a V4 hop is
            //     the flat terminal (hop2 == V4); otherwise a flash is the root
            //     and the V4Unlock nests inside its callback (the V4 hop is a
            //     sub-enclosure folded into the flash that seeds/repays it).
            //   R2 (repay-graph nest order) — flashes nest by repay chain
            //     (reverse for the V3<->V3 nests; the single forward nest is
            //     v3v2v4, driven by `fb.repay_mechanism = AutoFromExecutor`);
            //     the V4 delta threading (Sync/Settle/Take/SettleAll) per
            //     currency boundary reuses `v4_scaffold_table` + the inline
            //     ledger already factored in the rule_walk_v4_led arms.
            //   R3 (seed placement) — the optimal-WETH prefund to a leading V2
            //     pool is a plain `Erc20Transfer`, EXCEPT v2v3v4 where
            //     `fa.seed_delivery = V4TakeCompact` emits it as a
            //     `V4TakeCompact` inside the unlock plus a profit take to SELF.
            // Byte-identity is pinned by `glopcn_bytepin` + the
            // `degenbot-simulation` revm matrix; the per-family golden-ordered
            // AddressTable staging lives inside `rule_walk_v2v3_v4_mixed`.
            return rule_walk_v2v3_v4_mixed(facts, inputs);
        } else if facts[0].prot == Prot::V3
            && facts[1].prot == Prot::V4
            && matches!(facts[2].prot, Prot::V2 | Prot::V4)
        {
            // ── v3v4{v2,v4} — MERGED on the terminal-form axis (T6 / PZBGP7).
            // Same leading V3 flash + V4 mid unlock; the trailing hop reads
            // `facts[2].terminal_form`.
            let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
            let terminal_form = fc.terminal_form?;
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
            let c_swap_in = match terminal_form {
                TerminalForm::UnlockInternal => {
                    let c = *inputs.consumed_inputs.get(2)?;
                    if !fits_i128(c) {
                        return None;
                    }
                    Some(c)
                }
                TerminalForm::DirectHandoff => None,
            };
            let fwd_a = fa.out_currency;
            let forward_b_cur = fb.out_currency;
            let in_currency_b = fb.in_currency;
            let weth = inputs.weth_address;
            let mut at = v4_scaffold_table(inputs);
            let pm_idx = at.add(inputs.pool_manager_address).ok()?;
            let v3a = at.add(fa.pool_address).ok()?;
            // AddressTable staging differs per terminal form (golden-ordered):
            // v3v4v2 stages v2c + forward_b pre-forward_a; v3v4v4 stages c0_c/c1_c post-c1_b.
            let v2c = match terminal_form {
                TerminalForm::DirectHandoff => Some(at.add(fc.pool_address).ok()?),
                TerminalForm::UnlockInternal => None,
            };
            let forward_a = at.add(fwd_a).ok()?;
            let forward_b = match terminal_form {
                TerminalForm::DirectHandoff => Some(at.add(forward_b_cur).ok()?),
                TerminalForm::UnlockInternal => None,
            };
            let fee_b = fb.swap_fee;
            let ts_b = fb.tick_spacing;
            let c0_b = at.add(fb.currency0_address).ok()?;
            let c1_b = at.add(fb.currency1_address).ok()?;
            let (c0_c, c1_c) = match terminal_form {
                TerminalForm::UnlockInternal => (
                    Some(at.add(fc.currency0_address).ok()?),
                    Some(at.add(fc.currency1_address).ok()?),
                ),
                TerminalForm::DirectHandoff => (None, None),
            };

            let v4_unlock_inner_start: Vec<PlanStep> = vec![
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
            ];
            let v4_inner: Plan = match terminal_form {
                TerminalForm::DirectHandoff => {
                    let mut inner = v4_unlock_inner_start;
                    inner.push(PlanStep::V4TakeDelta {
                        currency_idx: forward_b.unwrap_or_default(),
                        currency_addr: forward_b_cur,
                        recipient_idx: v2c.unwrap_or_default(),
                        seeds_pool: Some(fc.pool_address),
                    });
                    inner.push(PlanStep::V4SettleAll);
                    inner
                }
                TerminalForm::UnlockInternal => {
                    let c_in = c_swap_in.unwrap_or_default();
                    let mut inner = v4_unlock_inner_start;
                    inner.push(PlanStep::V4Swap {
                        c0_idx: c0_c.unwrap_or_default(),
                        c1_idx: c1_c.unwrap_or_default(),
                        fee: fc.swap_fee,
                        tick_spacing: fc.tick_spacing,
                        hooks_idx: SENTINEL_NATIVE,
                        zfo: fc.zfo,
                        amount: c_in,
                        in_currency: fc.in_currency,
                        in_amount: c_in,
                        out_currency: fc.out_currency,
                        out_amount: out_c,
                    });
                    inner.push(PlanStep::V4TakeDelta {
                        currency_idx: SENTINEL_WETH,
                        currency_addr: weth,
                        recipient_idx: SENTINEL_SELF,
                        seeds_pool: None,
                    });
                    inner.push(PlanStep::V4SettleAll);
                    inner
                }
            };
            let unlock_step = PlanStep::V4Unlock {
                inner: v4_inner,
                pool_manager_idx: pm_idx,
            };
            let repay_step = PlanStep::Erc20Transfer {
                token_idx: SENTINEL_WETH,
                token_addr: weth,
                recipient_idx: v3a,
                amount: optimal_input,
                seeds_pool: None,
                repays_flash: Some(fa.pool_address),
            };
            let cb: Plan = match terminal_form {
                TerminalForm::DirectHandoff => {
                    let c_step =
                        mechanics::v2_swap(&mut at, fc, out_c, SENTINEL_SELF, None, false)?;
                    vec![unlock_step, c_step, repay_step]
                }
                TerminalForm::UnlockInternal => vec![repay_step, unlock_step],
            };
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

// ─────────────────────────────────────────────────────────────────────
// T6 — rule-driven walker for the 7 V2/V3-only 3-hop families.
//
// Replaces the 7 hand-written arms (`v3v3v3` … `v2v3v2`) with a single
// rule-driven derivation. The rules (see `/tmp/t6_analysis.md`):
//
// **R1 (enclosure root).** Every family is SelfFund-led: a `SelfFund{WETH,
// optimal}` prepends the plan. The outermost frame is the **last
// flash-capable hop in the repay chain**. A V3 hop is always flash-capable;
// a V2 hop is flash-capable iff it is the terminal hop (`i=2`) whose
// preceding hop is a V3 flash — the **V2-flash upgrade** that keeps the
// debt chain a pure flash nest (the `v2v3v2` case, where `fc` flashes to
// wrap `fb`'s V3 flash).
//
// **R2 (repay-graph nest).** Flashes nest with the **first flash in swap
// order innermost**; each outer flash's callback wraps the inner flash step
// as its sole element (reverse-swap nesting for the V3↔V3 chains). The
// folded calc chain — the non-flash V2 hops — attaches to the innermost
// flash's callback, in swap order. A calc whose output lands in the next
// hop's FLASH repays it (`recipient_repays=true`); a calc feeding a
// downstream calc seeds it. A V2 calc at hop0 whose forward repays hop1's
// flash uses the **direct handoff** (`V2SwapDirect`); every other calc uses
// `V2SwapCalc`.
//
// **R3 (leaf + WETH-seed placement).** V3-led (hop0 flashes the optimal
// WETH) repays the seed flash inside its own callback via an explicit
// `Erc20Transfer{repays_flash=fa}` at the END of the innermost callback —
// unless the nest is the pure all-flash `v3v3v3`, where `auto_repay`
// handles the WETH and no transfer is emitted. V2-led prefunds the leading
// V2 pool with `Erc20Transfer{seeds_pool=fa, repays_flash=None}` at the
// START of the innermost callback (before the calc chain).
//
// The one irreducible per-family residue is the **AddressTable staging
// order** (the preamble dumps addresses in insertion order and every
// `pool_idx` reference rides on it) — that is byte-pinned by the golden +
// revm suites and lives in [`stage_pools`].
/// `rule_walk_v2v3` — the rule-driven walker over the 7 families.
#[expect(clippy::too_many_lines)]
fn rule_walk_v2v3(facts: &[HopFacts], inputs: &ComposerInputs<'_>) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3 {
        return None;
    }
    let prots = [facts[0].prot, facts[1].prot, facts[2].prot];
    if !prots.iter().all(|p| matches!(p, Prot::V2 | Prot::V3)) {
        return None;
    }
    let hops: [&HopFacts; 3] = [&facts[0], &facts[1], &facts[2]];
    let optimal = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) || !fits_i128(optimal) {
        return None;
    }
    let outs = [
        inputs.hop_outputs[0],
        inputs.hop_outputs[1],
        inputs.hop_outputs[2],
    ];
    let weth = inputs.weth_address;

    // ── R1: flash set. V3 always flashes; a V2 hop flashes iff it is the
    // terminal hop (i=2) in a V2-led chain (hop0 is V2) whose mid hop is a V3
    // flash — the V2-flash upgrade (the v2v3v2 case: keeps the debt chain a
    // pure flash nest so the outer V2 flash wraps the mid V3 flash). In a
    // V3-led chain (hop0 V3) the trailing V2 is a plain calc, not a flash.
    let flash = [
        hops[0].prot == Prot::V3,
        hops[1].prot == Prot::V3,
        hops[2].prot == Prot::V3
            || (hops[2].prot == Prot::V2 && hops[0].prot == Prot::V2 && hops[1].prot == Prot::V3),
    ];
    let all_flash = flash[0] && flash[1] && flash[2];

    // Borrow amounts for the flashes that actually flash. hop0 always borrows
    // the optimal WETH seed; hop1 borrows `b_in`, hop2 borrows `c_in`.
    let b_in = if flash[1] {
        let v = *inputs.consumed_inputs.get(1)?;
        if !fits_i128(v) {
            return None;
        }
        v
    } else {
        0
    };
    let c_in = if flash[2] {
        let v = *inputs.consumed_inputs.get(2)?;
        if !fits_i128(v) {
            return None;
        }
        v
    } else {
        0
    };
    let in_amounts = [optimal, b_in, c_in];

    // ── Per-family golden-ordered AddressTable staging. The staging ORDER
    // (not the set) is byte-pinned — the preamble + every pool_idx reference
    // ride on it — so each family reproduces its hand-authored staging exactly.
    let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
    let (a_idx, b_idx, c_idx) = stage_pools(&mut at, &hops)?;
    let pool_idx = [a_idx, b_idx, c_idx];

    // ── Folded calc chain: the non-flash V2 hops, in swap order. A hop0 calc
    // whose forward repays hop1's flash uses the direct handoff; every other
    // calc uses V2SwapCalc. Recipient routing: the next hop's pool (or SELF
    // for a terminal), repays=true iff that next hop is a flash.
    let mut calc_chain: Vec<PlanStep> = Vec::new();
    for i in 0..3 {
        if flash[i] {
            continue;
        }
        let (recipient_idx, recipient_pool_addr, recipient_repays) = if i < 2 {
            (
                pool_idx[i + 1],
                Some(hops[i + 1].pool_address),
                flash[i + 1],
            )
        } else {
            (SENTINEL_SELF, None, false)
        };
        if i == 0 && flash[1] {
            calc_chain.push(mechanics::v2_swap_direct(
                &mut at,
                hops[0],
                outs[0],
                hops[0].out_currency,
                recipient_idx,
                recipient_pool_addr,
                recipient_repays,
            )?);
        } else {
            calc_chain.push(mechanics::v2_swap(
                &mut at,
                hops[i],
                outs[i],
                recipient_idx,
                recipient_pool_addr,
                recipient_repays,
            )?);
        }
    }

    // ── R3: WETH seed/repay placement into the innermost flash's callback.
    let v3_led = flash[0];
    let innermost = (0..3).find(|i| flash[*i])?;
    let mut inner_cb: Vec<PlanStep> = Vec::new();
    if !v3_led {
        // V2-led: prefund the leading V2 pool BEFORE the calc chain.
        inner_cb.push(PlanStep::Erc20Transfer {
            token_idx: SENTINEL_WETH,
            token_addr: weth,
            recipient_idx: a_idx,
            amount: optimal,
            seeds_pool: Some(hops[0].pool_address),
            repays_flash: None,
        });
    }
    inner_cb.extend(calc_chain);
    if v3_led && !all_flash {
        // V3-led, non-pure: repay the seed flash AFTER the calc chain.
        inner_cb.push(PlanStep::Erc20Transfer {
            token_idx: SENTINEL_WETH,
            token_addr: weth,
            recipient_idx: a_idx,
            amount: optimal,
            seeds_pool: None,
            repays_flash: Some(hops[0].pool_address),
        });
    }
    // Pure v3v3v3: no transfer — auto_repay handles the WETH (inner_cb stays []).

    // ── R2: nest flashes in swap order (innermost first). The innermost's
    // callback holds the folded calc chain + seed transfer; each outer flash
    // wraps the prior flash step as its callback's sole element.
    let flash_idx: Vec<usize> = (0..3).filter(|i| flash[*i]).collect();
    let mut inner_step: Option<PlanStep> = None;
    for &i in &flash_idx {
        let cb = if i == innermost {
            inner_cb.clone()
        } else {
            vec![inner_step.clone()?]
        };
        let step = if hops[i].prot == Prot::V3 {
            let (recipient_idx, recipient_pool_addr, recipient_pool_repays) = if i < 2 {
                (
                    pool_idx[i + 1],
                    Some(hops[i + 1].pool_address),
                    flash[i + 1],
                )
            } else {
                (SENTINEL_SELF, None, false)
            };
            mechanics::v3_flash_to(
                &mut at,
                hops[i],
                outs[i],
                in_amounts[i],
                all_flash && i == 0,
                recipient_idx,
                recipient_pool_addr,
                recipient_pool_repays,
                cb,
            )?
        } else {
            // V2 flash — the only V2 flash is the terminal hop2 in v2v3v2.
            mechanics::v2_flash(
                &mut at,
                hops[i],
                outs[i],
                hops[i].in_currency,
                in_amounts[i],
                cb,
            )?
        };
        inner_step = Some(step);
    }

    let outer = inner_step?;
    Some((
        vec![
            PlanStep::SelfFund {
                currency: weth,
                amount: optimal,
            },
            outer,
        ],
        at,
    ))
}

/// Per-family golden-ordered `AddressTable` pool staging for the 7 V2/V3-only
/// 3-hop families. Returns `(a_idx, b_idx, c_idx)` — the table indices of the
/// three hop pools. The staging ORDER (not the set) is byte-pinned: the
/// preamble dumps addresses in insertion order and every `pool_idx` reference
/// rides on it, so each family reproduces its hand-authored staging exactly.
#[expect(clippy::match_same_arms)]
fn stage_pools(at: &mut AddressTable, hops: &[&HopFacts; 3]) -> Option<(u8, u8, u8)> {
    let (fa, fb, fc) = (hops[0], hops[1], hops[2]);
    match [fa.prot, fb.prot, fc.prot] {
        [Prot::V3, Prot::V3, Prot::V3] => {
            let a = at.add(fa.pool_address).ok()?;
            let b = at.add(fb.pool_address).ok()?;
            let c = at.add(fc.pool_address).ok()?;
            Some((a, b, c))
        }
        // v3v3v2 stages the terminal V2 pool first, then fa, fb.
        [Prot::V3, Prot::V3, Prot::V2] => {
            let c = at.add(fc.pool_address).ok()?;
            let a = at.add(fa.pool_address).ok()?;
            let b = at.add(fb.pool_address).ok()?;
            Some((a, b, c))
        }
        // v3v2v3 stages the mid V2 pool first, then fa, fc.
        [Prot::V3, Prot::V2, Prot::V3] => {
            let b = at.add(fb.pool_address).ok()?;
            let a = at.add(fa.pool_address).ok()?;
            let c = at.add(fc.pool_address).ok()?;
            Some((a, b, c))
        }
        // v3v2v2 stages both V2 pools before the leading V3 flash pool.
        [Prot::V3, Prot::V2, Prot::V2] => {
            let b = at.add(fb.pool_address).ok()?;
            let c = at.add(fc.pool_address).ok()?;
            let a = at.add(fa.pool_address).ok()?;
            Some((a, b, c))
        }
        // v2v2v3 + v2v3v3 stage in swap order (calcs then the flash(es)).
        [Prot::V2, Prot::V2 | Prot::V3, Prot::V3] => {
            let a = at.add(fa.pool_address).ok()?;
            let b = at.add(fb.pool_address).ok()?;
            let c = at.add(fc.pool_address).ok()?;
            Some((a, b, c))
        }
        // v2v3v2 (V2-flash upgrade): fa.out_currency is staged (index unused)
        // — a golden quirk of the hand-authored byte layout — then fa, fc, fb.
        [Prot::V2, Prot::V3, Prot::V2] => {
            let _ = at.add(fa.out_currency).ok()?;
            let a = at.add(fa.pool_address).ok()?;
            let c = at.add(fc.pool_address).ok()?;
            let b = at.add(fb.pool_address).ok()?;
            Some((a, b, c))
        }
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────
// T6b — rule-driven walker for the 9 V4-led 3-hop families (hop0 == V4).
//
// Replaces the 9 hand-written arms (`v4v4v4`, `v4v2v2`, `v4v2v4`, `v4v3v3`,
// `v4v3v4`, `v4v4v2`, `v4v4v3`, `v4v2v3`, `v4v3v2`) with one rule-driven
// derivation. The legs that DRIVE these shapes are fully derivable from the
// existing `HopFacts` — no new fact is introduced. The rules:
//
// **R1 (enclosure root).** hop0 is a V4 pool ⇒ the enclosure root is a
// `V4Unlock{inner, pm_idx}` wrapping the leg work, with the single exception
// of **v4v2v3** (hop1=V2, hop2=V3): there the terminal V3 hop's borrowed
// currency is produced by the V2 calc that must run *inside* the flash
// callback, so the flash is the **outer** frame wrapping the `V4Unlock`. This
// is the sole V4-led arm whose root is not the `V4Unlock`.
//
// **R2 (leg threading inside the unlock).** The lead `V4Swap(a)` runs first
// (its amount is `optimal` when hop1≠V4 — the self-funded WETH seed — and
// `consumed[0]` when hop1=V4, where hop0 is a plain swap, not the seeder).
// A V2 calc that follows a `V4Swap` is seeded by a `V4TakeCompact` of the
// V4 forward to the V2 pool. A V3 flash whose repay currency is the V4
// forward is delivered by a `V4TakeCompact(repays_flash=…)`. Calcs thread in
// swap order, recipient = the next hop's pool (or SELF for a terminal),
// `repays=true` iff the next hop is a flash. Flashes nest inside the unlock
// by repay-graph (v4v3v3 nests fb inside fc; v4v3v2/v4v3v4/v4v4v3 emit a
// single flash whose callback holds the take + any terminal calc).
//
// **R3 (finish + capture placement).** The unlock's finish is `V4SettleAll`
// when a downstream hop is V4 (a `V4Swap` absorbs the PM delta), else
// `V4SettleDelta(WETH)` (settling the WETH the seeded `V4Swap(a)` debited).
// Terminal profit capture (`v4_terminal_capture_steps`) fires **only** for the
// pure-V4 family (all three hops V4) — every other family delivers its
// terminal output to SELF via a flash or a V2 calc, so no PM-delta capture is
// emitted. The `use_v4_batch` + `any_gap` batch/bridge split is data-driven
// (the pure-V4 `v4v4v4` path); the batch layout is reproduced inline since
// `mechanics::v4_batch` is gated dead-code (T7 will wire it).
//
// The irreducible per-family residue is the `AddressTable` staging ORDER
// (the preamble dumps addresses in insertion order and every `pool_idx` /
// currency reference rides on it) — byte-pinned by `glopcn_bytepin` + the
// `degenbot-simulation` revm matrix. Each family reproduces its hand-authored
// staging sequence exactly (the `at.add` calls below mirror the arms 1:1;
// `mechanics::v4_swap` / `v2_swap` / `v3_flash_to` dedup against the
// pre-staged entries, preserving the preamble order).
/// `rule_walk_v4_led` — the rule-driven walker over the 9 V4-led families.
#[expect(clippy::too_many_lines, clippy::similar_names)]
fn rule_walk_v4_led(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3 {
        return None;
    }
    if facts[0].prot != Prot::V4
        || !facts
            .iter()
            .all(|f| matches!(f.prot, Prot::V2 | Prot::V3 | Prot::V4))
    {
        return None;
    }
    let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
    let optimal_input = inputs.optimal_input;
    let out_a = inputs.hop_outputs[0];
    let out_b = inputs.hop_outputs[1];
    let out_c = inputs.hop_outputs[2];
    let weth = inputs.weth_address;
    let pm_idx_sentinel = inputs.pool_manager_address;

    match [fb.prot, fc.prot] {
        // ── v4v4v4: pure-V4. V4Batch (use_v4_batch && !any_gap) or V4Swap×3
        // with optional native↔WETH bridges; terminal profit capture +
        // V4SettleAll. Root is the V4Unlock. (R3 capture: pure-V4 only.)
        [Prot::V4, Prot::V4] => {
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
            let pm_idx = at.add(pm_idx_sentinel).ok()?;

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
            if !any_gap
                && erc6909_batch_capture_declines(capture, inputs.opts.use_v4_batch, output_c, weth)
            {
                // SMOZG3: batch tail-settle + V4_MINT on the WETH terminal is
                // unexecutable on the current executor artifact (D0) — decline
                // until TGUZCT ships the composable artifact.
                return None;
            }
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
        }
        // ── v4v2v2: V4Swap(a, optimal/WETH) + Take→v2b(seeds) + b_swap +
        // c_swap(→SELF) + V4SettleΔ(WETH). hop1=V2 ⇒ finish SettleΔ(WETH).
        [Prot::V2, Prot::V2] => {
            if out_a == 0 || inputs.hop_outputs.contains(&0) {
                return None;
            }
            if !fits_i128(optimal_input) {
                return None;
            }
            let forward_a_cur = fa.out_currency;
            let in_currency_a = fa.in_currency;
            if in_currency_a != weth {
                return None;
            }
            let mut at = v4_scaffold_table(inputs);
            let forward_a = at.add(forward_a_cur).ok()?;
            let v4swap_a = mechanics::v4_swap(&mut at, fa, optimal_input, out_a)?;
            let v2b = at.add(fb.pool_address).ok()?;
            let v2c = at.add(fc.pool_address).ok()?;
            let pm_idx = at.add(pm_idx_sentinel).ok()?;
            let b_step = mechanics::v2_swap(&mut at, fb, out_b, v2c, Some(fc.pool_address), false)?;
            let c_step = mechanics::v2_swap(&mut at, fc, out_c, SENTINEL_SELF, None, false)?;
            let inner: Plan = vec![
                v4swap_a,
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
        }
        // ── v4v2v4: V4Swap(a, optimal) + Take→v2b(seeds) + b_swap + V4Swap(c)
        // + V4SettleAll. hop2=V4 ⇒ finish SettleAll.
        [Prot::V2, Prot::V4] => {
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
            let b_forward_cur = fb.out_currency;
            let mut at = v4_scaffold_table(inputs);
            let forward_a = at.add(forward_a_cur).ok()?;
            let _ = at.add(b_forward_cur).ok()?;
            let v4swap_a = mechanics::v4_swap(&mut at, fa, optimal_input, out_a)?;
            let v4swap_c = mechanics::v4_swap(&mut at, fc, c_swap_in, out_c)?;
            let v2b = at.add(fb.pool_address).ok()?;
            let pm_idx = at.add(pm_idx_sentinel).ok()?;
            let b_step = mechanics::v2_swap(&mut at, fb, out_b, SENTINEL_SELF, None, false)?;
            let inner: Plan = vec![
                v4swap_a,
                PlanStep::V4TakeCompact {
                    currency_idx: forward_a,
                    currency_addr: forward_a_cur,
                    recipient_idx: v2b,
                    amount: out_a,
                    seeds_pool: Some(fb.pool_address),
                    repays_flash: None,
                },
                b_step,
                v4swap_c,
                PlanStep::V4SettleAll,
            ];
            let plan: Plan = vec![PlanStep::V4Unlock {
                inner,
                pool_manager_idx: pm_idx,
            }];
            Some((plan, at))
        }
        // ── v4v3v3: V4Swap(a, optimal) + fc[fb[Take(repays fb)]] +
        // V4SettleΔ(WETH). hop1,hop2 ∈ {V3} ⇒ finish SettleΔ(WETH).
        [Prot::V3, Prot::V3] => {
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
            let mut at = v4_scaffold_table(inputs);
            let pm_idx = at.add(pm_idx_sentinel).ok()?;
            let v3b = at.add(fb.pool_address).ok()?;
            let v3c = at.add(fc.pool_address).ok()?;
            let _ = at.add(forward_a_cur).ok()?;
            let v4swap_a = mechanics::v4_swap(&mut at, fa, optimal_input, out_a)?;
            let inner_take =
                mechanics::v4_take_compact(&mut at, fa, v3b, out_a, Some(fb.pool_address))?;
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
                v4swap_a,
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
        }
        // ── v4v3v4: V4Swap(a, optimal) + V4Sync(fwd_b) + fb[Take(repays fb)]
        // + V4Settle(fwd_b) + V4Swap(c) + V4SettleAll. hop2=V4 ⇒ SettleAll.
        [Prot::V3, Prot::V4] => {
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
            let fwd_b = fb.out_currency;
            let mut at = v4_scaffold_table(inputs);
            let pm_idx = at.add(pm_idx_sentinel).ok()?;
            let v3b = at.add(fb.pool_address).ok()?;
            let _ = at.add(forward_a_cur).ok()?;
            let forward_b = at.add(fwd_b).ok()?;
            let v4swap_a = mechanics::v4_swap(&mut at, fa, optimal_input, out_a)?;
            let v4swap_c = mechanics::v4_swap(&mut at, fc, c_swap_in, out_c)?;
            let take = mechanics::v4_take_compact(&mut at, fa, v3b, out_a, Some(fb.pool_address))?;
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
                v4swap_a,
                PlanStep::V4Sync {
                    currency_idx: forward_b,
                    currency_addr: fwd_b,
                },
                flash_b,
                PlanStep::V4Settle {
                    currency_addr: fwd_b,
                    amount: out_b,
                },
                v4swap_c,
                PlanStep::V4SettleAll,
            ];
            let plan: Plan = vec![PlanStep::V4Unlock {
                inner,
                pool_manager_idx: pm_idx,
            }];
            Some((plan, at))
        }
        // ── v4v4v2: V4Swap(a, consumed[0]) + V4Swap(b) + Take→v2c(seeds) +
        // c_swap + V4SettleAll. hop1=V4 ⇒ hop0 is a plain swap (consumed[0]).
        [Prot::V4, Prot::V2] => {
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
            let v4swap_a = mechanics::v4_swap(&mut at, fa, a_swap_in, out_a)?;
            let v4swap_b = mechanics::v4_swap(&mut at, fb, b_swap_in, out_b)?;
            let pm_idx = at.add(pm_idx_sentinel).ok()?;
            let v2step = mechanics::v2_swap(&mut at, fc, out_c, SENTINEL_SELF, None, false)?;
            let inner: Plan = vec![
                v4swap_a,
                v4swap_b,
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
            let _ = (forward_a_cur, in_currency_a, in_currency_b);
            let plan: Plan = vec![PlanStep::V4Unlock {
                inner,
                pool_manager_idx: pm_idx,
            }];
            Some((plan, at))
        }
        // ── v4v4v3: V4Swap(a) + V4Swap(b) + flash_c(SELF)[Take(repays fc)]
        // + V4SettleAll. hop1=V4 ⇒ consumed[0] seed. hop2=V3 ⇒ SettleAll.
        // flash_c borrows forward_b (not fc.in_currency) ⇒ inline FlashSwap.
        [Prot::V4, Prot::V3] => {
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
            let forward_b_cur = fb.out_currency;
            let out_currency_c = fc.out_currency;
            let fee_c = fc.swap_fee;
            let mut at = v4_scaffold_table(inputs);
            let forward_b = at.add(forward_b_cur).ok()?;
            let v3c = at.add(fc.pool_address).ok()?;
            let v4swap_a = mechanics::v4_swap(&mut at, fa, a_swap_in, out_a)?;
            let v4swap_b = mechanics::v4_swap(&mut at, fb, b_swap_in, out_b)?;
            let pm_idx = at.add(pm_idx_sentinel).ok()?;
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
            let inner: Plan = vec![v4swap_a, v4swap_b, flash_c, PlanStep::V4SettleAll];
            let plan: Plan = vec![PlanStep::V4Unlock {
                inner,
                pool_manager_idx: pm_idx,
            }];
            Some((plan, at))
        }
        // ── v4v2v3: the R1 enclosure EXCEPTION. The terminal V3 hop flashes
        // (borrows fb.out) wrapping the V4Unlock; inside, V4Swap(a, optimal)
        // + Take→v2b(seeds) + b_swap(repays fc) + V4SettleΔ(WETH). hop1=V2
        // ⇒ in_a must be WETH; finish SettleΔ(WETH) (no downstream V4).
        [Prot::V2, Prot::V3] => {
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
            if in_currency_a != weth {
                return None;
            }
            let mut at = v4_scaffold_table(inputs);
            let v3c = at.add(fc.pool_address).ok()?;
            let forward_a = at.add(forward_a_cur).ok()?;
            let _ = at.add(fb.out_currency).ok()?;
            let v4swap_a = mechanics::v4_swap(&mut at, fa, optimal_input, out_a)?;
            let v2b = at.add(fb.pool_address).ok()?;
            let pm_idx = at.add(pm_idx_sentinel).ok()?;
            let b_step = mechanics::v2_swap(&mut at, fb, out_b, v3c, Some(fc.pool_address), true)?;
            let v4_inner: Plan = vec![
                v4swap_a,
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
        }
        // ── v4v3v2: V4Swap(a, optimal) + fb[Take(repays fb), c_swap(→SELF)]
        // + V4SettleΔ(WETH). hop1,hop2 ∈ {V3,V2} ⇒ finish SettleΔ(WETH).
        // flash_b borrows forward_a (in-callback take repays fb; terminal
        // c_swap threads to SELF) ⇒ inline FlashSwap.
        [Prot::V3, Prot::V2] => {
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
            let v4swap_a = mechanics::v4_swap(&mut at, fa, optimal_input, out_a)?;
            let v2c = at.add(fc.pool_address).ok()?;
            let pm_idx = at.add(pm_idx_sentinel).ok()?;
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
                v4swap_a,
                flash_b,
                PlanStep::V4SettleDelta {
                    currency_idx: SENTINEL_WETH,
                    currency_addr: weth,
                },
            ];
            let _ = in_currency_a;
            let plan: Plan = vec![PlanStep::V4Unlock {
                inner,
                pool_manager_idx: pm_idx,
            }];
            Some((plan, at))
        } // A V4-led 3-hop family's (hop1,hop2) ∈ {V2,V3,V4}² is exhaustive
          // under the gate above; no other combination is reachable.
    }
}

// ─────────────────────────────────────────────────────────────────────
// T6c — rule-driven walker for the 7 group-C 3-hop families.
//
// Replaces the 7 hand-written arms whose hop0 is V2/V3 AND some hop is V4
// (but not the v4-led block — that is `rule_walk_v4_led` — and not the
// `v3v4{v2,v4}` terminal-form merge): `v2v2v4`, `v2v4v4`, `v2v3v4`,
// `v3v2v4`, `v3v3v4`, `v2v4v2`, `v2v4v3`. They collapse to one rule-driven
// derivation. The rules (stated below; T6 record in ADR-031's Resolution):
//
// **R1 (enclosure root).** Two root forms:
//   • A `V4Unlock` is the root when the V4 hop is the flat terminal
//     (`hop2 == V4` and no flash separates them) — `v2v2v4`, `v2v4v4`. The
//     outer plan is `SelfFund + V4Unlock{flat inner}`.
//   • Otherwise a flash is the root and the `V4Unlock` nests inside its
//     callback (the V4 hop is a sub-enclosure folded into the flash that
//     seeds/repays it). When hop1 flashes (`v2v3v4`, `v3v3v4`) the plan is
//     `V4Sync + fa-flash[fb-flash[Unlock]]` or `V4Sync + fb-flash[a-direct,
//     Unlock]`. When hop2 flashes (`v2v4v2`, `v2v4v3`) the trailing flash
//     wraps the unlock. When the leading hop is a V3 flash and hop1 is a V2
//     flash (`v3v2v4`) the V2 flash nests inside the V3 flash (forward).
//
// **R2 (repay-graph nest + V4 delta threading).** Flashes nest by repay
// chain (reverse for the V3↔V3 nest `v3v3v4`; the single FORWARD nest is
// `v3v2v4`, driven by the new `fb.repay_mechanism = AutoFromExecutor` fact —
// the seeder flash must run outer so the leading V3 flash wraps the V2 flash
// whose repay is drawn at borrow `auto_repay=true`). The V4 delta threading
// (Sync / Settle / Take / SettleΔ / SettleAll) per currency boundary reuses
// the shared `v4_scaffold_table` + the inline ledger already factored in
// `rule_walk_v4_led`; the walker emits the boundary sequence each family's
// bytes pin (`V4Sync`, `V4Settle`, `V4TakeCompact(repays_flash=…)`,
// `V4SettleDelta`, `V4SettleAll`).
//
// **R3 (leaf + seed placement).** The optimal-WETH prefund to a leading V2
// pool is a plain `Erc20Transfer`, EXCEPT `v2v3v4` where the new
// `fa.seed_delivery = V4TakeCompact` fact emits it as a `V4TakeCompact`
// inside the `V4Unlock` (the seed currency is a V4-managed WETH delta),
// plus a matching profit-take `V4TakeCompact→SELF`. Profit capture via
// `v4_terminal_capture_steps` is NOT emitted here (no group-C family is the
// pure-V4 capture case; the terminal output leaves via a flash or a V2 calc).
//
// The irreducible per-family residue is the `AddressTable` staging ORDER
// (the preamble dumps addresses in insertion order and every `pool_idx` /
// currency reference rides on it) — byte-pinned by `glopcn_bytepin` + the
// `degenbot-simulation` revm matrix. Each family reproduces its hand-authored
// staging sequence exactly.
/// `rule_walk_v2v3_v4_mixed` — the rule-driven walker over the group-C
/// families. `facts_for` sets the two T6c facts only where a consumer arm
/// exists (`v3v2v4.repay_mechanism`, `v2v3v4.seed_delivery`); every other hop
/// carries `None`.
#[expect(clippy::too_many_lines, clippy::similar_names)]
fn rule_walk_v2v3_v4_mixed(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3 {
        return None;
    }
    let (fa, fb, fc) = (&facts[0], &facts[1], &facts[2]);
    // Gate mirrors `derive`'s group-C arm (defense in depth).
    if !matches!(fa.prot, Prot::V2 | Prot::V3) {
        return None;
    }
    if fb.prot != Prot::V4 && fc.prot != Prot::V4 {
        return None;
    }
    if fa.prot == Prot::V3 && fb.prot == Prot::V4 {
        return None; // the v3v4 terminal-form merge owns this slice
    }
    let optimal_input = inputs.optimal_input;
    let out_a = inputs.hop_outputs[0];
    let out_b = inputs.hop_outputs[1];
    let out_c = inputs.hop_outputs[2];
    let weth = inputs.weth_address;
    let pm = inputs.pool_manager_address;

    match [fa.prot, fb.prot, fc.prot] {
        // ── v2v2v4: SelfFund + V4Unlock[V4Sync(fwd_b), prefund→v2a, a_calc,
        // b_calc, V4Settle, V4Swap(c), V4SettleAll]. hop2 == V4 ⇒ flat unlock;
        // a calc feeds b (recipient v2b), b feeds PM.
        [Prot::V2, Prot::V2, Prot::V4] => {
            if inputs.hop_outputs.contains(&0) || !fits_i128(optimal_input) {
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
            let pm_idx = at.add(pm).ok()?;
            let v2a = at.add(fa.pool_address).ok()?;
            let v2b = at.add(fb.pool_address).ok()?;
            let fee_c = fc.swap_fee;
            let ts_c = fc.tick_spacing;
            let c0 = at.add(fc.currency0_address).ok()?;
            let c1 = at.add(fc.currency1_address).ok()?;
            let forward_b = at.add(fwd_b).ok()?;
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
        }
        // ── v2v4v4: SelfFund + V4Unlock[V4Sync(fwd_a), prefund→v2a, a_calc,
        // V4Settle, V4Swap(b), V4Swap(c), V4SettleAll]. a_calc feeds PM.
        [Prot::V2, Prot::V4, Prot::V4] => {
            if inputs.hop_outputs.contains(&0) || !fits_i128(optimal_input) {
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
            let pm_idx = at.add(pm).ok()?;
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
        }
        // ── v2v3v4: V4Sync(fwd_b) + fb-flash[Unlock[V4Settle, V4Swap(c),
        // V4TakeCompact→v2a(seeds), V4TakeCompact→SELF(profit), V4Sync(WETH),
        // V4SettleAll], a_direct(repays fb)]. HOLDOUT — the WETH prefund to v2a
        // is a V4TakeCompact inside the unlock, NOT an Erc20Transfer, driven
        // by `fa.seed_delivery = V4TakeCompact`.
        [Prot::V2, Prot::V3, Prot::V4] => {
            if inputs.hop_outputs.contains(&0) || !fits_i128(optimal_input) {
                return None;
            }
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(b_swap_in) || !fits_i128(c_swap_in) {
                return None;
            }
            // The T6c fact: the seed must be a V4-take (decline if absent).
            if fa.seed_delivery != Some(SeedDelivery::V4TakeCompact) {
                return None;
            }
            let fwd_b = fb.out_currency;
            let fwd_a = fa.out_currency;
            let output_c = fc.out_currency;
            let in_currency_c = fc.in_currency;
            let mut at = v4_scaffold_table(inputs);
            let pm_idx = at.add(pm).ok()?;
            let v2a = at.add(fa.pool_address).ok()?;
            let v3b = at.add(fb.pool_address).ok()?;
            let fee_c = fc.swap_fee;
            let ts_c = fc.tick_spacing;
            let c0 = at.add(fc.currency0_address).ok()?;
            let c1 = at.add(fc.currency1_address).ok()?;
            let forward_b = at.add(fwd_b).ok()?;
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
        }
        // ── v3v2v4: SelfFund + fa-flash[v2-flash(auto_repay=true)[Unlock[
        // V4Swap(c), V4TakeCompact→SELF(out_c), V4SettleΔ(fwd_b)]],
        // Erc20Transfer repays fa]. FORWARD nest a(b). HOLDOUT — the V2 flash
        // draws its repay at borrow (`auto_repay=true`), driven by
        // `fb.repay_mechanism = AutoFromExecutor`; without that timing sub-fact
        // the walker cannot tell this forward nest from v2v4v2's reverse.
        [Prot::V3, Prot::V2, Prot::V4] => {
            if inputs.hop_outputs.contains(&0) || !fits_i128(optimal_input) {
                return None;
            }
            let b_swap_in = *inputs.consumed_inputs.get(1)?;
            let c_swap_in = *inputs.consumed_inputs.get(2)?;
            if !fits_i128(b_swap_in) || !fits_i128(c_swap_in) {
                return None;
            }
            // The T6c fact: forward nest needs the pre-callback repay draw.
            if fb.repay_mechanism != Some(RepayMechanism::AutoFromExecutor) {
                return None;
            }
            let fwd_b = fb.out_currency;
            let in_b = fb.in_currency;
            let output_c = fc.out_currency;
            let in_currency_c = fc.in_currency;
            let mut at = v4_scaffold_table(inputs);
            let v3a = at.add(fa.pool_address).ok()?;
            let v2b = at.add(fb.pool_address).ok()?;
            let forward_b = at.add(fwd_b).ok()?;
            let fee_c = fc.swap_fee;
            let ts_c = fc.tick_spacing;
            let c0 = at.add(fc.currency0_address).ok()?;
            let c1 = at.add(fc.currency1_address).ok()?;
            let pm_idx = at.add(pm).ok()?;
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
        }
        // ── v3v3v4: V4Sync(fwd_b) + fb-flash fa-flash[resp=fb,
        // rpr=true][Unlock[V4Settle, V4Swap(c), V4TakeCompact→v3a(repays fa),
        // V4SettleAll]]. Reverse V3↔V3 nest (the inner flash repays the outer).
        [Prot::V3, Prot::V3, Prot::V4] => {
            if inputs.hop_outputs.contains(&0) || !fits_i128(optimal_input) {
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
            let pm_idx = at.add(pm).ok()?;
            let v3a = at.add(fa.pool_address).ok()?;
            let v3b = at.add(fb.pool_address).ok()?;
            let forward_b = at.add(fwd_b).ok()?;
            let fee_c = fc.swap_fee;
            let ts_c = fc.tick_spacing;
            let c0 = at.add(fc.currency0_address).ok()?;
            let c1 = at.add(fc.currency1_address).ok()?;
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
        }
        // ── v2v4v2: SelfFund + v2-flash(fc)[prefund→v2a, V4Unlock[V4Sync(fwd_a),
        // a_calc, V4Settle, V4Swap(b), V4TakeCompact→v2c(repays fc),
        // V4SettleΔ(fwd_a)]]. Trailing V2 flash wraps the unlock.
        [Prot::V2, Prot::V4, Prot::V2] => {
            if inputs.hop_outputs.contains(&0) || !fits_i128(optimal_input) {
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
            let pm_idx = at.add(pm).ok()?;
            let forward_a = at.add(forward_a_cur).ok()?;
            let forward_b = at.add(forward_b_cur).ok()?;
            let v2a = at.add(fa.pool_address).ok()?;
            let v2c = at.add(fc.pool_address).ok()?;
            let fee_b = fb.swap_fee;
            let ts_b = fb.tick_spacing;
            let c0_b = at.add(fb.currency0_address).ok()?;
            let c1_b = at.add(fb.currency1_address).ok()?;
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
        }
        // ── v2v4v3: SelfFund + v3-flash(fc)[prefund→v2a, V4Unlock[V4Sync(fwd_a),
        // a_calc, V4Settle, V4Swap(b), V4TakeCompact→v3c(repays fc),
        // V4SettleΔ(fwd_a)]]. Trailing V3 flash wraps the unlock.
        [Prot::V2, Prot::V4, Prot::V3] => {
            if inputs.hop_outputs.contains(&0) || !fits_i128(optimal_input) {
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
            let pm_idx = at.add(pm).ok()?;
            let forward_a = at.add(forward_a_cur).ok()?;
            let forward_b = at.add(forward_b_cur).ok()?;
            let v2a = at.add(fa.pool_address).ok()?;
            let v3c = at.add(fc.pool_address).ok()?;
            let fee_b = fb.swap_fee;
            let ts_b = fb.tick_spacing;
            let c0_b = at.add(fb.currency0_address).ok()?;
            let c1_b = at.add(fb.currency1_address).ok()?;
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
        }
        // The group-C (hop1,hop2) under the gate is exhaustive; `derive`'s
        // gate filters everything else (v4-led, pure V2/V3, the v3v4 merge).
        _ => None,
    }
}

#[cfg(test)]
mod rule_walker_tests {
    #![expect(clippy::cast_possible_truncation, clippy::expect_used)]
    use super::{derive, rule_walk_v2v3, rule_walk_v2v3_v4_mixed, rule_walk_v4_led};
    use crate::composers::{
        ComposerInputs, EncodeOptions, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
    };
    use crate::encoders::enc_preamble;
    use crate::grammar_ledger::Prot;
    use crate::grammar_plan::plan_to_bytes;
    use crate::grammar_walker::facts_for;
    use alloy::primitives::{address, Address};

    const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    const WBTC: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    const PM: Address = address!("000000000004444c5dc75cB358380D2e3dE08A90");
    const EXEC: Address = address!("DeAd0000000000000000000000000000000000Be");

    fn hop(prots: &[Prot], i: usize) -> HopInfo {
        let cycle = [WETH, USDC, WBTC];
        let in_t = cycle[i % 3];
        let out_t = cycle[(i + 1) % 3];
        match prots[i] {
            Prot::V2 => HopInfo::V2(V2HopInfo {
                pool_address: Address::from([0xA0 + i as u8; 20]),
                token0_address: in_t,
                token1_address: out_t,
                fee: 30,
                zfo: true,
            }),
            Prot::V3 => HopInfo::V3(V3HopInfo {
                pool_address: Address::from([0xB0 + i as u8; 20]),
                token0_address: in_t,
                token1_address: out_t,
                fee: 3000,
                zfo: true,
            }),
            Prot::V4 => unreachable!("V4 excluded by the walker gate"),
        }
    }

    fn full_bytes(plan: &crate::grammar_plan::Plan, at: &crate::encoders::AddressTable) -> Vec<u8> {
        let mut b = enc_preamble(at);
        b.extend_from_slice(&plan_to_bytes(plan, at));
        b
    }

    /// Shadow gate: the rule-driven walker produces byte-identical output to
    /// the hand-authored arms (pre-cutover) / routes through the walker
    /// (post-cutover) for every V2/V3-only 3-hop family, across the amount +
    /// output grid. Byte-identity is the cutover contract.
    #[test]
    fn rule_walker_shadows_the_7_arms() {
        let families: [[Prot; 3]; 7] = [
            [Prot::V3, Prot::V3, Prot::V3],
            [Prot::V3, Prot::V3, Prot::V2],
            [Prot::V3, Prot::V2, Prot::V3],
            [Prot::V3, Prot::V2, Prot::V2],
            [Prot::V2, Prot::V2, Prot::V3],
            [Prot::V2, Prot::V3, Prot::V3],
            [Prot::V2, Prot::V3, Prot::V2],
        ];
        // (optimal, [outs], [consumed]) grid — mirrors glopcn_bytepin.
        let configs: [(u128, [u128; 3], [u128; 3]); 4] = [
            (
                1_000_000_000_000_000_000,
                [1_000_000_000_000_000_000; 3],
                [999_999_999_999_999_999; 3],
            ),
            (
                1_000_000_000_000_000_000,
                [1_000_000_000_000_000_000; 3],
                [1_000_000_000_000_000_000; 3],
            ),
            (2u128.pow(95), [2u128.pow(95); 3], [2u128.pow(95) - 1; 3]),
            (2u128.pow(95), [2u128.pow(95); 3], [2u128.pow(95); 3]),
        ];

        for fam in families {
            let hops: Vec<HopInfo> = (0..3).map(|i| hop(&fam, i)).collect();
            let path = PathInfo::new(hops);
            let label = format!("{}{}{}", fam[0] as u8, fam[1] as u8, fam[2] as u8);
            for (ci, (optimal, outs, consumed)) in configs.iter().enumerate() {
                let inputs = ComposerInputs {
                    executor_address: EXEC,
                    pool_manager_address: PM,
                    weth_address: WETH,
                    optimal_input: *optimal,
                    hop_outputs: outs,
                    consumed_inputs: consumed,
                    opts: EncodeOptions::default(),
                };
                let facts = facts_for(&path, &inputs).expect("facts exist");
                let (plan_arm, at_arm) = derive(&facts, &inputs).expect("arm produces a plan");
                let (plan_wlk, at_wlk) =
                    rule_walk_v2v3(&facts, &inputs).expect("walker produces a plan");
                let bytes_arm = full_bytes(&plan_arm, &at_arm);
                let bytes_wlk = full_bytes(&plan_wlk, &at_wlk);
                assert_eq!(
                    bytes_arm,
                    bytes_wlk,
                    "walker diverged from derive for family {} config {} (bytes {} vs {})",
                    label,
                    ci,
                    bytes_arm.len(),
                    bytes_wlk.len(),
                );
            }
        }
    }

    /// Mirror of `glopcn_bytepin`'s V4-led slice: a 3-hop WETH/USDC/WBTC cycle
    /// where each slot is independently V4-led (`hop0 == V4`).
    fn v4_led_hops(prots: &[Prot]) -> Vec<HopInfo> {
        let cycle = [WETH, USDC, WBTC];
        (0..3)
            .map(|i| {
                let in_t = cycle[i % 3];
                let out_t = cycle[(i + 1) % 3];
                match prots[i] {
                    Prot::V2 => HopInfo::V2(V2HopInfo {
                        pool_address: Address::from([0xA0 + i as u8; 20]),
                        token0_address: in_t,
                        token1_address: out_t,
                        fee: 30,
                        zfo: true,
                    }),
                    Prot::V3 => HopInfo::V3(V3HopInfo {
                        pool_address: Address::from([0xB0 + i as u8; 20]),
                        token0_address: in_t,
                        token1_address: out_t,
                        fee: 3000,
                        zfo: true,
                    }),
                    Prot::V4 => HopInfo::V4(V4HopInfo {
                        pool_manager_address: PM,
                        pool_id_hex: format!("0x{i:02x}"),
                        currency0_address: in_t,
                        currency1_address: out_t,
                        fee: 500,
                        tick_spacing: 10,
                        hook_address: Address::ZERO,
                        zfo: true,
                    }),
                }
            })
            .collect()
    }

    /// Shadow gate: the rule-driven walker produces byte-identical output to
    /// the hand-authored V4-led arms (pre-cutover) / routes through the walker
    /// (post-cutover) for every V4-led 3-hop family, across the same amount +
    /// `EncodeOptions` grid `glopcn_bytepin` pins. Byte-identity is the cutover
    /// contract. (The validator `Reject` partition the bytepin also pins is a
    /// post-`derive` concern; this gate compares the produced Plan bytes, which
    /// the validator then rejects identically for both paths.)
    #[test]
    fn rule_walker_shadows_the_v4_led_group() {
        // The 9 V4-led families — hop0 is always V4; (hop1,hop2) ∈ {V2,V3,V4}².
        let families: [[Prot; 3]; 9] = [
            [Prot::V4, Prot::V4, Prot::V4],
            [Prot::V4, Prot::V2, Prot::V2],
            [Prot::V4, Prot::V2, Prot::V4],
            [Prot::V4, Prot::V3, Prot::V3],
            [Prot::V4, Prot::V3, Prot::V4],
            [Prot::V4, Prot::V4, Prot::V2],
            [Prot::V4, Prot::V4, Prot::V3],
            [Prot::V4, Prot::V2, Prot::V3],
            [Prot::V4, Prot::V3, Prot::V2],
        ];
        // (optimal, [outs], [consumed]) grid — mirrors glopcn_bytepin.
        let configs: [(u128, [u128; 3], [u128; 3]); 4] = [
            (
                1_000_000_000_000_000_000,
                [1_000_000_000_000_000_000; 3],
                [999_999_999_999_999_999; 3],
            ),
            (
                1_000_000_000_000_000_000,
                [1_000_000_000_000_000_000; 3],
                [1_000_000_000_000_000_000; 3],
            ),
            (2u128.pow(95), [2u128.pow(95); 3], [2u128.pow(95) - 1; 3]),
            (2u128.pow(95), [2u128.pow(95); 3], [2u128.pow(95); 3]),
        ];
        let opts: [(&str, EncodeOptions); 3] = [
            ("base", EncodeOptions::default()),
            (
                "erc6909",
                EncodeOptions {
                    erc6909_profit: true,
                    use_v4_batch: false,
                    ..Default::default()
                },
            ),
            (
                "batch",
                EncodeOptions {
                    erc6909_profit: false,
                    use_v4_batch: true,
                    ..Default::default()
                },
            ),
        ];

        for fam in families {
            let hops = v4_led_hops(&fam);
            let path = PathInfo::new(hops);
            let label = format!("{}{}{}", fam[0] as u8, fam[1] as u8, fam[2] as u8);
            for (oi, (olabel, opt)) in opts.iter().enumerate() {
                for (ci, (optimal, outs, consumed)) in configs.iter().enumerate() {
                    let inputs = ComposerInputs {
                        executor_address: EXEC,
                        pool_manager_address: PM,
                        weth_address: WETH,
                        optimal_input: *optimal,
                        hop_outputs: outs,
                        consumed_inputs: consumed,
                        opts: *opt,
                    };
                    let facts = facts_for(&path, &inputs).expect("facts exist");
                    let arm = derive(&facts, &inputs);
                    let wlk = rule_walk_v4_led(&facts, &inputs);
                    // Both must decline together (guard-ladder parity).
                    assert_eq!(
                        arm.is_some(),
                        wlk.is_some(),
                        "walker/derive decline mismatch family {label} opt {olabel} cfg {ci}"
                    );
                    let (Some((plan_arm, at_arm)), Some((plan_wlk, at_wlk))) = (arm, wlk) else {
                        continue;
                    };
                    let bytes_arm = full_bytes(&plan_arm, &at_arm);
                    let bytes_wlk = full_bytes(&plan_wlk, &at_wlk);
                    assert_eq!(
                        bytes_arm,
                        bytes_wlk,
                        "walker diverged from derive for family {label} opt {olabel} cfg {ci} \
                         (bytes {} vs {})",
                        bytes_arm.len(),
                        bytes_wlk.len(),
                    );
                    let _ = oi;
                }
            }
        }
    }

    /// Shadow gate: the rule-driven walker produces byte-identical output to
    /// `derive` for every group-C 3-hop family (hop0 ∈ {V2,V3}, some hop V4,
    /// not the v4-led block, not the v3v4 terminal-form merge), across the same
    /// amount + `EncodeOptions` grid `glopcn_bytepin` pins. Byte-identity is the
    /// cutover contract. Post-cutover this is a routing tautology (`derive`
    /// routes through the walker); it stays as the per-family guard regression
    /// pin, mirroring the T6a/T6b shadow gates.
    #[test]
    fn rule_walker_shadows_the_group_c() {
        // The 7 group-C families — hop0 ∈ {V2,V3}; (hop1,hop2) span the V4-crossing
        // pairs that are neither v4-led nor the v3v4 terminal-form merge.
        let families: [[Prot; 3]; 7] = [
            [Prot::V2, Prot::V2, Prot::V4],
            [Prot::V2, Prot::V4, Prot::V4],
            [Prot::V2, Prot::V3, Prot::V4],
            [Prot::V3, Prot::V2, Prot::V4],
            [Prot::V3, Prot::V3, Prot::V4],
            [Prot::V2, Prot::V4, Prot::V2],
            [Prot::V2, Prot::V4, Prot::V3],
        ];
        // (optimal, [outs], [consumed]) grid — mirrors glopcn_bytepin.
        let configs: [(u128, [u128; 3], [u128; 3]); 4] = [
            (
                1_000_000_000_000_000_000,
                [1_000_000_000_000_000_000; 3],
                [999_999_999_999_999_999; 3],
            ),
            (
                1_000_000_000_000_000_000,
                [1_000_000_000_000_000_000; 3],
                [1_000_000_000_000_000_000; 3],
            ),
            (2u128.pow(95), [2u128.pow(95); 3], [2u128.pow(95) - 1; 3]),
            (2u128.pow(95), [2u128.pow(95); 3], [2u128.pow(95); 3]),
        ];
        let opts: [(&str, EncodeOptions); 3] = [
            ("base", EncodeOptions::default()),
            (
                "erc6909",
                EncodeOptions {
                    erc6909_profit: true,
                    use_v4_batch: false,
                    ..Default::default()
                },
            ),
            (
                "batch",
                EncodeOptions {
                    erc6909_profit: false,
                    use_v4_batch: true,
                    ..Default::default()
                },
            ),
        ];

        for fam in families {
            let hops = v4_led_hops(&fam);
            let path = PathInfo::new(hops);
            let label = format!("{}{}{}", fam[0] as u8, fam[1] as u8, fam[2] as u8);
            for (oi, (olabel, opt)) in opts.iter().enumerate() {
                for (ci, (optimal, outs, consumed)) in configs.iter().enumerate() {
                    let inputs = ComposerInputs {
                        executor_address: EXEC,
                        pool_manager_address: PM,
                        weth_address: WETH,
                        optimal_input: *optimal,
                        hop_outputs: outs,
                        consumed_inputs: consumed,
                        opts: *opt,
                    };
                    let facts = facts_for(&path, &inputs).expect("facts exist");
                    let arm = derive(&facts, &inputs);
                    let wlk = rule_walk_v2v3_v4_mixed(&facts, &inputs);
                    // Both must decline together (guard-ladder parity).
                    assert_eq!(
                        arm.is_some(),
                        wlk.is_some(),
                        "walker/derive decline mismatch family {label} opt {olabel} cfg {ci}"
                    );
                    let (Some((plan_arm, at_arm)), Some((plan_wlk, at_wlk))) = (arm, wlk) else {
                        continue;
                    };
                    let bytes_arm = full_bytes(&plan_arm, &at_arm);
                    let bytes_wlk = full_bytes(&plan_wlk, &at_wlk);
                    assert_eq!(
                        bytes_arm,
                        bytes_wlk,
                        "walker diverged from derive for family {label} opt {olabel} cfg {ci} \
                         (bytes {} vs {})",
                        bytes_arm.len(),
                        bytes_wlk.len(),
                    );
                    let _ = oi;
                }
            }
        }
    }
}
