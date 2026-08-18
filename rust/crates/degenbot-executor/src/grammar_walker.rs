//! ADR-031 D6 — the sole facts-driven Plan producer (epic `6SU5LM`).
//!
//! The pipeline has three stages:
//!
//! 1. **Hop facts** (data): per-protocol [`HopFacts`] descriptors produced by
//!    [`hop_facts`] (the default per-hop mapping) or one of five per-position
//!    override fns (`facts_of_*`). [`facts_for`] is the single dispatcher that
//!    routes each path to the right facts source.
//! 2. **Mechanics** (code): [`derive_plan`] is the shape dispatcher over the
//!    per-enclosure-block modules in `grammar_walker/shapes/*.rs` — it reads
//!    the `Repay`/`OutDest` facts tags to determine which `FlashSwap`/
//!    `V4Unlock` wraps which, the repayment order, and the capture arms.
//! 3. **Validator gate**: lives in `grammar_shape` (`derive_shape_detailed`):
//!    build → `plan_to_ledger_ops` → `LedgerValidator::validate_full` →
//!    `preamble + plan_to_bytes`.
//!
//! [`build_walk`] is the single pipeline entry: `facts_for` → `derive_plan`
//! → `enc_preamble`, returning `(preamble, plan, at)`. The
//! `LedgerValidator` gate (one representation) is reused unchanged: the
//! walker emits exactly one `Plan`, and the encoder + validator are pure
//! functions of it. Structural + behavioral parity with the pre-refactor
//! reference producer is pinned by the revm contract matrix + the
//! `spike_derivation` golden suite.

use crate::composers::{ComposerInputs, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo};
use crate::encoders::AddressTable;
use crate::grammar_ledger::Prot;
use crate::grammar_plan::{v2_forward, v3_forward, v3_input, Plan};
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

/// The terminal-form axis (T5 / PZBGP7): how the trailing hop of a
/// V4-containing 3-hop shape completes its stream. `DirectHandoff` — the
/// trailing swap completes on its own pool and hands output to SELF (the
/// v3v4v2 trailing `v2_swap`). `UnlockInternal` — the trailing swap is an op
/// inside the enclosing V4Unlock's inner (the v3v4v4 trailing V4Swap); the
/// unlock's ledger settlements are sequenced by the shape, not by the
/// trailing hop alone.
///
/// `None` for every non-terminal hop and for terminal hops in shapes that
/// don't consume the axis. Set exactly once per family, by the facts
/// dispatcher's terminal-position override; consumed only by the merged
/// v3v4[v2|v4] arm in [`shapes::three_hop`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalForm {
    /// The trailing swap is a direct pool swap; its output leaves the
    /// unlock's accounting.
    DirectHandoff,
    /// The trailing swap lives inside the enclosing V4Unlock's inner; its
    /// deltas settle through the unlock.
    UnlockInternal,
}

/// The **repay-mechanism** axis (T6c / PZBGP7): how a flash hop's borrowed
/// input is repaid, AND the timing of the draw relative to the callback.
/// The existing [`Repay`] tag fixes the *obligation category* (who owes what)
/// but is identical for the V2 flash in `v3v2v4` (forward nest, draws the
/// repay at borrow — `auto_repay=true`) and the V2 flash in `v2v4v2`
/// (reverse nest, repays in-callback). This sub-fact disambiguates the two —
/// only the forward-nesting family needs it.
///
/// `None` (the default) for every flash hop except where a consumer family
/// needs the timing distinction: scoped exactly like [`TerminalForm`]. Set
/// only by the facts dispatcher's per-position override; consumed only by the
/// group-C arm of [`shapes::three_hop`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RepayMechanism {
    /// The flash draws its repay at borrow (pre-callback) — `auto_repay=true`.
    /// Only the `v3v2v4` V2 flash (the sole forward-nested arm): the seeder
    /// flash must run first (outer), so the leading V3 flash wraps it.
    AutoFromExecutor,
    /// Repaid by an explicit transfer inside the flash's own callback.
    TransferInCallback,
    /// Repaid by a `V4TakeCompact`/`V4TakeDelta` inside the enclosing V4Unlock.
    V4TakeInUnlock,
    /// The repay currency is delivered by a downstream flash's forward.
    DownstreamFlashDelivery,
    /// The repay currency is delivered by a downstream non-flash take (seed).
    DownstreamTakeSeeds,
}

/// The **seed-delivery** axis (T6c / PZBGP7): how a WETH prefund (the optimal
/// seed that funds a leading V2/V3 calc) is emitted. `Erc20Transfer` (the
/// default) is the plain pre-callback transfer; `V4TakeCompact` emits the
/// prefund as a `V4TakeCompact` *inside* the active V4Unlock's delta ledger
/// (because the seed currency is a V4-managed WETH delta), plus a matching
/// profit-take to SELF. Only the `v2v3v4` family needs the V4-ledger variant.
///
/// `None` (the default) for every hop except where a consumer family needs
/// it: scoped exactly like [`TerminalForm`]. Set only by the facts
/// dispatcher's per-position override; consumed only by the group-C arm of
/// [`shapes::three_hop`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeedDelivery {
    /// The prefund is a plain `Erc20Transfer` in the flash callback.
    Erc20Transfer,
    /// The prefund is a `V4TakeCompact` inside the enclosing V4Unlock.
    V4TakeCompact,
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
    /// [`TerminalForm`] — set on the terminal hop only when a shape consumes it (see enum).
    pub terminal_form: Option<TerminalForm>,
    /// [`RepayMechanism`] — set on a flash hop only when a shape needs the
    /// repay timing distinction (see enum). `None` everywhere else.
    pub repay_mechanism: Option<RepayMechanism>,
    /// [`SeedDelivery`] — set on the seeded hop only when a shape needs the
    /// prefund-mechanism distinction (see enum). `None` everywhere else.
    pub seed_delivery: Option<SeedDelivery>,
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

    /// The V3 flash-swap step, built from the hop's facts. `pool_address` +
    /// `zfo` come from the facts. Recipient routing: `None` derives from
    /// `facts.out_dest` (Executor → SELF, PoolManager → PM); `Some((idx,
    /// pool_addr, pool_repays))` sets it explicitly — the 3-hop nested-flash
    /// families (T5) route a flash's repayment to a downstream recipient pool
    /// (`pool_repays`), which the out-derivation cannot express.
    /// Single primitive since T2 (5AZSLE, epic 6SWFBS) folded the old
    /// `v3_flash`/`v3_flash_to` pair; byte-identity pinned by the glopcn
    /// goldens.
    pub fn v3_flash(
        at: &mut AddressTable,
        facts: &HopFacts,
        out_amount: u128,
        in_amount: u128,
        auto_repay: bool,
        recipient: Option<(u8, Option<Address>, bool)>,
        callback: Vec<PlanStep>,
    ) -> Option<PlanStep> {
        let (recipient_idx, recipient_pool_addr, recipient_pool_repays) = match recipient {
            Some(r) => r,
            None => match facts.out_dest {
                OutDest::Executor => (SENTINEL_SELF, None, false),
                OutDest::PoolManager => (SENTINEL_PM, None, false),
                OutDest::Repay(_) => unreachable!("V3 hop never repays a pool here"),
            },
        };
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
        terminal_form: None,
        repay_mechanism: None,
        seed_delivery: None,
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
        terminal_form: None,
        repay_mechanism: None,
        seed_delivery: None,
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
        terminal_form: None,
        repay_mechanism: None,
        seed_delivery: None,
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
            terminal_form: None,
            repay_mechanism: None,
            seed_delivery: None,
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
            terminal_form: None,
            repay_mechanism: None,
            seed_delivery: None,
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
                terminal_form: None,
                repay_mechanism: None,
                seed_delivery: None,
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
            terminal_form: None,
            repay_mechanism: None,
            seed_delivery: None,
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
            terminal_form: None,
            repay_mechanism: None,
            seed_delivery: None,
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
            terminal_form: None,
            repay_mechanism: None,
            seed_delivery: None,
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
            terminal_form: None,
            repay_mechanism: None,
            seed_delivery: None,
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
        terminal_form: None,
        repay_mechanism: None,
        seed_delivery: None,
        currency0_address: h.token0_address,
        currency1_address: h.token1_address,
    }
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
        terminal_form: None,
        repay_mechanism: None,
        seed_delivery: None,
        currency0_address: h.token0_address,
        currency1_address: h.token1_address,
    }
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
        terminal_form: None,
        repay_mechanism: None,
        seed_delivery: None,
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

/// Map a [`HopInfo`] to its [`HopFacts`] using the default per-protocol
/// mapping: V2→[`v2_hop_facts`], V3→[`v3_hop_facts`], V4→[`v4_hop_facts_netzero`].
/// Used by [`facts_for`] for families without per-position overrides.
fn hop_facts(h: &HopInfo) -> HopFacts {
    match h {
        HopInfo::V2(a) => v2_hop_facts(a),
        HopInfo::V3(a) => v3_hop_facts(a),
        HopInfo::V4(a) => v4_hop_facts_netzero(a),
    }
}

/// Whether the 3-tuple key `(Option<Prot>, Option<Prot>, Option<Prot>)`
/// corresponds to a recognized family shape. Membership is **key-based and
/// len-agnostic** — a `len ≥ 4` path whose first 3 prots match a 3-hop arm
/// still yields `true` (preserving the `family_axis_support` presence check:
/// the axis surface is declared even though the facts dispatcher may decline
/// on arity). This mirrors the old `build_for_walk` table: every listed arm
/// has `Some` in slots 1–2.
pub(crate) fn recognized_key(key: (Option<Prot>, Option<Prot>, Option<Prot>)) -> bool {
    matches!(key, (Some(_), Some(_), _))
}

/// The protocol of a hop. (Local mirror of `grammar_shape::prot_of`'s
/// single-hop form, kept here so `facts_for` is self-contained.)
fn hop_prot(h: &HopInfo) -> Prot {
    match h {
        HopInfo::V2(_) => Prot::V2,
        HopInfo::V3(_) => Prot::V3,
        HopInfo::V4(_) => Prot::V4,
    }
}

/// The single facts dispatcher (ADR-031 D6). Routes a path to its hop facts:
///
/// - All-V2 (≥2 hops) → [`facts_of_all_v2`] (the funding-branched any-N family).
/// - The five per-position override families (`v3v4v3`, `v2v3`, `v3v3`, `v3v2`)
///   → their explicit facts fn.
/// - All other 2-hop and 3-hop families → per-hop [`hop_facts`] (the default
///   mapping: V4 hops tagged `Repay::NetZero`).
/// - `len < 2` or `len > 3` (non-all-V2) → `None` (no producer).
pub(crate) fn facts_for(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<HopFacts>> {
    let n = path.hops.len();
    let prots: Vec<Prot> = path.hops.iter().map(hop_prot).collect();
    if n >= 2 && prots.iter().all(|p| *p == Prot::V2) {
        return facts_of_all_v2(path, inputs);
    }
    // The terminal hop's terminal_form is the axis the merged v3v4[v2|v4] arm
    // routes on: a trailing V2 swap hands output to SELF directly (DirectHandoff),
    // a trailing V4 swap settles inside the enclosing V4Unlock (UnlockInternal).
    // Every other position carries None.
    let mut facts = match prots.as_slice() {
        [Prot::V3, Prot::V4, Prot::V3] => facts_of_v3v4v3(path, inputs)?,
        [Prot::V2, Prot::V3] => facts_of_v2v3(path, inputs)?,
        [Prot::V3, Prot::V3] => facts_of_v3v3(path, inputs)?,
        [Prot::V3, Prot::V2] => facts_of_v3v2(path, inputs)?,
        _ if (2..=3).contains(&n) => path.hops.iter().map(hop_facts).collect(),
        _ => return None,
    };
    if prots.len() == 3 && prots[0] == Prot::V3 && prots[1] == Prot::V4 {
        let form = match prots[2] {
            Prot::V2 => Some(TerminalForm::DirectHandoff),
            Prot::V4 => Some(TerminalForm::UnlockInternal),
            Prot::V3 => None, // v3v4v3 stays with the residual tag partition
        };
        if let Some(form) = form {
            facts[2].terminal_form = Some(form);
        }
    }
    // ── T6c new-fact overrides (scoped like terminal_form: only the two
    // group-C holdout families carry Some; every other hop stays None).
    //
    // v3v2v4: the V2 flash (hop1) draws its repay at borrow
    // (`auto_repay=true`) — the sole forward-nested arm. Without this timing
    // sub-fact the walker cannot tell the V2 flash here (forward, seeder
    // outer) from the V2 flash in v2v4v2 (reverse, in-callback repay);
    // both carry `Repay::SelfRefund`.
    if prots == [Prot::V3, Prot::V2, Prot::V4] {
        facts[1].repay_mechanism = Some(RepayMechanism::AutoFromExecutor);
    }
    // v2v3v4: the optimal-WETH prefund to the leading V2 pool (hop0) is
    // emitted as a `V4TakeCompact` *inside* the V4Unlock (a V4-managed WETH
    // delta), plus a matching profit-take to SELF — not the plain
    // `Erc20Transfer` every other V2-led family uses.
    if prots == [Prot::V2, Prot::V3, Prot::V4] {
        facts[0].seed_delivery = Some(SeedDelivery::V4TakeCompact);
    }
    Some(facts)
}

/// The single pipeline entry (ADR-031 D6): `facts_for` → `derive_plan` →
/// `enc_preamble`. Returns `(preamble, plan, address_table)` or `None` on a
/// routine decline. The `LedgerValidator` gate (build → `plan_to_ledger_ops`
/// → `LedgerValidator::validate_full` → `plan_to_bytes`) stays in
/// `grammar_shape::derive_shape_detailed`.
#[must_use]
pub fn build_walk(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let facts = facts_for(path, inputs)?;
    let (plan, at) = derive_plan(&facts, inputs)?;
    let preamble = crate::encoders::enc_preamble(&at);
    Some((preamble, plan, at))
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

#[cfg(test)]
mod tests {
    #![expect(clippy::cast_possible_truncation, clippy::panic)]

    use super::*;
    use crate::composers::{
        ComposerInputs, EncodeOptions, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
    };
    use alloy::primitives::{address, Address};

    const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    const WBTC: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    const PM: Address = address!("000000000004444c5dc75cB358380D2e3dE08A90");
    const EXEC: Address = address!("DeAd0000000000000000000000000000000000Be");
    const OPTIMAL: u128 = 1_000_000_000_000_000_000;
    static OUTS: [u128; 3] = [1_000_000_000_000_000_000; 3];
    static CONSUMED: [u128; 3] = [999_999_999_999_999_999; 3];

    fn combo_hops(prots: &[Prot]) -> Vec<HopInfo> {
        (0..prots.len())
            .map(|i| {
                let in_t = match i % 3 {
                    0 => WETH,
                    1 => USDC,
                    _ => WBTC,
                };
                let out_t = match (i + 1) % 3 {
                    0 => WETH,
                    1 => USDC,
                    _ => WBTC,
                };
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

    fn family_name(prots: &[Prot]) -> String {
        prots
            .iter()
            .map(|p| match p {
                Prot::V2 => "v2",
                Prot::V3 => "v3",
                Prot::V4 => "v4",
            })
            .collect()
    }

    /// Every supported family's hop facts tag its V4 hops `Repay::NetZero` —
    /// the tag the residual tag-driven partition genuinely routes on. Facts
    /// are fetched through `facts_for`, the same dispatcher the production
    /// path uses (the single facts route).
    #[test]
    fn d6_enclosure_derived_from_facts() {
        let fams = [Prot::V2, Prot::V3, Prot::V4];
        let mut netzero_missing: Vec<String> = Vec::new();

        for n in [2usize, 3] {
            for fidx in 0..fams.len().pow(n as u32) {
                let prots: Vec<Prot> = (0..n)
                    .map(|i| fams[(fidx / fams.len().pow(i as u32)) % fams.len()])
                    .collect();
                // (v2,v2) 2-hop and (v2,v2,v2) both resolve to all_v2.
                if prots == vec![Prot::V2, Prot::V2] {
                    continue;
                }
                let name = family_name(&prots);
                let path = PathInfo::new(combo_hops(&prots));
                let inputs = ComposerInputs {
                    executor_address: EXEC,
                    pool_manager_address: PM,
                    weth_address: WETH,
                    optimal_input: OPTIMAL,
                    hop_outputs: &OUTS[..n],
                    consumed_inputs: &CONSUMED[..n],
                    opts: EncodeOptions::default(),
                };
                let Some(tags) = facts_for(&path, &inputs)
                    .map(|fs| fs.iter().map(|f| f.repay).collect::<Vec<_>>())
                else {
                    continue;
                };

                let has_v4 = prots.contains(&Prot::V4);
                let v4_has_netzero = prots
                    .iter()
                    .zip(tags.iter())
                    .any(|(p, r)| *p == Prot::V4 && *r == Repay::NetZero);
                if has_v4 && !v4_has_netzero {
                    let tag_strs: Vec<String> = tags.iter().map(|r| format!("{r:?}")).collect();
                    netzero_missing.push(format!("{name} (tags=[{}])", tag_strs.join(", ")));
                }
            }
        }

        assert!(
            netzero_missing.is_empty(),
            "D6 violation — families whose V4 hops lack Repay::NetZero (the tag \
             the residual tag partition routes on):\n  {}",
            netzero_missing.join("\n  ")
        );
    }
    /// T5 (PZBGP7): the terminal-form axis — one is-terminal-only field on
    /// `HopFacts`, driving the merge of the v3v4v2/v3v4v4 pair behind a
    /// single three_hop arm body. Only the terminal hop carries Some; non-
    /// terminal positions carry None.
    #[test]
    fn terminal_form_routes_the_v3v4_pair() {
        let v2 = combo_hops(&[Prot::V3, Prot::V4, Prot::V2]);
        let v4 = combo_hops(&[Prot::V3, Prot::V4, Prot::V4]);
        let inputs = ComposerInputs {
            executor_address: EXEC,
            pool_manager_address: PM,
            weth_address: WETH,
            optimal_input: OPTIMAL,
            hop_outputs: &OUTS,
            consumed_inputs: &CONSUMED,
            opts: EncodeOptions::default(),
        };
        for (hops, expect) in [(v2, "DirectHandoff"), (v4, "UnlockInternal")] {
            let Some(facts) = facts_for(&PathInfo::new(hops), &inputs) else {
                panic!("facts exist");
            };
            assert_eq!(facts.len(), 3);
            assert!(facts[0].terminal_form.is_none());
            assert!(facts[1].terminal_form.is_none());
            let Some(tf) = facts[2].terminal_form else {
                panic!("terminal_form set on terminal");
            };
            let kind = format!("{tf:?}");
            assert!(kind.contains(expect), "got {kind}, want {expect}");
        }
    }
}
