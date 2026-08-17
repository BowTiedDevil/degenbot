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

use crate::composers::{ComposerInputs, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo};
use crate::encoders::AddressTable;
use crate::grammar_ledger::Prot;
use crate::grammar_plan::{plan_to_ledger_ops, v2_forward, v3_forward, v3_input, Plan};
use crate::grammar_shape::v4_hop_currencies;
use alloy::primitives::Address;

/// Whether an amount fits the on-chain i128 swap-input field.
fn fits_i128(v: u128) -> bool {
    v <= i128::MAX as u128
}

/// Where a hop's swap output is routed (the hop-coupling fact).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutDest {
    /// Credits the executor.
    Executor,
    /// Routed into the PoolManager (seeds the V4 unlock ledger).
    PoolManager,
    /// Taken to a pool to REPAY its flash borrow.
    Repay(Address),
}

/// How a hop's borrowed input is repaid (the repayment-obligation fact).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Repay {
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
pub struct HopFacts {
    pub prot: Prot,
    pub zfo: bool,
    pub swap_fee: u16,
    pub tick_spacing: i16,
    pub out_currency: Address,
    pub in_currency: Address,
    pub out_dest: OutDest,
    pub repay: Repay,
    /// The V2/V3 pool, or the V4 pool-manager — the mechanics' pool identity.
    pub pool_address: Address,
    /// V4 only — the pool-id hex. `None` for V2/V3.
    pub pool_id_hex: Option<String>,
    /// V4 only — currency0 / currency1.
    pub currency0_address: Address,
    pub currency1_address: Address,
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

mod shapes;

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

/// `v4_hop_facts` with `repay: Repay::NetZero` (the characteristic tag of the
/// 2-hop V4-crossing families v3v4/v2v4/v4v4/v4v3/v4v2).
pub(crate) fn v4_hop_facts_netzero(h: &V4HopInfo) -> HopFacts {
    let mut f = v4_hop_facts(h);
    f.repay = Repay::NetZero;
    f
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
    Some(vec![v4_hop_facts_netzero(a), v4_hop_facts_netzero(b)])
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
    Some(vec![v4_hop_facts_netzero(a), v2_hop_facts(b)])
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
    Some(vec![
        v4_hop_facts_netzero(a),
        v4_hop_facts_netzero(b),
        v4_hop_facts_netzero(c),
    ])
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
    Some(vec![v2_hop_facts(a), v4_hop_facts_netzero(b)])
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
    Some(vec![v3_hop_facts(a), v4_hop_facts_netzero(b)])
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
    Some(vec![v4_hop_facts_netzero(a), v3_hop_facts(b)])
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
    Some(vec![
        v4_hop_facts_netzero(a),
        v2_hop_facts(b),
        v2_hop_facts(c),
    ])
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
    Some(vec![
        v4_hop_facts_netzero(a),
        v2_hop_facts(b),
        v4_hop_facts_netzero(c),
    ])
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
    Some(vec![
        v4_hop_facts_netzero(a),
        v3_hop_facts(b),
        v3_hop_facts(c),
    ])
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
    Some(vec![
        v4_hop_facts_netzero(a),
        v3_hop_facts(b),
        v4_hop_facts_netzero(c),
    ])
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
    Some(vec![
        v4_hop_facts_netzero(a),
        v4_hop_facts_netzero(b),
        v2_hop_facts(c),
    ])
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
    Some(vec![
        v4_hop_facts_netzero(a),
        v4_hop_facts_netzero(b),
        v3_hop_facts(c),
    ])
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
    Some(vec![
        v4_hop_facts_netzero(a),
        v2_hop_facts(b),
        v3_hop_facts(c),
    ])
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
    Some(vec![
        v4_hop_facts_netzero(a),
        v3_hop_facts(b),
        v2_hop_facts(c),
    ])
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
    Some(vec![
        v2_hop_facts(a),
        v2_hop_facts(b),
        v4_hop_facts_netzero(c),
    ])
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
    Some(vec![
        v2_hop_facts(a),
        v4_hop_facts_netzero(b),
        v4_hop_facts_netzero(c),
    ])
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
    Some(vec![
        v2_hop_facts(a),
        v3_hop_facts(b),
        v4_hop_facts_netzero(c),
    ])
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
    Some(vec![
        v3_hop_facts(a),
        v2_hop_facts(b),
        v4_hop_facts_netzero(c),
    ])
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
    Some(vec![
        v3_hop_facts(a),
        v3_hop_facts(b),
        v4_hop_facts_netzero(c),
    ])
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
    Some(vec![
        v2_hop_facts(a),
        v4_hop_facts_netzero(b),
        v2_hop_facts(c),
    ])
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
    Some(vec![
        v2_hop_facts(a),
        v4_hop_facts_netzero(b),
        v3_hop_facts(c),
    ])
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
    Some(vec![
        v3_hop_facts(a),
        v4_hop_facts_netzero(b),
        v2_hop_facts(c),
    ])
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
    Some(vec![
        v3_hop_facts(a),
        v4_hop_facts_netzero(b),
        v4_hop_facts_netzero(c),
    ])
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
    let (plan, at) = derive_plan(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
#[must_use]
pub fn build_v2v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v2v3(path, inputs)?;
    let (plan, at) = derive_plan(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
#[must_use]
pub fn build_v3v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v2(path, inputs)?;
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
#[must_use]
pub fn build_v3v2v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v2v3(path, inputs)?;
    let (plan, at) = derive_plan(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
#[must_use]
pub fn build_v3v3v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v3v2(path, inputs)?;
    let (plan, at) = derive_plan(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
#[must_use]
pub fn build_v3v3v3_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v3v3(path, inputs)?;
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
#[must_use]
pub fn build_v2v3v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v2v3v2(path, inputs)?;
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
#[must_use]
pub fn build_v3v2v2_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_of_v3v2v2(path, inputs)?;
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
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
    let (plan, at) = derive_plan(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}
/// A facts-descriptor function pointer (the same signature as `facts_of_*`).
pub type FactsFn = fn(&PathInfo, &ComposerInputs<'_>) -> Option<Vec<HopFacts>>;

/// The per-family facts descriptor for the given key. This match is the
/// future single facts dispatcher (T3 collapses the twin `build_for_walk`
/// match into it); the D6 enclosure invariant in
/// `tests/facts_driven_invariant.rs` reads facts through it.
#[must_use]
pub fn family_facts(key: (Option<Prot>, Option<Prot>, Option<Prot>)) -> Option<FactsFn> {
    Some(match key {
        // ── 3-hop families (27) ──
        (Some(Prot::V4), Some(Prot::V4), Some(Prot::V4)) => facts_of_v4v4v4,
        (Some(Prot::V4), Some(Prot::V2), Some(Prot::V2)) => facts_of_v4v2v2,
        (Some(Prot::V2), Some(Prot::V2), Some(Prot::V4)) => facts_of_v2v2v4,
        (Some(Prot::V2), Some(Prot::V3), Some(Prot::V4)) => facts_of_v2v3v4,
        (Some(Prot::V3), Some(Prot::V2), Some(Prot::V4)) => facts_of_v3v2v4,
        (Some(Prot::V3), Some(Prot::V3), Some(Prot::V4)) => facts_of_v3v3v4,
        (Some(Prot::V2), Some(Prot::V4), Some(Prot::V2)) => facts_of_v2v4v2,
        (Some(Prot::V2), Some(Prot::V4), Some(Prot::V3)) => facts_of_v2v4v3,
        (Some(Prot::V3), Some(Prot::V4), Some(Prot::V2)) => facts_of_v3v4v2,
        (Some(Prot::V3), Some(Prot::V4), Some(Prot::V3)) => facts_of_v3v4v3,
        (Some(Prot::V2), Some(Prot::V4), Some(Prot::V4)) => facts_of_v2v4v4,
        (Some(Prot::V3), Some(Prot::V4), Some(Prot::V4)) => facts_of_v3v4v4,
        (Some(Prot::V4), Some(Prot::V4), Some(Prot::V2)) => facts_of_v4v4v2,
        (Some(Prot::V4), Some(Prot::V4), Some(Prot::V3)) => facts_of_v4v4v3,
        (Some(Prot::V4), Some(Prot::V2), Some(Prot::V3)) => facts_of_v4v2v3,
        (Some(Prot::V4), Some(Prot::V2), Some(Prot::V4)) => facts_of_v4v2v4,
        (Some(Prot::V4), Some(Prot::V3), Some(Prot::V2)) => facts_of_v4v3v2,
        (Some(Prot::V4), Some(Prot::V3), Some(Prot::V3)) => facts_of_v4v3v3,
        (Some(Prot::V4), Some(Prot::V3), Some(Prot::V4)) => facts_of_v4v3v4,
        (Some(Prot::V2), Some(Prot::V2), Some(Prot::V2) | None) => facts_of_all_v2,
        (Some(Prot::V2), Some(Prot::V2), Some(Prot::V3)) => facts_of_v2v2v3,
        (Some(Prot::V2), Some(Prot::V3), Some(Prot::V2)) => facts_of_v2v3v2,
        (Some(Prot::V2), Some(Prot::V3), Some(Prot::V3)) => facts_of_v2v3v3,
        (Some(Prot::V3), Some(Prot::V2), Some(Prot::V2)) => facts_of_v3v2v2,
        (Some(Prot::V3), Some(Prot::V2), Some(Prot::V3)) => facts_of_v3v2v3,
        (Some(Prot::V3), Some(Prot::V3), Some(Prot::V2)) => facts_of_v3v3v2,
        (Some(Prot::V3), Some(Prot::V3), Some(Prot::V3)) => facts_of_v3v3v3,
        // ── 2-hop families (8; third slot `None`) ──
        (Some(Prot::V4), Some(Prot::V4), None) => facts_of_v4v4,
        (Some(Prot::V4), Some(Prot::V3), None) => facts_of_v4v3,
        (Some(Prot::V3), Some(Prot::V4), None) => facts_of_v3v4,
        (Some(Prot::V4), Some(Prot::V2), None) => facts_of_v4v2,
        (Some(Prot::V2), Some(Prot::V4), None) => facts_of_v2v4,
        (Some(Prot::V2), Some(Prot::V3), None) => facts_of_v2v3,
        (Some(Prot::V3), Some(Prot::V2), None) => facts_of_v3v2,
        (Some(Prot::V3), Some(Prot::V3), None) => facts_of_v3v3,
        _ => return None,
    })
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

/// The enclosure-deriving walker (ADR-031 D3/D6).
///
/// Reads an arbitrary-length hop sequence's [`HopFacts`] and computes the
/// nesting (which `FlashSwap`/`V4Unlock` wraps which, and the repayment order)
/// per shape module under [`shapes`] — a `(len, repay-sequence)` partition,
/// with a genuine `Repay`/`OutDest`-tag partition for the single-V4-middle
/// residual. See the ADR-031 record correction (2026-08): ordering defects are
/// caught by the `LedgerValidator` + revm matrix, not unrepresentable by
/// construction.
#[must_use]
pub(crate) fn derive_plan(
    facts: &[HopFacts],
    inputs: &ComposerInputs<'_>,
) -> Option<(Plan, AddressTable)> {
    // ── Shape dispatch (D3/D6 — the enclosure shape is read from the facts).
    // Each enclosure block lives in `grammar_walker/shapes/*.rs` (one module
    // per shape); this fn is a pure (len, repay-sequence) gate dispatcher.
    if facts.len() >= 2
        && facts
            .iter()
            .all(|f| f.repay != Repay::NetZero && f.prot == Prot::V2)
    {
        return shapes::all_v2_chain::derive(facts, inputs);
    }
    if facts.len() == 2 && facts[0].repay == Repay::SelfRefund && facts[1].repay == Repay::NetZero {
        return shapes::two_hop_seed_v4::derive(facts, inputs);
    }
    if facts.len() == 2 && facts[0].repay == Repay::NetZero {
        return shapes::two_hop_v4_led::derive(facts, inputs);
    }
    if facts.len() == 3 && !facts.iter().any(|f| f.repay == Repay::Offstream) {
        return shapes::three_hop::derive(facts, inputs);
    }
    if facts.iter().all(|f| f.repay != Repay::NetZero) && facts.len() == 2 {
        return shapes::two_hop_uniswap_only::derive(facts, inputs);
    }
    shapes::tag_residual::derive(facts, inputs)
}
