//! ADR-031 walker spike (feature `walk`).
//!
//! A facts-driven Plan walker proof: the hardest family — `v3_v4_v3` (a
//! 3-level `V3c→V3a→V4_UNLOCK` nesting) — is expressed as per-protocol
//! **hop facts** (data) + per-protocol **mechanics** (code) + one walker that
//! **derives the enclosure** (which `FlashSwap`/`V4Unlock` wraps which, and
//! the repayment order) from those facts, byte-identical to the hand-authored
//! `build_v3v4v3_plan`.
//!
//! This is the smallest proof that the ADR-031 hybrid expresses the worst
//! enclosure before A2 generalizes to every family. `plan_to_bytes` and the
//! `LedgerValidator` gate are reused unchanged (one representation): the
//! walker emits exactly one `Plan`, and the encoder + validator are pure
//! functions of it.

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
    checked_swap_input, finish_plan, guard_no_zeroed_output, native_capture_declines,
    seed_address_table, v4_bridge_steps, v4_hop_currencies, v4_scaffold_table,
    v4_terminal_capture_steps,
};
use alloy::primitives::Address;

/// Whether an amount fits the on-chain i128 swap-input field.
fn fits_i128(v: u128) -> bool {
    v <= i128::MAX as u128
}

/// Where a hop's swap output is routed (the hop-coupling fact).
#[derive(Clone, Copy, PartialEq, Eq)]
enum OutDest {
    /// Credits the executor.
    Executor,
    /// Routed into the PoolManager (seeds the V4 unlock ledger).
    PoolManager,
    /// Taken to a pool to REPAY its flash borrow.
    Repay(Address),
}

/// How a hop's borrowed input is repaid (the repayment-obligation fact).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Repay {
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
#[derive(Clone, Copy)]
#[expect(
    dead_code,
    reason = "spike declares the full facts schema; A2's generic walker consumes prot/zfo/repay"
)]
struct HopFacts {
    prot: Prot,
    zfo: bool,
    swap_fee: u16,
    tick_spacing: i16,
    out_currency: Address,
    in_currency: Address,
    out_dest: OutDest,
    repay: Repay,
}

/// Per-protocol **mechanics** (ADR-031 D4 code half): how a protocol's hop
/// becomes a `PlanStep` tree. For the spike only the V3 mechanics the
/// `v3_v4_v3` shape exercises is implemented; A2 generalizes.
mod mechanics {
    use super::{AddressTable, HopFacts, OutDest};
    use crate::composers::V3HopInfo;
    use crate::encoders::{SENTINEL_PM, SENTINEL_SELF};
    use crate::grammar_ledger::Prot;
    use crate::grammar_plan::PlanStep;

    /// The V3 flash-swap step. `out_dest` picks the recipient routing.
    pub fn v3_flash(
        at: &mut AddressTable,
        pool: &V3HopInfo,
        facts: &HopFacts,
        out_amount: u128,
        in_amount: u128,
        callback: Vec<PlanStep>,
    ) -> Option<PlanStep> {
        let pool_idx = at.add(pool.pool_address).ok()?;
        let (recipient_idx, recipient_pool_addr, recipient_pool_repays) = match facts.out_dest {
            OutDest::Executor => (SENTINEL_SELF, None, false),
            OutDest::PoolManager => (SENTINEL_PM, None, false),
            OutDest::Repay(_) => unreachable!("V3 hop never repays a pool here"),
        };
        Some(PlanStep::FlashSwap {
            pool_idx,
            pool_addr: pool.pool_address,
            protocol: Prot::V3,
            zfo: pool.zfo,
            fee: facts.swap_fee,
            out_currency: facts.out_currency,
            out_amount,
            in_currency: facts.in_currency,
            in_amount,
            recipient_idx,
            recipient_pool_addr,
            recipient_pool_repays,
            auto_repay: false,
            callback,
        })
    }
}

/// Read the `v3_v4_v3` per-protocol facts (the D4 data half).
fn facts_of(path: &PathInfo) -> Option<(HopFacts, HopFacts, HopFacts)> {
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
    };
    Some((fa, fb, fc))
}

/// Build the `v3_v4_v3` Plan from hop facts + inputs, deriving the enclosure
/// (which flash wraps which, the V4 unlock, the repayment order).
///
/// Returns `None` on a decline; a produced Plan is guaranteed validator-safe by
/// the shared `LedgerValidator` gate (ADR-030 Reject path).
#[must_use]
pub fn build_v3v4v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V4(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
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
    let (fa, fb, fc) = facts_of(path)?;

    // AddressTable order must mirror `derive_3hop_v3v4v3`: pm, v3a, v3c,
    // forward_a, forward_b, c0_b, c1_b. (Adds are idempotent — the mechanics
    // re-resolve already-present pools to the same index.)
    let mut at = v4_scaffold_table(inputs);
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let v3a = at.add(a.pool_address).ok()?;
    let v3c = at.add(c.pool_address).ok()?;
    let forward_a = at.add(fa.out_currency).ok()?;
    let forward_b = at.add(fb.out_currency).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let weth = inputs.weth_address;
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;

    // The V4 middle ledger: settle the leading V3's PM forward, swap, take to
    // the terminal V3 (repaying its flash), and net every PM ledger to zero.
    let v4_inner: Plan = vec![
        PlanStep::V4Settle {
            currency_addr: fa.out_currency,
            amount: b_swap_in,
        },
        PlanStep::V4Swap {
            c0_idx: c0_b,
            c1_idx: c1_b,
            fee: fb.swap_fee,
            tick_spacing: fb.tick_spacing,
            hooks_idx: SENTINEL_NATIVE,
            zfo: b.zfo,
            amount: b_swap_in,
            in_currency: fb.in_currency,
            in_amount: b_swap_in,
            out_currency: fb.out_currency,
            out_amount: out_b,
        },
        PlanStep::V4TakeCompact {
            currency_idx: forward_b,
            currency_addr: fb.out_currency,
            recipient_idx: v3c,
            amount: c_swap_in,
            seeds_pool: None,
            repays_flash: Some(c.pool_address),
        },
        PlanStep::V4SettleAll,
    ];

    // DERIVED ENCLOSURE (from the `Repay` facts): the `Offstream` hop (c) is
    // the OUTERMOST flash; the `SelfRefund` hop (a) is INNER, and its callback
    // runs the WETH self-refund then the V4 unlock; the `NetZero` V4 lives
    // inside the unlock. The leading V3 `out_dest` (PoolManager) seeds it.
    let a_callback: Vec<PlanStep> = vec![
        PlanStep::Erc20Transfer {
            token_idx: SENTINEL_WETH,
            token_addr: weth,
            recipient_idx: v3a,
            amount: optimal_input,
            seeds_pool: None,
            repays_flash: Some(a.pool_address),
        },
        PlanStep::V4Unlock {
            inner: v4_inner,
            pool_manager_idx: pm_idx,
        },
    ];
    let a_flash = mechanics::v3_flash(&mut at, a, &fa, out_a, optimal_input, a_callback)?;
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::V4Sync {
            currency_idx: forward_a,
            currency_addr: fa.out_currency,
        },
        mechanics::v3_flash(&mut at, c, &fc, out_c, c_swap_in, vec![a_flash])?,
    ];
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
fn funding_branch<T>(
    funding: FundingSource,
    self_fund: impl FnOnce() -> Option<T>,
    in_path_flash: impl FnOnce() -> Option<T>,
) -> Option<T> {
    if funding == FundingSource::SelfFund {
        self_fund()
    } else {
        in_path_flash()
    }
}

/// Build the any-N (≥2) all-V2 chain Plan from hop facts + inputs, deriving
/// the enclosure (the funding branch + the `V2SwapCalc` walk) — byte-identical
/// to `build_all_v2_chain` (ADR-031; the `walk` feature).
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "the any-N V2 chain walk is inherently a per-hop loop with two funding arms"
)]
pub fn build_all_v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let n = path.hops.len();
    if n < 2 {
        return None;
    }
    let v2_hops: Vec<&V2HopInfo> = path
        .hops
        .iter()
        .map(|h| match h {
            HopInfo::V2(h) => Some(h),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let optimal_input = inputs.optimal_input;
    let fwd_a = v2_forward(v2_hops[0]);
    let closing = v2_forward(v2_hops[n - 1]);

    // Same insertion order as the speedrail: pools in hop order, then the
    // leading pair's forward token, then the closing currency.
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let pool_idx: Vec<u8> = v2_hops
        .iter()
        .map(|h| at.add(h.pool_address).ok())
        .collect::<Option<Vec<_>>>()?;
    let forward_idx = at.add(fwd_a).ok()?;
    let closing_idx = at.add(closing).ok()?;

    let plan: Plan = funding_branch(
        inputs.opts.funding,
        || {
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
                    seeds_pool: Some(v2_hops[0].pool_address),
                    repays_flash: None,
                },
            ];
            for i in 0..n {
                let hop = v2_hops[i];
                let terminal = i == n - 1;
                steps.push(PlanStep::V2SwapCalc {
                    pool_idx: pool_idx[i],
                    pool_addr: hop.pool_address,
                    zfo: hop.zfo,
                    recipient_idx: if terminal {
                        SENTINEL_SELF
                    } else {
                        pool_idx[i + 1]
                    },
                    fee: hop.fee,
                    out_currency: if terminal { closing } else { v2_forward(hop) },
                    out_amount: inputs.hop_outputs[i],
                    recipient_pool_addr: if terminal {
                        None
                    } else {
                        Some(v2_hops[i + 1].pool_address)
                    },
                    recipient_repays: false,
                });
            }
            Some(steps)
        },
        || {
            let mut callback: Plan = vec![PlanStep::Erc20Transfer {
                token_idx: forward_idx,
                token_addr: fwd_a,
                recipient_idx: pool_idx[1],
                amount: inputs.hop_outputs[0],
                seeds_pool: Some(v2_hops[1].pool_address),
                repays_flash: None,
            }];
            for i in 1..n {
                let hop = v2_hops[i];
                let terminal = i == n - 1;
                callback.push(PlanStep::V2SwapCalc {
                    pool_idx: pool_idx[i],
                    pool_addr: hop.pool_address,
                    zfo: hop.zfo,
                    recipient_idx: if terminal {
                        SENTINEL_SELF
                    } else {
                        pool_idx[i + 1]
                    },
                    fee: hop.fee,
                    out_currency: if terminal { closing } else { v2_forward(hop) },
                    out_amount: inputs.hop_outputs[i],
                    recipient_pool_addr: if terminal {
                        None
                    } else {
                        Some(v2_hops[i + 1].pool_address)
                    },
                    recipient_repays: false,
                });
            }
            callback.push(PlanStep::Erc20Transfer {
                token_idx: closing_idx,
                token_addr: closing,
                recipient_idx: pool_idx[0],
                amount: optimal_input,
                seeds_pool: None,
                repays_flash: Some(v2_hops[0].pool_address),
            });
            let hop_a = v2_hops[0];
            Some(vec![PlanStep::FlashSwap {
                pool_idx: pool_idx[0],
                pool_addr: hop_a.pool_address,
                protocol: Prot::V2,
                zfo: hop_a.zfo,
                fee: hop_a.fee,
                out_currency: fwd_a,
                out_amount: inputs.hop_outputs[0],
                in_currency: closing,
                in_amount: optimal_input,
                recipient_idx: SENTINEL_SELF,
                recipient_pool_addr: None,
                recipient_pool_repays: false,
                auto_repay: false,
                callback,
            }])
        },
    )?;

    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 2-hop arity guard: a routine decline for any non-2-hop shape.
fn guard_arity(n: usize, expect: usize) -> Option<()> {
    (n == expect).then_some(())
}

/// The leading-output guard: `Some(forward_out)` when nonzero, else decline.
fn guard_forward_out(inputs: &ComposerInputs<'_>) -> Option<u128> {
    let f = *inputs.hop_outputs.first()?;
    (f != 0).then_some(f)
}

/// The 2-hop V2→V3 Plan, byte-identical to `build_v2v3_plan` (both funding axes).
#[must_use]
#[expect(clippy::too_many_lines, reason = "two funding arms of a nested flash")]
pub fn build_v2v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 2)?;
    let (HopInfo::V2(a), HopInfo::V3(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = guard_forward_out(inputs)?;
    if !fits_i128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_i128(b_swap_in) {
        return None;
    }
    let fwd_a = v2_forward(a);
    let weth = inputs.weth_address;
    let terminal_out = *inputs.hop_outputs.get(1)?;

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v2_idx = at.add(a.pool_address).ok()?;
    let v3_idx = at.add(b.pool_address).ok()?;

    let plan: Plan = funding_branch(
        inputs.opts.funding,
        || {
            Some(vec![
                PlanStep::SelfFund {
                    currency: weth,
                    amount: optimal_input,
                },
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v2_idx,
                    amount: optimal_input,
                    seeds_pool: Some(a.pool_address),
                    repays_flash: None,
                },
                PlanStep::V2SwapCalc {
                    pool_idx: v2_idx,
                    pool_addr: a.pool_address,
                    zfo: a.zfo,
                    recipient_idx: SENTINEL_SELF,
                    fee: a.fee,
                    out_currency: fwd_a,
                    out_amount: forward_out,
                    recipient_pool_addr: None,
                    recipient_repays: false,
                },
                PlanStep::FlashSwap {
                    pool_idx: v3_idx,
                    pool_addr: b.pool_address,
                    protocol: Prot::V3,
                    zfo: b.zfo,
                    fee: u16::try_from(b.fee).ok()?,
                    out_currency: weth,
                    out_amount: terminal_out,
                    in_currency: fwd_a,
                    in_amount: b_swap_in,
                    recipient_idx: SENTINEL_SELF,
                    recipient_pool_addr: None,
                    recipient_pool_repays: false,
                    auto_repay: true,
                    callback: vec![],
                },
            ])
        },
        || {
            let forward_idx = at.add(fwd_a).ok()?;
            Some(vec![PlanStep::FlashSwap {
                pool_idx: v2_idx,
                pool_addr: a.pool_address,
                protocol: Prot::V2,
                zfo: a.zfo,
                fee: a.fee,
                out_currency: fwd_a,
                out_amount: forward_out,
                in_currency: weth,
                in_amount: optimal_input,
                recipient_idx: SENTINEL_SELF,
                recipient_pool_addr: None,
                recipient_pool_repays: false,
                auto_repay: false,
                callback: vec![
                    PlanStep::FlashSwap {
                        pool_idx: v3_idx,
                        pool_addr: b.pool_address,
                        protocol: Prot::V3,
                        zfo: b.zfo,
                        fee: u16::try_from(b.fee).ok()?,
                        out_currency: weth,
                        out_amount: terminal_out,
                        in_currency: fwd_a,
                        in_amount: b_swap_in,
                        recipient_idx: SENTINEL_SELF,
                        recipient_pool_addr: None,
                        recipient_pool_repays: false,
                        auto_repay: false,
                        callback: vec![PlanStep::Erc20Transfer {
                            token_idx: forward_idx,
                            token_addr: fwd_a,
                            recipient_idx: v3_idx,
                            amount: b_swap_in,
                            seeds_pool: None,
                            repays_flash: Some(b.pool_address),
                        }],
                    },
                    PlanStep::Erc20Transfer {
                        token_idx: SENTINEL_WETH,
                        token_addr: weth,
                        recipient_idx: v2_idx,
                        amount: optimal_input,
                        seeds_pool: None,
                        repays_flash: Some(a.pool_address),
                    },
                ],
            }])
        },
    )?;

    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 2-hop V3→V2 Plan (SelfFund), byte-identical to `build_v3v2_plan`.
#[must_use]
pub fn build_v3v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 2)?;
    let (HopInfo::V3(a), HopInfo::V2(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = guard_forward_out(inputs)?;
    let weth = inputs.weth_address;
    let fwd_a = v3_forward(a);

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v3_idx = at.add(a.pool_address).ok()?;
    let v2_idx = at.add(b.pool_address).ok()?;
    let forward_idx = at.add(fwd_a).ok()?;

    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v3_idx,
            pool_addr: a.pool_address,
            protocol: Prot::V3,
            zfo: a.zfo,
            fee: u16::try_from(a.fee).ok()?,
            out_currency: fwd_a,
            out_amount: forward_out,
            in_currency: weth,
            in_amount: optimal_input,
            recipient_idx: SENTINEL_SELF,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v3_idx,
                    amount: optimal_input,
                    seeds_pool: None,
                    repays_flash: Some(a.pool_address),
                },
                PlanStep::Erc20Transfer {
                    token_idx: forward_idx,
                    token_addr: fwd_a,
                    recipient_idx: v2_idx,
                    amount: forward_out,
                    seeds_pool: Some(b.pool_address),
                    repays_flash: None,
                },
                PlanStep::V2SwapCalc {
                    pool_idx: v2_idx,
                    pool_addr: b.pool_address,
                    zfo: b.zfo,
                    recipient_idx: SENTINEL_SELF,
                    fee: b.fee,
                    out_currency: weth,
                    out_amount: *inputs.hop_outputs.get(1)?,
                    recipient_pool_addr: None,
                    recipient_repays: false,
                },
            ],
        },
    ];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 2-hop V3→V3 Plan (SelfFund), byte-identical to `build_v3v3_plan`.
#[must_use]
pub fn build_v3v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 2)?;
    let (HopInfo::V3(a), HopInfo::V3(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = guard_forward_out(inputs)?;
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let terminal_out = *inputs.hop_outputs.get(1)?;
    let weth = inputs.weth_address;
    let fwd_a = v3_forward(a);

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v3_a = at.add(a.pool_address).ok()?;
    let v3_b = at.add(b.pool_address).ok()?;

    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v3_a,
            pool_addr: a.pool_address,
            protocol: Prot::V3,
            zfo: a.zfo,
            fee: u16::try_from(a.fee).ok()?,
            out_currency: fwd_a,
            out_amount: forward_out,
            in_currency: weth,
            in_amount: optimal_input,
            recipient_idx: SENTINEL_SELF,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v3_a,
                    amount: optimal_input,
                    seeds_pool: None,
                    repays_flash: Some(a.pool_address),
                },
                PlanStep::FlashSwap {
                    pool_idx: v3_b,
                    pool_addr: b.pool_address,
                    protocol: Prot::V3,
                    zfo: b.zfo,
                    fee: u16::try_from(b.fee).ok()?,
                    out_currency: weth,
                    out_amount: terminal_out,
                    in_currency: fwd_a,
                    in_amount: b_swap_in,
                    recipient_idx: SENTINEL_SELF,
                    recipient_pool_addr: None,
                    recipient_pool_repays: false,
                    auto_repay: true,
                    callback: vec![],
                },
            ],
        },
    ];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 2-hop V4→V4 Plan, byte-identical to `build_v4v4_plan` (the WETH-only
/// slice incl. gap/batch/erc6909/native-withdraw variants).
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "gap/batch/erc6909/native-withdraw variants of a two-swap V4 family"
)]
pub fn build_v4v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 2)?;
    let (HopInfo::V4(a), HopInfo::V4(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let b_out = *inputs.hop_outputs.get(1)?;
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

    let (mid_currency_a, in_currency_a) = v4_hop_currencies(a);
    let (out_currency_b, mid_currency_b) = v4_hop_currencies(b);
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
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;

    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;

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
        let out_idx = if b.zfo { c1_b } else { c0_b };
        vec![
            PlanStep::V4Swap {
                c0_idx: c0_a,
                c1_idx: c1_a,
                fee: fee_a,
                tick_spacing: ts_a,
                hooks_idx: SENTINEL_NATIVE,
                zfo: a.zfo,
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
                zfo: b.zfo,
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
                        zfo: a.zfo,
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
                        zfo: b.zfo,
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
                    zfo: a.zfo,
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
                    zfo: b.zfo,
                    amount: b_swap_in,
                    in_currency: out_currency_a,
                    in_amount: b_swap_in,
                    out_currency: out_currency_b,
                    out_amount: b_out,
                },
            ]
        };
        let out_idx = if b.zfo { c1_b } else { c0_b };
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
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 2-hop V4→V3 Plan (boundary-take), byte-identical to `build_v4v3_plan`.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "native-output / erc20-output / native-input / non-WETH-terminal branches"
)]
pub fn build_v4v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 2)?;
    let (HopInfo::V4(a), HopInfo::V3(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
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

    let out_currency_a = if a.zfo {
        a.currency1_address
    } else {
        a.currency0_address
    };
    let in_currency_a = if a.zfo {
        a.currency0_address
    } else {
        a.currency1_address
    };
    let v4_out_native = out_currency_a == NATIVE_CURRENCY_ADDRESS;
    let out_currency_b = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };
    if v4_out_native {
        let in_currency_b = if b.zfo {
            b.token0_address
        } else {
            b.token1_address
        };
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
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let v3_idx = at.add(b.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let forward_idx = if a.zfo { c1_a } else { c0_a };

    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let v4_in_native = in_currency_a == NATIVE_CURRENCY_ADDRESS;
    let input_idx = if a.zfo { c0_a } else { c1_a };

    let mut inner: Plan = vec![PlanStep::V4Swap {
        c0_idx: c0_a,
        c1_idx: c1_a,
        fee: fee_a,
        tick_spacing: ts_a,
        hooks_idx: SENTINEL_NATIVE,
        zfo: a.zfo,
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
                pool_addr: b.pool_address,
                protocol: Prot::V3,
                zfo: b.zfo,
                fee: u16::try_from(b.fee).ok()?,
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
                pool_addr: b.pool_address,
                protocol: Prot::V3,
                zfo: b.zfo,
                fee: u16::try_from(b.fee).ok()?,
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
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 2-hop V4→V2 Plan (boundary-seed), byte-identical to `build_v4v2_plan`.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "native-output / erc20-output / native-input / non-WETH-terminal branches"
)]
pub fn build_v4v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 2)?;
    let (HopInfo::V4(a), HopInfo::V2(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_i128(optimal_input) || !fits_i128(forward_out) {
        return None;
    }
    let weth = inputs.weth_address;

    let out_currency_a = if a.zfo {
        a.currency1_address
    } else {
        a.currency0_address
    };
    let in_currency_a = if a.zfo {
        a.currency0_address
    } else {
        a.currency1_address
    };
    let v4_out_native = out_currency_a == NATIVE_CURRENCY_ADDRESS;
    let out_currency_b = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };
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
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let v2_idx = at.add(b.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let forward_idx = if a.zfo { c1_a } else { c0_a };

    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let v4_in_native = in_currency_a == NATIVE_CURRENCY_ADDRESS;
    let input_idx = if a.zfo { c0_a } else { c1_a };

    let mut inner: Plan = vec![PlanStep::V4Swap {
        c0_idx: c0_a,
        c1_idx: c1_a,
        fee: fee_a,
        tick_spacing: ts_a,
        hooks_idx: SENTINEL_NATIVE,
        zfo: a.zfo,
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
            PlanStep::Erc20Transfer {
                token_idx: weth_idx,
                token_addr: weth,
                recipient_idx: v2_idx,
                amount: forward_out,
                seeds_pool: Some(b.pool_address),
                repays_flash: None,
            },
            PlanStep::V2SwapCalc {
                pool_idx: v2_idx,
                pool_addr: b.pool_address,
                zfo: b.zfo,
                recipient_idx: SENTINEL_SELF,
                fee: b.fee,
                out_currency: out_currency_b,
                out_amount: weth_out,
                recipient_pool_addr: None,
                recipient_repays: false,
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
                recipient_idx: v2_idx,
                amount: forward_out,
                seeds_pool: Some(b.pool_address),
                repays_flash: None,
            },
            PlanStep::V2SwapCalc {
                pool_idx: v2_idx,
                pool_addr: b.pool_address,
                zfo: b.zfo,
                recipient_idx: SENTINEL_SELF,
                fee: b.fee,
                out_currency: out_currency_b,
                out_amount: weth_out,
                recipient_pool_addr: None,
                recipient_repays: false,
            },
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
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V4→V4 Plan, byte-identical to `build_v4v4v4_plan` (facts-driven:
/// the same shared `v4_bridge_steps`/`v4_terminal_capture_steps` helpers the
/// hand-written producer calls).
#[must_use]
#[expect(
    clippy::too_many_lines,
    clippy::similar_names,
    reason = "three swaps + optional bridges + batch + terminal capture variants"
)]
pub fn build_v4v4v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V4(a), HopInfo::V4(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
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
    let (mid_a_out, in_currency_a) = v4_hop_currencies(a);
    let (mid_b_out, mid_b_in) = v4_hop_currencies(b);
    let (output_c, mid_c_in) = v4_hop_currencies(c);
    let weth = inputs.weth_address;
    let capture = resolve_axes(inputs.opts).1;
    if native_capture_declines(capture, output_c, weth) {
        return None;
    }
    let bridge_ab = CurrencyBridge::at_boundary(mid_a_out, mid_b_in);
    let bridge_bc = CurrencyBridge::at_boundary(mid_b_out, mid_c_in);
    let any_gap = bridge_ab.needs_bridge() || bridge_bc.needs_bridge();

    let mut at = v4_scaffold_table(inputs);
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let c0_c = at.add(c.currency0_address).ok()?;
    let c1_c = at.add(c.currency1_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;

    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;

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
        zfo: a.zfo,
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
        zfo: b.zfo,
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
        zfo: c.zfo,
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
                    zfo: a.zfo,
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
                    zfo: b.zfo,
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
                    zfo: c.zfo,
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
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V2→V2 Plan, byte-identical to `build_v4v2v2_plan`.
#[must_use]
pub fn build_v4v2v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V4(a), HopInfo::V2(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.first()?;
    if out_a == 0 || inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_i128(optimal_input) {
        return None;
    }
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let weth = inputs.weth_address;
    if in_currency_a != weth {
        return None;
    }
    let mut at = v4_scaffold_table(inputs);
    let forward_a = at.add(forward_a_cur).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let v2b = at.add(b.pool_address).ok()?;
    let v2c = at.add(c.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let fwd_b = v2_forward(b);
    let fwd_c = v2_forward(c);
    let b_out = *inputs.hop_outputs.get(1)?;
    let c_out = *inputs.hop_outputs.get(2)?;

    let inner: Plan = vec![
        PlanStep::V4Swap {
            c0_idx: c0_a,
            c1_idx: c1_a,
            fee: fee_a,
            tick_spacing: ts_a,
            hooks_idx: SENTINEL_NATIVE,
            zfo: a.zfo,
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
            seeds_pool: Some(b.pool_address),
            repays_flash: None,
        },
        PlanStep::V2SwapCalc {
            pool_idx: v2b,
            pool_addr: b.pool_address,
            zfo: b.zfo,
            recipient_idx: v2c,
            fee: b.fee,
            out_currency: fwd_b,
            out_amount: b_out,
            recipient_pool_addr: Some(c.pool_address),
            recipient_repays: false,
        },
        PlanStep::V2SwapCalc {
            pool_idx: v2c,
            pool_addr: c.pool_address,
            zfo: c.zfo,
            recipient_idx: SENTINEL_SELF,
            fee: c.fee,
            out_currency: fwd_c,
            out_amount: c_out,
            recipient_pool_addr: None,
            recipient_repays: false,
        },
        PlanStep::V4SettleDelta {
            currency_idx: SENTINEL_WETH,
            currency_addr: weth,
        },
    ];
    let plan: Plan = vec![PlanStep::V4Unlock {
        inner,
        pool_manager_idx: pm_idx,
    }];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V2→V4 Plan, byte-identical to `build_v4v2v4_plan`.
#[must_use]
pub fn build_v4v2v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V4(a), HopInfo::V2(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.first()?;
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
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let b_forward_cur = v2_forward(b);
    let out_c = *inputs.hop_outputs.get(2)?;
    let b_out = *inputs.hop_outputs.get(1)?;
    let mut at = v4_scaffold_table(inputs);
    let forward_a = at.add(forward_a_cur).ok()?;
    at.add(b_forward_cur).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let c0_c = at.add(c.currency0_address).ok()?;
    let c1_c = at.add(c.currency1_address).ok()?;
    let v2b = at.add(b.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;

    let inner: Plan = vec![
        PlanStep::V4Swap {
            c0_idx: c0_a,
            c1_idx: c1_a,
            fee: fee_a,
            tick_spacing: ts_a,
            hooks_idx: SENTINEL_NATIVE,
            zfo: a.zfo,
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
            seeds_pool: Some(b.pool_address),
            repays_flash: None,
        },
        PlanStep::V2SwapCalc {
            pool_idx: v2b,
            pool_addr: b.pool_address,
            zfo: b.zfo,
            recipient_idx: SENTINEL_SELF,
            fee: b.fee,
            out_currency: b_forward_cur,
            out_amount: b_out,
            recipient_pool_addr: None,
            recipient_repays: false,
        },
        PlanStep::V4Swap {
            c0_idx: c0_c,
            c1_idx: c1_c,
            fee: fee_c,
            tick_spacing: ts_c,
            hooks_idx: SENTINEL_NATIVE,
            zfo: c.zfo,
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
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V3→V3 Plan, byte-identical to `build_v4v3v3_plan`.
#[must_use]
pub fn build_v4v3v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V4(a), HopInfo::V3(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.first()?;
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
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let fwd_c = v3_forward(c);
    let in_c = v3_input(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let mut at = v4_scaffold_table(inputs);
    at.add(inputs.pool_manager_address).ok()?;
    let v3b = at.add(b.pool_address).ok()?;
    let v3c = at.add(c.pool_address).ok()?;
    let forward_a = at.add(forward_a_cur).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let weth = inputs.weth_address;

    let inner: Plan = vec![
        PlanStep::V4Swap {
            c0_idx: c0_a,
            c1_idx: c1_a,
            fee: fee_a,
            tick_spacing: ts_a,
            hooks_idx: SENTINEL_NATIVE,
            zfo: a.zfo,
            amount: optimal_input,
            in_currency: in_currency_a,
            in_amount: optimal_input,
            out_currency: forward_a_cur,
            out_amount: out_a,
        },
        PlanStep::FlashSwap {
            pool_idx: v3c,
            pool_addr: c.pool_address,
            protocol: Prot::V3,
            zfo: c.zfo,
            fee: u16::try_from(c.fee).ok()?,
            out_currency: fwd_c,
            out_amount: out_c,
            in_currency: in_c,
            in_amount: c_swap_in,
            recipient_idx: SENTINEL_SELF,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![PlanStep::FlashSwap {
                pool_idx: v3b,
                pool_addr: b.pool_address,
                protocol: Prot::V3,
                zfo: b.zfo,
                fee: u16::try_from(b.fee).ok()?,
                out_currency: fwd_b,
                out_amount: out_b,
                in_currency: in_b,
                in_amount: b_swap_in,
                recipient_idx: v3c,
                recipient_pool_addr: Some(c.pool_address),
                recipient_pool_repays: true,
                auto_repay: false,
                callback: vec![PlanStep::V4TakeCompact {
                    currency_idx: forward_a,
                    currency_addr: forward_a_cur,
                    recipient_idx: v3b,
                    amount: out_a,
                    seeds_pool: None,
                    repays_flash: Some(b.pool_address),
                }],
            }],
        },
        PlanStep::V4SettleDelta {
            currency_idx: SENTINEL_WETH,
            currency_addr: weth,
        },
    ];
    let plan: Plan = vec![PlanStep::V4Unlock {
        inner,
        pool_manager_idx: pm_idx,
    }];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V3→V4 Plan, byte-identical to `build_v4v3v4_plan`.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "V4→V3 flash → V4 swap with sync/settle"
)]
pub fn build_v4v3v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V4(a), HopInfo::V3(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.first()?;
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
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let mut at = v4_scaffold_table(inputs);
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let v3b = at.add(b.pool_address).ok()?;
    let forward_a = at.add(forward_a_cur).ok()?;
    let forward_b = at.add(fwd_b).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let c0_c = at.add(c.currency0_address).ok()?;
    let c1_c = at.add(c.currency1_address).ok()?;

    let inner: Plan = vec![
        PlanStep::V4Swap {
            c0_idx: c0_a,
            c1_idx: c1_a,
            fee: fee_a,
            tick_spacing: ts_a,
            hooks_idx: SENTINEL_NATIVE,
            zfo: a.zfo,
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
        PlanStep::FlashSwap {
            pool_idx: v3b,
            pool_addr: b.pool_address,
            protocol: Prot::V3,
            zfo: b.zfo,
            fee: u16::try_from(b.fee).ok()?,
            out_currency: fwd_b,
            out_amount: out_b,
            in_currency: in_b,
            in_amount: b_swap_in,
            recipient_idx: pm_idx,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![PlanStep::V4TakeCompact {
                currency_idx: forward_a,
                currency_addr: forward_a_cur,
                recipient_idx: v3b,
                amount: out_a,
                seeds_pool: None,
                repays_flash: Some(b.pool_address),
            }],
        },
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
            zfo: c.zfo,
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
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V3→V2→V3 Plan, byte-identical to `build_v3v2v3_plan`.
#[must_use]
pub fn build_v3v2v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V3(a), HopInfo::V2(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    guard_no_zeroed_output(inputs)?;
    if !fits_i128(optimal_input) {
        return None;
    }
    let c_swap_in = checked_swap_input(inputs, 2)?;
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let fwd_b = v2_forward(b);
    let fwd_c = v3_forward(c);
    let in_c = v3_input(c);
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let mut at = seed_address_table(inputs);
    let v2b = at.add(b.pool_address).ok()?;
    let v3a = at.add(a.pool_address).ok()?;
    let v3c = at.add(c.pool_address).ok()?;
    let weth = inputs.weth_address;

    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v3c,
            pool_addr: c.pool_address,
            protocol: Prot::V3,
            zfo: c.zfo,
            fee: u16::try_from(c.fee).ok()?,
            out_currency: fwd_c,
            out_amount: out_c,
            in_currency: in_c,
            in_amount: c_swap_in,
            recipient_idx: SENTINEL_SELF,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![PlanStep::FlashSwap {
                pool_idx: v3a,
                pool_addr: a.pool_address,
                protocol: Prot::V3,
                zfo: a.zfo,
                fee: u16::try_from(a.fee).ok()?,
                out_currency: fwd_a,
                out_amount: out_a,
                in_currency: in_a,
                in_amount: optimal_input,
                recipient_idx: v2b,
                recipient_pool_addr: Some(b.pool_address),
                recipient_pool_repays: false,
                auto_repay: false,
                callback: vec![
                    PlanStep::V2SwapCalc {
                        pool_idx: v2b,
                        pool_addr: b.pool_address,
                        zfo: b.zfo,
                        recipient_idx: v3c,
                        fee: b.fee,
                        out_currency: fwd_b,
                        out_amount: out_b,
                        recipient_pool_addr: Some(c.pool_address),
                        recipient_repays: true,
                    },
                    PlanStep::Erc20Transfer {
                        token_idx: SENTINEL_WETH,
                        token_addr: weth,
                        recipient_idx: v3a,
                        amount: optimal_input,
                        seeds_pool: None,
                        repays_flash: Some(a.pool_address),
                    },
                ],
            }],
        },
    ];
    Some(finish_plan(at, plan))
}

/// The 3-hop V3→V3→V2 Plan, byte-identical to `build_v3v3v2_plan`.
#[must_use]
pub fn build_v3v3v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V3(a), HopInfo::V3(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    guard_no_zeroed_output(inputs)?;
    if !fits_i128(optimal_input) {
        return None;
    }
    let b_swap_in = checked_swap_input(inputs, 1)?;
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let fwd_c = v2_forward(c);
    let out_c = *inputs.hop_outputs.get(2)?;
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let mut at = seed_address_table(inputs);
    let v2c = at.add(c.pool_address).ok()?;
    let v3a = at.add(a.pool_address).ok()?;
    let v3b = at.add(b.pool_address).ok()?;
    let weth = inputs.weth_address;

    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v3b,
            pool_addr: b.pool_address,
            protocol: Prot::V3,
            zfo: b.zfo,
            fee: u16::try_from(b.fee).ok()?,
            out_currency: fwd_b,
            out_amount: out_b,
            in_currency: in_b,
            in_amount: b_swap_in,
            recipient_idx: v2c,
            recipient_pool_addr: Some(c.pool_address),
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![PlanStep::FlashSwap {
                pool_idx: v3a,
                pool_addr: a.pool_address,
                protocol: Prot::V3,
                zfo: a.zfo,
                fee: u16::try_from(a.fee).ok()?,
                out_currency: fwd_a,
                out_amount: out_a,
                in_currency: in_a,
                in_amount: optimal_input,
                recipient_idx: v3b,
                recipient_pool_addr: Some(b.pool_address),
                recipient_pool_repays: true,
                auto_repay: false,
                callback: vec![
                    PlanStep::V2SwapCalc {
                        pool_idx: v2c,
                        pool_addr: c.pool_address,
                        zfo: c.zfo,
                        recipient_idx: SENTINEL_SELF,
                        fee: c.fee,
                        out_currency: fwd_c,
                        out_amount: out_c,
                        recipient_pool_addr: None,
                        recipient_repays: false,
                    },
                    PlanStep::Erc20Transfer {
                        token_idx: SENTINEL_WETH,
                        token_addr: weth,
                        recipient_idx: v3a,
                        amount: optimal_input,
                        seeds_pool: None,
                        repays_flash: Some(a.pool_address),
                    },
                ],
            }],
        },
    ];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V3→V3→V3 Plan, byte-identical to `build_v3v3v3_plan`.
#[must_use]
pub fn build_v3v3v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V3(a), HopInfo::V3(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    guard_no_zeroed_output(inputs)?;
    if !fits_i128(optimal_input) {
        return None;
    }
    let b_swap_in = checked_swap_input(inputs, 1)?;
    let c_swap_in = checked_swap_input(inputs, 2)?;
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let fwd_c = v3_forward(c);
    let in_c = v3_input(c);
    let out_c = *inputs.hop_outputs.get(2)?;
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let mut at = seed_address_table(inputs);
    let v3a = at.add(a.pool_address).ok()?;
    let v3b = at.add(b.pool_address).ok()?;
    let v3c = at.add(c.pool_address).ok()?;
    let weth = inputs.weth_address;

    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v3c,
            pool_addr: c.pool_address,
            protocol: Prot::V3,
            zfo: c.zfo,
            fee: u16::try_from(c.fee).ok()?,
            out_currency: fwd_c,
            out_amount: out_c,
            in_currency: in_c,
            in_amount: c_swap_in,
            recipient_idx: SENTINEL_SELF,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![PlanStep::FlashSwap {
                pool_idx: v3b,
                pool_addr: b.pool_address,
                protocol: Prot::V3,
                zfo: b.zfo,
                fee: u16::try_from(b.fee).ok()?,
                out_currency: fwd_b,
                out_amount: out_b,
                in_currency: in_b,
                in_amount: b_swap_in,
                recipient_idx: v3c,
                recipient_pool_addr: Some(c.pool_address),
                recipient_pool_repays: true,
                auto_repay: false,
                callback: vec![PlanStep::FlashSwap {
                    pool_idx: v3a,
                    pool_addr: a.pool_address,
                    protocol: Prot::V3,
                    zfo: a.zfo,
                    fee: u16::try_from(a.fee).ok()?,
                    out_currency: fwd_a,
                    out_amount: out_a,
                    in_currency: in_a,
                    in_amount: optimal_input,
                    recipient_idx: v3b,
                    recipient_pool_addr: Some(b.pool_address),
                    recipient_pool_repays: true,
                    auto_repay: true,
                    callback: vec![],
                }],
            }],
        },
    ];
    Some(finish_plan(at, plan))
}

/// The 3-hop V2→V2→V3 Plan, byte-identical to `build_v2v2v3_plan`.
#[must_use]
pub fn build_v2v2v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V2(a), HopInfo::V2(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    guard_no_zeroed_output(inputs)?;
    if !fits_i128(optimal_input) {
        return None;
    }
    let c_swap_in = checked_swap_input(inputs, 2)?;
    let fwd_a = v2_forward(a);
    let fwd_b = v2_forward(b);
    let fwd_c = v3_forward(c);
    let in_c = v3_input(c);
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let mut at = seed_address_table(inputs);
    let v2a = at.add(a.pool_address).ok()?;
    let v2b = at.add(b.pool_address).ok()?;
    let v3c = at.add(c.pool_address).ok()?;
    let weth = inputs.weth_address;

    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v3c,
            pool_addr: c.pool_address,
            protocol: Prot::V3,
            zfo: c.zfo,
            fee: u16::try_from(c.fee).ok()?,
            out_currency: fwd_c,
            out_amount: out_c,
            in_currency: in_c,
            in_amount: c_swap_in,
            recipient_idx: SENTINEL_SELF,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v2a,
                    amount: optimal_input,
                    seeds_pool: Some(a.pool_address),
                    repays_flash: None,
                },
                PlanStep::V2SwapCalc {
                    pool_idx: v2a,
                    pool_addr: a.pool_address,
                    zfo: a.zfo,
                    recipient_idx: v2b,
                    fee: a.fee,
                    out_currency: fwd_a,
                    out_amount: out_a,
                    recipient_pool_addr: Some(b.pool_address),
                    recipient_repays: false,
                },
                PlanStep::V2SwapCalc {
                    pool_idx: v2b,
                    pool_addr: b.pool_address,
                    zfo: b.zfo,
                    recipient_idx: v3c,
                    fee: b.fee,
                    out_currency: fwd_b,
                    out_amount: out_b,
                    recipient_pool_addr: Some(c.pool_address),
                    recipient_repays: true,
                },
            ],
        },
    ];
    Some(finish_plan(at, plan))
}

/// The 2-hop V3→V4 Plan, byte-identical to `build_v3v4_plan` (dispatch).
#[must_use]
pub fn build_v3v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 2)?;
    let (HopInfo::V3(a), HopInfo::V4(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let v4_out_amount = *inputs.hop_outputs.get(1)?;
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
    let weth = inputs.weth_address;

    let v4_in_currency = if b.zfo {
        b.currency0_address
    } else {
        b.currency1_address
    };
    let v4_out_currency = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };
    if v4_out_currency == NATIVE_CURRENCY_ADDRESS {
        return build_v3v4_native_output_walk(
            a,
            b,
            v4_in_currency,
            v4_out_amount,
            v4_swap_in,
            forward_out,
            optimal_input,
            weth,
            inputs,
        );
    }
    if v4_in_currency == NATIVE_CURRENCY_ADDRESS {
        return build_v3v4_native_input_walk(
            a,
            b,
            v4_out_currency,
            v4_out_amount,
            v4_swap_in,
            forward_out,
            optimal_input,
            weth,
            inputs,
        );
    }
    build_v3v4_erc20_input_walk(
        a,
        b,
        v4_in_currency,
        v4_out_currency,
        v4_out_amount,
        v4_swap_in,
        forward_out,
        optimal_input,
        weth,
        inputs,
    )
}

/// `v3_v4` ERC-20 V4 input branch.
#[expect(clippy::too_many_lines)]
#[expect(clippy::too_many_arguments)]
fn build_v3v4_erc20_input_walk(
    a: &V3HopInfo,
    b: &V4HopInfo,
    v4_in_currency: Address,
    v4_out_currency: Address,
    v4_out_amount: u128,
    v4_swap_in: u128,
    forward_out: u128,
    optimal_input: u128,
    weth: Address,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let forward_addr = if a.zfo {
        a.token1_address
    } else {
        a.token0_address
    };
    let v3_in_currency = if a.zfo {
        a.token0_address
    } else {
        a.token1_address
    };
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
    let v3_idx = at.add(a.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let forward_idx = at.add(forward_addr).ok()?;
    let weth_idx = SENTINEL_WETH;
    let output_idx = if b.zfo { c1_b } else { c0_b };
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
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
            zfo: b.zfo,
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
            repays_flash: Some(a.pool_address),
        },
    ];
    let flash = PlanStep::FlashSwap {
        pool_idx: v3_idx,
        pool_addr: a.pool_address,
        protocol: Prot::V3,
        zfo: a.zfo,
        fee: u16::try_from(a.fee).ok()?,
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
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// `v3_v4` native V4 output branch.
#[expect(clippy::too_many_lines)]
#[expect(clippy::too_many_arguments)]
fn build_v3v4_native_output_walk(
    a: &V3HopInfo,
    b: &V4HopInfo,
    v4_in_currency: Address,
    v4_out_amount: u128,
    v4_swap_in: u128,
    forward_out: u128,
    optimal_input: u128,
    weth: Address,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let forward_addr = if a.zfo {
        a.token1_address
    } else {
        a.token0_address
    };
    let v3_in_currency = if a.zfo {
        a.token0_address
    } else {
        a.token1_address
    };
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
    let v3_idx = at.add(a.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let forward_idx = at.add(forward_addr).ok()?;
    let weth_idx = SENTINEL_WETH;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
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
            zfo: b.zfo,
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
            repays_flash: Some(a.pool_address),
        },
    ];
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v3_idx,
            pool_addr: a.pool_address,
            protocol: Prot::V3,
            zfo: a.zfo,
            fee: u16::try_from(a.fee).ok()?,
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
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// `v3_v4` native V4 input branch.
#[expect(clippy::too_many_arguments)]
fn build_v3v4_native_input_walk(
    a: &V3HopInfo,
    b: &V4HopInfo,
    v4_out_currency: Address,
    v4_out_amount: u128,
    v4_swap_in: u128,
    forward_out: u128,
    optimal_input: u128,
    weth: Address,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let tok = if a.zfo {
        a.token0_address
    } else {
        a.token1_address
    };
    if tok == weth || tok == NATIVE_CURRENCY_ADDRESS {
        return None;
    }
    let mut at = AddressTable::with_sentinels(
        Some(weth),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let v3_idx = at.add(a.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let tok_idx = at.add(tok).ok()?;
    let weth_idx = SENTINEL_WETH;
    let output_idx = if b.zfo { c1_b } else { c0_b };
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let v4_inner: Plan = vec![
        PlanStep::V4Swap {
            c0_idx: c0_b,
            c1_idx: c1_b,
            fee: fee_b,
            tick_spacing: ts_b,
            hooks_idx: SENTINEL_NATIVE,
            zfo: b.zfo,
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
            repays_flash: Some(a.pool_address),
        },
    ];
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: tok,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v3_idx,
            pool_addr: a.pool_address,
            protocol: Prot::V3,
            zfo: a.zfo,
            fee: u16::try_from(a.fee).ok()?,
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
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 2-hop V2→V4 Plan, byte-identical to `build_v2v4_plan` (dispatch).
#[must_use]
pub fn build_v2v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 2)?;
    let (HopInfo::V2(a), HopInfo::V4(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let v4_out_amount = *inputs.hop_outputs.get(1)?;
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
    let weth = inputs.weth_address;

    let v4_in_currency = if b.zfo {
        b.currency0_address
    } else {
        b.currency1_address
    };
    let v4_out_currency = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };
    if v4_out_currency == NATIVE_CURRENCY_ADDRESS {
        return build_v2v4_native_output_walk(
            a,
            b,
            v4_in_currency,
            v4_out_amount,
            v4_swap_in,
            forward_out,
            optimal_input,
            weth,
            inputs,
        );
    }
    if v4_in_currency == NATIVE_CURRENCY_ADDRESS {
        return build_v2v4_native_input_walk(
            a,
            b,
            v4_out_currency,
            v4_out_amount,
            v4_swap_in,
            forward_out,
            optimal_input,
            weth,
            inputs,
        );
    }
    build_v2v4_erc20_input_walk(
        a,
        b,
        v4_in_currency,
        v4_out_currency,
        v4_out_amount,
        v4_swap_in,
        forward_out,
        optimal_input,
        weth,
        inputs,
    )
}

/// `v2_v4` ERC-20 V4 input branch (V2 flash).
#[expect(clippy::too_many_lines)]
#[expect(clippy::too_many_arguments)]
fn build_v2v4_erc20_input_walk(
    a: &V2HopInfo,
    b: &V4HopInfo,
    v4_in_currency: Address,
    v4_out_currency: Address,
    v4_out_amount: u128,
    v4_swap_in: u128,
    forward_out: u128,
    optimal_input: u128,
    weth: Address,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let forward_addr = if a.zfo {
        a.token1_address
    } else {
        a.token0_address
    };
    let v2_in_currency = if a.zfo {
        a.token0_address
    } else {
        a.token1_address
    };
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
    let v2_idx = at.add(a.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let forward_idx = at.add(forward_addr).ok()?;
    let weth_idx = SENTINEL_WETH;
    let output_idx = if b.zfo { c1_b } else { c0_b };
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
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
            zfo: b.zfo,
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
            repays_flash: Some(a.pool_address),
        },
    ];
    let flash = PlanStep::FlashSwap {
        pool_idx: v2_idx,
        pool_addr: a.pool_address,
        protocol: Prot::V2,
        zfo: a.zfo,
        fee: a.fee,
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
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// `v2_v4` native V4 output branch (wrap-and-repay).
#[expect(clippy::too_many_lines)]
#[expect(clippy::too_many_arguments)]
fn build_v2v4_native_output_walk(
    a: &V2HopInfo,
    b: &V4HopInfo,
    v4_in_currency: Address,
    v4_out_amount: u128,
    v4_swap_in: u128,
    forward_out: u128,
    optimal_input: u128,
    weth: Address,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let forward_addr = if a.zfo {
        a.token1_address
    } else {
        a.token0_address
    };
    let v2_in_currency = if a.zfo {
        a.token0_address
    } else {
        a.token1_address
    };
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
    let v2_idx = at.add(a.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let forward_idx = at.add(forward_addr).ok()?;
    let weth_idx = SENTINEL_WETH;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
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
            zfo: b.zfo,
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
            repays_flash: Some(a.pool_address),
        },
    ];
    let plan: Plan = vec![PlanStep::FlashSwap {
        pool_idx: v2_idx,
        pool_addr: a.pool_address,
        protocol: Prot::V2,
        zfo: a.zfo,
        fee: a.fee,
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
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// `v2_v4` native V4 input branch.
#[expect(clippy::too_many_arguments)]
fn build_v2v4_native_input_walk(
    a: &V2HopInfo,
    b: &V4HopInfo,
    v4_out_currency: Address,
    v4_out_amount: u128,
    v4_swap_in: u128,
    forward_out: u128,
    optimal_input: u128,
    weth: Address,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let tok = if a.zfo {
        a.token0_address
    } else {
        a.token1_address
    };
    if tok == weth || tok == NATIVE_CURRENCY_ADDRESS {
        return None;
    }
    let mut at = AddressTable::with_sentinels(
        Some(weth),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let v2_idx = at.add(a.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let tok_idx = at.add(tok).ok()?;
    let weth_idx = SENTINEL_WETH;
    let output_idx = if b.zfo { c1_b } else { c0_b };
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let v4_inner: Plan = vec![
        PlanStep::V4Swap {
            c0_idx: c0_b,
            c1_idx: c1_b,
            fee: fee_b,
            tick_spacing: ts_b,
            hooks_idx: SENTINEL_NATIVE,
            zfo: b.zfo,
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
            repays_flash: Some(a.pool_address),
        },
    ];
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: tok,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v2_idx,
            pool_addr: a.pool_address,
            protocol: Prot::V2,
            zfo: a.zfo,
            fee: a.fee,
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
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V4→V2 Plan, byte-identical to `build_v4v4v2_plan`.
#[must_use]
#[expect(
    clippy::similar_names,
    reason = "index-fidelity names mirror the reference"
)]
pub fn build_v4v4v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V4(a), HopInfo::V4(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let out_b = *inputs.hop_outputs.get(1)?;
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
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let (forward_b_cur, in_currency_b) = v4_hop_currencies(b);
    let fwd_c = v2_forward(c);
    let out_a = *inputs.hop_outputs.first()?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let mut at = v4_scaffold_table(inputs);
    let forward_b = at.add(forward_b_cur).ok()?;
    let v2c = at.add(c.pool_address).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;

    let inner: Plan = vec![
        PlanStep::V4Swap {
            c0_idx: c0_a,
            c1_idx: c1_a,
            fee: fee_a,
            tick_spacing: ts_a,
            hooks_idx: SENTINEL_NATIVE,
            zfo: a.zfo,
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
            zfo: b.zfo,
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
            seeds_pool: Some(c.pool_address),
            repays_flash: None,
        },
        PlanStep::V2SwapCalc {
            pool_idx: v2c,
            pool_addr: c.pool_address,
            zfo: c.zfo,
            recipient_idx: SENTINEL_SELF,
            fee: c.fee,
            out_currency: fwd_c,
            out_amount: out_c,
            recipient_pool_addr: None,
            recipient_repays: false,
        },
        PlanStep::V4SettleAll,
    ];
    let plan: Plan = vec![PlanStep::V4Unlock {
        inner,
        pool_manager_idx: pm_idx,
    }];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V4→V3 Plan, byte-identical to `build_v4v4v3_plan`.
#[must_use]
#[expect(
    clippy::similar_names,
    reason = "index-fidelity names mirror the reference"
)]
pub fn build_v4v4v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V4(a), HopInfo::V4(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
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
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let (forward_b_cur, in_currency_b) = v4_hop_currencies(b);
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let mut at = v4_scaffold_table(inputs);
    let forward_b = at.add(forward_b_cur).ok()?;
    let v3c = at.add(c.pool_address).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let out_currency_c = v3_forward(c);

    let inner: Plan = vec![
        PlanStep::V4Swap {
            c0_idx: c0_a,
            c1_idx: c1_a,
            fee: fee_a,
            tick_spacing: ts_a,
            hooks_idx: SENTINEL_NATIVE,
            zfo: a.zfo,
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
            zfo: b.zfo,
            amount: b_swap_in,
            in_currency: in_currency_b,
            in_amount: b_swap_in,
            out_currency: forward_b_cur,
            out_amount: out_b,
        },
        PlanStep::FlashSwap {
            pool_idx: v3c,
            pool_addr: c.pool_address,
            protocol: Prot::V3,
            zfo: c.zfo,
            fee: fee_c,
            out_currency: out_currency_c,
            out_amount: out_c,
            in_currency: forward_b_cur,
            in_amount: c_swap_in,
            recipient_idx: SENTINEL_SELF,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![PlanStep::V4TakeCompact {
                currency_idx: forward_b,
                currency_addr: forward_b_cur,
                recipient_idx: v3c,
                amount: c_swap_in,
                seeds_pool: None,
                repays_flash: Some(c.pool_address),
            }],
        },
        PlanStep::V4SettleAll,
    ];
    let plan: Plan = vec![PlanStep::V4Unlock {
        inner,
        pool_manager_idx: pm_idx,
    }];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V2→V3 Plan, byte-identical to `build_v4v2v3_plan`.
#[must_use]
pub fn build_v4v2v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V4(a), HopInfo::V2(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.first()?;
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
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let b_forward_cur = v2_forward(b);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let out_currency_c = v3_forward(c);
    let in_currency_c = v3_input(c);
    if in_currency_a != inputs.weth_address {
        return None;
    }
    let mut at = v4_scaffold_table(inputs);
    let v3c = at.add(c.pool_address).ok()?;
    let forward_a = at.add(forward_a_cur).ok()?;
    at.add(b_forward_cur).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let v2b = at.add(b.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let weth = inputs.weth_address;

    let v4_inner: Plan = vec![
        PlanStep::V4Swap {
            c0_idx: c0_a,
            c1_idx: c1_a,
            fee: fee_a,
            tick_spacing: ts_a,
            hooks_idx: SENTINEL_NATIVE,
            zfo: a.zfo,
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
            seeds_pool: Some(b.pool_address),
            repays_flash: None,
        },
        PlanStep::V2SwapCalc {
            pool_idx: v2b,
            pool_addr: b.pool_address,
            zfo: b.zfo,
            recipient_idx: v3c,
            fee: b.fee,
            out_currency: b_forward_cur,
            out_amount: out_b,
            recipient_pool_addr: Some(c.pool_address),
            recipient_repays: true,
        },
        PlanStep::V4SettleDelta {
            currency_idx: SENTINEL_WETH,
            currency_addr: weth,
        },
    ];
    let plan: Plan = vec![PlanStep::FlashSwap {
        pool_idx: v3c,
        pool_addr: c.pool_address,
        protocol: Prot::V3,
        zfo: c.zfo,
        fee: u16::try_from(c.fee).ok()?,
        out_currency: out_currency_c,
        out_amount: out_c,
        in_currency: in_currency_c,
        in_amount: c_swap_in,
        recipient_idx: SENTINEL_SELF,
        recipient_pool_addr: None,
        recipient_pool_repays: false,
        auto_repay: false,
        callback: vec![PlanStep::V4Unlock {
            inner: v4_inner,
            pool_manager_idx: pm_idx,
        }],
    }];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V4→V3→V2 Plan, byte-identical to `build_v4v3v2_plan`.
#[must_use]
pub fn build_v4v3v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V4(a), HopInfo::V3(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.first()?;
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
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let fwd_b = v3_forward(b);
    let fwd_c = v2_forward(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let mut at = v4_scaffold_table(inputs);
    let v3b = at.add(b.pool_address).ok()?;
    let forward_a = at.add(forward_a_cur).ok()?;
    at.add(fwd_b).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let v2c = at.add(c.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let weth = inputs.weth_address;

    let inner: Plan = vec![
        PlanStep::V4Swap {
            c0_idx: c0_a,
            c1_idx: c1_a,
            fee: fee_a,
            tick_spacing: ts_a,
            hooks_idx: SENTINEL_NATIVE,
            zfo: a.zfo,
            amount: optimal_input,
            in_currency: in_currency_a,
            in_amount: optimal_input,
            out_currency: forward_a_cur,
            out_amount: out_a,
        },
        PlanStep::FlashSwap {
            pool_idx: v3b,
            pool_addr: b.pool_address,
            protocol: Prot::V3,
            zfo: b.zfo,
            fee: u16::try_from(b.fee).ok()?,
            out_currency: fwd_b,
            out_amount: out_b,
            in_currency: forward_a_cur,
            in_amount: b_swap_in,
            recipient_idx: v2c,
            recipient_pool_addr: Some(c.pool_address),
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![
                PlanStep::V4TakeCompact {
                    currency_idx: forward_a,
                    currency_addr: forward_a_cur,
                    recipient_idx: v3b,
                    amount: out_a,
                    seeds_pool: None,
                    repays_flash: Some(b.pool_address),
                },
                PlanStep::V2SwapCalc {
                    pool_idx: v2c,
                    pool_addr: c.pool_address,
                    zfo: c.zfo,
                    recipient_idx: SENTINEL_SELF,
                    fee: c.fee,
                    out_currency: fwd_c,
                    out_amount: out_c,
                    recipient_pool_addr: None,
                    recipient_repays: false,
                },
            ],
        },
        PlanStep::V4SettleDelta {
            currency_idx: SENTINEL_WETH,
            currency_addr: weth,
        },
    ];
    let plan: Plan = vec![PlanStep::V4Unlock {
        inner,
        pool_manager_idx: pm_idx,
    }];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V2→V2→V4 Plan, byte-identical to `build_v2v2v4_plan`.
#[must_use]
pub fn build_v2v2v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V2(a), HopInfo::V2(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
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
    let fwd_a = v2_forward(a);
    let fwd_b = v2_forward(b);
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let out_c = *inputs.hop_outputs.get(2)?;
    let mut at = v4_scaffold_table(inputs);
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let v2a = at.add(a.pool_address).ok()?;
    let v2b = at.add(b.pool_address).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0 = at.add(c.currency0_address).ok()?;
    let c1 = at.add(c.currency1_address).ok()?;
    let forward_b = at.add(fwd_b).ok()?;
    let weth = inputs.weth_address;

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
            seeds_pool: Some(a.pool_address),
            repays_flash: None,
        },
        PlanStep::V2SwapCalc {
            pool_idx: v2a,
            pool_addr: a.pool_address,
            zfo: a.zfo,
            recipient_idx: v2b,
            fee: a.fee,
            out_currency: fwd_a,
            out_amount: *inputs.hop_outputs.first()?,
            recipient_pool_addr: Some(b.pool_address),
            recipient_repays: false,
        },
        PlanStep::V2SwapCalc {
            pool_idx: v2b,
            pool_addr: b.pool_address,
            zfo: b.zfo,
            recipient_idx: pm_idx,
            fee: b.fee,
            out_currency: fwd_b,
            out_amount: c_swap_in,
            recipient_pool_addr: None,
            recipient_repays: false,
        },
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
            zfo: c.zfo,
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
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V2→V4→V4 Plan, byte-identical to `build_v2v4v4_plan`.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "PM-settle V4Unlock with two V4 swaps"
)]
#[expect(
    clippy::similar_names,
    reason = "index-fidelity names mirror the reference"
)]
pub fn build_v2v4v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V2(a), HopInfo::V4(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
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
    let forward_a_cur = v2_forward(a);
    let (forward_b_cur, in_currency_b) = v4_hop_currencies(b);
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let mut at = v4_scaffold_table(inputs);
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let forward_a = at.add(forward_a_cur).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let c0_c = at.add(c.currency0_address).ok()?;
    let c1_c = at.add(c.currency1_address).ok()?;
    let v2a = at.add(a.pool_address).ok()?;
    let weth = inputs.weth_address;

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
            seeds_pool: Some(a.pool_address),
            repays_flash: None,
        },
        PlanStep::V2SwapCalc {
            pool_idx: v2a,
            pool_addr: a.pool_address,
            zfo: a.zfo,
            recipient_idx: pm_idx,
            fee: a.fee,
            out_currency: forward_a_cur,
            out_amount: b_swap_in,
            recipient_pool_addr: None,
            recipient_repays: false,
        },
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
            zfo: b.zfo,
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
            zfo: c.zfo,
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
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V2→V3→V2 Plan, byte-identical to `build_v2v3v2_plan`.
#[must_use]
pub fn build_v2v3v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V2(a), HopInfo::V3(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.first()?;
    let out_c = *inputs.hop_outputs.get(2)?;
    guard_no_zeroed_output(inputs)?;
    if !fits_i128(optimal_input) {
        return None;
    }
    let b_swap_in = checked_swap_input(inputs, 1)?;
    let c_swap_in = checked_swap_input(inputs, 2)?;
    let fwd_a = v2_forward(a);
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let fwd_c = v2_forward(c);
    let in_c = if c.zfo {
        c.token0_address
    } else {
        c.token1_address
    };
    let out_b = *inputs.hop_outputs.get(1)?;
    let mut at = seed_address_table(inputs);
    at.add(fwd_a).ok()?;
    let v2a = at.add(a.pool_address).ok()?;
    let v2c = at.add(c.pool_address).ok()?;
    let v3b = at.add(b.pool_address).ok()?;
    let weth = inputs.weth_address;

    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v2c,
            pool_addr: c.pool_address,
            protocol: Prot::V2,
            zfo: c.zfo,
            fee: c.fee,
            out_currency: fwd_c,
            out_amount: out_c,
            in_currency: in_c,
            in_amount: c_swap_in,
            recipient_idx: SENTINEL_SELF,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![PlanStep::FlashSwap {
                pool_idx: v3b,
                pool_addr: b.pool_address,
                protocol: Prot::V3,
                zfo: b.zfo,
                fee: u16::try_from(b.fee).ok()?,
                out_currency: fwd_b,
                out_amount: out_b,
                in_currency: in_b,
                in_amount: b_swap_in,
                recipient_idx: v2c,
                recipient_pool_addr: Some(c.pool_address),
                recipient_pool_repays: true,
                auto_repay: false,
                callback: vec![
                    PlanStep::Erc20Transfer {
                        token_idx: SENTINEL_WETH,
                        token_addr: weth,
                        recipient_idx: v2a,
                        amount: optimal_input,
                        seeds_pool: Some(a.pool_address),
                        repays_flash: None,
                    },
                    PlanStep::V2SwapDirect {
                        pool_idx: v2a,
                        pool_addr: a.pool_address,
                        zfo: a.zfo,
                        out_amount: out_a,
                        recipient_idx: v3b,
                        out_currency: fwd_a,
                        recipient_pool_addr: Some(b.pool_address),
                        recipient_repays: true,
                    },
                ],
            }],
        },
    ];
    Some(finish_plan(at, plan))
}

/// The 3-hop V2→V3→V3 Plan, byte-identical to `build_v2v3v3_plan`.
#[must_use]
pub fn build_v2v3v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V2(a), HopInfo::V3(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.first()?;
    guard_no_zeroed_output(inputs)?;
    if !fits_i128(optimal_input) {
        return None;
    }
    let b_swap_in = checked_swap_input(inputs, 1)?;
    let c_swap_in = checked_swap_input(inputs, 2)?;
    let fwd_a = v2_forward(a);
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let fwd_c = v3_forward(c);
    let in_c = v3_input(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let mut at = seed_address_table(inputs);
    let v2a = at.add(a.pool_address).ok()?;
    let v3b = at.add(b.pool_address).ok()?;
    let v3c = at.add(c.pool_address).ok()?;
    let weth = inputs.weth_address;

    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v3c,
            pool_addr: c.pool_address,
            protocol: Prot::V3,
            zfo: c.zfo,
            fee: u16::try_from(c.fee).ok()?,
            out_currency: fwd_c,
            out_amount: out_c,
            in_currency: in_c,
            in_amount: c_swap_in,
            recipient_idx: SENTINEL_SELF,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![PlanStep::FlashSwap {
                pool_idx: v3b,
                pool_addr: b.pool_address,
                protocol: Prot::V3,
                zfo: b.zfo,
                fee: u16::try_from(b.fee).ok()?,
                out_currency: fwd_b,
                out_amount: out_b,
                in_currency: in_b,
                in_amount: b_swap_in,
                recipient_idx: v3c,
                recipient_pool_addr: Some(c.pool_address),
                recipient_pool_repays: true,
                auto_repay: false,
                callback: vec![
                    PlanStep::Erc20Transfer {
                        token_idx: SENTINEL_WETH,
                        token_addr: weth,
                        recipient_idx: v2a,
                        amount: optimal_input,
                        seeds_pool: Some(a.pool_address),
                        repays_flash: None,
                    },
                    PlanStep::V2SwapDirect {
                        pool_idx: v2a,
                        pool_addr: a.pool_address,
                        zfo: a.zfo,
                        out_amount: out_a,
                        recipient_idx: v3b,
                        out_currency: fwd_a,
                        recipient_pool_addr: Some(b.pool_address),
                        recipient_repays: true,
                    },
                ],
            }],
        },
    ];
    Some(finish_plan(at, plan))
}

/// The 3-hop V2→V3→V4 Plan, byte-identical to `build_v2v3v4_plan`.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "V4 unlock in a V3 flash callback + V2SwapDirect"
)]
pub fn build_v2v3v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V2(a), HopInfo::V3(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.first()?;
    let out_c = *inputs.hop_outputs.get(2)?;
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
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let fwd_a = v2_forward(a);
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let mut at = v4_scaffold_table(inputs);
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let v2a = at.add(a.pool_address).ok()?;
    let v3b = at.add(b.pool_address).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0 = at.add(c.currency0_address).ok()?;
    let c1 = at.add(c.currency1_address).ok()?;
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
            zfo: c.zfo,
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
            seeds_pool: Some(a.pool_address),
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
    let plan: Plan = vec![
        PlanStep::V4Sync {
            currency_idx: forward_b,
            currency_addr: fwd_b,
        },
        PlanStep::FlashSwap {
            pool_idx: v3b,
            pool_addr: b.pool_address,
            protocol: Prot::V3,
            zfo: b.zfo,
            fee: u16::try_from(b.fee).ok()?,
            out_currency: fwd_b,
            out_amount: out_b,
            in_currency: in_b,
            in_amount: b_swap_in,
            recipient_idx: pm_idx,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![
                PlanStep::V4Unlock {
                    inner: v4_inner,
                    pool_manager_idx: pm_idx,
                },
                PlanStep::V2SwapDirect {
                    pool_idx: v2a,
                    pool_addr: a.pool_address,
                    zfo: a.zfo,
                    out_amount: out_a,
                    recipient_idx: v3b,
                    out_currency: fwd_a,
                    recipient_pool_addr: Some(b.pool_address),
                    recipient_repays: true,
                },
            ],
        },
    ];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V3→V2→V2 Plan, byte-identical to `build_v3v2v2_plan`.
#[must_use]
pub fn build_v3v2v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V3(a), HopInfo::V2(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    guard_no_zeroed_output(inputs)?;
    if !fits_i128(optimal_input) {
        return None;
    }
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let fwd_b = v2_forward(b);
    let fwd_c = v2_forward(c);
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let mut at = seed_address_table(inputs);
    let v2b = at.add(b.pool_address).ok()?;
    let v2c = at.add(c.pool_address).ok()?;
    let v3a = at.add(a.pool_address).ok()?;
    let weth = inputs.weth_address;

    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v3a,
            pool_addr: a.pool_address,
            protocol: Prot::V3,
            zfo: a.zfo,
            fee: u16::try_from(a.fee).ok()?,
            out_currency: fwd_a,
            out_amount: out_a,
            in_currency: in_a,
            in_amount: optimal_input,
            recipient_idx: v2b,
            recipient_pool_addr: Some(b.pool_address),
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![
                PlanStep::V2SwapCalc {
                    pool_idx: v2b,
                    pool_addr: b.pool_address,
                    zfo: b.zfo,
                    recipient_idx: v2c,
                    fee: b.fee,
                    out_currency: fwd_b,
                    out_amount: out_b,
                    recipient_pool_addr: Some(c.pool_address),
                    recipient_repays: false,
                },
                PlanStep::V2SwapCalc {
                    pool_idx: v2c,
                    pool_addr: c.pool_address,
                    zfo: c.zfo,
                    recipient_idx: SENTINEL_SELF,
                    fee: c.fee,
                    out_currency: fwd_c,
                    out_amount: out_c,
                    recipient_pool_addr: None,
                    recipient_repays: false,
                },
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v3a,
                    amount: optimal_input,
                    seeds_pool: None,
                    repays_flash: Some(a.pool_address),
                },
            ],
        },
    ];
    Some(finish_plan(at, plan))
}

/// The 3-hop V3→V2→V4 Plan, byte-identical to `build_v3v2v4_plan`.
#[must_use]
#[expect(clippy::too_many_lines, reason = "V4 unlock inside V2/V3 flashes")]
pub fn build_v3v2v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V3(a), HopInfo::V2(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
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
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let fwd_b = v2_forward(b);
    let in_b = if b.zfo {
        b.token0_address
    } else {
        b.token1_address
    };
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let mut at = v4_scaffold_table(inputs);
    at.add(inputs.pool_manager_address).ok()?;
    let v3a = at.add(a.pool_address).ok()?;
    let v2b = at.add(b.pool_address).ok()?;
    let forward_b = at.add(fwd_b).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0 = at.add(c.currency0_address).ok()?;
    let c1 = at.add(c.currency1_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let weth = inputs.weth_address;

    let v4_inner: Plan = vec![
        PlanStep::V4Swap {
            c0_idx: c0,
            c1_idx: c1,
            fee: fee_c,
            tick_spacing: ts_c,
            hooks_idx: SENTINEL_NATIVE,
            zfo: c.zfo,
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
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v3a,
            pool_addr: a.pool_address,
            protocol: Prot::V3,
            zfo: a.zfo,
            fee: u16::try_from(a.fee).ok()?,
            out_currency: fwd_a,
            out_amount: out_a,
            in_currency: in_a,
            in_amount: optimal_input,
            recipient_idx: v2b,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![
                PlanStep::FlashSwap {
                    pool_idx: v2b,
                    pool_addr: b.pool_address,
                    protocol: Prot::V2,
                    zfo: b.zfo,
                    fee: b.fee,
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
                },
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v3a,
                    amount: optimal_input,
                    seeds_pool: None,
                    repays_flash: Some(a.pool_address),
                },
            ],
        },
    ];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V3→V3→V4 Plan, byte-identical to `build_v3v3v4_plan`.
#[must_use]
#[expect(clippy::too_many_lines, reason = "V4 unlock inside nested V3 flashes")]
pub fn build_v3v3v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V3(a), HopInfo::V3(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
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
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let out_c = *inputs.hop_outputs.get(2)?;
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let mut at = v4_scaffold_table(inputs);
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let v3a = at.add(a.pool_address).ok()?;
    let v3b = at.add(b.pool_address).ok()?;
    let forward_b = at.add(fwd_b).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0 = at.add(c.currency0_address).ok()?;
    let c1 = at.add(c.currency1_address).ok()?;
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
            zfo: c.zfo,
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
            repays_flash: Some(a.pool_address),
        },
        PlanStep::V4SettleAll,
    ];
    let plan: Plan = vec![
        PlanStep::V4Sync {
            currency_idx: forward_b,
            currency_addr: fwd_b,
        },
        PlanStep::FlashSwap {
            pool_idx: v3b,
            pool_addr: b.pool_address,
            protocol: Prot::V3,
            zfo: b.zfo,
            fee: u16::try_from(b.fee).ok()?,
            out_currency: fwd_b,
            out_amount: out_b,
            in_currency: in_b,
            in_amount: b_swap_in,
            recipient_idx: pm_idx,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![PlanStep::FlashSwap {
                pool_idx: v3a,
                pool_addr: a.pool_address,
                protocol: Prot::V3,
                zfo: a.zfo,
                fee: u16::try_from(a.fee).ok()?,
                out_currency: fwd_a,
                out_amount: out_a,
                in_currency: in_a,
                in_amount: optimal_input,
                recipient_idx: v3b,
                recipient_pool_addr: Some(b.pool_address),
                recipient_pool_repays: true,
                auto_repay: false,
                callback: vec![PlanStep::V4Unlock {
                    inner: v4_inner,
                    pool_manager_idx: pm_idx,
                }],
            }],
        },
    ];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V2→V4→V2 Plan, byte-identical to `build_v2v4v2_plan`.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "V4 unlock inside the terminal V2 flash"
)]
#[expect(
    clippy::similar_names,
    reason = "index-fidelity names mirror the reference"
)]
pub fn build_v2v4v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V2(a), HopInfo::V4(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
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
    let forward_a_cur = v2_forward(a);
    let (forward_b_cur, in_currency_b) = v4_hop_currencies(b);
    let fwd_c = v2_forward(c);
    let in_c = if c.zfo {
        c.token0_address
    } else {
        c.token1_address
    };
    let mut at = v4_scaffold_table(inputs);
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let forward_a = at.add(forward_a_cur).ok()?;
    let forward_b = at.add(forward_b_cur).ok()?;
    let v2a = at.add(a.pool_address).ok()?;
    let v2c = at.add(c.pool_address).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let weth = inputs.weth_address;

    let v4_inner: Plan = vec![
        PlanStep::V4Sync {
            currency_idx: forward_a,
            currency_addr: forward_a_cur,
        },
        PlanStep::V2SwapCalc {
            pool_idx: v2a,
            pool_addr: a.pool_address,
            zfo: a.zfo,
            recipient_idx: pm_idx,
            fee: a.fee,
            out_currency: forward_a_cur,
            out_amount: b_swap_in,
            recipient_pool_addr: None,
            recipient_repays: false,
        },
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
            zfo: b.zfo,
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
            repays_flash: Some(c.pool_address),
        },
        PlanStep::V4SettleDelta {
            currency_idx: forward_a,
            currency_addr: forward_a_cur,
        },
    ];
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v2c,
            pool_addr: c.pool_address,
            protocol: Prot::V2,
            zfo: c.zfo,
            fee: c.fee,
            out_currency: fwd_c,
            out_amount: out_c,
            in_currency: in_c,
            in_amount: c_swap_in,
            recipient_idx: SENTINEL_SELF,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v2a,
                    amount: optimal_input,
                    seeds_pool: Some(a.pool_address),
                    repays_flash: None,
                },
                PlanStep::V4Unlock {
                    inner: v4_inner,
                    pool_manager_idx: pm_idx,
                },
            ],
        },
    ];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V2→V4→V3 Plan, byte-identical to `build_v2v4v3_plan`.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "V4 unlock inside the terminal V3 flash"
)]
#[expect(
    clippy::similar_names,
    reason = "index-fidelity names mirror the reference"
)]
pub fn build_v2v4v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V2(a), HopInfo::V4(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
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
    let forward_a_cur = v2_forward(a);
    let (forward_b_cur, in_currency_b) = v4_hop_currencies(b);
    let fwd_c = v3_forward(c);
    let in_c = v3_input(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let mut at = v4_scaffold_table(inputs);
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let forward_a = at.add(forward_a_cur).ok()?;
    let forward_b = at.add(forward_b_cur).ok()?;
    let v2a = at.add(a.pool_address).ok()?;
    let v3c = at.add(c.pool_address).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let weth = inputs.weth_address;

    let v4_inner: Plan = vec![
        PlanStep::V4Sync {
            currency_idx: forward_a,
            currency_addr: forward_a_cur,
        },
        PlanStep::V2SwapCalc {
            pool_idx: v2a,
            pool_addr: a.pool_address,
            zfo: a.zfo,
            recipient_idx: pm_idx,
            fee: a.fee,
            out_currency: forward_a_cur,
            out_amount: v4_swap_in,
            recipient_pool_addr: None,
            recipient_repays: false,
        },
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
            zfo: b.zfo,
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
            repays_flash: Some(c.pool_address),
        },
        PlanStep::V4SettleDelta {
            currency_idx: forward_a,
            currency_addr: forward_a_cur,
        },
    ];
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::FlashSwap {
            pool_idx: v3c,
            pool_addr: c.pool_address,
            protocol: Prot::V3,
            zfo: c.zfo,
            fee: u16::try_from(c.fee).ok()?,
            out_currency: fwd_c,
            out_amount: out_c,
            in_currency: in_c,
            in_amount: c_swap_in,
            recipient_idx: SENTINEL_SELF,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v2a,
                    amount: optimal_input,
                    seeds_pool: Some(a.pool_address),
                    repays_flash: None,
                },
                PlanStep::V4Unlock {
                    inner: v4_inner,
                    pool_manager_idx: pm_idx,
                },
            ],
        },
    ];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V3→V4→V2 Plan, byte-identical to `build_v3v4v2_plan`.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "V4 unlock inside the leading V3 flash"
)]
pub fn build_v3v4v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V3(a), HopInfo::V4(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
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
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let (forward_b_cur, in_currency_b) = v4_hop_currencies(b);
    let fwd_c = v2_forward(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let out_a = *inputs.hop_outputs.first()?;
    let mut at = v4_scaffold_table(inputs);
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let v3a = at.add(a.pool_address).ok()?;
    let v2c = at.add(c.pool_address).ok()?;
    let forward_a = at.add(fwd_a).ok()?;
    let forward_b = at.add(forward_b_cur).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
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
            zfo: b.zfo,
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
            seeds_pool: Some(c.pool_address),
        },
        PlanStep::V4SettleAll,
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
        PlanStep::FlashSwap {
            pool_idx: v3a,
            pool_addr: a.pool_address,
            protocol: Prot::V3,
            zfo: a.zfo,
            fee: u16::try_from(a.fee).ok()?,
            out_currency: fwd_a,
            out_amount: out_a,
            in_currency: in_a,
            in_amount: optimal_input,
            recipient_idx: pm_idx,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![
                PlanStep::V4Unlock {
                    inner: v4_inner,
                    pool_manager_idx: pm_idx,
                },
                PlanStep::V2SwapCalc {
                    pool_idx: v2c,
                    pool_addr: c.pool_address,
                    zfo: c.zfo,
                    recipient_idx: SENTINEL_SELF,
                    fee: c.fee,
                    out_currency: fwd_c,
                    out_amount: out_c,
                    recipient_pool_addr: None,
                    recipient_repays: false,
                },
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v3a,
                    amount: optimal_input,
                    seeds_pool: None,
                    repays_flash: Some(a.pool_address),
                },
            ],
        },
    ];
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// The 3-hop V3→V4→V4 Plan, byte-identical to `build_v3v4v4_plan`.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "V4 unlock with two V4 swaps inside the V3 flash"
)]
pub fn build_v3v4v4_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    guard_arity(path.hops.len(), 3)?;
    let (HopInfo::V3(a), HopInfo::V4(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
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
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let (forward_b_cur, in_currency_b) = v4_hop_currencies(b);
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let out_a = *inputs.hop_outputs.first()?;
    let mut at = v4_scaffold_table(inputs);
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let v3a = at.add(a.pool_address).ok()?;
    let forward_a = at.add(fwd_a).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let c0_c = at.add(c.currency0_address).ok()?;
    let c1_c = at.add(c.currency1_address).ok()?;
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
            zfo: b.zfo,
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
            zfo: c.zfo,
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
    let plan: Plan = vec![
        PlanStep::SelfFund {
            currency: weth,
            amount: optimal_input,
        },
        PlanStep::V4Sync {
            currency_idx: forward_a,
            currency_addr: fwd_a,
        },
        PlanStep::FlashSwap {
            pool_idx: v3a,
            pool_addr: a.pool_address,
            protocol: Prot::V3,
            zfo: a.zfo,
            fee: u16::try_from(a.fee).ok()?,
            out_currency: fwd_a,
            out_amount: out_a,
            in_currency: in_a,
            in_amount: optimal_input,
            recipient_idx: pm_idx,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback: vec![
                PlanStep::Erc20Transfer {
                    token_idx: SENTINEL_WETH,
                    token_addr: weth,
                    recipient_idx: v3a,
                    amount: optimal_input,
                    seeds_pool: None,
                    repays_flash: Some(a.pool_address),
                },
                PlanStep::V4Unlock {
                    inner: v4_inner,
                    pool_manager_idx: pm_idx,
                },
            ],
        },
    ];
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
