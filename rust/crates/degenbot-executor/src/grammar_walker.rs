//! ADR-031 D6 — the sole facts-driven Plan producer (epic `6SU5LM`).
//!
//! Every one of the 35 `build_*_walk` family producers is a **thin delegate**
//! to this module: it produces the per-hop `facts_of_<family>` descriptor
//! (protocol metadata: direction, fees, currencies, tick spacing) and hands
//! them to `derive_plan`, which **derives the enclosure** (which
//! `FlashSwap`/`V4Unlock` wraps which, the repayment order, and the
//! barrier/batch/capture arms) from those facts + `inputs.opts`. No
//! `build_*_walk` body emits a `PlanStep` directly — the D6 invariant the
//! `facts_driven_tests` probe enforces.
//!
//! `plan_to_bytes` and the `LedgerValidator` gate are reused unchanged (one
//! representation): the walker emits exactly one `Plan`, and the encoder +
//! validator are pure functions of it. Structural + behavioral parity with
//! the pre-refactor reference producer is pinned by the revm contract matrix
//! + the `spike_derivation` golden suite.

use crate::composers::{
    resolve_axes, ComposerInputs, CurrencyBridge, HopInfo, PathInfo, V2HopInfo, V3HopInfo,
    V4HopInfo, NATIVE_CURRENCY_ADDRESS,
};
use crate::encoders::{AddressTable, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH};
use crate::grammar_ledger::{FundingSource, ProfitCapture, Prot};
use crate::grammar_plan::{
    plan_to_ledger_ops, v2_forward, v3_forward, v3_input, Plan, PlanStep, V4BatchSwap,
};
use crate::grammar_shape::{
    native_capture_declines, v4_bridge_steps, v4_hop_currencies, v4_scaffold_table,
    v4_terminal_capture_steps,
};
use alloy::primitives::Address;

/// Whether an amount fits the on-chain i128 swap-input field.
fn fits_i128(v: u128) -> bool {
    v <= i128::MAX as u128
}

/// Where a hop's swap output is routed (the hop-coupling fact).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutDest {
    /// Credits the executor.
    Executor,
    /// Routed into the PoolManager (seeds the V4 unlock ledger).
    PoolManager,
    /// Taken to a pool to REPAY its flash borrow.
    Repay(Address),
}

/// How a hop's borrowed input is repaid (the repayment-obligation fact).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Repay {
    /// Repaid with a currency by an explicit transfer in its own callback.
    SelfRefund,
    /// Repaid off-stream by a downstream hop's take to this pool.
    Offstream,
    /// No borrow to repay (a V4 middle nets to zero inside its unlock).
    NetZero,
}

/// Per-protocol **hop facts** — the ADR-031 D4 data half: ledgers a hop
/// touches, direction, output slot, and repayment obligation. The walker
/// derives the enclosure from these; the mechanics (the swap/callback step) is
/// per-protocol code below.
///
/// `prot`/`zfo`/`repay` are declared here as part of the facts schema but are
/// not yet consumed by the spike (A2's generic walker reads them): they pin the
/// schema A2 generalizes over.
#[expect(
    dead_code,
    reason = "T3-T7 (epic 6SU5LM): V4 mechanics read pool_id_hex; prot selects mechanics in the multi-shape deriver"
)]
pub(crate) struct HopFacts {
    pub(crate) prot: Prot,
    pub(crate) zfo: bool,
    pub(crate) swap_fee: u16,
    pub(crate) tick_spacing: i16,
    pub(crate) out_currency: Address,
    pub(crate) in_currency: Address,
    pub(crate) out_dest: OutDest,
    pub(crate) repay: Repay,
    /// The V2/V3 pool, or the V4 pool-manager — the mechanics' pool identity.
    pub(crate) pool_address: Address,
    /// V4 only — the pool-id hex. `None` for V2/V3.
    pub(crate) pool_id_hex: Option<String>,
    /// V4 only — currency0 / currency1.
    pub(crate) currency0_address: Address,
    pub(crate) currency1_address: Address,
}

/// Per-protocol **mechanics** (ADR-031 D4 code half): how a protocol's hop
/// becomes a `PlanStep` tree. For the spike only the V3 mechanics the
/// `v3_v4_v3` shape exercises is implemented; A2 generalizes.
mod mechanics {
    use super::{AddressTable, HopFacts, OutDest};
    use crate::encoders::{SENTINEL_NATIVE, SENTINEL_PM, SENTINEL_SELF};
    use crate::grammar_ledger::Prot;
    use crate::grammar_plan::{Plan, PlanStep, V4BatchSwap};
    use alloy::primitives::Address;

    /// The V3 flash-swap step, built from the hop's facts. `out_dest` picks
    /// the recipient routing. `pool_address` + `zfo` come from the facts.
    pub fn v3_flash(
        at: &mut AddressTable,
        facts: &HopFacts,
        out_amount: u128,
        in_amount: u128,
        auto_repay: bool,
        callback: Vec<PlanStep>,
    ) -> Option<PlanStep> {
        let pool_idx = at.add(facts.pool_address).ok()?;
        let (recipient_idx, recipient_pool_addr, recipient_pool_repays) = match facts.out_dest {
            OutDest::Executor => (SENTINEL_SELF, None, false),
            OutDest::PoolManager => (SENTINEL_PM, None, false),
            OutDest::Repay(_) => unreachable!("V3 hop never repays a pool here"),
        };
        Some(PlanStep::FlashSwap {
            pool_idx,
            pool_addr: facts.pool_address,
            protocol: Prot::V3,
            zfo: facts.zfo,
            fee: facts.swap_fee,
            out_currency: facts.out_currency,
            out_amount,
            in_currency: facts.in_currency,
            in_amount,
            recipient_idx,
            recipient_pool_addr,
            recipient_pool_repays,
            auto_repay,
            callback,
        })
    }

    /// The full-form V3 flash: a `FlashSwap` with explicit recipient routing
    /// (`recipient_idx`/`recipient_pool_addr`/`recipient_pool_repays`). The
    /// 3-hop nested-flash families (T5) route a flash's repayment to a
    /// downstream recipient pool (`recipient_pool_repays`), which the default
    /// `v3_flash` (SELF/None/false) cannot express.
    #[expect(clippy::too_many_arguments)]
    pub fn v3_flash_to(
        at: &mut AddressTable,
        facts: &HopFacts,
        out_amount: u128,
        in_amount: u128,
        auto_repay: bool,
        recipient_idx: u8,
        recipient_pool_addr: Option<Address>,
        recipient_pool_repays: bool,
        callback: Vec<PlanStep>,
    ) -> Option<PlanStep> {
        Some(PlanStep::FlashSwap {
            pool_idx: at.add(facts.pool_address).ok()?,
            pool_addr: facts.pool_address,
            protocol: Prot::V3,
            zfo: facts.zfo,
            fee: facts.swap_fee,
            out_currency: facts.out_currency,
            out_amount,
            in_currency: facts.in_currency,
            in_amount,
            recipient_idx,
            recipient_pool_addr,
            recipient_pool_repays,
            auto_repay,
            callback,
        })
    }

    /// The V2 forward-swap step — a `V2SwapCalc` (the terminal-V2 exact-draw
    /// rule: swap from whatever the feeder delivered to the pair, never an
    /// exact-out `V2_SWAP_COMPACT`). `recipient` routing is positional (the
    /// next pool in the chain, or SENTINEL_SELF for a terminal), so it is
    /// passed explicitly rather than derived from `out_dest`.
    pub fn v2_swap(
        at: &mut AddressTable,
        facts: &HopFacts,
        out_amount: u128,
        recipient_idx: u8,
        recipient_pool_addr: Option<Address>,
        recipient_repays: bool,
    ) -> Option<PlanStep> {
        Some(PlanStep::V2SwapCalc {
            pool_idx: at.add(facts.pool_address).ok()?,
            pool_addr: facts.pool_address,
            zfo: facts.zfo,
            recipient_idx,
            fee: facts.swap_fee,
            out_currency: facts.out_currency,
            out_amount,
            recipient_pool_addr,
            recipient_repays,
        })
    }

    /// The V2 flash-swap step — a `FlashSwap { protocol: Prot::V2 }`.
    /// `out_dest` picks the recipient routing (mirrors `v3_flash`).
    pub fn v2_flash(
        at: &mut AddressTable,
        facts: &HopFacts,
        out_amount: u128,
        in_currency: Address,
        in_amount: u128,
        callback: Vec<PlanStep>,
    ) -> Option<PlanStep> {
        let pool_idx = at.add(facts.pool_address).ok()?;
        let (recipient_idx, recipient_pool_addr, recipient_pool_repays) = match facts.out_dest {
            OutDest::Executor => (SENTINEL_SELF, None, false),
            OutDest::PoolManager => (SENTINEL_PM, None, false),
            OutDest::Repay(addr) => (at.add(addr).ok()?, Some(addr), false),
        };
        Some(PlanStep::FlashSwap {
            pool_idx,
            pool_addr: facts.pool_address,
            protocol: Prot::V2,
            zfo: facts.zfo,
            fee: facts.swap_fee,
            out_currency: facts.out_currency,
            out_amount,
            in_currency,
            in_amount,
            recipient_idx,
            recipient_pool_addr,
            recipient_pool_repays,
            auto_repay: false,
            callback,
        })
    }

    /// The V2 direct-forward swap routed to an explicit recipient whose flash
    /// it repays (`recipient_repays`). The 3-hop V2-leading families (T5) use
    /// `V2SwapDirect` — the recipient pool's flash is repaid by this swap's
    /// forward, so the executor never hands the token to the recipient.
    pub fn v2_swap_direct(
        at: &mut AddressTable,
        facts: &HopFacts,
        out_amount: u128,
        out_currency: Address,
        recipient_idx: u8,
        recipient_pool_addr: Option<Address>,
        recipient_repays: bool,
    ) -> Option<PlanStep> {
        Some(PlanStep::V2SwapDirect {
            pool_idx: at.add(facts.pool_address).ok()?,
            pool_addr: facts.pool_address,
            zfo: facts.zfo,
            out_amount,
            recipient_idx,
            out_currency,
            recipient_pool_addr,
            recipient_repays,
        })
    }

    // ── V4 mechanics (ADR-031 D4 code half) ──────────────────────────────

    /// The V4 swap step, parameterized by the hop's facts (c0/c1 from the
    /// currency pair, fee + tick_spacing from the facts).
    pub fn v4_swap(
        at: &mut AddressTable,
        facts: &HopFacts,
        amount_in: u128,
        out_amount: u128,
    ) -> Option<PlanStep> {
        Some(PlanStep::V4Swap {
            c0_idx: at.add(facts.currency0_address).ok()?,
            c1_idx: at.add(facts.currency1_address).ok()?,
            fee: facts.swap_fee,
            tick_spacing: facts.tick_spacing,
            hooks_idx: SENTINEL_NATIVE,
            zfo: facts.zfo,
            amount: amount_in,
            in_currency: facts.in_currency,
            in_amount: amount_in,
            out_currency: facts.out_currency,
            out_amount,
        })
    }

    /// A V4 unlock wrapper — nests the unlock interior (`inner`) at the given
    /// pool-manager index.
    pub fn v4_unlock(inner: Plan, pool_manager_idx: u8) -> PlanStep {
        PlanStep::V4Unlock {
            inner,
            pool_manager_idx,
        }
    }

    /// A V4 take-to-repay step: take the hop's `out_currency` (the ledger the
    /// PM credits at swap) to `recipient_idx`, repaying its flash debt when
    /// `repays_flash` is set.
    pub fn v4_take_compact(
        at: &mut AddressTable,
        facts: &HopFacts,
        recipient_idx: u8,
        amount: u128,
        repays_flash: Option<Address>,
    ) -> Option<PlanStep> {
        Some(PlanStep::V4TakeCompact {
            currency_idx: at.add(facts.out_currency).ok()?,
            currency_addr: facts.out_currency,
            recipient_idx,
            amount,
            seeds_pool: None,
            repays_flash,
        })
    }

    /// Settle a currency into the pool-manager ledger (credit the forward).
    pub fn v4_settle(currency_addr: Address, amount: u128) -> PlanStep {
        PlanStep::V4Settle {
            currency_addr,
            amount,
        }
    }

    /// Net every pool-manager ledger to zero (the unlock exit).
    pub fn v4_settle_all() -> PlanStep {
        PlanStep::V4SettleAll
    }

    /// A batched V4 swap (the `use_v4_batch` optimization) over the hop.
    #[expect(
        dead_code,
        reason = "T7 (epic 6SU5LM) wires the v4_v4/v4_v4_v4 batch paths"
    )]
    pub fn v4_batch(
        at: &mut AddressTable,
        facts: &HopFacts,
        entries: Vec<V4BatchSwap>,
    ) -> PlanStep {
        // `currency`/`fee`/`tick_spacing` ride on the entries; the pool-manager
        // index is implicit. Kept as a thin wrapper so the mechanics surface is
        // homogeneous (T7 wires the `v4_v4`/`v4_v4_v4` batch paths through it).
        let _ = (at, facts);
        PlanStep::V4Batch { entries }
    }
}

/// Read the `v3_v4_v3` per-protocol facts (the D4 data half).
pub(crate) fn facts_of_v3v4v3(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    let (HopInfo::V3(a), HopInfo::V4(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let (forward_b, in_currency_b) = v4_hop_currencies(b);
    let fwd_c = v3_forward(c);
    let in_c = v3_input(c);
    let fa = HopFacts {
        prot: Prot::V3,
        zfo: a.zfo,
        swap_fee: u16::try_from(a.fee).ok()?,
        tick_spacing: 0,
        out_currency: fwd_a,
        in_currency: in_a,
        out_dest: OutDest::PoolManager, // the leading V3 seeds the PM for the unlock
        repay: Repay::SelfRefund,
        pool_address: a.pool_address,
        pool_id_hex: None,
        currency0_address: a.token0_address,
        currency1_address: a.token1_address,
    };
    let fb = HopFacts {
        prot: Prot::V4,
        zfo: b.zfo,
        swap_fee: u16::try_from(b.fee).ok()?,
        tick_spacing: i16::try_from(b.tick_spacing).ok()?,
        out_currency: forward_b,
        in_currency: in_currency_b,
        out_dest: OutDest::Repay(c.pool_address), // take to the terminal V3, repaying its flash
        repay: Repay::NetZero,
        pool_address: b.pool_manager_address,
        pool_id_hex: Some(b.pool_id_hex.clone()),
        currency0_address: b.currency0_address,
        currency1_address: b.currency1_address,
    };
    let fc = HopFacts {
        prot: Prot::V3,
        zfo: c.zfo,
        swap_fee: u16::try_from(c.fee).ok()?,
        tick_spacing: 0,
        out_currency: fwd_c,
        in_currency: in_c,
        out_dest: OutDest::Executor,
        repay: Repay::Offstream,
        pool_address: c.pool_address,
        pool_id_hex: None,
        currency0_address: c.token0_address,
        currency1_address: c.token1_address,
    };
    Some(vec![fa, fb, fc])
}

/// The 2-hop V2→V3 facts (`v2v3`, funding-branched). The terminal's
/// `out_currency` is the closing WETH; the terminal's `in_currency` is the
/// leading forward.
pub(crate) fn facts_of_v2v3(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 2 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V3(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let fwd_a = v2_forward(a);
    Some(vec![
        HopFacts {
            prot: Prot::V2,
            zfo: a.zfo,
            swap_fee: a.fee,
            tick_spacing: 0,
            out_currency: fwd_a,
            in_currency: if a.zfo {
                a.token0_address
            } else {
                a.token1_address
            },
            out_dest: OutDest::Executor,
            repay: Repay::SelfRefund,
            pool_address: a.pool_address,
            pool_id_hex: None,
            currency0_address: a.token0_address,
            currency1_address: a.token1_address,
        },
        HopFacts {
            prot: Prot::V3,
            zfo: b.zfo,
            swap_fee: u16::try_from(b.fee).ok()?,
            tick_spacing: 0,
            out_currency: inputs.weth_address, // closing WETH, per the gold
            in_currency: fwd_a,
            out_dest: OutDest::Executor,
            repay: Repay::Offstream,
            pool_address: b.pool_address,
            pool_id_hex: None,
            currency0_address: b.token0_address,
            currency1_address: b.token1_address,
        },
    ])
}

/// The 2-hop V2→V3 funding-branched shape (`v2v3`): SelfFund prefunds + V2Swap
/// + a V3 flash terminal, or an InPathFlash leading V2 flash wrapping the V3.
pub(crate) fn derive_2hop_v2v3(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 2 || facts[0].prot != Prot::V2 || facts[1].prot != Prot::V3 {
        return None;
    }
    let (fa, fb) = (&facts[0], &facts[1]);
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    if forward_out == 0 {
        return None;
    }
    if !fits_i128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_i128(b_swap_in) {
        return None;
    }
    let terminal_out = *inputs.hop_outputs.get(1)?;
    let weth = inputs.weth_address;

    let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
    let a_idx = at.add(fa.pool_address).ok()?;
    let b_idx = at.add(fb.pool_address).ok()?;

    // One funding branch's `at` borrow is live at a time (a plain if/else,
    // not two simultaneous FnOnce closures), so the borrow checker is happy.
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
            mechanics::v3_flash(&mut at, fb, terminal_out, b_swap_in, true, vec![])?,
        ]
    } else {
        let forward_idx = at.add(fa.out_currency).ok()?;
        let inner_v3 = mechanics::v3_flash(
            &mut at,
            fb,
            terminal_out,
            b_swap_in,
            false,
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
    Some((plan, at))
}

/// The any-N all-V2 facts (`all_v2`, funding-branched). `closing` is the last
/// hop's `out_currency`; no `weth` is baked in (the chain is the whole stream).
pub(crate) fn facts_of_all_v2(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    let v2s = path
        .hops
        .iter()
        .map(|h| match h {
            HopInfo::V2(h) => Some(HopFacts {
                prot: Prot::V2,
                zfo: h.zfo,
                swap_fee: h.fee,
                tick_spacing: 0,
                out_currency: v2_forward(h),
                in_currency: if h.zfo {
                    h.token0_address
                } else {
                    h.token1_address
                },
                out_dest: OutDest::Executor,
                repay: Repay::SelfRefund,
                pool_address: h.pool_address,
                pool_id_hex: None,
                currency0_address: h.token0_address,
                currency1_address: h.token1_address,
            }),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    if v2s.len() < 2 {
        return None;
    }
    Some(v2s)
}

/// The any-N all-V2 chain shape (`all_v2`, funding-branched): a SelfFund walk
/// of `V2SwapCalc` steps, or an InPathFlash leading V2 flash wrapping the chain.
#[expect(clippy::too_many_lines)]
pub(crate) fn derive_all_v2_chain(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let n = facts.len();
    if n < 2 || facts.iter().any(|f| f.prot != Prot::V2) {
        return None;
    }
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
    Some((plan, at))
}

/// The 2-hop V3→V3 facts (`v3v3`, SelfFund, two V3 flashes). The terminal's
/// `out_currency` is the closing WETH (the hand-authored producer hardcodes
/// `weth` for the terminal V3), so the descriptor takes `inputs`.
pub(crate) fn facts_of_v3v3(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 2 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V3(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let fwd_a = v3_forward(a);
    Some(vec![
        HopFacts {
            prot: Prot::V3,
            zfo: a.zfo,
            swap_fee: u16::try_from(a.fee).ok()?,
            tick_spacing: 0,
            out_currency: fwd_a,
            in_currency: if a.zfo {
                a.token0_address
            } else {
                a.token1_address
            },
            out_dest: OutDest::Executor,
            repay: Repay::SelfRefund,
            pool_address: a.pool_address,
            pool_id_hex: None,
            currency0_address: a.token0_address,
            currency1_address: a.token1_address,
        },
        HopFacts {
            prot: Prot::V3,
            zfo: b.zfo,
            swap_fee: u16::try_from(b.fee).ok()?,
            tick_spacing: 0,
            out_currency: inputs.weth_address, // closing WETH, per the gold
            in_currency: fwd_a,
            out_dest: OutDest::Executor,
            repay: Repay::Offstream,
            pool_address: b.pool_address,
            pool_id_hex: None,
            currency0_address: b.token0_address,
            currency1_address: b.token1_address,
        },
    ])
}

/// The 2-hop V3→V2 facts (`v3v2`, SelfFund, terminal V2 swap). The terminal
/// V2 `V2SwapCalc` outputs the closing WETH (`inputs.weth_address`).
pub(crate) fn facts_of_v3v2(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 2 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V2(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let fwd_a = v3_forward(a);
    Some(vec![
        HopFacts {
            prot: Prot::V3,
            zfo: a.zfo,
            swap_fee: u16::try_from(a.fee).ok()?,
            tick_spacing: 0,
            out_currency: fwd_a,
            in_currency: if a.zfo {
                a.token0_address
            } else {
                a.token1_address
            },
            out_dest: OutDest::Executor,
            repay: Repay::SelfRefund,
            pool_address: a.pool_address,
            pool_id_hex: None,
            currency0_address: a.token0_address,
            currency1_address: a.token1_address,
        },
        HopFacts {
            prot: Prot::V2,
            zfo: b.zfo,
            swap_fee: b.fee,
            tick_spacing: 0,
            out_currency: inputs.weth_address, // closing WETH, per the gold
            in_currency: fwd_a,
            out_dest: OutDest::Executor,
            repay: Repay::Offstream,
            pool_address: b.pool_address,
            pool_id_hex: None,
            currency0_address: b.token0_address,
            currency1_address: b.token1_address,
        },
    ])
}

/// The 2-hop leading-V3-flash shape (`v3v2`, `v3v3`) — a SelfFund + a leading
/// V3 flash; the terminal is a V2 `V2SwapCalc` from the seeded forward (`v3v2`)
/// or a V3 flash with `auto_repay` (`v3v3`). Both families share this shape;
/// `facts[1].prot` picks the terminal mechanics (ADR-031 D3/D6).
/// A V3 hop's facts (shared by the V3-involving shape derivers).
pub(crate) fn v3_hop_facts(h: &V3HopInfo) -> HopFacts {
    HopFacts {
        prot: Prot::V3,
        zfo: h.zfo,
        swap_fee: u16::try_from(h.fee).unwrap_or(0),
        tick_spacing: 0,
        out_currency: v3_forward(h),
        in_currency: v3_input(h),
        out_dest: OutDest::Executor,
        repay: Repay::SelfRefund,
        pool_address: h.pool_address,
        pool_id_hex: None,
        currency0_address: h.token0_address,
        currency1_address: h.token1_address,
    }
}

/// The 3-hop V3→V3→V3 facts (`v3v3v3`).
pub(crate) fn facts_of_v3v3v3(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V3(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v3_hop_facts(a), v3_hop_facts(b), v3_hop_facts(c)])
}

/// The 3-hop V3→V3→V3 shape: a 3-deep nested V3 flash chain. The enclosure is
/// built inside-out (innermost to outermost): hop2 is the OUTERMOST flash
/// (SELF recipient), hop1 the middle (recipient = hop2's pool, rpr=true), hops0
/// the innermost (recipient = hop1's pool, rpr=true, auto_repay).
pub(crate) fn derive_3hop_v3v3v3(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3 || facts.iter().any(|f| f.prot != Prot::V3) {
        return None;
    }
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

    // innermost first (release the `at` borrow), then wrap outward.
    let inner_a = mechanics::v3_flash_to(
        &mut at,
        fa,
        out_a,
        optimal,
        true, // auto_repay
        b_idx,
        Some(fb.pool_address),
        true, // recipient_pool_repays
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
        true, // recipient_pool_repays
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
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal,
        },
        outer,
    ];
    Some((plan, at))
}

/// A V2 hop's facts (shared by the V2-involving shape derivers).
pub(crate) fn v2_hop_facts(h: &V2HopInfo) -> HopFacts {
    HopFacts {
        prot: Prot::V2,
        zfo: h.zfo,
        swap_fee: h.fee,
        tick_spacing: 0,
        out_currency: v2_forward(h),
        in_currency: if h.zfo {
            h.token0_address
        } else {
            h.token1_address
        },
        out_dest: OutDest::Executor,
        repay: Repay::SelfRefund,
        pool_address: h.pool_address,
        pool_id_hex: None,
        currency0_address: h.token0_address,
        currency1_address: h.token1_address,
    }
}

/// The 3-hop V3→V3→V2 facts (`v3v3v2`).
pub(crate) fn facts_of_v3v3v2(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V3(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v3_hop_facts(a), v3_hop_facts(b), v2_hop_facts(c)])
}

/// The 3-hop V3→V3→V2 shape: the MIDDLE V3 flash is the OUTERMOST (recipient =
/// the V2's pool, rpr=false); hop0 nests inside it (recipient = hop1's pool,
/// rpr=true) with a V2SwapCalc terminal + WETH self-repay callback.
pub(crate) fn derive_3hop_v3v3v2(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V3
        || facts[1].prot != Prot::V3
        || facts[2].prot != Prot::V2
    {
        return None;
    }
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

    // innermost (hop0) first: its callback carries the V2SwapCalc terminal + repay.
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
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal,
        },
        outer_b,
    ];
    Some((plan, at))
}

/// The 3-hop V3→V2→V3 facts (`v3v2v3`).
pub(crate) fn facts_of_v3v2v3(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V2(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v3_hop_facts(a), v2_hop_facts(b), v3_hop_facts(c)])
}

/// The 3-hop V3→V2→V3 shape: the TERMINAL V3 flash is the OUTERMOST (SELF);
/// hop0 nests inside it (recipient = the V2's pool, rpr=false) with a V2SwapCalc
/// seeded to the terminal (repays) + WETH self-repay callback.
pub(crate) fn derive_3hop_v3v2v3(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V3
        || facts[1].prot != Prot::V2
        || facts[2].prot != Prot::V3
    {
        return None;
    }
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
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal,
        },
        outer_c,
    ];
    Some((plan, at))
}

/// The 3-hop V3→V2→V2 facts (`v3v2v2`).
pub(crate) fn facts_of_v3v2v2(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V2(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v3_hop_facts(a), v2_hop_facts(b), v2_hop_facts(c)])
}

/// The 3-hop V3→V2→V2 shape: the leading V3 flash (recipient = the first V2's
/// pool, rpr=false) whose callback is a V2SwapCalc chain to SELF + WETH repay.
pub(crate) fn derive_3hop_v3v2v2(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V3
        || facts[1].prot != Prot::V2
        || facts[2].prot != Prot::V2
    {
        return None;
    }
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
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal,
        },
        outer,
    ];
    Some((plan, at))
}

/// The 3-hop V2→V2→V3 facts (`v2v2v3`).
pub(crate) fn facts_of_v2v2v3(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V2(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v2_hop_facts(a), v2_hop_facts(b), v3_hop_facts(c)])
}

/// The 3-hop V2→V2→V3 shape: the TERMINAL V3 flash (SELF) whose callback is a
/// WETH prefund + a V2SwapCalc chain to the terminal (hop1 repays it).
pub(crate) fn derive_3hop_v2v2v3(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V2
        || facts[1].prot != Prot::V2
        || facts[2].prot != Prot::V3
    {
        return None;
    }
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
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal,
        },
        outer_c,
    ];
    Some((plan, at))
}

/// The 3-hop V2→V3→V3 facts (`v2v3v3`).
pub(crate) fn facts_of_v2v3v3(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V3(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v2_hop_facts(a), v3_hop_facts(b), v3_hop_facts(c)])
}

/// The 3-hop V2→V3→V3 shape: the TERMINAL V3 flash (SELF) whose callback nests
/// hop1 (recipient = terminal pool, rpr=true) with a WETH prefund + V2SwapDirect.
pub(crate) fn derive_3hop_v2v3v3(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V2
        || facts[1].prot != Prot::V3
        || facts[2].prot != Prot::V3
    {
        return None;
    }
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
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal,
        },
        outer_c,
    ];
    Some((plan, at))
}

/// The 3-hop V2→V3→V2 facts (`v2v3v2`).
pub(crate) fn facts_of_v2v3v2(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V3(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v2_hop_facts(a), v3_hop_facts(b), v2_hop_facts(c)])
}

/// The 3-hop V2→V3→V2 shape: the TERMINAL V2 flash (SELF) whose callback nests
/// hop1 (recipient = terminal pool, rpr=true) with a WETH prefund + V2SwapDirect.
pub(crate) fn derive_3hop_v2v3v2(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V2
        || facts[1].prot != Prot::V3
        || facts[2].prot != Prot::V2
    {
        return None;
    }
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
    let _ = at.add(fa.out_currency).ok()?; // fwd_a (order-preserving, per gold)
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
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal,
        },
        outer_c,
    ];
    Some((plan, at))
}

/// A V4 hop's facts (shared by the V4-crossing shape derivers).
pub(crate) fn v4_hop_facts(h: &V4HopInfo) -> HopFacts {
    let (fwd, inv) = v4_hop_currencies(h);
    HopFacts {
        prot: Prot::V4,
        zfo: h.zfo,
        swap_fee: u16::try_from(h.fee).unwrap_or(0),
        tick_spacing: i16::try_from(h.tick_spacing).unwrap_or(0),
        out_currency: fwd,
        in_currency: inv,
        out_dest: OutDest::Executor,
        repay: Repay::Offstream,
        pool_address: h.pool_manager_address,
        pool_id_hex: Some(h.pool_id_hex.clone()),
        currency0_address: h.currency0_address,
        currency1_address: h.currency1_address,
    }
}

/// The 2-hop V4→V4 facts (`v4v4`).
pub(crate) fn facts_of_v4v4(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 2 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V4(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    Some(vec![v4_hop_facts(a), v4_hop_facts(b)])
}

/// The 2-hop V4→V4 shape: a single `V4Unlock` whose inner ledger is the
/// native-bridge (gap) / individual-swap / batch / capture slaughterhouse
/// shared by every pure-V4 cross. As with the gold, all opts-driven branches
/// are reproduced from `inputs.opts`; the facts carry only per-hop V4 metadata.
/// The 2-hop V4→V2 facts (`v4v2`).
pub(crate) fn facts_of_v4v2(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 2 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V2(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    Some(vec![v4_hop_facts(a), v2_hop_facts(b)])
}

/// The 2-hop V4→V2 shape (boundary-seed): a `V4Unlock` that runs the V4 swap,
/// seeds the V2 pool (prefund style), and runs a `V2SwapCalc` to SELF. The
/// outer `SelfFund`/`V4Unlock` wrap depends on the terminal output currency.
/// The 3-hop V4→V4→V4 facts (`v4v4v4`).
pub(crate) fn facts_of_v4v4v4(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V4(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v4_hop_facts(a), v4_hop_facts(b), v4_hop_facts(c)])
}

/// The 3-hop V4→V4→V4 shape: a single `V4Unlock` of three V4 swaps connected
/// by optional native bridges + the shared terminal-capture steps. As with the
/// gold, the bridge/capture/batch arms reuse the shared `v4_bridge_steps` and
/// `v4_terminal_capture_steps` helpers; the facts carry only per-hop metadata.
#[expect(clippy::too_many_lines)]
#[expect(clippy::similar_names)]
pub(crate) fn derive_3hop_v4v4v4(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3 || facts.iter().any(|f| f.prot != Prot::V4) {
        return None;
    }
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
}

/// The 2-hop V2→V4 facts (`v2v4`).
pub(crate) fn facts_of_v2v4(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 2 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V4(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    Some(vec![v2_hop_facts(a), v4_hop_facts(b)])
}

/// The 2-hop V2→V4 shape: dispatch over the three V4-input/output arms (as
/// `build_v2v4_walk`), each a V2 flash whose callback V4-unlocks the V4 swap.
pub(crate) fn derive_2hop_v2v4(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 2 || facts[0].prot != Prot::V2 || facts[1].prot != Prot::V4 {
        return None;
    }
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
        derive_2hop_v2v4_native_output(fa, fb, v4_in_currency, inputs)
    } else if v4_in_currency == NATIVE_CURRENCY_ADDRESS {
        derive_2hop_v2v4_native_input(fa, fb, v4_out_currency, inputs)
    } else {
        derive_2hop_v2v4_erc20(fa, fb, v4_in_currency, v4_out_currency, inputs)
    }
}

/// `v2_v4` ERC-20 V4 input branch (V2 flash; byte-identical to `build_v2v4_erc20_input_walk`).
#[expect(clippy::too_many_lines)]
pub(crate) fn derive_2hop_v2v4_erc20(
    fa: &HopFacts,
    fb: &HopFacts,
    v4_in_currency: Address,
    v4_out_currency: Address,
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
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

/// `v2_v4` native V4 output branch (wrap-and-repay; `build_v2v4_native_output_walk`).
#[expect(clippy::too_many_lines)]
pub(crate) fn derive_2hop_v2v4_native_output(
    fa: &HopFacts,
    fb: &HopFacts,
    v4_in_currency: Address,
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
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

/// `v2_v4` native V4 input branch (byte-identical to `build_v2v4_native_input_walk`).
pub(crate) fn derive_2hop_v2v4_native_input(
    fa: &HopFacts,
    fb: &HopFacts,
    v4_out_currency: Address,
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
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

/// The 2-hop V3→V4 facts (`v3v4`).
pub(crate) fn facts_of_v3v4(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 2 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V4(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    Some(vec![v3_hop_facts(a), v4_hop_facts(b)])
}

/// The 2-hop V3→V4 shape (dispatch over the three V4-input/output arms, as the
/// gold's `build_v3v4_walk`): erc20-input / native-output / native-input. Each
/// arm runs the V3 flash whose callback V4-unlocks the V4 swap and repays the
/// V3 flash.
pub(crate) fn derive_2hop_v3v4(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 2 || facts[0].prot != Prot::V3 || facts[1].prot != Prot::V4 {
        return None;
    }
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
        derive_2hop_v3v4_native_output(fa, fb, v4_in_currency, inputs)
    } else if v4_in_currency == NATIVE_CURRENCY_ADDRESS {
        derive_2hop_v3v4_native_input(fa, fb, v4_out_currency, inputs)
    } else {
        derive_2hop_v3v4_erc20(fa, fb, v4_in_currency, v4_out_currency, inputs)
    }
}

/// `v3_v4` ERC-20 V4 input branch (facts-driven, byte-identical to the gold's
/// `build_v3v4_erc20_input_walk`).
#[expect(clippy::too_many_lines)]
pub(crate) fn derive_2hop_v3v4_erc20(
    fa: &HopFacts,
    fb: &HopFacts,
    v4_in_currency: Address,
    v4_out_currency: Address,
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
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

/// `v3_v4` native V4 output branch (byte-identical to `build_v3v4_native_output_walk`).
#[expect(clippy::too_many_lines)]
pub(crate) fn derive_2hop_v3v4_native_output(
    fa: &HopFacts,
    fb: &HopFacts,
    v4_in_currency: Address,
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
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

/// `v3_v4` native V4 input branch (byte-identical to `build_v3v4_native_input_walk`).
pub(crate) fn derive_2hop_v3v4_native_input(
    fa: &HopFacts,
    fb: &HopFacts,
    v4_out_currency: Address,
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
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

#[expect(clippy::too_many_lines)]
pub(crate) fn derive_2hop_v4v2(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 2 || facts[0].prot != Prot::V4 || facts[1].prot != Prot::V2 {
        return None;
    }
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
        {}
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
    Some((plan, at))
}

#[expect(clippy::too_many_lines)]
pub(crate) fn derive_2hop_v4v4(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 2 || facts[0].prot != Prot::V4 || facts[1].prot != Prot::V4 {
        return None;
    }
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

/// The 2-hop V4→V3 facts (`v4v3`).
pub(crate) fn facts_of_v4v3(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 2 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V3(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    Some(vec![v4_hop_facts(a), v3_hop_facts(b)])
}

/// The 2-hop V4→V3 shape (boundary-take): a `V4Unlock` that runs the V4 swap,
/// takes its forward out of the PM, and hands it to a V3 flash (auto-repay),
/// then settles the PM ledger. As with the gold, the native/output/input arms
/// are read from the hop currencies + `inputs.opts`; `fb.in_currency` is NOT
/// the flash's input (it is weth on the native arm, else a.out) — so the
/// `FlashSwap` is assembled here directly rather than via a fixed-in-currency
/// flash mechanic.
#[expect(clippy::too_many_lines)]
pub(crate) fn derive_2hop_v4v3(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 2 || facts[0].prot != Prot::V4 || facts[1].prot != Prot::V3 {
        return None;
    }
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

/// The 3-hop V4-crossing shape dispatcher: routes the 18 T7 families to their
/// per-shape derivers by the exact (a,b,c) protocol triple.
/// The 3-hop V4→V2→V2 facts (`v4v2v2`).
pub(crate) fn facts_of_v4v2v2(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V2(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v4_hop_facts(a), v2_hop_facts(b), v2_hop_facts(c)])
}

/// The 3-hop V4→V2→V2 shape: a `V4Unlock` that runs the V4 swap, takes its
/// forward V4 output directly into the first V2 pool, then a V2SwapCalc chain
/// to SELF, settling the PM's WETH delta.
pub(crate) fn derive_3hop_v4v2v2(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V4
        || facts[1].prot != Prot::V2
        || facts[2].prot != Prot::V2
    {
        return None;
    }
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
}

/// The 3-hop V4→V2→V4 facts (`v4v2v4`).
pub(crate) fn facts_of_v4v2v4(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V2(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v4_hop_facts(a), v2_hop_facts(b), v4_hop_facts(c)])
}

/// The 3-hop V4→V2→V4 shape: a `V4Unlock` running V4Swap → V2SwapCalc → V4Swap
/// (the V2 forward handed to SELF between the two V4 swaps).
pub(crate) fn derive_3hop_v4v2v4(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V4
        || facts[1].prot != Prot::V2
        || facts[2].prot != Prot::V4
    {
        return None;
    }
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
}

/// The 3-hop V4→V3→V3 facts (`v4v3v3`).
pub(crate) fn facts_of_v4v3v3(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V3(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v4_hop_facts(a), v3_hop_facts(b), v3_hop_facts(c)])
}

/// The 3-hop V4→V3→V3 shape: a `V4Unlock` running the V4 swap whose forward is
/// handed to a nested V3-flash pair (the inner V3 flash is repaid by the V4
/// take-compact, and repays the outer one) — `v4_v3_v3`'s boundary-take.
pub(crate) fn derive_3hop_v4v3v3(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V4
        || facts[1].prot != Prot::V3
        || facts[2].prot != Prot::V3
    {
        return None;
    }
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
}

/// The 3-hop V4→V3→V4 facts (`v4v3v4`).
pub(crate) fn facts_of_v4v3v4(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V3(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v4_hop_facts(a), v3_hop_facts(b), v4_hop_facts(c)])
}

/// The 3-hop V4→V3→V4 shape: a `V4Unlock` running the V4 swap, a V3 flash that
/// repays the PM (its forward settled into the ledger), and a closing V4 swap.
#[expect(clippy::too_many_lines)]
pub(crate) fn derive_3hop_v4v3v4(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V4
        || facts[1].prot != Prot::V3
        || facts[2].prot != Prot::V4
    {
        return None;
    }
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
}

/// The 3-hop V4→V4→V2 facts (`v4v4v2`).
pub(crate) fn facts_of_v4v4v2(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V4(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v4_hop_facts(a), v4_hop_facts(b), v2_hop_facts(c)])
}

/// The 3-hop V4→V4→V2 shape: a `V4Unlock` of two V4 swaps whose second forward
/// is taken directly into the V2 pool and swapped to SELF.
#[expect(clippy::similar_names)]
pub(crate) fn derive_3hop_v4v4v2(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V4
        || facts[1].prot != Prot::V4
        || facts[2].prot != Prot::V2
    {
        return None;
    }
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
}

/// The 3-hop V4→V4→V3 facts (`v4v4v3`).
pub(crate) fn facts_of_v4v4v3(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V4(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v4_hop_facts(a), v4_hop_facts(b), v3_hop_facts(c)])
}

/// The 3-hop V4→V4→V3 shape: a `V4Unlock` of two V4 swaps whose second forward
/// feeds a V3 flash (repaid by taking that forward out of the PM ledger).
#[expect(clippy::too_many_lines)]
#[expect(clippy::similar_names)]
pub(crate) fn derive_3hop_v4v4v3(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V4
        || facts[1].prot != Prot::V4
        || facts[2].prot != Prot::V3
    {
        return None;
    }
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
}

/// The 3-hop V4→V2→V3 facts (`v4v2v3`).
pub(crate) fn facts_of_v4v2v3(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V2(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v4_hop_facts(a), v2_hop_facts(b), v3_hop_facts(c)])
}

/// The 3-hop V4→V2→V3 shape: a V3 (terminal) flash whose callback V4-unlocks
/// the V4 swap + V2SwapCalc chain, the V2 forward repaying the V3 flash.
pub(crate) fn derive_3hop_v4v2v3(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V4
        || facts[1].prot != Prot::V2
        || facts[2].prot != Prot::V3
    {
        return None;
    }
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
}

/// The 3-hop V4→V3→V2 facts (`v4v3v2`).
pub(crate) fn facts_of_v4v3v2(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V3(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v4_hop_facts(a), v3_hop_facts(b), v2_hop_facts(c)])
}

/// The 3-hop V4→V3→V2 shape: a `V4Unlock` running the V4 swap, then a V3 flash
/// (its in-poison the V4 forward) whose callback takes that forward out of the
/// PM to repay the V3 flash and runs a V2SwapCalc to SELF.
pub(crate) fn derive_3hop_v4v3v2(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V4
        || facts[1].prot != Prot::V3
        || facts[2].prot != Prot::V2
    {
        return None;
    }
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
}

/// The 3-hop V2→V2→V4 facts (`v2v2v4`).
pub(crate) fn facts_of_v2v2v4(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V2(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v2_hop_facts(a), v2_hop_facts(b), v4_hop_facts(c)])
}

/// The 3-hop V2→V2→V4 shape: a WETH-funded V4Unlock that seeds V2 pool a,
/// runs the V2SwapCalc chain into the PM, then the closing V4 swap.
pub(crate) fn derive_3hop_v2v2v4(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V2
        || facts[1].prot != Prot::V2
        || facts[2].prot != Prot::V4
    {
        return None;
    }
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
}

/// The 3-hop V2→V4→V4 facts (`v2v4v4`).
pub(crate) fn facts_of_v2v4v4(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V4(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v2_hop_facts(a), v4_hop_facts(b), v4_hop_facts(c)])
}

/// The 3-hop V2→V4→V4 shape: a WETH-funded V4Unlock that seeds V2 pool a (its
/// forward entering the PM), then two V4 swaps.
#[expect(clippy::similar_names)]
pub(crate) fn derive_3hop_v2v4v4(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V2
        || facts[1].prot != Prot::V4
        || facts[2].prot != Prot::V4
    {
        return None;
    }
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
}

/// The 3-hop V2→V3→V4 facts (`v2v3v4`).
pub(crate) fn facts_of_v2v3v4(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V3(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v2_hop_facts(a), v3_hop_facts(b), v4_hop_facts(c)])
}

/// The 3-hop V2→V3→V4 shape: a V3 flash repaid by the PM (its callback V4-unlocks
/// the closing V4 swap) and by a V2SwapDirect from pool a.
pub(crate) fn derive_3hop_v2v3v4(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V2
        || facts[1].prot != Prot::V3
        || facts[2].prot != Prot::V4
    {
        return None;
    }
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
        mechanics::v2_swap_direct(&mut at, fa, out_a, fwd_a, v3b, Some(fb.pool_address), true)?,
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

/// The 3-hop V3→V2→V4 facts (`v3v2v4`).
pub(crate) fn facts_of_v3v2v4(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V2(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v3_hop_facts(a), v2_hop_facts(b), v4_hop_facts(c)])
}

/// The 3-hop V3→V2→V4 shape: a WETH-funded V3 flash whose callback runs a V2
/// flash (auto-repay) whose callback V4-unlocks the closing V4 swap, and repays
/// the V3 flash from WETH.
#[expect(clippy::too_many_lines)]
pub(crate) fn derive_3hop_v3v2v4(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V3
        || facts[1].prot != Prot::V2
        || facts[2].prot != Prot::V4
    {
        return None;
    }
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
}

/// The 3-hop V3→V3→V4 facts (`v3v3v4`).
pub(crate) fn facts_of_v3v3v4(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V3(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v3_hop_facts(a), v3_hop_facts(b), v4_hop_facts(c)])
}

/// The 3-hop V3→V3→V4 shape: a V3 flash repaid by the PM whose callback nests
/// a V3 flash repaid by the first, whose callback V4-unlocks the closing swap.
pub(crate) fn derive_3hop_v3v3v4(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V3
        || facts[1].prot != Prot::V3
        || facts[2].prot != Prot::V4
    {
        return None;
    }
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
}

/// The 3-hop V2→V4→V2 facts (`v2v4v2`).
pub(crate) fn facts_of_v2v4v2(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V4(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v2_hop_facts(a), v4_hop_facts(b), v2_hop_facts(c)])
}

/// The 3-hop V2→V4→V2 shape: a WETH-funded terminal V2 flash whose callback seeds
/// V2 pool a and V4-unlocks the middle V4 swap (the V4 forward repaying the V2
/// flash).
/// The 3-hop V2→V4→V3 facts (`v2v4v3`).
pub(crate) fn facts_of_v2v4v3(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V4(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v2_hop_facts(a), v4_hop_facts(b), v3_hop_facts(c)])
}

/// The 3-hop V2→V4→V3 shape: a WETH-funded terminal V3 flash whose callback seeds
/// V2 pool a and V4-unlocks the middle V4 swap (the V4 forward repaying the V3
/// flash).
#[expect(clippy::too_many_lines)]
#[expect(clippy::similar_names)]
pub(crate) fn derive_3hop_v2v4v3(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V2
        || facts[1].prot != Prot::V4
        || facts[2].prot != Prot::V3
    {
        return None;
    }
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
}

#[expect(clippy::similar_names)]
pub(crate) fn derive_3hop_v2v4v2(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V2
        || facts[1].prot != Prot::V4
        || facts[2].prot != Prot::V2
    {
        return None;
    }
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
}

/// The 3-hop V3→V4→V2 facts (`v3v4v2`).
pub(crate) fn facts_of_v3v4v2(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V4(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v3_hop_facts(a), v4_hop_facts(b), v2_hop_facts(c)])
}

/// The 3-hop V3→V4→V2 shape: a WETH-funded V3 flash repaid by the PM whose
/// callback V4-unlocks the middle V4 swap, runs a V2SwapCalc to SELF, and repays
/// the V3 flash from WETH.
pub(crate) fn derive_3hop_v3v4v2(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V3
        || facts[1].prot != Prot::V4
        || facts[2].prot != Prot::V2
    {
        return None;
    }
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
}

/// The 3-hop V3→V4→V4 facts (`v3v4v4`).
pub(crate) fn facts_of_v3v4v4(
    path: &PathInfo,
    _inputs: &ComposerInputs<'_>,
) -> Option<Vec<HopFacts>> {
    if path.hops.len() != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V4(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    Some(vec![v3_hop_facts(a), v4_hop_facts(b), v4_hop_facts(c)])
}

/// The 3-hop V3→V4→V4 shape: a WETH-funded V3 flash repaid by the PM whose
/// callback V4-unlocks two V4 swaps and takes the terminal WETH delta.
#[expect(clippy::too_many_lines)]
pub(crate) fn derive_3hop_v3v4v4(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3
        || facts[0].prot != Prot::V3
        || facts[1].prot != Prot::V4
        || facts[2].prot != Prot::V4
    {
        return None;
    }
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
}

pub(crate) fn derive_3hop_v4cross(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 3 {
        return None;
    }
    match (facts[0].prot, facts[1].prot, facts[2].prot) {
        (Prot::V4, Prot::V4, Prot::V4) => derive_3hop_v4v4v4(facts, inputs),
        (Prot::V4, Prot::V2, Prot::V2) => derive_3hop_v4v2v2(facts, inputs),
        (Prot::V4, Prot::V2, Prot::V4) => derive_3hop_v4v2v4(facts, inputs),
        (Prot::V4, Prot::V3, Prot::V3) => derive_3hop_v4v3v3(facts, inputs),
        (Prot::V4, Prot::V3, Prot::V4) => derive_3hop_v4v3v4(facts, inputs),
        (Prot::V4, Prot::V4, Prot::V2) => derive_3hop_v4v4v2(facts, inputs),
        (Prot::V4, Prot::V4, Prot::V3) => derive_3hop_v4v4v3(facts, inputs),
        (Prot::V4, Prot::V2, Prot::V3) => derive_3hop_v4v2v3(facts, inputs),
        (Prot::V4, Prot::V3, Prot::V2) => derive_3hop_v4v3v2(facts, inputs),
        (Prot::V2, Prot::V2, Prot::V4) => derive_3hop_v2v2v4(facts, inputs),
        (Prot::V2, Prot::V4, Prot::V4) => derive_3hop_v2v4v4(facts, inputs),
        (Prot::V2, Prot::V3, Prot::V4) => derive_3hop_v2v3v4(facts, inputs),
        (Prot::V3, Prot::V2, Prot::V4) => derive_3hop_v3v2v4(facts, inputs),
        (Prot::V3, Prot::V3, Prot::V4) => derive_3hop_v3v3v4(facts, inputs),
        (Prot::V2, Prot::V4, Prot::V2) => derive_3hop_v2v4v2(facts, inputs),
        (Prot::V2, Prot::V4, Prot::V3) => derive_3hop_v2v4v3(facts, inputs),
        (Prot::V3, Prot::V4, Prot::V2) => derive_3hop_v3v4v2(facts, inputs),
        (Prot::V3, Prot::V4, Prot::V4) => derive_3hop_v3v4v4(facts, inputs),
        _ => None, // T7: remaining 3-hop V4-crossing shapes arrive as migrated
    }
}

pub(crate) fn derive_2hop_v3x(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    if facts.len() != 2 || facts[0].prot != Prot::V3 {
        return None;
    }
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

    // Build the terminal step first (releases the `at` borrow), then the
    // leading flash whose callback nests it.
    let mut callback: Plan = vec![PlanStep::Erc20Transfer {
        token_idx: SENTINEL_WETH,
        token_addr: weth,
        recipient_idx: a_idx,
        amount: optimal_input,
        seeds_pool: None,
        repays_flash: Some(fa.pool_address),
    }];
    if fb.prot == Prot::V2 {
        // `v3v2`: seed the V2 with the forward, then the V2SwapCalc terminal.
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
        // `v3v3`: a nested V3 terminal flash with auto_repay.
        let b_swap_in = *inputs.consumed_inputs.get(1)?;
        if !fits_i128(b_swap_in) {
            return None;
        }
        let terminal = mechanics::v3_flash(&mut at, fb, terminal_out, b_swap_in, true, vec![])?;
        callback.push(terminal);
    }

    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        mechanics::v3_flash(&mut at, fa, forward_out, optimal_input, false, callback)?,
    ];
    Some((plan, at))
}

/// Build the `v3_v4_v3` Plan from hop facts + inputs, deriving the enclosure
/// (which flash wraps which, the V4 unlock, the repayment order).
///
/// Returns `None` on a decline; a produced Plan is guaranteed validator-safe by
/// the shared `LedgerValidator` gate (ADR-030 Reject path).
#[must_use]
/// The `v3_v4_v3` family — a thin delegate to the generic facts-driven
/// deriver (ADR-031 D3/D6). The enclosure (which FlashSwap/V4Unlock wraps
/// which, the repayment order) is DERIVED from the `Repay`/`OutDest` facts by
/// [`derive_plan`], not authored here.
pub fn build_v3v4v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    // 3 hops, exactly one NetZero V4 middle, exactly one SelfRefund leading
    // + one Offstream terminal — the single-V4-middle enclosure shape.
    let facts = facts_of_v3v4v3(path, inputs)?;
    let (plan, at) = derive_plan(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The facts-driven walker wired through the shared validator gate (one
/// representation): build the `v3_v4_v3` Plan from facts, gate it, fold
/// `preamble + plan_to_bytes`. `None` on a routine decline.
#[must_use]
pub fn derive_v3v4v3_walk(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let (preamble, plan, at) = build_v3v4v3_walk(path, inputs)?;
    let ops = plan_to_ledger_ops(&plan);
    let mut v = crate::grammar_ledger::LedgerValidator::default();
    v.validate_full(&ops).ok()?;
    let mut out = preamble;
    out.extend_from_slice(&crate::grammar_plan::plan_to_bytes(&plan, &at));
    Some(out)
}

/// The funding-axis dispatch (a V2/V3 stream-varying axis), matching
/// `funding_branch` in `grammar_shape`. The walker derives the funding branch
/// from the funding axis value rather than hand-authoring it per family.
/// Build the any-N (≥2) all-V2 chain Plan from hop facts + inputs, deriving
/// the enclosure (the funding branch + the `V2SwapCalc` walk) — byte-identical
/// to `build_all_v2_chain` (ADR-031; the `walk` feature).
// The `guard_arity` helper was decommissioned by the facts-driven T4–T7
// migration (epic 6SU5LM): every pure + V4-crossing family now delegates to
// `derive_plan`, whose per-shape derivers inline the arity checks.
/// The any-N all-V2 family — a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_all_v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_all_v2(path, inputs)?;
    let (plan, at) = derive_all_v2_chain(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
#[must_use]
pub fn build_v2v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v2v3(path, inputs)?;
    let (plan, at) = derive_2hop_v2v3(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
#[must_use]
pub fn build_v3v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v2(path, inputs)?;
    let (plan, at) = derive_2hop_v3x(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 2-hop V3→V3 Plan (SelfFund), byte-identical to `build_v3v3_plan`.
#[must_use]
/// The 2-hop V3→V3 family — a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
pub fn build_v3v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v3(path, inputs)?;
    let (plan, at) = derive_2hop_v3x(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 2-hop V4→V4 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v4v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v4v4(path, inputs)?;
    let (plan, at) = derive_2hop_v4v4(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 2-hop V4→V3 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v4v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v4v3(path, inputs)?;
    let (plan, at) = derive_2hop_v4v3(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 2-hop V4→V2 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v4v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v4v2(path, inputs)?;
    let (plan, at) = derive_2hop_v4v2(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V4→V4 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v4v4v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v4v4v4(path, inputs)?;
    let (plan, at) = derive_3hop_v4v4v4(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V2→V2 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v4v2v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v4v2v2(path, inputs)?;
    let (plan, at) = derive_3hop_v4v2v2(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V2→V4 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v4v2v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v4v2v4(path, inputs)?;
    let (plan, at) = derive_3hop_v4v2v4(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V3→V3 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v4v3v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v4v3v3(path, inputs)?;
    let (plan, at) = derive_3hop_v4v3v3(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V3→V4 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v4v3v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v4v3v4(path, inputs)?;
    let (plan, at) = derive_3hop_v4v3v4(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
#[must_use]
pub fn build_v3v2v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v2v3(path, inputs)?;
    let (plan, at) = derive_3hop_v3v2v3(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
#[must_use]
pub fn build_v3v3v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v3v2(path, inputs)?;
    let (plan, at) = derive_3hop_v3v3v2(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
#[must_use]
pub fn build_v3v3v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v3v3(path, inputs)?;
    let (plan, at) = derive_3hop_v3v3v3(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V2→V2→V3 family — a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v2v2v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v2v2v3(path, inputs)?;
    let (plan, at) = derive_3hop_v2v2v3(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 2-hop V3→V4 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v3v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v4(path, inputs)?;
    let (plan, at) = derive_2hop_v3v4(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 2-hop V2→V4 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v2v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v2v4(path, inputs)?;
    let (plan, at) = derive_2hop_v2v4(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V4→V2 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v4v4v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v4v4v2(path, inputs)?;
    let (plan, at) = derive_3hop_v4v4v2(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V4→V3 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v4v4v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v4v4v3(path, inputs)?;
    let (plan, at) = derive_3hop_v4v4v3(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V2→V3 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v4v2v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v4v2v3(path, inputs)?;
    let (plan, at) = derive_3hop_v4v2v3(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V3→V2 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v4v3v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v4v3v2(path, inputs)?;
    let (plan, at) = derive_3hop_v4v3v2(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V2→V2→V4 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v2v2v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v2v2v4(path, inputs)?;
    let (plan, at) = derive_3hop_v2v2v4(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V2→V4→V4 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v2v4v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v2v4v4(path, inputs)?;
    let (plan, at) = derive_3hop_v2v4v4(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
#[must_use]
pub fn build_v2v3v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v2v3v2(path, inputs)?;
    let (plan, at) = derive_3hop_v2v3v2(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
/// The 3-hop V2→V3→V3 family — a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v2v3v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v2v3v3(path, inputs)?;
    let (plan, at) = derive_3hop_v2v3v3(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V2→V3→V4 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v2v3v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v2v3v4(path, inputs)?;
    let (plan, at) = derive_3hop_v2v3v4(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
#[must_use]
pub fn build_v3v2v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v2v2(path, inputs)?;
    let (plan, at) = derive_3hop_v3v2v2(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V3→V2→V4 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v3v2v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v2v4(path, inputs)?;
    let (plan, at) = derive_3hop_v3v2v4(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V3→V3→V4 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v3v3v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v3v4(path, inputs)?;
    let (plan, at) = derive_3hop_v3v3v4(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V2→V4→V2 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v2v4v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v2v4v2(path, inputs)?;
    let (plan, at) = derive_3hop_v2v4v2(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V2→V4→V3 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v2v4v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v2v4v3(path, inputs)?;
    let (plan, at) = derive_3hop_v2v4v3(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V3→V4→V2 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v3v4v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v4v2(path, inputs)?;
    let (plan, at) = derive_3hop_v3v4v2(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V3→V4→V4 family - a thin delegate to the facts-driven deriver
/// (ADR-031 D3/D6).
#[must_use]
pub fn build_v3v4v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v4v4(path, inputs)?;
    let (plan, at) = derive_3hop_v3v4v4(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
/// The walk feature-flag dispatch: the same `(Prot, Prot, Option<Prot>)`
/// family keys as the reference producer, but every row routes to the
/// facts-driven walker builder (`build_*_walk`) instead of the hand-written
/// reference producer. Axes mirror the reference exactly (the walker itself
/// derives funding/capture from hop facts inside the build; the axes here keep
/// `family_axis_support` bit-identical to the reference so the walk/off
/// streams can't drift on the declared axis surface). When `walk` is on, both
/// `derive_shape_detailed` and `family_axis_support` inline `build_for_walk`
/// directly — no private re-export shell — with byte-parity to the reference
/// already pinned by the walker test suite.
#[must_use]
pub(crate) fn build_for_walk(
    key: (Option<Prot>, Option<Prot>, Option<Prot>),
) -> Option<crate::grammar_shape::BuildPlan> {
    Some(match key {
        // ── 3-hop families (27) ──
        (Some(Prot::V4), Some(Prot::V4), Some(Prot::V4)) => build_v4v4v4_walk,
        (Some(Prot::V4), Some(Prot::V2), Some(Prot::V2)) => build_v4v2v2_walk,
        (Some(Prot::V2), Some(Prot::V2), Some(Prot::V4)) => build_v2v2v4_walk,
        (Some(Prot::V2), Some(Prot::V3), Some(Prot::V4)) => build_v2v3v4_walk,
        (Some(Prot::V3), Some(Prot::V2), Some(Prot::V4)) => build_v3v2v4_walk,
        (Some(Prot::V3), Some(Prot::V3), Some(Prot::V4)) => build_v3v3v4_walk,
        (Some(Prot::V2), Some(Prot::V4), Some(Prot::V2)) => build_v2v4v2_walk,
        (Some(Prot::V2), Some(Prot::V4), Some(Prot::V3)) => build_v2v4v3_walk,
        (Some(Prot::V3), Some(Prot::V4), Some(Prot::V2)) => build_v3v4v2_walk,
        (Some(Prot::V3), Some(Prot::V4), Some(Prot::V3)) => build_v3v4v3_walk,
        (Some(Prot::V2), Some(Prot::V4), Some(Prot::V4)) => build_v2v4v4_walk,
        (Some(Prot::V3), Some(Prot::V4), Some(Prot::V4)) => build_v3v4v4_walk,
        (Some(Prot::V4), Some(Prot::V4), Some(Prot::V2)) => build_v4v4v2_walk,
        (Some(Prot::V4), Some(Prot::V4), Some(Prot::V3)) => build_v4v4v3_walk,
        (Some(Prot::V4), Some(Prot::V2), Some(Prot::V3)) => build_v4v2v3_walk,
        (Some(Prot::V4), Some(Prot::V2), Some(Prot::V4)) => build_v4v2v4_walk,
        (Some(Prot::V4), Some(Prot::V3), Some(Prot::V2)) => build_v4v3v2_walk,
        (Some(Prot::V4), Some(Prot::V3), Some(Prot::V3)) => build_v4v3v3_walk,
        (Some(Prot::V4), Some(Prot::V3), Some(Prot::V4)) => build_v4v3v4_walk,
        (Some(Prot::V2), Some(Prot::V2), Some(Prot::V2) | None) => build_all_v2_walk,
        (Some(Prot::V2), Some(Prot::V2), Some(Prot::V3)) => build_v2v2v3_walk,
        (Some(Prot::V2), Some(Prot::V3), Some(Prot::V2)) => build_v2v3v2_walk,
        (Some(Prot::V2), Some(Prot::V3), Some(Prot::V3)) => build_v2v3v3_walk,
        (Some(Prot::V3), Some(Prot::V2), Some(Prot::V2)) => build_v3v2v2_walk,
        (Some(Prot::V3), Some(Prot::V2), Some(Prot::V3)) => build_v3v2v3_walk,
        (Some(Prot::V3), Some(Prot::V3), Some(Prot::V2)) => build_v3v3v2_walk,
        (Some(Prot::V3), Some(Prot::V3), Some(Prot::V3)) => build_v3v3v3_walk,
        // ── 2-hop families (8; third slot `None`) ──
        (Some(Prot::V4), Some(Prot::V4), None) => build_v4v4_walk,
        (Some(Prot::V4), Some(Prot::V3), None) => build_v4v3_walk,
        (Some(Prot::V3), Some(Prot::V4), None) => build_v3v4_walk,
        (Some(Prot::V4), Some(Prot::V2), None) => build_v4v2_walk,
        (Some(Prot::V2), Some(Prot::V4), None) => build_v2v4_walk,
        (Some(Prot::V2), Some(Prot::V3), None) => build_v2v3_walk,
        (Some(Prot::V3), Some(Prot::V2), None) => build_v3v2_walk,
        (Some(Prot::V3), Some(Prot::V3), None) => build_v3v3_walk,
        _ => return None,
    })
}

/// PLACEHOLDER_REVERT_START T1 (v3_v4_v3)
// + T4 (the 2-hop V2/V3 + any-N all-V2) provide the descriptors the migration
// tasks consume; derive_plan dispatches on the shape they imply.
/// The generic enclosure-deriving walker (ADR-031 D3/D6).
///
/// Reads an arbitrary-length hop sequence's [`HopFacts`] and derives the
/// nesting (which `FlashSwap`/`V4Unlock` wraps which, and the repayment order)
/// from the `out_dest` + `repay` facts — NOT from hardcoded per-family indices.
///
/// T1 generalizes this from [`build_v3v4v3_walk`]; T0 stubs it `None` so the
/// D6 probe (`facts_driven_tests` below) compiles RED. Each migration task
/// (T4–T7) turns its family's probe row green.
#[must_use]
#[expect(clippy::too_many_lines)]
pub(crate) fn derive_plan(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    // ── Shape dispatch (D3/D6 — the enclosure shape is read from the facts).
    // All 35 families route through here: pure V2/V3 shapes first, then the
    // V4-crossing dispatcher, then the single-V4-middle fallthrough.
    if facts.len() >= 2
        && facts
            .iter()
            .all(|f| f.repay != Repay::NetZero && f.prot == Prot::V2)
    {
        return derive_all_v2_chain(facts, inputs);
    }
    if facts.len() == 2 && facts[0].prot == Prot::V3 && facts[1].prot == Prot::V4 {
        return derive_2hop_v3v4(facts, inputs);
    }
    if facts.len() == 2 && facts[0].prot == Prot::V4 {
        return match facts[1].prot {
            Prot::V4 => derive_2hop_v4v4(facts, inputs),
            Prot::V3 => derive_2hop_v4v3(facts, inputs),
            Prot::V2 => derive_2hop_v4v2(facts, inputs),
        };
    }
    if facts.len() == 3 && facts.iter().all(|f| f.repay != Repay::NetZero) {
        return match (facts[0].prot, facts[1].prot, facts[2].prot) {
            (Prot::V3, Prot::V3, Prot::V3) => derive_3hop_v3v3v3(facts, inputs),
            (Prot::V3, Prot::V3, Prot::V2) => derive_3hop_v3v3v2(facts, inputs),
            (Prot::V3, Prot::V2, Prot::V3) => derive_3hop_v3v2v3(facts, inputs),
            (Prot::V3, Prot::V2, Prot::V2) => derive_3hop_v3v2v2(facts, inputs),
            (Prot::V2, Prot::V2, Prot::V3) => derive_3hop_v2v2v3(facts, inputs),
            (Prot::V2, Prot::V3, Prot::V3) => derive_3hop_v2v3v3(facts, inputs),
            (Prot::V2, Prot::V3, Prot::V2) => derive_3hop_v2v3v2(facts, inputs),
            _ => derive_3hop_v4cross(facts, inputs),
        };
    }
    if facts.len() == 2 && facts[0].prot == Prot::V2 && facts[1].prot == Prot::V4 {
        return derive_2hop_v2v4(facts, inputs);
    }
    if facts.iter().all(|f| f.repay != Repay::NetZero) && facts.len() == 2 {
        match (facts[0].prot, facts[1].prot) {
            (Prot::V2, Prot::V3) => return derive_2hop_v2v3(facts, inputs),
            (Prot::V3, Prot::V2 | Prot::V3) => return derive_2hop_v3x(facts, inputs),
            _ => {}
        }
    }
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

#[cfg(test)]
mod facts_driven_tests {
    //! The D6 honesty invariant (epic `6SU5LM` / T0).
    //!
    //! For every family, the Plan produced by `build_<fam>_walk` must equal
    //! the Plan produced by the generic `derive_plan(&facts_of_<fam>(path),
    //! inputs)`. A family is D6-complete iff its row is un-ignored and green.
    //!
    //! Do NOT "fix" a failing row by relaxing the probe — the probe is the
    //! truth (same discipline as `honesty_invariant.rs`). The migration tasks
    //! (T4–T7) un-ignore their rows as they generalize `derive_plan` to each
    //! family cluster.

    #![allow(
        clippy::too_many_lines,
        clippy::similar_names,
        clippy::expect_used,
        clippy::panic,
        clippy::cast_possible_truncation
    )]

    use super::*;
    use crate::composers::EncodeOptions;
    use alloy::primitives::{address, Address};

    const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    const WBTC: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    const PM: Address = address!("000000000004444c5dc75cB358380D2e3De08A90");
    const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");

    static OPTIMAL: u128 = 1_000_000_000_000_000_000;
    static OUTS: [u128; 3] = [1_000_000_000_000_000_000; 3];
    static CONSUMED: [u128; 3] = [999_999_999_999_999_999; 3];

    fn make_hops(prots: &[Prot]) -> Vec<HopInfo> {
        (0..prots.len())
            .map(|i| {
                let (in_t, out_t) = (
                    match i % 3 {
                        0 => WETH,
                        1 => USDC,
                        _ => WBTC,
                    },
                    match (i + 1) % 3 {
                        0 => WETH,
                        1 => USDC,
                        _ => WBTC,
                    },
                );
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

    /// Probe: assert `derive_plan` + the family's facts descriptor produces
    /// the same Plan as the hand-authored `build_*_walk`.
    #[expect(clippy::type_complexity)]
    fn assert_facts_driven(
        prots: &[Prot],
        facts_of: Option<fn(&PathInfo, &ComposerInputs<'_>) -> Option<Vec<HopFacts>>>,
    ) {
        let path = PathInfo::new(make_hops(prots));
        let n = prots.len();
        let inputs = ComposerInputs {
            executor_address: EXECUTOR,
            pool_manager_address: PM,
            weth_address: WETH,
            optimal_input: OPTIMAL,
            hop_outputs: &OUTS[..n],
            consumed_inputs: &CONSUMED[..n],
            opts: EncodeOptions::default(),
        };
        let key = (
            prots.first().copied(),
            prots.get(1).copied(),
            prots.get(2).copied(),
        );
        let build = build_for_walk(key).expect("family has a builder");
        let built = build(&path, &inputs).expect("family encodes under fixture");
        let facts = facts_of.and_then(|f| f(&path, &inputs));
        let derived = facts.and_then(|f| derive_plan(&f, &inputs));
        assert_eq!(
            Some(built.1),
            derived.map(|(plan, _at)| plan),
            "D6 invariant broken: derive_plan does not produce the same Plan as build_*_walk"
        );
    }

    // ═════════════════════════════════════════════════════════════════════
    // The reference family (T1+T3 complete it)
    // ═════════════════════════════════════════════════════════════════════

    #[test]
    fn v3v4v3_facts_driven() {
        assert_facts_driven(&[Prot::V3, Prot::V4, Prot::V3], Some(facts_of_v3v4v3));
    }

    // ═════════════════════════════════════════════════════════════════════
    // T4: 2-hop + any-N V2/V3 families (4)
    // ═════════════════════════════════════════════════════════════════════

    #[test]
    fn v2v3_facts_driven() {
        assert_facts_driven(&[Prot::V2, Prot::V3], Some(facts_of_v2v3));
    }

    #[test]
    fn v3v2_facts_driven() {
        assert_facts_driven(&[Prot::V3, Prot::V2], Some(facts_of_v3v2));
    }

    #[test]
    fn v3v3_facts_driven() {
        assert_facts_driven(&[Prot::V3, Prot::V3], Some(facts_of_v3v3));
    }

    #[test]
    fn all_v2_facts_driven() {
        assert_facts_driven(&[Prot::V2, Prot::V2, Prot::V2], Some(facts_of_all_v2));
    }

    // ═════════════════════════════════════════════════════════════════════
    // T5: 3-hop pure V2/V3 families (7)
    // ═════════════════════════════════════════════════════════════════════

    #[test]
    fn v2v2v3_facts_driven() {
        assert_facts_driven(&[Prot::V2, Prot::V2, Prot::V3], Some(facts_of_v2v2v3));
    }

    #[test]
    fn v2v3v2_facts_driven() {
        assert_facts_driven(&[Prot::V2, Prot::V3, Prot::V2], Some(facts_of_v2v3v2));
    }

    #[test]
    fn v2v3v3_facts_driven() {
        assert_facts_driven(&[Prot::V2, Prot::V3, Prot::V3], Some(facts_of_v2v3v3));
    }

    #[test]
    fn v3v2v2_facts_driven() {
        assert_facts_driven(&[Prot::V3, Prot::V2, Prot::V2], Some(facts_of_v3v2v2));
    }

    #[test]
    fn v3v2v3_facts_driven() {
        assert_facts_driven(&[Prot::V3, Prot::V2, Prot::V3], Some(facts_of_v3v2v3));
    }

    #[test]
    fn v3v3v2_facts_driven() {
        assert_facts_driven(&[Prot::V3, Prot::V3, Prot::V2], Some(facts_of_v3v3v2));
    }

    #[test]
    fn v3v3v3_facts_driven() {
        assert_facts_driven(&[Prot::V3, Prot::V3, Prot::V3], Some(facts_of_v3v3v3));
    }

    // ═════════════════════════════════════════════════════════════════════
    // T6: 2-hop V4-crossing families (5)
    // ═════════════════════════════════════════════════════════════════════

    #[test]
    fn v4v4_facts_driven() {
        assert_facts_driven(&[Prot::V4, Prot::V4], Some(facts_of_v4v4));
    }

    #[test]
    fn v4v3_facts_driven() {
        assert_facts_driven(&[Prot::V4, Prot::V3], Some(facts_of_v4v3));
    }

    #[test]
    fn v4v2_facts_driven() {
        assert_facts_driven(&[Prot::V4, Prot::V2], Some(facts_of_v4v2));
    }

    #[test]
    fn v3v4_facts_driven() {
        assert_facts_driven(&[Prot::V3, Prot::V4], Some(facts_of_v3v4));
    }

    #[test]
    fn v2v4_facts_driven() {
        assert_facts_driven(&[Prot::V2, Prot::V4], Some(facts_of_v2v4));
    }

    // ═════════════════════════════════════════════════════════════════════
    // T7: 3-hop V4-crossing families (18, excl. v3v4v3 reference)
    // ═════════════════════════════════════════════════════════════════════

    #[test]
    fn v4v4v4_facts_driven() {
        assert_facts_driven(&[Prot::V4, Prot::V4, Prot::V4], Some(facts_of_v4v4v4));
    }

    #[test]
    fn v4v2v2_facts_driven() {
        assert_facts_driven(&[Prot::V4, Prot::V2, Prot::V2], Some(facts_of_v4v2v2));
    }

    #[test]
    fn v2v2v4_facts_driven() {
        assert_facts_driven(&[Prot::V2, Prot::V2, Prot::V4], Some(facts_of_v2v2v4));
    }

    #[test]
    fn v2v3v4_facts_driven() {
        assert_facts_driven(&[Prot::V2, Prot::V3, Prot::V4], Some(facts_of_v2v3v4));
    }

    #[test]
    fn v3v2v4_facts_driven() {
        assert_facts_driven(&[Prot::V3, Prot::V2, Prot::V4], Some(facts_of_v3v2v4));
    }

    #[test]
    fn v3v3v4_facts_driven() {
        assert_facts_driven(&[Prot::V3, Prot::V3, Prot::V4], Some(facts_of_v3v3v4));
    }

    #[test]
    fn v2v4v2_facts_driven() {
        assert_facts_driven(&[Prot::V2, Prot::V4, Prot::V2], Some(facts_of_v2v4v2));
    }

    #[test]
    fn v2v4v3_facts_driven() {
        assert_facts_driven(&[Prot::V2, Prot::V4, Prot::V3], Some(facts_of_v2v4v3));
    }

    #[test]
    fn v3v4v2_facts_driven() {
        assert_facts_driven(&[Prot::V3, Prot::V4, Prot::V2], Some(facts_of_v3v4v2));
    }

    #[test]
    fn v2v4v4_facts_driven() {
        assert_facts_driven(&[Prot::V2, Prot::V4, Prot::V4], Some(facts_of_v2v4v4));
    }

    #[test]
    fn v3v4v4_facts_driven() {
        assert_facts_driven(&[Prot::V3, Prot::V4, Prot::V4], Some(facts_of_v3v4v4));
    }

    #[test]
    fn v4v4v2_facts_driven() {
        assert_facts_driven(&[Prot::V4, Prot::V4, Prot::V2], Some(facts_of_v4v4v2));
    }

    #[test]
    fn v4v4v3_facts_driven() {
        assert_facts_driven(&[Prot::V4, Prot::V4, Prot::V3], Some(facts_of_v4v4v3));
    }

    #[test]
    fn v4v2v3_facts_driven() {
        assert_facts_driven(&[Prot::V4, Prot::V2, Prot::V3], Some(facts_of_v4v2v3));
    }

    #[test]
    fn v4v2v4_facts_driven() {
        assert_facts_driven(&[Prot::V4, Prot::V2, Prot::V4], Some(facts_of_v4v2v4));
    }

    #[test]
    fn v4v3v2_facts_driven() {
        assert_facts_driven(&[Prot::V4, Prot::V3, Prot::V2], Some(facts_of_v4v3v2));
    }

    #[test]
    fn v4v3v3_facts_driven() {
        assert_facts_driven(&[Prot::V4, Prot::V3, Prot::V3], Some(facts_of_v4v3v3));
    }

    #[test]
    fn v4v3v4_facts_driven() {
        assert_facts_driven(&[Prot::V4, Prot::V3, Prot::V4], Some(facts_of_v4v3v4));
    }
}
