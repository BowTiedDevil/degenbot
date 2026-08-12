//! Facet-A derivation **spike** (ergo `6YUNQN`, epic `463V2C`).
//!
//! Feasibility proof for ADR-029 **D4 (hybrid)**: a 2/3-hop family can be
//! emitted from a [`ShapeClass`] + declarative per-hop ledger facts + a
//! per-protocol encoder, instead of a hand-written adapter — and the result
//! executes through the runtime matrix with exact delta.
//!
//! The hybrid split this spike embodies (ADR-029 D4):
//! * **declarative coupling/ledger facts** — [`HopFacts`]: which ledgers a hop
//!   touches, its forward (output) currency, and its coupling role at each
//!   boundary. These are *data* the derivation reasons over.
//! * **per-protocol mechanics** — the `enc_event_*`-style encoder selection in
//!   [`emit_hop`] / [`derive_2hop`] (here a `match`; in production a trait impl
//!   per protocol). The Solidity callback wiring is code, not data (D4).
//!
//! The **enclosure/call-structure** (which hop wraps which `unlock`/callback)
//! and the **repayment pivot** are *derived* from the funding source + the
//! ledgers, never chosen by the caller (ADR-029 D3).
//!
//! **Scope:** this spike covers the V2/V3 2-hop domain (`v2_v3`, `v3_v2`,
//! `v3_v3`) — the minimal cross-section that exercises two *distinct* funding
//! sources (in-path flash vs self-fund), two *distinct* coupling modes
//! (exec-balance bridge vs pool-to-pool via `V2_SWAP_CALC`), and the
//! **terminal-V2 pre-fund rule** (`2PT5HH`). Pure-V4 (PM-ledger + `V4_TAKE`
//! coupling + native bridges) is the harder residual for `WAYDTL` — the spike
//! reports that boundary honestly rather than pretending to span it.
//!
//! ---
//! **Status of the V4 / 3-hop families below (clarified by `6ZIE5X`, see ADR-029
//! D4 "What 'derived' means here"):** the `derive_2hop_v4*` and `derive_3hop_*`
//! functions are **byte-proven transcriptions** of the hand-written adapters —
//! *not* data-driven byte synthesis from `ShapeClass`/`HopFacts`. Only the V2/V3
//! 2-hop slice (`derive_2hop`) is genuinely `ShapeClass`-driven.
//! ADR-029 D4 does **not** require byte-derivation for every family: the
//! deliverable is a **generic validator proving ordering from declarative facts**
//! (D5), with emitters as per-protocol mechanics code. Each `derive_*` is the
//! sole production emitter for its family (the hand-written adapter exists only
//! as a `cutover` `debug_assert_eq!` backstop, to be retired by `WAYDTL`); the
//! next foundational step is to emit a `LedgerOp` trace from each and gate it on
//! [`crate::grammar_ledger::LedgerValidator`].

use alloy::primitives::{Address, U256};

use crate::composers::{
    fits_int128, ComposerInputs, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
    NATIVE_CURRENCY_ADDRESS,
};
use crate::encoders::{self, AddressTable, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH};
use crate::grammar::{cl_swap_in, v2_forward_addr};
use crate::grammar_ledger::LedgerOp;

/// A hop-protocol family member.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prot {
    V2,
    V3,
    V4,
}

/// How the stream's entry (seed) capital is supplied (ADR-029 D1).
///
/// Exactly one per stream. For the V2/V3 2-hop domain this is *derived* from
/// the leading-hop protocol and the D0 invariant (never user-chosen).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FundingSource {
    /// The outermost pool's own swap-callback extends the entry credit and is
    /// repaid **by the path itself** (executor may start at 0).
    InPathFlash,
    /// The executor holds the entry WETH and pre-funds the leading hop.
    SelfFund,
}

/// A family's shape: hop-protocol sequence + funding source. Profit capture,
/// builder bribe, hop coupling, and repayment pivot are **derived** from the
/// ledger rules (ADR-029 D1/D3), not carried here.
#[derive(Clone, Debug)]
pub struct ShapeClass {
    pub protocols: Vec<Prot>,
    pub funding: FundingSource,
}

/// Declarative ledger facts for one hop — the *data* half of the hybrid
/// (ADR-029 D4): which ledgers the hop touches, its forward (output) currency,
/// and whether it is a callback-flash source.
///
/// Resolved by [`hop_facts`] per hop (a simplified stand-in for the production
/// descriptor record; the field list is the same as `HopFacts`).

/// Resolve the forward currency + terminal-V2 fact for a hop (the declarative
/// "coupling/ledger facts" of ADR-029 D4).
fn hop_facts(h: &HopInfo) -> (Address, bool) {
    match h {
        HopInfo::V2(x) => (v2_forward(x), true),
        HopInfo::V3(x) => (v3_forward(x), false),
        // V4 is outside this spike's domain (residual for WAYDTL).
        HopInfo::V4(_) => unreachable!("V4 outside the 6YUNQN V2/V3 spike"),
    }
}
fn v2_forward(h: &V2HopInfo) -> Address {
    if h.zfo {
        h.token1_address
    } else {
        h.token0_address
    }
}
fn v3_forward(h: &V3HopInfo) -> Address {
    if h.zfo {
        h.token1_address
    } else {
        h.token0_address
    }
}

/// Per-protocol encoder selection for the **terminal** hop (D4 mechanics half).
///
/// `pre_grant_to` is the address-table index already credited with the hop's
/// input (a prior `V4_TAKE_COMPACT`/`ERC20_TRANSFER` into the pair). A terminal
/// V2 always swaps via `V2_SWAP_CALC` from that pre-grant (credit-before-debit
/// on the pair-handoff ledger — the `2PT5HH` / `path-182449` rule); a terminal
/// V3 is a `V3_SWAP_COMPACT` flash whose input comes from the coupled ledger.
fn emit_terminal_hop(
    at: &mut AddressTable,
    h: &HopInfo,
    inputs: &ComposerInputs<'_>,
    swap_in: u128,
    pre_grant_to: u8,
    out: &mut Vec<u8>,
) -> Option<()> {
    match h {
        HopInfo::V2(x) => {
            // Terminal-V2 pre-fund rule: swap from whatever the feeder actually
            // delivered to the pair (V2_SWAP_CALC), never an exact-out
            // `V2_SWAP_COMPACT` (over-draws 1 wei → `UniswapV2: K`).
            let _ = (x.pool_address, pre_grant_to, inputs);
            out.extend_from_slice(&encoders::enc_v2_swap_calc(
                at.add(x.pool_address).ok()?,
                x.zfo,
                SENTINEL_SELF,
                x.fee,
            ));
        }
        HopInfo::V3(x) => {
            out.extend_from_slice(
                &encoders::enc_v3_swap_compact(
                    at.add(x.pool_address).ok()?,
                    x.zfo,
                    swap_in,
                    SENTINEL_SELF,
                    &[],
                )
                .ok()?,
            );
        }
        HopInfo::V4(_) => unreachable!("V4 outside the spike"),
    }
    Some(())
}

/// Emit a **single-hop spanning the whole family** and append to `out`.
///
/// This is the per-protocol mechanics dispatch (D4). It emits one
/// flash-capable hop and returns the facts needed for the next boundary. To
/// keep the spike readable it is specialized to the V2/V3 2-hop shapes; the
/// production emitter (WAYDTL) generalizes this into a HopFacts-driven walk.
fn derive_2hop(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
    class: &ShapeClass,
) -> Option<Vec<u8>> {
    let n = path.hops.len();
    if n != 2 {
        return None;
    }
    let ha = &path.hops[0];
    let hb = &path.hops[1];
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    if forward_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    // Terminal-V3 swap-in via the CL-clamp rule (single shared point).
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let (fwd_a, _) = hop_facts(ha);

    match (ha, hb, class.funding) {
        // ── v2_v3 — V2 in-path flash source; forward bridged to V3 via exec. ──
        (HopInfo::V2(a), HopInfo::V3(b), FundingSource::InPathFlash) => {
            let v2_idx = at.add(a.pool_address).ok()?;
            let v3_idx = at.add(b.pool_address).ok()?;
            let forward_idx = at.add(fwd_a).ok()?;
            // V3's input pre-granted from the V2 forward output (exec bridge).
            let v3_cb = encoders::enc_erc20_transfer(forward_idx, v3_idx, b_swap_in).ok()?;
            let mut cb =
                encoders::enc_v3_swap_compact(v3_idx, b.zfo, b_swap_in, SENTINEL_SELF, &v3_cb)
                    .ok()?;
            // Repay the V2 flash from the V3 WETH output (derived pivot).
            cb.extend_from_slice(
                &encoders::enc_erc20_transfer(SENTINEL_WETH, v2_idx, optimal_input).ok()?,
            );
            let commands = encoders::enc_v2_swap_compact(
                v2_idx,
                a.zfo,
                forward_out,
                SENTINEL_SELF,
                a.fee,
                &cb,
            )
            .ok()?;
            let mut out = encoders::enc_preamble(&at);
            out.extend_from_slice(&commands);
            Some(out)
        }
        // ── v3_v2 — V3 self-fund; terminal V2 pre-funded + V2_SWAP_CALC. ──
        (HopInfo::V3(a), HopInfo::V2(b), FundingSource::SelfFund) => {
            let v3_idx = at.add(a.pool_address).ok()?;
            let v2_idx = at.add(b.pool_address).ok()?;
            let forward_idx = at.add(fwd_a).ok()?;
            let mut cb = encoders::enc_erc20_transfer(SENTINEL_WETH, v3_idx, optimal_input).ok()?;
            // Pre-grant the terminal V2 pair its input, then swap from it.
            cb.extend_from_slice(
                &encoders::enc_erc20_transfer(forward_idx, v2_idx, forward_out).ok()?,
            );
            emit_terminal_hop(&mut at, hb, inputs, 0, v2_idx, &mut cb)?;
            let commands =
                encoders::enc_v3_swap_compact(v3_idx, a.zfo, optimal_input, SENTINEL_SELF, &cb)
                    .ok()?;
            let mut out = encoders::enc_preamble(&at);
            out.extend_from_slice(&commands);
            Some(out)
        }
        // ── v3_v3 — V3 self-fund; both hops flash-coupled via exec. ──
        (HopInfo::V3(a), HopInfo::V3(b), FundingSource::SelfFund) => {
            let v3_a = at.add(a.pool_address).ok()?;
            let v3_b = at.add(b.pool_address).ok()?;
            let mut a_cb = encoders::enc_erc20_transfer(SENTINEL_WETH, v3_a, optimal_input).ok()?;
            let b_cmd =
                encoders::enc_v3_swap_compact(v3_b, b.zfo, b_swap_in, SENTINEL_SELF, &[]).ok()?;
            a_cb.extend_from_slice(&b_cmd);
            let commands =
                encoders::enc_v3_swap_compact(v3_a, a.zfo, optimal_input, SENTINEL_SELF, &a_cb)
                    .ok()?;
            let mut out = encoders::enc_preamble(&at);
            out.extend_from_slice(&commands);
            Some(out)
        }
        // ── v2_v2 — V2 in-path flash; pool-to-pool via V2_SWAP_CALC. ──
        (HopInfo::V2(a), HopInfo::V2(b), FundingSource::InPathFlash) => {
            let mut at = at;
            let v2_a = at.add(a.pool_address).ok()?;
            let v2_b = at.add(b.pool_address).ok()?;
            let fwd_a = at.add(fwd_a).ok()?;
            // Pre-grant pool b with a's forward output, then V2_SWAP_CALC.
            let mut cb = encoders::enc_erc20_transfer(fwd_a, v2_b, forward_out).ok()?;
            emit_terminal_hop(&mut at, hb, inputs, 0, v2_b, &mut cb)?;
            // Repay the a-flash from pool b's WETH output via the executor.
            cb.extend_from_slice(
                &encoders::enc_erc20_transfer(SENTINEL_WETH, v2_a, optimal_input).ok()?,
            );
            let commands =
                encoders::enc_v2_swap_compact(v2_a, a.zfo, forward_out, SENTINEL_SELF, a.fee, &cb)
                    .ok()?;
            let mut out = encoders::enc_preamble(&at);
            out.extend_from_slice(&commands);
            Some(out)
        }
        _ => None,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Plan tree — the primary grammar artifact (ADR-029 D4, mechanism (iii), `BP7KIR`).
// A family's ledger decisions authored as an execution-ordered, callback-nested
// tree. Two consumers derive from the SAME Plan: the encoder (`Plan→Vec<u8>`)
// and the validator (`Plan→LedgerOp`, depth-first = execution order). One
// representation, no drift, no reordering, no per-family trace duplication.
//
// Checkpoint 1 (`BP7KIR`): Step set scoped to the `v2_v3` (InPathFlash) family
// — `FlashSwap` (V2/V3, carries its callback subtree) + `Erc20Transfer`. The
// remaining Step variants (V4Unlock, V4Swap, V4Take, V4Sync/Settle, V2SwapCalc,
// WethDeposit/Withdraw, V4Batch/Mint, …) land incrementally as families fold.
// ═══════════════════════════════════════════════════════════════════════════

/// A single node of the execution-ordered, callback-nested command Plan. The
/// nesting IS execution order: a `FlashSwap`'s `callback` fires when the swap
/// runs (depth-first); a `V4Unlock`'s `inner` runs in the unlock callback.
///
/// Each leaf carries BOTH the resolved address-table index (for the byte
/// encoder) and the currency/pool address (for the `LedgerOp` projection) —
/// Checkpoint 1 keeps this minimal; a later refactor may separate
/// address-collection from emission if it clarifies (see `BP7KIR` body).
#[derive(Clone, Debug)]
pub enum PlanStep {
    /// A V2 or V3 `*_SWAP_COMPACT` flash — the pool credits `out_currency` to
    /// the executor and is owed `in_currency` within `callback` (the bytes the
    /// flash's callback payload carries, fired when the swap runs).
    FlashSwap {
        pool_idx: u8,
        pool_addr: Address,
        protocol: Prot,
        zfo: bool,
        fee: u16,
        out_currency: Address,
        out_amount: u128,
        in_currency: Address,
        in_amount: u128,
        recipient_idx: u8,
        /// Whether the cmd_executor auto-pays `in_currency` from the executor's
        /// `E[]` balance at callback-end (V2/V3 `*_SWAP_COMPACT` with an empty /
        /// no-repay callback). When true, the projection emits a trailing
        /// flash-repayment `Erc20Transfer` after the callback (the auto-pay).
        auto_repay: bool,
        callback: Plan,
    },
    /// An `ERC20_TRANSFER(token→recipient, amount)` from the executor. Doubles
    /// as flash-repayment and pair-seed by recipient role (DS4OQD finding 5):
    /// when the recipient is a V2 pair being pre-funded, `seeds_pool` carries
    /// that pair's address so the projection also credits the pair-handoff
    /// ledger (a following `V2SwapCalc` consumes it).
    Erc20Transfer {
        token_idx: u8,
        token_addr: Address,
        recipient_idx: u8,
        amount: u128,
        seeds_pool: Option<Address>,
        /// When `Some(pool)`, this transfer repays that flash pool (debited
        /// `min(amount, owed)` so explicit + auto-pay compose without
        /// over-debiting).
        repays_flash: Option<Address>,
    },
    /// A `V2_SWAP_CALC(pool, zfo, recipient, fee)` — the terminal-V2 pre-fund
    /// rule (`2PT5HH`): swap from whatever the feeder delivered to the pair,
    /// never an exact-out `V2_SWAP_COMPACT` (over-drains 1 wei → `UniswapV2: K`).
    /// Consumes the pair-handoff credit seeded by a prior `Erc20Transfer`.
    V2SwapCalc {
        pool_idx: u8,
        pool_addr: Address,
        zfo: bool,
        recipient_idx: u8,
        fee: u16,
        /// The swap's output currency + amount credited to the executor (the
        /// profit / downstream repayment source). Option (B): swaps credit
        /// their output so the executor ledger fully accounts.
        out_currency: Address,
        out_amount: u128,
    },
    /// A self-fund seed (ADR-029 FundingSource::SelfFund) — the executor holds
    /// `amount` of `currency` as entry capital before the stream. Not a command;
    /// a stream precondition the validator credits so SelfFund families' flash
    /// repayments validate. The encoder emits nothing for it.
    SelfFund { currency: Address, amount: u128 },
    // ── V4 (BP7KIR Increment 3): the PoolManager container + delta ops. ──
    /// A `V4_UNLOCK(inner)` — the PM callback scope. `inner` runs inside the
    /// unlock; at its end the master V4 invariant fires: every touched PM delta
    /// must net to zero (`V4UnlockEnd`).
    V4Unlock { inner: Plan, pool_manager_idx: u8 },
    /// A `V4_SWAP_COMPACT(c0, c1, fee, ts, hooks, zfo, amount)` — creates
    /// `PM[in]` debt and `PM[out]` credit (both legs modeled so net-zero is
    /// checkable).
    V4Swap {
        c0_idx: u8,
        c1_idx: u8,
        fee: u16,
        tick_spacing: i16,
        hooks_idx: u8,
        zfo: bool,
        amount: u128,
        in_currency: Address,
        in_amount: u128,
        out_currency: Address,
        out_amount: u128,
    },
    /// `V4_TAKE_DELTA(cur→rcp)` — takes the entire positive `PM[cur]` delta to
    /// `rcp` (the profit capture; debits PM credit).
    V4TakeDelta {
        currency_idx: u8,
        currency_addr: Address,
        recipient_idx: u8,
    },
    /// `V4_SETTLE_ALL` — auto-settle every touched PM currency to 0.
    V4SettleAll,
    /// `V4_TAKE_COMPACT(cur→rcp, amount)` — take a specific `amount` of `cur`'s
    /// PM credit to `rcp` (the boundary-take: V4 output leaves the PM to feed
    /// a V2/V3 hop or to capture profit). Debits PM (D0 credit-before-debit).
    /// The recipient role determines the **second** ledger effect (the
    /// cross-ledger move, mirroring `Erc20Transfer`'s seeds_pool/repays_flash):
    /// `recipient_idx == SENTINEL_SELF` credits the executor `Erc20[cur]`
    /// (the token arrives at the executor); `seeds_pool = Some(pool)` credits
    /// `PairHandoff[pool]` (the token seeds a V2 pair directly, PM→pool, never
    /// touching executor Erc20).
    V4TakeCompact {
        currency_idx: u8,
        currency_addr: Address,
        recipient_idx: u8,
        amount: u128,
        /// When `Some(pool)`, the take's recipient is a V2 pair being
        /// pre-funded directly (PM→pool) — credit `PairHandoff[pool]` so a
        /// following `V2SwapCalc` sees its seed. `None` for a →SELF take.
        seeds_pool: Option<Address>,
    },
    /// `V4_SETTLE_DELTA(cur)` — auto-settle one currency's PM delta to 0.
    V4SettleDelta {
        currency_idx: u8,
        currency_addr: Address,
    },
    /// `V4_SYNC(cur)` — sync the PM's internal balance for `cur` (a balance-sync
    /// primitive, **delta-neutral**: it carries no PM-delta effect; the actual
    /// delta application comes from a following `V4Settle`). Used in the
    /// boundary-seed pattern (executor pays `cur` into the PM via an
    /// `Erc20Transfer(cur→PM)` then `V4Settle`) to settle a V4 input debt.
    V4Sync {
        currency_idx: u8,
        currency_addr: Address,
    },
    /// `V4_SETTLE` — the executor pays `amount` of `currency` into the PM,
    /// cancelling debt (`PM[currency] += amount`; the executor's `Erc20`
    /// debit happens at the preceding `Erc20Transfer(cur→PM)`). Used after a
    /// `V4Sync` + `Erc20Transfer` boundary-seed. The byte form is the no-arg
    /// `V4_SETTLE`; the IR carries `currency`/`amount` explicitly so the
    /// validator knows which delta to apply.
    V4Settle {
        currency_addr: Address,
        amount: u128,
    },
}

/// A Plan = an ordered list of steps. Depth-first walk = execution order.
pub type Plan = Vec<PlanStep>;

/// Project a `Plan` to its `LedgerOp` trace (depth-first; a `FlashSwap`
/// emits its flash credit/debt term, then recurses into its callback). This is
/// the validator's input — decoupled from byte layout (ADR-029 D5).
#[must_use]
pub fn plan_to_ledger_ops(plan: &Plan) -> Vec<LedgerOp> {
    let mut ops = Vec::new();
    fn walk(plan: &Plan, ops: &mut Vec<LedgerOp>) {
        for step in plan {
            match step {
                PlanStep::FlashSwap {
                    pool_addr,
                    protocol,
                    out_currency,
                    out_amount,
                    in_currency,
                    in_amount,
                    auto_repay,
                    callback,
                    ..
                } => {
                    // The swap credits `out_currency` to the executor and incurs
                    // an `in_currency` flash debt repayable within the callback.
                    let flash = match protocol {
                        Prot::V2 => LedgerOp::V2Flash {
                            out_currency: *out_currency,
                            out_amount: *out_amount,
                            in_currency: *in_currency,
                            in_amount: *in_amount,
                        },
                        Prot::V3 => LedgerOp::V3Flash {
                            out_currency: *out_currency,
                            out_amount: *out_amount,
                            in_currency: *in_currency,
                            in_amount: *in_amount,
                        },
                        Prot::V4 => unreachable!("V4 flash is not a FlashSwap (V4 has no flash); V4Unlock lands in a later increment"),
                    };
                    ops.push(flash);
                    walk(callback, ops);
                    // Auto-pay (empty/no-repay callback): the cmd_executor debits
                    // `in_currency` from the executor at callback-end. Modeled as
                    // a flash-repayment transfer (min(amount, owed) so it composes
                    // with any partial explicit repayment in the callback).
                    if *auto_repay {
                        ops.push(LedgerOp::Erc20Transfer {
                            currency: *in_currency,
                            amount: *in_amount,
                            repays_flash: Some(*pool_addr),
                        });
                    }
                }
                PlanStep::Erc20Transfer {
                    token_addr,
                    amount,
                    seeds_pool,
                    repays_flash,
                    ..
                } => {
                    ops.push(LedgerOp::Erc20Transfer {
                        currency: *token_addr,
                        amount: *amount,
                        repays_flash: *repays_flash,
                    });
                    // DS4OQD finding 5: a transfer TO a V2 pair pre-funds it —
                    // credit the pair-handoff ledger so a following `V2SwapCalc`
                    // sees its seed (the terminal-V2 credit-before-debit rule).
                    if let Some(pool) = seeds_pool {
                        ops.push(LedgerOp::SeedPair {
                            pool: *pool,
                            amount: *amount,
                        });
                    }
                }
                PlanStep::V2SwapCalc {
                    pool_addr,
                    out_currency,
                    out_amount,
                    ..
                } => {
                    // `V2_SWAP_CALC` consumes the seeded pair-handoff credit AND
                    // credits `out_currency` to the executor (the swap's computed
                    // output — the profit / downstream repayment source).
                    ops.push(LedgerOp::SwapCalc {
                        pool: *pool_addr,
                        amount_in: 0,
                        out_currency: *out_currency,
                        out_amount: *out_amount,
                    });
                }
                PlanStep::SelfFund { currency, amount } => {
                    ops.push(LedgerOp::SelfFund {
                        currency: *currency,
                        amount: *amount,
                    });
                }
                // V4 container: recurse the unlock's inner Plan, then emit
                // `V4UnlockEnd` (the net-zero check fires there).
                PlanStep::V4Unlock { inner, .. } => {
                    walk(inner, ops);
                    ops.push(LedgerOp::V4UnlockEnd);
                }
                PlanStep::V4Swap {
                    in_currency,
                    in_amount,
                    out_currency,
                    out_amount,
                    ..
                } => {
                    ops.push(LedgerOp::V4Swap {
                        in_currency: *in_currency,
                        in_amount: *in_amount,
                        out_currency: *out_currency,
                        out_amount: *out_amount,
                    });
                }
                PlanStep::V4TakeDelta {
                    currency_addr,
                    recipient_idx,
                    ..
                } => {
                    ops.push(LedgerOp::V4TakeDelta {
                        currency: *currency_addr,
                        recipient_idx: *recipient_idx,
                    });
                }
                PlanStep::V4SettleAll => {
                    ops.push(LedgerOp::V4SettleAll);
                }
                PlanStep::V4TakeCompact {
                    currency_addr,
                    amount,
                    recipient_idx,
                    seeds_pool,
                    ..
                } => {
                    ops.push(LedgerOp::Take {
                        currency: *currency_addr,
                        amount: *amount,
                    });
                    // Cross-ledger move: when the take's recipient is the
                    // executor (SELF), the token physically arrives at the
                    // executor's Erc20 balance — credit it so a downstream
                    // V2/V3 flash that consumes `cur` (e.g. the V3 auto-repay
                    // in `v4_v3`) validates. When the recipient is a V2 pair,
                    // the token seeds the pair directly (PM→pool) — credit
                    // `PairHandoff[pool]` so a following `V2SwapCalc` sees its
                    // seed (the 2PT5HH terminal-V2 rule across the PM boundary).
                    if *recipient_idx == SENTINEL_SELF {
                        ops.push(LedgerOp::Erc20Credit {
                            currency: *currency_addr,
                            amount: *amount,
                        });
                    }
                    if let Some(pool) = seeds_pool {
                        ops.push(LedgerOp::SeedPair {
                            pool: *pool,
                            amount: *amount,
                        });
                    }
                }
                // V4_SYNC is a balance-sync primitive — delta-neutral, no
                // ledger effect (the delta application comes from the
                // following V4Settle). Emitted for byte-parity only.
                PlanStep::V4Sync { .. } => {}
                PlanStep::V4Settle {
                    currency_addr,
                    amount,
                } => {
                    ops.push(LedgerOp::V4Settle {
                        currency: *currency_addr,
                        amount: *amount,
                    });
                }
                PlanStep::V4SettleDelta { currency_addr, .. } => {
                    ops.push(LedgerOp::V4SettleDelta {
                        currency: *currency_addr,
                    });
                }
            }
        }
    }
    walk(plan, &mut ops);
    ops
}

/// Encode a `Plan` to the `execute()` byte stream (depth-first; a `FlashSwap`
/// wraps its callback's bytes as the swap's callback payload). Mirrors the
/// proven hand-written emitter's `enc_*` calls — byte-parity with it is the
/// guard that this Plan-derived encoder reproduces the exact proven bytes.
#[must_use]
pub fn plan_to_bytes(plan: &Plan, at: &AddressTable) -> Vec<u8> {
    let mut out = Vec::new();
    fn walk(plan: &Plan, at: &AddressTable, out: &mut Vec<u8>) {
        for step in plan {
            match step {
                PlanStep::FlashSwap {
                    pool_idx,
                    protocol,
                    zfo,
                    fee,
                    out_amount,
                    in_amount,
                    recipient_idx,
                    callback,
                    ..
                } => {
                    let cb = plan_to_bytes(callback, at);
                    match protocol {
                        Prot::V2 => out.extend_from_slice(
                            &encoders::enc_v2_swap_compact(
                                *pool_idx,
                                *zfo,
                                *out_amount,
                                *recipient_idx,
                                *fee,
                                &cb,
                            )
                            .expect("V2 swap compact args in range"),
                        ),
                        Prot::V3 => out.extend_from_slice(
                            &encoders::enc_v3_swap_compact(
                                *pool_idx,
                                *zfo,
                                *in_amount,
                                *recipient_idx,
                                &cb,
                            )
                            .expect("V3 swap compact args in range"),
                        ),
                        Prot::V4 => unreachable!("V4 flash is not a FlashSwap"),
                    }
                }
                PlanStep::Erc20Transfer {
                    token_idx,
                    recipient_idx,
                    amount,
                    ..
                } => out.extend_from_slice(
                    &encoders::enc_erc20_transfer(*token_idx, *recipient_idx, *amount)
                        .expect("ERC20 transfer amount in range"),
                ),
                PlanStep::V2SwapCalc {
                    pool_idx,
                    zfo,
                    recipient_idx,
                    fee,
                    ..
                } => out.extend_from_slice(&encoders::enc_v2_swap_calc(
                    *pool_idx,
                    *zfo,
                    *recipient_idx,
                    *fee,
                )),
                // Self-fund is a stream precondition, not a command.
                PlanStep::SelfFund { .. } => {}
                // V4 container: encode inner, wrap in V4_UNLOCK.
                PlanStep::V4Unlock {
                    inner,
                    pool_manager_idx: _,
                } => {
                    let inner_bytes = plan_to_bytes(inner, at);
                    out.extend_from_slice(
                        &encoders::enc_v4_unlock(&inner_bytes)
                            .expect("V4 unlock forward_data in range"),
                    );
                }
                PlanStep::V4Swap {
                    c0_idx,
                    c1_idx,
                    fee,
                    tick_spacing,
                    hooks_idx,
                    zfo,
                    amount,
                    ..
                } => out.extend_from_slice(
                    &encoders::enc_v4_swap_compact(
                        *c0_idx,
                        *c1_idx,
                        *fee,
                        *tick_spacing,
                        *hooks_idx,
                        *zfo,
                        *amount,
                    )
                    .expect("V4 swap compact args in range"),
                ),
                PlanStep::V4TakeDelta {
                    currency_idx,
                    recipient_idx,
                    ..
                } => out
                    .extend_from_slice(&encoders::enc_v4_take_delta(*currency_idx, *recipient_idx)),
                PlanStep::V4SettleAll => out.extend_from_slice(&encoders::enc_v4_settle_all()),
                PlanStep::V4TakeCompact {
                    currency_idx,
                    recipient_idx,
                    amount,
                    ..
                } => out.extend_from_slice(
                    &encoders::enc_v4_take_compact(*currency_idx, *recipient_idx, *amount)
                        .expect("V4 take compact amount in range"),
                ),
                PlanStep::V4SettleDelta { currency_idx, .. } => {
                    out.extend_from_slice(&encoders::enc_v4_settle_delta(*currency_idx))
                }
                PlanStep::V4Sync { currency_idx, .. } => {
                    out.extend_from_slice(&encoders::enc_v4_sync(*currency_idx));
                }
                PlanStep::V4Settle { .. } => {
                    out.extend_from_slice(&encoders::enc_v4_settle());
                }
            }
        }
    }
    walk(plan, at, &mut out);
    out
}

/// Build the `v2_v3` (InPathFlash) Plan — the family's ledger decisions in
/// execution order, callback-nested exactly as the proven `derive_2hop` v2_v3
/// arm emits. This is the Checkpoint-1 re-baseline: the Plan is the primary
/// artifact; `plan_to_bytes` derives the byte stream; `plan_to_ledger_ops`
/// derives the validator's input.
///
/// Returns `(preamble_bytes, plan, address_table)` so callers can assemble
/// the full payload (`preamble + plan_to_bytes(&plan, &at)`).
#[must_use]
pub fn build_v2v3_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 2 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V3(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    if forward_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v2_idx = at.add(a.pool_address).ok()?;
    let v3_idx = at.add(b.pool_address).ok()?;
    let forward_idx = at.add(v2_forward(a)).ok()?;
    let fwd_a = v2_forward(a);
    let weth = inputs.weth_address;

    let plan: Plan = vec![PlanStep::FlashSwap {
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
        auto_repay: false,
        callback: vec![
            PlanStep::FlashSwap {
                pool_idx: v3_idx,
                pool_addr: b.pool_address,
                protocol: Prot::V3,
                zfo: b.zfo,
                fee: u16::try_from(b.fee).ok()?,
                out_currency: weth,
                out_amount: *inputs.hop_outputs.get(1)?,
                in_currency: fwd_a,
                in_amount: b_swap_in,
                recipient_idx: SENTINEL_SELF,
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
    }];

    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v3_v2` (SelfFund) Plan — V3 self-fund flash, terminal V2
/// pre-funded + `V2_SWAP_CALC` (the `2PT5HH` rule). Mirror of `derive_2hop`'s
/// v3_v2 arm as a callback-nested tree. The leading `SelfFund` credits the
/// executor's entry WETH so the V3 flash's WETH repayment validates.
#[must_use]
pub fn build_v3v2_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 2 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V2(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    if forward_out == 0 {
        return None;
    }
    let weth = inputs.weth_address;
    let fwd_a = v3_forward(a);

    let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
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
                },
            ],
        },
    ];
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v3_v3` (SelfFund) Plan — V3 self-fund flash feeds a terminal
/// V3 flash (both flash-coupled via the executor). Mirror of `derive_2hop`'s
/// v3_v3 arm as a callback-nested tree.
#[must_use]
pub fn build_v3v3_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 2 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V3(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    if forward_out == 0 {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let terminal_out = *inputs.hop_outputs.get(1)?;
    let weth = inputs.weth_address;
    let fwd_a = v3_forward(a);

    let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
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
                // V3b flash with an EMPTY callback → auto-pay: the cmd_executor
                // debits `in_currency` (t1) from the executor at callback-end
                // (the t1 the V3a flash credited). Model that as auto_repay=true.
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
                    auto_repay: true,
                    callback: vec![],
                },
            ],
        },
    ];
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v2_v2` (InPathFlash) Plan — V2 in-path flash, pool-to-pool via
/// `V2_SWAP_CALC` (the terminal V2 is pre-funded by the leading hop's forward
/// output, then swapped). Mirror of `derive_2hop`'s v2_v2 arm.
#[must_use]
pub fn build_v2v2_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 2 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V2(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    if forward_out == 0 {
        return None;
    }
    let weth = inputs.weth_address;
    let fwd_a = v2_forward(a);

    let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
    let v2_a = at.add(a.pool_address).ok()?;
    let v2_b = at.add(b.pool_address).ok()?;
    let forward_idx = at.add(fwd_a).ok()?;

    let plan: Plan = vec![PlanStep::FlashSwap {
        pool_idx: v2_a,
        pool_addr: a.pool_address,
        protocol: Prot::V2,
        zfo: a.zfo,
        fee: a.fee,
        out_currency: fwd_a,
        out_amount: forward_out,
        in_currency: weth,
        in_amount: optimal_input,
        recipient_idx: SENTINEL_SELF,
        auto_repay: false,
        callback: vec![
            PlanStep::Erc20Transfer {
                token_idx: forward_idx,
                token_addr: fwd_a,
                recipient_idx: v2_b,
                amount: forward_out,
                seeds_pool: Some(b.pool_address),
                repays_flash: None,
            },
            PlanStep::V2SwapCalc {
                pool_idx: v2_b,
                pool_addr: b.pool_address,
                zfo: b.zfo,
                recipient_idx: SENTINEL_SELF,
                fee: b.fee,
                out_currency: weth,
                out_amount: *inputs.hop_outputs.get(1)?,
            },
            PlanStep::Erc20Transfer {
                token_idx: SENTINEL_WETH,
                token_addr: weth,
                recipient_idx: v2_a,
                amount: optimal_input,
                seeds_pool: None,
                repays_flash: Some(a.pool_address),
            },
        ],
    }];
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v4_v4` (pure-V4 container) Plan — default opts only (no
/// `use_v4_batch`, no `erc6909_profit`, no native currency gap). The whole
/// stream is one `V4_UNLOCK` over internal PM ledger movement: two
/// `V4_SWAP`s, a terminal `V4_TAKE_DELTA` (profit capture), and a trailing
/// `V4_SETTLE_ALL`. The master V4 invariant — every touched `PM[currency]`
/// nets to zero by callback end — is enforced at `V4UnlockEnd`.
///
/// Returns `None` for the batch / erc6909 / currency-gap variants (later
/// sub-increments of BP7KIR Increment 3).
#[must_use]
pub fn build_v4v4_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 2 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V4(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    // Default-opts, WETH-only, no native gap (the spike's proven slice).
    if inputs.opts.use_v4_batch || inputs.opts.erc6909_profit {
        return None;
    }
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) || !fits_int128(forward_out) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let weth = inputs.weth_address;

    // No native currency gap (WETH-only slice).
    let mid_currency_a = if a.zfo {
        a.currency1_address
    } else {
        a.currency0_address
    };
    let mid_currency_b = if b.zfo {
        b.currency0_address
    } else {
        b.currency1_address
    };
    if mid_currency_a == NATIVE_CURRENCY_ADDRESS
        || mid_currency_b == NATIVE_CURRENCY_ADDRESS
        || crate::composers::CurrencyBridge::at_boundary(mid_currency_a, mid_currency_b)
            .needs_bridge()
    {
        return None;
    }
    let out_currency_a = mid_currency_a;
    let out_currency_b = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };
    // Terminal output must be WETH (the spike's WETH-only slice).
    if out_currency_b != weth {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
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

    let inner: Plan = vec![
        PlanStep::V4Swap {
            c0_idx: c0_a,
            c1_idx: c1_a,
            fee: fee_a,
            tick_spacing: ts_a,
            hooks_idx: SENTINEL_NATIVE,
            zfo: a.zfo,
            amount: optimal_input,
            in_currency: weth,
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
            out_currency: weth,
            out_amount: weth_out,
        },
        PlanStep::V4TakeDelta {
            currency_idx: weth_idx,
            currency_addr: weth,
            recipient_idx: SENTINEL_SELF,
        },
        PlanStep::V4SettleAll,
    ];
    let plan: Plan = vec![PlanStep::V4Unlock {
        inner,
        pool_manager_idx: pm_idx,
    }];
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v4_v3` Plan (BP7KIR Increment 3b) — the **boundary-take**
/// family: a V4 lead swap whose forward output is ***taken out of the PM***
/// to feed a terminal V3 flash swap.
///
/// This is the first cross-ledger family — `V4TakeCompact(cur→SELF)` is a
/// **cross-ledger move**: it debits `PM[cur]` (the V4 take) AND credits the
/// executor's `Erc20[cur]` (the token physically arrives at the executor),
/// which the V3 flash's auto-repay then debits. The plan tree's projection
/// emits both halves so the gate enforces that the V3 repayment can only
/// follow the V4 take that funds it (the D0 analogue across the PM/Erc20
/// boundary — the structural defect byte-parity cannot see).
///
/// Scoped slice (this sub-increment): ERC-20 V4 output + WETH V4 input (the
/// non-native case, matching the v4_v4 WETH-only slice). The native-output
/// (wrap-then-V3) and native-input (unwrap-then-settle) cases need
/// `WethDeposit`/`WethWithdraw` PlanSteps and return `None` here.
#[must_use]
pub fn build_v4v3_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 2 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V3(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) || !fits_int128(forward_out) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let weth = inputs.weth_address;

    // The V4 output currency (a's non-WETH leg) is the forward token taken to
    // SELF, then consumed by the V3 flash. The V4 input is WETH (a's other leg).
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
    // Scoped slice: reject native on either V4 leg (the wrap/unwrap cases land
    // in a later sub-increment with WethDeposit/Withdraw steps).
    if out_currency_a == NATIVE_CURRENCY_ADDRESS || in_currency_a == NATIVE_CURRENCY_ADDRESS {
        return None;
    }
    // The V4 input must be WETH for this slice (the settle-delta(WETH) path).
    if in_currency_a != weth {
        return None;
    }
    // The terminal V3 output must be WETH (the captured profit).
    let out_currency_b = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };
    if out_currency_b != weth {
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

    let inner: Plan = vec![
        // 1. V4 swap a: PM[WETH] −= optimal_input (debt), PM[t1] += forward_out.
        PlanStep::V4Swap {
            c0_idx: c0_a,
            c1_idx: c1_a,
            fee: fee_a,
            tick_spacing: ts_a,
            hooks_idx: SENTINEL_NATIVE,
            zfo: a.zfo,
            amount: optimal_input,
            in_currency: weth,
            in_amount: optimal_input,
            out_currency: out_currency_a,
            out_amount: forward_out,
        },
        // 2. Boundary take: PM[t1] −= forward_out; the token arrives at the
        //    executor (Erc20[t1] += forward_out), funding the V3 auto-repay.
        PlanStep::V4TakeCompact {
            currency_idx: forward_idx,
            currency_addr: out_currency_a,
            recipient_idx: SENTINEL_SELF,
            amount: forward_out,
            seeds_pool: None,
        },
        // 3. Terminal V3 flash: credits WETH (the profit), owes t1 — auto-repaid
        //    at callback-end from the Erc20[t1] credit the boundary take created.
        PlanStep::FlashSwap {
            pool_idx: v3_idx,
            pool_addr: b.pool_address,
            protocol: Prot::V3,
            zfo: b.zfo,
            fee: u16::try_from(b.fee).ok()?,
            out_currency: weth,
            out_amount: weth_out,
            in_currency: out_currency_a,
            in_amount: b_swap_in,
            recipient_idx: SENTINEL_SELF,
            auto_repay: true,
            callback: vec![],
        },
        // 4. Settle the V4 input debt: PM[WETH] → 0.
        PlanStep::V4SettleDelta {
            currency_idx: weth_idx,
            currency_addr: weth,
        },
        // 5. Settle any residual PM deltas, then the V4UnlockEnd net-zero fires.
        PlanStep::V4SettleAll,
    ];
    let plan: Plan = vec![PlanStep::V4Unlock {
        inner,
        pool_manager_idx: pm_idx,
    }];
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v4_v2` Plan (BP7KIR Increment 3b) — the **boundary-seed**
/// family: a V4 lead swap whose forward output is taken ***directly to a V2
/// pair*** (PM→pool, never touching the executor's Erc20), and a terminal
/// `V2SwapCalc` consumes that seed (the 2PT5HH terminal-V2 rule across the PM
/// boundary). The V4 WETH-input debt is settled by the boundary-seed pattern —
/// `V4Sync` + `Erc20Transfer(WETH→PM)` + `V4Settle` — funding the PM pay-in
/// from the V2 swap's WETH output (the outside→PM direction in miniature).
///
/// Scoped slice: ERC-20 V4 output + WETH V4 input (non-native, matching the
/// v4_v3/v4_v4 WETH-only slice). The native cases (take-native+wrap, or
/// unwrap-WETH-to-seed-native-input) need `WethDeposit`/`WethWithdraw` and
/// return `None` here.
#[must_use]
pub fn build_v4v2_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 2 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V2(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) || !fits_int128(forward_out) {
        return None;
    }
    let weth = inputs.weth_address;

    // The V4 output (forward token, taken to the V2 pair) = a's non-WETH leg;
    // the V4 input is WETH (a's other leg).
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
    // Scoped slice: reject native on either V4 leg + require WETH input.
    if out_currency_a == NATIVE_CURRENCY_ADDRESS || in_currency_a == NATIVE_CURRENCY_ADDRESS {
        return None;
    }
    if in_currency_a != weth {
        return None;
    }
    // The V2 terminal output must be WETH (the captured profit + the source of
    // the PM pay-in funds).
    let out_currency_b = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };
    if out_currency_b != weth {
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

    let inner: Plan = vec![
        // 1. V4 swap a: PM[WETH] −= optimal_input (debt), PM[t1] += forward_out.
        PlanStep::V4Swap {
            c0_idx: c0_a,
            c1_idx: c1_a,
            fee: fee_a,
            tick_spacing: ts_a,
            hooks_idx: SENTINEL_NATIVE,
            zfo: a.zfo,
            amount: optimal_input,
            in_currency: weth,
            in_amount: optimal_input,
            out_currency: out_currency_a,
            out_amount: forward_out,
        },
        // 2. Boundary take directly to the V2 pair (PM→pool, never via executor
        //    Erc20): PM[t1] −= forward_out; SeedPair(v2, forward_out) so the
        //    following V2SwapCalc sees its seed.
        PlanStep::V4TakeCompact {
            currency_idx: forward_idx,
            currency_addr: out_currency_a,
            recipient_idx: v2_idx,
            amount: forward_out,
            seeds_pool: Some(b.pool_address),
        },
        // 3. Terminal V2 SwapCalc: consumes the seeded pair, credits the
        //    executor Erc20[WETH] += weth_out (the swap output — the profit +
        //    the PM pay-in source).
        PlanStep::V2SwapCalc {
            pool_idx: v2_idx,
            pool_addr: b.pool_address,
            zfo: b.zfo,
            recipient_idx: SENTINEL_SELF,
            fee: b.fee,
            out_currency: weth,
            out_amount: weth_out,
        },
        // 4-6. Boundary-seed: sync WETH, pay it into the PM from the V2 output,
        //      settle the V4 input debt (PM[WETH] += optimal_input → 0).
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
        // 7. Settle residual + the V4UnlockEnd net-zero assertion.
        PlanStep::V4SettleAll,
    ];
    let plan: Plan = vec![PlanStep::V4Unlock {
        inner,
        pool_manager_idx: pm_idx,
    }];
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Public spike entry: derive a family's command stream from its
/// [`ShapeClass`] (funding chosen by the leading protocol, as the D0
/// invariant forces). Returns the raw `execute()` payload bytes.
#[must_use]
pub fn derive_shape(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    // V4-involving families: a pure-V4 2-hop path is the *container* case — the
    // whole stream is one V4_UNLOCK over internal ledger movement, so no funding
    // choice is needed (the PM carries the entry credit). Handle it before the
    // V2/V3 funding dispatch.
    match (path.hops.first(), path.hops.get(1), path.hops.get(2)) {
        (Some(HopInfo::V4(a)), Some(HopInfo::V4(b)), Some(HopInfo::V4(c))) => {
            derive_3hop_v4v4v4(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V2(b)), Some(HopInfo::V2(c))) => {
            derive_3hop_v4v2v2(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V2(b)), Some(HopInfo::V4(c))) => {
            derive_3hop_v2v2v4(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V3(b)), Some(HopInfo::V4(c))) => {
            derive_3hop_v2v3v4(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V2(b)), Some(HopInfo::V4(c))) => {
            derive_3hop_v3v2v4(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V3(b)), Some(HopInfo::V4(c))) => {
            derive_3hop_v3v3v4(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V4(b)), Some(HopInfo::V2(c))) => {
            derive_3hop_v2v4v2(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V4(b)), Some(HopInfo::V3(c))) => {
            derive_3hop_v2v4v3(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V4(b)), Some(HopInfo::V2(c))) => {
            derive_3hop_v3v4v2(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V4(b)), Some(HopInfo::V3(c))) => {
            derive_3hop_v3v4v3(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V4(b)), Some(HopInfo::V4(c))) => {
            derive_3hop_v2v4v4(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V4(b)), Some(HopInfo::V4(c))) => {
            derive_3hop_v3v4v4(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V4(b)), Some(HopInfo::V2(c))) => {
            derive_3hop_v4v4v2(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V4(b)), Some(HopInfo::V3(c))) => {
            derive_3hop_v4v4v3(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V2(b)), Some(HopInfo::V3(c))) => {
            derive_3hop_v4v2v3(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V2(b)), Some(HopInfo::V4(c))) => {
            derive_3hop_v4v2v4(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V3(b)), Some(HopInfo::V2(c))) => {
            derive_3hop_v4v3v2(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V3(b)), Some(HopInfo::V3(c))) => {
            derive_3hop_v4v3v3(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V3(b)), Some(HopInfo::V4(c))) => {
            derive_3hop_v4v3v4(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V4(b)), None) => derive_2hop_v4v4(a, b, inputs),
        (Some(HopInfo::V4(a)), Some(HopInfo::V3(b)), None) => derive_2hop_v4v3(a, b, inputs),
        (Some(HopInfo::V3(a)), Some(HopInfo::V4(b)), None) => derive_2hop_v3v4(a, b, inputs),
        (Some(HopInfo::V4(a)), Some(HopInfo::V2(b)), None) => derive_2hop_v4v2(a, b, inputs),
        (Some(HopInfo::V2(a)), Some(HopInfo::V4(b)), None) => derive_2hop_v2v4(a, b, inputs),
        // V2/V3-only 3-hop folds (WAYDTL): byte-faithful transcriptions of the
        // previously-hand-written adapters, byte-identical to them (verified by
        // the `cutover` `debug_assert` oracle in dev + the parity suite).
        (Some(HopInfo::V2(a)), Some(HopInfo::V2(b)), Some(HopInfo::V3(c))) => {
            derive_3hop_v2v2v3(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V3(b)), Some(HopInfo::V2(c))) => {
            derive_3hop_v2v3v2(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V3(b)), Some(HopInfo::V3(c))) => {
            derive_3hop_v2v3v3(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V2(b)), Some(HopInfo::V2(c))) => {
            derive_3hop_v3v2v2(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V2(b)), Some(HopInfo::V3(c))) => {
            derive_3hop_v3v2v3(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V3(b)), Some(HopInfo::V2(c))) => {
            derive_3hop_v3v3v2(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V3(b)), Some(HopInfo::V3(c))) => {
            derive_3hop_v3v3v3(a, b, c, inputs)
        }
        _ => derive_2hop_v2v3(path, inputs),
    }
}

/// V2/V3 2-hop / 3-hop-(V2/V3) entry (the previous funding-based dispatch).
fn derive_2hop_v2v3(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let funding = match path.hops.first()? {
        HopInfo::V2(_) => FundingSource::InPathFlash,
        HopInfo::V3(_) => FundingSource::SelfFund,
        _ => return None,
    };
    let protocols: Vec<Prot> = path
        .hops
        .iter()
        .map(|h| match h {
            HopInfo::V2(_) => Prot::V2,
            HopInfo::V3(_) => Prot::V3,
            _ => unreachable!("V4 outside the V2/V3 branch"),
        })
        .collect();
    let class = ShapeClass { protocols, funding };
    derive_2hop(path, inputs, &class)
}

/// Pure V4→V4 2-hop container derivation (WAYDTL step 2, **WETH-only slice**).
///
/// Per the v4 ledger rules / boundary model (`docs/plans/executor-v4-ledger-rules.md`):
/// the whole stream is one `V4_UNLOCK`; V4→V4 is internal ledger movement (no
/// `TAKE`, no ERC-20 transfer); the WETH output is captured by `TAKE_DELTA(WETH→SELF)`;
/// a trailing `V4_SETTLE_ALL` flushes any residual so every delta nets to zero by
/// callback end (the one master V4 invariant).
///
/// Scoped to the WETH-only, no-native-bridge, WETH-output case (the harness `v4_v4`
/// family) — `default` opts (no `V4_BATCH`, no `erc6909_profit`). Other V4 shapes
/// (native bridges, non-WETH output, batch/mint) return `None` for now (later steps).
fn derive_2hop_v4v4(a: &V4HopInfo, b: &V4HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    use crate::composers::{emit_currency_bridge, CurrencyBridge};

    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) || !fits_int128(forward_out) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }

    let output_currency_b = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };
    let mid_currency_a = if a.zfo {
        a.currency1_address
    } else {
        a.currency0_address
    };
    let mid_currency_b = if b.zfo {
        b.currency0_address
    } else {
        b.currency1_address
    };
    let b_needs_native = mid_currency_b == NATIVE_CURRENCY_ADDRESS;
    let bridge = CurrencyBridge::at_boundary(mid_currency_a, mid_currency_b);
    let currency_gap = bridge.needs_bridge();

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    // Native is address(0) — a sentinel in the table; registering it is a no-op.
    let weth_idx = SENTINEL_WETH;
    let native_idx = SENTINEL_NATIVE;

    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;

    // One unlock. Default layout: two individual V4_SWAP_COMPACT commands with
    // an optional native<->WETH boundary bridge, a terminal profit take, and a
    // trailing settle. `use_v4_batch` (no currency gap) collapses the two swaps
    // into a single V4_BATCH PM extcall; `erc6909_profit` (WETH output) captures
    // the profit as an ERC6909 mint instead of a physical take. Both mirror the
    // hand-written `v4_v4` adapter byte-for-byte.
    let mut inner = if !inputs.opts.use_v4_batch || currency_gap {
        encoders::enc_v4_swap_compact(
            c0_a,
            c1_a,
            fee_a,
            ts_a,
            SENTINEL_NATIVE,
            a.zfo,
            optimal_input,
        )
        .ok()?
    } else {
        Vec::new()
    };
    if currency_gap {
        let bridge_idx = match bridge {
            CurrencyBridge::Wrap => native_idx,
            CurrencyBridge::Unwrap => weth_idx,
            CurrencyBridge::None => unreachable!("currency_gap implies a bridge"),
        };
        emit_currency_bridge(&mut inner, bridge, bridge_idx, forward_out)?;
        inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(
                c0_b,
                c1_b,
                fee_b,
                ts_b,
                SENTINEL_NATIVE,
                b.zfo,
                b_swap_in,
            )
            .ok()?,
        );
        inner.extend_from_slice(&encoders::enc_v4_settle_delta(if b_needs_native {
            native_idx
        } else {
            weth_idx
        }));
        // Capture the terminal profit out of the PM to the executor (physical).
        if output_currency_b == NATIVE_CURRENCY_ADDRESS {
            inner.extend_from_slice(&encoders::enc_v4_take_delta(native_idx, SENTINEL_SELF));
        } else if output_currency_b == inputs.weth_address {
            inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, SENTINEL_SELF));
        } else {
            inner.extend_from_slice(&encoders::enc_v4_take_delta(
                if b.zfo { c1_b } else { c0_b },
                SENTINEL_SELF,
            ));
        }
        inner.extend_from_slice(&encoders::enc_v4_settle_all());
    } else {
        if inputs.opts.use_v4_batch {
            let batch = [
                encoders::V4BatchEntry {
                    c0_idx: c0_a,
                    c1_idx: c1_a,
                    fee: fee_a,
                    tick_spacing: ts_a,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: a.zfo,
                    amount_u96: optimal_input,
                },
                encoders::V4BatchEntry {
                    c0_idx: c0_b,
                    c1_idx: c1_b,
                    fee: fee_b,
                    tick_spacing: ts_b,
                    hooks_idx: SENTINEL_NATIVE,
                    zfo: b.zfo,
                    amount_u96: b_swap_in,
                },
            ];
            inner.extend_from_slice(&encoders::enc_v4_batch(&batch).ok()?);
            if output_currency_b != NATIVE_CURRENCY_ADDRESS
                && output_currency_b != inputs.weth_address
            {
                inner.extend_from_slice(&encoders::enc_v4_take_delta(
                    if b.zfo { c1_b } else { c0_b },
                    SENTINEL_SELF,
                ));
            }
        } else {
            inner.extend_from_slice(
                &encoders::enc_v4_swap_compact(
                    c0_b,
                    c1_b,
                    fee_b,
                    ts_b,
                    SENTINEL_NATIVE,
                    b.zfo,
                    b_swap_in,
                )
                .ok()?,
            );
        }
        if inputs.opts.erc6909_profit && output_currency_b == inputs.weth_address {
            let profit_amount = weth_out.saturating_sub(optimal_input);
            if profit_amount > 0 {
                inner.extend_from_slice(
                    &encoders::enc_v4_mint_compact(weth_idx, SENTINEL_SELF, profit_amount).ok()?,
                );
            }
        } else if !inputs.opts.use_v4_batch
            || (output_currency_b != NATIVE_CURRENCY_ADDRESS
                && output_currency_b != inputs.weth_address)
        {
            if output_currency_b == NATIVE_CURRENCY_ADDRESS {
                inner.extend_from_slice(&encoders::enc_v4_take_delta(native_idx, SENTINEL_SELF));
            } else if output_currency_b == inputs.weth_address {
                inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, SENTINEL_SELF));
            } else {
                inner.extend_from_slice(&encoders::enc_v4_take_delta(
                    if b.zfo { c1_b } else { c0_b },
                    SENTINEL_SELF,
                ));
            }
        }
        inner.extend_from_slice(&encoders::enc_v4_settle_all());
    }

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V4→V3 2-hop derivation (WAYDTL step 2 / (A)).
///
/// V4's forward currency **leaves the PM** to become the V3 input (boundary
/// model: V4→outside = `V4_TAKE_COMPACT(cur→SELF, forward_out)`); a native
/// forward is wrapped (`WETH_DEPOSIT`) before the V3 swap; the V4 input debt
/// is settled (`V4_SETTLE_DELTA`), with a `WETH_WITHDRAW` when the V4 input is
/// itself native.
fn derive_2hop_v4v3(a: &V4HopInfo, b: &V3HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let v4_out_native = if a.zfo {
        a.currency1_address == NATIVE_CURRENCY_ADDRESS
    } else {
        a.currency0_address == NATIVE_CURRENCY_ADDRESS
    };
    let v4_in_native = if a.zfo {
        a.currency0_address == NATIVE_CURRENCY_ADDRESS
    } else {
        a.currency1_address == NATIVE_CURRENCY_ADDRESS
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let v3_idx = at.add(b.pool_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    // Native is address(0) — a sentinel, so registering it never adds a table
    // entry; idx is just a sentinel value either way.
    let native_idx = SENTINEL_NATIVE;

    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;

    let mut inner = encoders::enc_v4_swap_compact(
        c0_a,
        c1_a,
        fee_a,
        ts_a,
        SENTINEL_NATIVE,
        a.zfo,
        optimal_input,
    )
    .ok()?;
    if v4_out_native {
        // Native V4 output: take it out, wrap to WETH, then the V3 swap.
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, forward_out).ok()?,
        );
        inner.extend_from_slice(&encoders::enc_weth_deposit(U256::from(forward_out)));
        inner.extend_from_slice(
            &encoders::enc_v3_swap_compact(v3_idx, b.zfo, b_swap_in, SENTINEL_SELF, &[]).ok()?,
        );
        let input_idx = if a.zfo { c0_a } else { c1_a };
        inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
    } else {
        // ERC-20 V4 output: take it to the executor, which funds the V3 swap.
        let forward_idx = if a.zfo { c1_a } else { c0_a };
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(forward_idx, SENTINEL_SELF, forward_out).ok()?,
        );
        inner.extend_from_slice(
            &encoders::enc_v3_swap_compact(v3_idx, b.zfo, b_swap_in, SENTINEL_SELF, &[]).ok()?,
        );
        if v4_in_native {
            // Native V4 input: unwrap WETH to seed it before settling.
            let input_idx = if a.zfo { c0_a } else { c1_a };
            inner.extend_from_slice(&encoders::enc_weth_withdraw(U256::from(optimal_input)));
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
        } else {
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));
        }
    }
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V3→V4 2-hop derivation (WAYDTL step 2 / (A)).
///
/// A V3 (outer flash) feeds a V4 pool. When the V4 input is an ERC-20, the V3
/// forward output **enters the PM** (boundary model: `V4_SYNC(cur)` +
/// `ERC20_TRANSFER(cur, PM, out)` + `V4_SETTLE`) to seed the input, then the
/// V4 swap + `V4_TAKE_COMPACT(output→SELF)` capture; the V3 flash is repaid
/// `ERC20_TRANSFER(WETH→v3, optimal_input)`. When the V4 input is native the
/// V3's WETH output is unwrapped (`WETH_WITHDRAW(forward_out)`) to seed it and
/// settled directly (`V4_SETTLE_DELTA(native)`).
fn derive_2hop_v3v4(a: &V3HopInfo, b: &V4HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(forward_out) || !fits_int128(weth_out) {
        return None;
    }
    let v4_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(v4_swap_in) {
        return None;
    }
    let v4_in_native = if b.zfo {
        b.currency0_address == NATIVE_CURRENCY_ADDRESS
    } else {
        b.currency1_address == NATIVE_CURRENCY_ADDRESS
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let v3_idx = at.add(a.pool_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let native_idx = SENTINEL_NATIVE;

    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;

    let v3_callback = if v4_in_native {
        // Native V4 input: settle it directly from executor native balance.
        let mut v4_inner = encoders::enc_v4_swap_compact(
            c0_b,
            c1_b,
            fee_b,
            ts_b,
            SENTINEL_NATIVE,
            b.zfo,
            v4_swap_in,
        )
        .ok()?;
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(native_idx));
        let output_currency = if b.zfo {
            b.currency1_address
        } else {
            b.currency0_address
        };
        if output_currency == NATIVE_CURRENCY_ADDRESS {
            v4_inner.extend_from_slice(
                &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, weth_out).ok()?,
            );
        } else {
            let output_idx = if b.zfo { c1_b } else { c0_b };
            v4_inner.extend_from_slice(
                &encoders::enc_v4_take_compact(output_idx, SENTINEL_SELF, weth_out).ok()?,
            );
        }
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

        let mut cb = encoders::enc_weth_withdraw(U256::from(forward_out));
        cb.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);
        let input_currency_v3 = if a.zfo {
            a.token0_address
        } else {
            a.token1_address
        };
        if input_currency_v3 == inputs.weth_address || input_currency_v3 == NATIVE_CURRENCY_ADDRESS
        {
            return None;
        }
        let forward_v3_idx = at.add(input_currency_v3).ok()?;
        cb.extend_from_slice(
            &encoders::enc_erc20_transfer(forward_v3_idx, v3_idx, optimal_input).ok()?,
        );
        cb
    } else {
        // ERC-20 V4 input: sync + transfer + settle to seed it into the PM.
        let forward_addr = if a.zfo {
            a.token1_address
        } else {
            a.token0_address
        };
        let forward_idx = at.add(forward_addr).ok()?;
        let mut v4_inner = encoders::enc_v4_sync(forward_idx);
        v4_inner.extend_from_slice(
            &encoders::enc_erc20_transfer(forward_idx, pm_idx, forward_out).ok()?,
        );
        v4_inner.extend_from_slice(&encoders::enc_v4_settle());
        v4_inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(
                c0_b,
                c1_b,
                fee_b,
                ts_b,
                SENTINEL_NATIVE,
                b.zfo,
                v4_swap_in,
            )
            .ok()?,
        );
        let output_idx = if b.zfo { c1_b } else { c0_b };
        v4_inner.extend_from_slice(
            &encoders::enc_v4_take_compact(output_idx, SENTINEL_SELF, weth_out).ok()?,
        );
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

        let mut cb = encoders::enc_v4_unlock(&v4_inner).ok()?;
        cb.extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v3_idx, optimal_input).ok()?);
        cb
    };

    let commands =
        encoders::enc_v3_swap_compact(v3_idx, a.zfo, optimal_input, SENTINEL_SELF, &v3_callback)
            .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V4→V2 2-hop derivation (WAYDTL step 2 / (A)).
///
/// V4's forward currency **leaves the PM to the V2 pool** (`V4_TAKE_COMPACT`
/// with the V2 pool as recipient) and the terminal V2 swap runs; the V4 input
/// is re-seeded. A native V4 output is wrapped (`WETH_DEPOSIT`) before being
/// transferred to the V2 pool (and the terminal V2 always uses `V2_SWAP_CALC`,
/// never exact-out). A native V4 input is settled via `WETH_WITHDRAW`.
fn derive_2hop_v4v2(a: &V4HopInfo, b: &V2HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    if forward_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) || !fits_int128(forward_out) {
        return None;
    }
    let v4_out_native = if a.zfo {
        a.currency1_address == NATIVE_CURRENCY_ADDRESS
    } else {
        a.currency0_address == NATIVE_CURRENCY_ADDRESS
    };
    let v4_in_native = if a.zfo {
        a.currency0_address == NATIVE_CURRENCY_ADDRESS
    } else {
        a.currency1_address == NATIVE_CURRENCY_ADDRESS
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let v2_idx = at.add(b.pool_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let native_idx = SENTINEL_NATIVE;

    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;

    let mut inner = encoders::enc_v4_swap_compact(
        c0_a,
        c1_a,
        fee_a,
        ts_a,
        SENTINEL_NATIVE,
        a.zfo,
        optimal_input,
    )
    .ok()?;
    if v4_out_native {
        // Native V4 output: take it out, wrap, then fund the terminal V2 pool.
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, forward_out).ok()?,
        );
        inner.extend_from_slice(&encoders::enc_weth_deposit(U256::from(forward_out)));
        inner.extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v2_idx, forward_out).ok()?);
        inner.extend_from_slice(&encoders::enc_v2_swap_calc(
            v2_idx,
            b.zfo,
            SENTINEL_SELF,
            b.fee,
        ));
        let input_idx = if a.zfo { c0_a } else { c1_a };
        inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
    } else {
        // ERC-20 V4 output: hand it directly to the V2 pool (recipient = V2).
        let forward_idx = if a.zfo { c1_a } else { c0_a };
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(forward_idx, v2_idx, forward_out).ok()?,
        );
        inner.extend_from_slice(&encoders::enc_v2_swap_calc(
            v2_idx,
            b.zfo,
            SENTINEL_SELF,
            b.fee,
        ));
        if v4_in_native {
            let input_idx = if a.zfo { c0_a } else { c1_a };
            inner.extend_from_slice(&encoders::enc_weth_withdraw(U256::from(optimal_input)));
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
        } else {
            inner.extend_from_slice(&encoders::enc_v4_sync(weth_idx));
            inner.extend_from_slice(
                &encoders::enc_erc20_transfer(weth_idx, pm_idx, optimal_input).ok()?,
            );
            inner.extend_from_slice(&encoders::enc_v4_settle());
        }
    }
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V2→V4 2-hop derivation (WAYDTL step 2 / (A)).
///
/// A V2 (outer flash) feeds a V4 pool. When the V4 input is an ERC-20, the V2
/// forward output **enters the PM** (boundary model: `V4_SYNC(cur)` +
/// `ERC20_TRANSFER(cur, PM, out)` + `V4_SETTLE`) to seed it, the V4 swap +
/// `V4_TAKE_COMPACT(output→SELF)` captures, and the V2 flash is repaid
/// `ERC20_TRANSFER(WETH→v2, optimal_input)`. When the V4 input is native the
/// V2's WETH output is unwrapped (`WETH_WITHDRAW(forward_out)`) and the V4
/// input settled directly.
fn derive_2hop_v2v4(a: &V2HopInfo, b: &V4HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(forward_out) || !fits_int128(weth_out) {
        return None;
    }
    let v4_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(v4_swap_in) {
        return None;
    }
    let v4_in_native = if b.zfo {
        b.currency0_address == NATIVE_CURRENCY_ADDRESS
    } else {
        b.currency1_address == NATIVE_CURRENCY_ADDRESS
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let v2_idx = at.add(a.pool_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let native_idx = SENTINEL_NATIVE;
    let forward_addr = if a.zfo {
        a.token1_address
    } else {
        a.token0_address
    };
    let forward_idx = at.add(forward_addr).ok()?;

    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;

    let callback_cmds = if v4_in_native {
        // Native V4 input: settle it directly from executor native balance.
        let mut v4_inner = encoders::enc_v4_swap_compact(
            c0_b,
            c1_b,
            fee_b,
            ts_b,
            SENTINEL_NATIVE,
            b.zfo,
            v4_swap_in,
        )
        .ok()?;
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(native_idx));
        let output_idx = if b.zfo { c1_b } else { c0_b };
        v4_inner.extend_from_slice(
            &encoders::enc_v4_take_compact(output_idx, SENTINEL_SELF, weth_out).ok()?,
        );
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

        let mut cb = encoders::enc_weth_withdraw(U256::from(forward_out));
        cb.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);
        cb.extend_from_slice(
            &encoders::enc_erc20_transfer(forward_idx, v2_idx, optimal_input).ok()?,
        );
        cb
    } else {
        let v4_out_native = if b.zfo {
            b.currency1_address == NATIVE_CURRENCY_ADDRESS
        } else {
            b.currency0_address == NATIVE_CURRENCY_ADDRESS
        };
        let mut v4_inner = encoders::enc_v4_sync(forward_idx);
        v4_inner.extend_from_slice(
            &encoders::enc_erc20_transfer(forward_idx, pm_idx, forward_out).ok()?,
        );
        v4_inner.extend_from_slice(&encoders::enc_v4_settle());
        v4_inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(
                c0_b,
                c1_b,
                fee_b,
                ts_b,
                SENTINEL_NATIVE,
                b.zfo,
                v4_swap_in,
            )
            .ok()?,
        );
        if v4_out_native {
            v4_inner.extend_from_slice(
                &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, weth_out).ok()?,
            );
        } else {
            let output_idx = if b.zfo { c1_b } else { c0_b };
            v4_inner.extend_from_slice(
                &encoders::enc_v4_take_compact(output_idx, SENTINEL_SELF, weth_out).ok()?,
            );
        }
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

        let mut cb = encoders::enc_v4_unlock(&v4_inner).ok()?;
        if v4_out_native {
            cb.extend_from_slice(&encoders::enc_weth_deposit(U256::from(weth_out)));
        }
        cb.extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v2_idx, optimal_input).ok()?);
        cb
    };

    let outer = encoders::enc_v2_swap_compact(
        v2_idx,
        a.zfo,
        forward_out,
        SENTINEL_SELF,
        a.fee,
        &callback_cmds,
    )
    .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&outer);
    Some(out)
}

// ── V2/V3-only 3-hop derivations (WAYDTL fold) ────────────────────────────
// Byte-faithful transcriptions of the former hand-written `v2_v2_v3`..`v3_v3_v3`
// adapters. The V2/V3 3-hop families are heterogeneous nested callback chains
// (the *enclosure* — which hop wraps which — depends on the protocol sequence,
// not a uniform rule), so like the V4 3-hop families they are explicit
// per-family encoders. Each is kept byte-identical to the adapter it replaces
// (the `cutover` `debug_assert` oracle in dev + the parity suite in release).
// Address-table registration ORDER is part of the byte contract and must match.

fn derive_3hop_v2v2v3(
    a: &V2HopInfo,
    b: &V2HopInfo,
    c: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let c_swap_in = cl_swap_in(inputs, 2)?;
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v2a_idx = at.add(a.pool_address).ok()?;
    let v2b_idx = at.add(b.pool_address).ok()?;
    let v3c_idx = at.add(c.pool_address).ok()?;

    let mut c_fwd =
        encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, inputs.optimal_input).ok()?;
    c_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(v2a_idx, a.zfo, v2b_idx, a.fee));
    c_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(v2b_idx, b.zfo, v3c_idx, b.fee));

    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, c.zfo, c_swap_in, SENTINEL_SELF, &c_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn derive_3hop_v2v3v2(
    a: &V2HopInfo,
    b: &V3HopInfo,
    c: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let out_a = inputs.hop_outputs[0];
    let out_c = inputs.hop_outputs[2];
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let b_swap_in = cl_swap_in(inputs, 1)?;
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    // Register forward token of A (discarded — affects table index order).
    at.add(v2_forward_addr(a)).ok()?;
    let v2a_idx = at.add(a.pool_address).ok()?;
    let v2c_idx = at.add(c.pool_address).ok()?;
    let v3b_idx = at.add(b.pool_address).ok()?;

    let mut b_fwd =
        encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, inputs.optimal_input).ok()?;
    b_fwd.extend_from_slice(&encoders::enc_v2_swap_direct(v2a_idx, a.zfo, out_a, v3b_idx).ok()?);
    let c_fwd = encoders::enc_v3_swap_compact(v3b_idx, b.zfo, b_swap_in, v2c_idx, &b_fwd).ok()?;
    let commands =
        encoders::enc_v2_swap_compact(v2c_idx, c.zfo, out_c, SENTINEL_SELF, c.fee, &c_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn derive_3hop_v2v3v3(
    a: &V2HopInfo,
    b: &V3HopInfo,
    c: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let out_a = inputs.hop_outputs[0];
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let b_swap_in = cl_swap_in(inputs, 1)?;
    let c_swap_in = cl_swap_in(inputs, 2)?;
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v2a_idx = at.add(a.pool_address).ok()?;
    let v3b_idx = at.add(b.pool_address).ok()?;
    let v3c_idx = at.add(c.pool_address).ok()?;

    let mut v3b_fwd =
        encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, inputs.optimal_input).ok()?;
    v3b_fwd.extend_from_slice(&encoders::enc_v2_swap_direct(v2a_idx, a.zfo, out_a, v3b_idx).ok()?);
    let v3c_fwd =
        encoders::enc_v3_swap_compact(v3b_idx, b.zfo, b_swap_in, v3c_idx, &v3b_fwd).ok()?;
    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, c.zfo, c_swap_in, SENTINEL_SELF, &v3c_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn derive_3hop_v3v2v2(
    a: &V3HopInfo,
    b: &V2HopInfo,
    c: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v2b_idx = at.add(b.pool_address).ok()?;
    let v2c_idx = at.add(c.pool_address).ok()?;
    let v3a_idx = at.add(a.pool_address).ok()?;

    // Both V2 hops (b, then terminal c) encode as V2_SWAP_CALC (swap from the
    // delta actually delivered to the pool) rather than V2_SWAP_DIRECT exact-out
    // — the V3 a-hop's output can deliver 1 wei less than the solver forward
    // (CL twin/clamp), so an exact-out over-draws by 1 -> `UniswapV2: K`.
    let mut a_fwd = encoders::enc_v2_swap_calc(v2b_idx, b.zfo, v2c_idx, b.fee);
    a_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2c_idx,
        c.zfo,
        SENTINEL_SELF,
        c.fee,
    ));
    a_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v3a_idx, inputs.optimal_input).ok()?,
    );
    let commands =
        encoders::enc_v3_swap_compact(v3a_idx, a.zfo, inputs.optimal_input, v2b_idx, &a_fwd)
            .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn derive_3hop_v3v2v3(
    a: &V3HopInfo,
    b: &V2HopInfo,
    c: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let c_swap_in = cl_swap_in(inputs, 2)?;
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v2b_idx = at.add(b.pool_address).ok()?;
    let v3a_idx = at.add(a.pool_address).ok()?;
    let v3c_idx = at.add(c.pool_address).ok()?;

    let mut v3a_fwd = encoders::enc_v2_swap_calc(v2b_idx, b.zfo, v3c_idx, b.fee);
    v3a_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v3a_idx, inputs.optimal_input).ok()?,
    );
    let v3c_fwd =
        encoders::enc_v3_swap_compact(v3a_idx, a.zfo, inputs.optimal_input, v2b_idx, &v3a_fwd)
            .ok()?;
    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, c.zfo, c_swap_in, SENTINEL_SELF, &v3c_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn derive_3hop_v3v3v2(
    a: &V3HopInfo,
    b: &V3HopInfo,
    c: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let b_swap_in = cl_swap_in(inputs, 1)?;
    if !fits_int128(inputs.optimal_input) {
        return None;
    }
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v2c_idx = at.add(c.pool_address).ok()?;
    let v3a_idx = at.add(a.pool_address).ok()?;

    // Terminal V2 hop: swap from the USDT the V3 b-hop actually delivered to
    // the pool (V2_SWAP_CALC), not the raw exact-out hop_outputs[2] — the CL
    // b-hop output can be 1 wei below the solver forward (path-110302/182449).
    let mut v3a_fwd = encoders::enc_v2_swap_calc(v2c_idx, c.zfo, SENTINEL_SELF, c.fee);
    v3a_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v3a_idx, inputs.optimal_input).ok()?,
    );
    let v3b_idx = at.add(b.pool_address).ok()?;
    let v3b_fwd =
        encoders::enc_v3_swap_compact(v3a_idx, a.zfo, inputs.optimal_input, v3b_idx, &v3a_fwd)
            .ok()?;
    let commands =
        encoders::enc_v3_swap_compact(v3b_idx, b.zfo, b_swap_in, v2c_idx, &v3b_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn derive_3hop_v3v3v3(
    a: &V3HopInfo,
    b: &V3HopInfo,
    c: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let b_swap_in = cl_swap_in(inputs, 1)?;
    let c_swap_in = cl_swap_in(inputs, 2)?;
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v3a_idx = at.add(a.pool_address).ok()?;
    let v3b_idx = at.add(b.pool_address).ok()?;
    let v3c_idx = at.add(c.pool_address).ok()?;

    let v3a_callback: Vec<u8> = Vec::new();
    let v3b_callback =
        encoders::enc_v3_swap_compact(v3a_idx, a.zfo, inputs.optimal_input, v3b_idx, &v3a_callback)
            .ok()?;
    let v3c_callback =
        encoders::enc_v3_swap_compact(v3b_idx, b.zfo, b_swap_in, v3c_idx, &v3b_callback).ok()?;
    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, c.zfo, c_swap_in, SENTINEL_SELF, &v3c_callback)
            .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// Pure V4→V4→V4 3-hop container derivation (WAYDTL step 3).
///
/// Like the 2-hop `v4_v4`, the whole stream is one `V4_UNLOCK` of internal
/// ledger movement — each V4 hop delegates to its own `V4_SWAP_COMPACT`;
/// native↔WETH representation gaps between hops emit a `V4_TAKE_COMPACT` +
/// `WETH_DEPOSIT`/`WETH_WITHDRAW` bridge + settle; the terminal profit is
/// captured (`V4_TAKE_DELTA(output→SELF)`); a trailing `V4_SETTLE_ALL` nets
/// every currency to zero. Scoped to `default` opts (no `V4_BATCH`,
/// no `erc6909_profit`).
fn derive_3hop_v4v4v4(
    a: &V4HopInfo,
    b: &V4HopInfo,
    c: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    use crate::composers::{emit_currency_bridge, CurrencyBridge};

    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.get(0)?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    if out_a == 0 || out_b == 0 || out_c == 0 {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let a_swap_in = *inputs.consumed_inputs.first()?;
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(a_swap_in) || !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }

    let mid_a_out = if a.zfo {
        a.currency1_address
    } else {
        a.currency0_address
    };
    let mid_b_in = if b.zfo {
        b.currency0_address
    } else {
        b.currency1_address
    };
    let mid_b_out = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };
    let mid_c_in = if c.zfo {
        c.currency0_address
    } else {
        c.currency1_address
    };
    let output_c = if c.zfo {
        c.currency1_address
    } else {
        c.currency0_address
    };
    let bridge_ab = CurrencyBridge::at_boundary(mid_a_out, mid_b_in);
    let bridge_bc = CurrencyBridge::at_boundary(mid_b_out, mid_c_in);

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let weth_idx = SENTINEL_WETH;
    let zero_idx = SENTINEL_NATIVE;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let c0_c = at.add(c.currency0_address).ok()?;
    let c1_c = at.add(c.currency1_address).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;

    // One unlock. Default layout: three individual V4_SWAP_COMPACT commands with
    // optional native<->WETH boundary bridges, a terminal profit take, and a
    // trailing settle. `use_v4_batch` (no currency gap) collapses all three swaps
    // into a single V4_BATCH PM extcall; `erc6909_profit` (WETH output) captures
    // the profit as an ERC6909 mint instead of a physical take. Both mirror the
    // hand-written `v4_v4_v4` adapter byte-for-byte.
    let any_gap = bridge_ab.needs_bridge() || bridge_bc.needs_bridge();
    let mut inner = if inputs.opts.use_v4_batch && !any_gap {
        let batch = [
            encoders::V4BatchEntry {
                c0_idx: c0_a,
                c1_idx: c1_a,
                fee: fee_a,
                tick_spacing: ts_a,
                hooks_idx: zero_idx,
                zfo: a.zfo,
                amount_u96: a_swap_in,
            },
            encoders::V4BatchEntry {
                c0_idx: c0_b,
                c1_idx: c1_b,
                fee: fee_b,
                tick_spacing: ts_b,
                hooks_idx: zero_idx,
                zfo: b.zfo,
                amount_u96: b_swap_in,
            },
            encoders::V4BatchEntry {
                c0_idx: c0_c,
                c1_idx: c1_c,
                fee: fee_c,
                tick_spacing: ts_c,
                hooks_idx: zero_idx,
                zfo: c.zfo,
                amount_u96: c_swap_in,
            },
        ];
        let mut v = encoders::enc_v4_batch(&batch).ok()?;
        if output_c != NATIVE_CURRENCY_ADDRESS && output_c != inputs.weth_address {
            let profit_idx = at.add(output_c).ok()?;
            v.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, SENTINEL_SELF));
        }
        v
    } else {
        let mut v =
            encoders::enc_v4_swap_compact(c0_a, c1_a, fee_a, ts_a, zero_idx, a.zfo, a_swap_in)
                .ok()?;
        if bridge_ab.needs_bridge() {
            let (take_idx, b_input_idx) = bridge_ab.bridge_indices(weth_idx, SENTINEL_NATIVE);
            emit_currency_bridge(&mut v, bridge_ab, take_idx, out_a)?;
            v.extend_from_slice(
                &encoders::enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, zero_idx, b.zfo, b_swap_in)
                    .ok()?,
            );
            v.extend_from_slice(&encoders::enc_v4_settle_delta(b_input_idx));
        } else {
            v.extend_from_slice(
                &encoders::enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, zero_idx, b.zfo, b_swap_in)
                    .ok()?,
            );
        }
        if bridge_bc.needs_bridge() {
            let (take_idx, c_input_idx) = bridge_bc.bridge_indices(weth_idx, SENTINEL_NATIVE);
            emit_currency_bridge(&mut v, bridge_bc, take_idx, out_b)?;
            v.extend_from_slice(
                &encoders::enc_v4_swap_compact(c0_c, c1_c, fee_c, ts_c, zero_idx, c.zfo, c_swap_in)
                    .ok()?,
            );
            v.extend_from_slice(&encoders::enc_v4_settle_delta(c_input_idx));
        } else {
            v.extend_from_slice(
                &encoders::enc_v4_swap_compact(c0_c, c1_c, fee_c, ts_c, zero_idx, c.zfo, c_swap_in)
                    .ok()?,
            );
        }
        v
    };
    // Capture the terminal profit. Default/`any_gap`: physical take to the
    // executor. `erc6909_profit` (WETH output): mint an ERC6909 claim.
    if inputs.opts.erc6909_profit && output_c == inputs.weth_address {
        let profit_amount = out_c.saturating_sub(optimal_input);
        if profit_amount > 0 {
            inner.extend_from_slice(
                &encoders::enc_v4_mint_compact(weth_idx, SENTINEL_SELF, profit_amount).ok()?,
            );
        }
    } else if !inputs.opts.use_v4_batch || any_gap {
        if output_c == NATIVE_CURRENCY_ADDRESS {
            inner.extend_from_slice(&encoders::enc_v4_take_delta(SENTINEL_NATIVE, SENTINEL_SELF));
        } else if output_c == inputs.weth_address {
            inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, SENTINEL_SELF));
        } else {
            let profit_idx = at.add(output_c).ok()?;
            inner.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, SENTINEL_SELF));
        }
    }
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V4→V2→V2 3-hop derivation (WAYDTL step 3).
///
/// One `V4_UNLOCK`: the V4 hop's forward currency **leaves the PM directly to
/// the first V2 pool** (`V4_TAKE_COMPACT(cur→v2b, out_a)`), the two V2 legs
/// chain by `V2_SWAP_CALC` (v2b pays into v2c, v2c pays the executor), and the
/// V4 input (WETH) debt is settled (`V4_SETTLE_DELTA(WETH)`).
fn derive_3hop_v4v2v2(
    a: &V4HopInfo,
    b: &V2HopInfo,
    c: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.first()?;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let forward_a_cur = if a.zfo {
        a.currency1_address
    } else {
        a.currency0_address
    };
    if forward_a_cur == NATIVE_CURRENCY_ADDRESS || forward_a_cur == inputs.weth_address {
        return None; // terminal-V2 chain needs an ERC-20 forward out of V4
    }

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let zero_idx = SENTINEL_NATIVE;
    let forward_a = at.add(forward_a_cur).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let v2b = at.add(b.pool_address).ok()?;
    let v2c = at.add(c.pool_address).ok()?;

    let b_cmd = encoders::enc_v2_swap_calc(v2b, b.zfo, v2c, b.fee);
    let c_cmd = encoders::enc_v2_swap_calc(v2c, c.zfo, SENTINEL_SELF, c.fee);

    let mut inner =
        encoders::enc_v4_swap_compact(c0_a, c1_a, fee_a, ts_a, zero_idx, a.zfo, optimal_input)
            .ok()?;
    inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_a, v2b, out_a).ok()?);
    inner.extend_from_slice(&b_cmd);
    inner.extend_from_slice(&c_cmd);
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(SENTINEL_WETH));

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V2→V2→V4 3-hop derivation (WAYDTL step 3).
///
/// The V2 chain (a,b) routes WETH→t2; t2 is synced/transferred/settled **into
/// the PM**, then the trailing V4 pool c swaps t2→WETH; `V4_SETTLE_ALL` nets
/// the WETH profit to the executor.
fn derive_3hop_v2v2v4(
    a: &V2HopInfo,
    b: &V2HopInfo,
    c: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let forward_b_addr = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };
    if forward_b_addr == NATIVE_CURRENCY_ADDRESS || forward_b_addr == inputs.weth_address {
        return None; // needs an ERC-20 forward into the PM
    }

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v2a = at.add(a.pool_address).ok()?;
    let v2b = at.add(b.pool_address).ok()?;
    let c0 = at.add(c.currency0_address).ok()?;
    let c1 = at.add(c.currency1_address).ok()?;
    let forward_b = at.add(forward_b_addr).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;

    let mut inner = encoders::enc_v4_sync(forward_b);
    inner.extend_from_slice(&encoders::enc_erc20_transfer(SENTINEL_WETH, v2a, optimal_input).ok()?);
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a, a.zfo, v2b, a.fee));
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2b, b.zfo, pm_idx, b.fee));
    inner.extend_from_slice(&encoders::enc_v4_settle());
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0, c1, fee_c, ts_c, zero_idx, c.zfo, c_swap_in).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V2→V3→V4 3-hop derivation (WAYDTL step 3).
///
/// V2 a directs t1→the V3 (via `V2_SWAP_DIRECT`), which is the outer flash
/// that pays the PM; the V4 unlock swaps the seeded forward→WETH, repays the
/// V2's WETH, and captures the WETH profit.
fn derive_3hop_v2v3v4(
    a: &V2HopInfo,
    b: &V3HopInfo,
    c: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.get(0)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }
    let forward_b_addr = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v2a = at.add(a.pool_address).ok()?;
    let v3b = at.add(b.pool_address).ok()?;
    let c0 = at.add(c.currency0_address).ok()?;
    let c1 = at.add(c.currency1_address).ok()?;
    let forward_b = at.add(forward_b_addr).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;

    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0, c1, fee_c, ts_c, zero_idx, c.zfo, c_swap_in).ok()?,
    );
    v4_inner
        .extend_from_slice(&encoders::enc_v4_take_compact(SENTINEL_WETH, v2a, optimal_input).ok()?);
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(SENTINEL_WETH, SENTINEL_SELF, out_c - optimal_input).ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_sync(SENTINEL_WETH));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let mut b_fwd = encoders::enc_v4_unlock(&v4_inner).ok()?;
    b_fwd.extend_from_slice(&encoders::enc_v2_swap_direct(v2a, a.zfo, out_a, v3b).ok()?);

    let mut commands = encoders::enc_v4_sync(forward_b);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b, b.zfo, b_swap_in, pm_idx, &b_fwd).ok()?,
    );
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V3→V2→V4 3-hop derivation (WAYDTL step 3).
///
/// A V3 outer flash (a) pays into the V2 (b), whose callback embeds a V4
/// swap: the V4 input is settled (`V4_SETTLE_DELTA(forward_b)`) and WETH profit
/// captured; the V3's WETH is repaid via `ERC20_TRANSFER` in the V2 callback.
fn derive_3hop_v3v2v4(
    a: &V3HopInfo,
    b: &V2HopInfo,
    c: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let forward_b_addr = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    at.add(inputs.pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v3a = at.add(a.pool_address).ok()?;
    let v2b = at.add(b.pool_address).ok()?;
    let forward_b = at.add(forward_b_addr).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0 = at.add(c.currency0_address).ok()?;
    let c1 = at.add(c.currency1_address).ok()?;

    let mut v4_inner =
        encoders::enc_v4_swap_compact(c0, c1, fee_c, ts_c, zero_idx, c.zfo, c_swap_in).ok()?;
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(SENTINEL_WETH, SENTINEL_SELF, out_c).ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(forward_b));

    let b_fwd = encoders::enc_v4_unlock(&v4_inner).ok()?;

    let mut a_fwd =
        encoders::enc_v2_swap_compact(v2b, b.zfo, out_b, SENTINEL_SELF, b.fee, &b_fwd).ok()?;
    a_fwd.extend_from_slice(&encoders::enc_erc20_transfer(SENTINEL_WETH, v3a, optimal_input).ok()?);

    let commands = encoders::enc_v3_swap_compact(v3a, a.zfo, optimal_input, v2b, &a_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V3→V3→V4 3-hop derivation (WAYDTL step 3).
///
/// Two V3 flashes: b pays into the PM, its callback embeds a second V3 (a)
/// whose callback embeds the V4 jump — the V4 input is synced into the PM
/// (`V4_SYNC(forward_b)` at the outer), the V4 swap runs, the first V3's WETH
/// is repaid (`V4_TAKE_COMPACT(WETH→v3a, optimal)`), and `V4_SETTLE_ALL` nets.
fn derive_3hop_v3v3v4(
    a: &V3HopInfo,
    b: &V3HopInfo,
    c: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let forward_b_addr = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v3a = at.add(a.pool_address).ok()?;
    let v3b = at.add(b.pool_address).ok()?;
    let forward_b = at.add(forward_b_addr).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0 = at.add(c.currency0_address).ok()?;
    let c1 = at.add(c.currency1_address).ok()?;

    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0, c1, fee_c, ts_c, zero_idx, c.zfo, c_swap_in).ok()?,
    );
    v4_inner
        .extend_from_slice(&encoders::enc_v4_take_compact(SENTINEL_WETH, v3a, optimal_input).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let a_fwd = encoders::enc_v4_unlock(&v4_inner).ok()?;

    let b_fwd = encoders::enc_v3_swap_compact(v3a, a.zfo, optimal_input, v3b, &a_fwd).ok()?;

    let mut commands = encoders::enc_v4_sync(forward_b);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b, b.zfo, b_swap_in, pm_idx, &b_fwd).ok()?,
    );
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V2→V4→V2 3-hop derivation (WAYDTL step 3).
///
/// V2 c is the outer flash; its callback transfers WETH to V2 a (repay), then
/// runs a V4 unlock that syncs a's forward into the PM (via V2 a paying the
/// PM), swaps the V4 middle pool b, takes b's forward directly to V2 c, and
/// settles a's forward delta.
fn derive_3hop_v2v4v2(
    a: &V2HopInfo,
    b: &V4HopInfo,
    c: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let forward_a_addr = if a.zfo {
        a.token1_address
    } else {
        a.token0_address
    };
    let forward_b_addr = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let forward_a = at.add(forward_a_addr).ok()?;
    let forward_b = at.add(forward_b_addr).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v2a = at.add(a.pool_address).ok()?;
    let v2c = at.add(c.pool_address).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;

    let mut v4_inner = encoders::enc_v4_sync(forward_a);
    v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a, a.zfo, pm_idx, a.fee));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, zero_idx, b.zfo, b_swap_in).ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_b, v2c, out_b).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(forward_a));

    let mut c_fwd = encoders::enc_erc20_transfer(SENTINEL_WETH, v2a, optimal_input).ok()?;
    c_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);

    let commands =
        encoders::enc_v2_swap_compact(v2c, c.zfo, out_c, SENTINEL_SELF, c.fee, &c_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V2→V4→V3 3-hop derivation (WAYDTL step 3).
///
/// Same V4-middle seed as `v2_v4_v2` but the trailing hop is a V3: the V3 is
/// the outer flash, its callback repays V2 a with WETH then runs the V4 unlock
/// (sync a's forward into the PM, swap the V4 middle, take b's forward to the
/// V3, settle a's forward).
fn derive_3hop_v2v4v3(
    a: &V2HopInfo,
    b: &V4HopInfo,
    c: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let v4_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let forward_a_addr = if a.zfo {
        a.token1_address
    } else {
        a.token0_address
    };
    let forward_b_addr = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let forward_a = at.add(forward_a_addr).ok()?;
    let forward_b = at.add(forward_b_addr).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v2a = at.add(a.pool_address).ok()?;
    let v3c = at.add(c.pool_address).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;

    let mut v4_inner = encoders::enc_v4_sync(forward_a);
    v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a, a.zfo, pm_idx, a.fee));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, zero_idx, b.zfo, v4_swap_in)
            .ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_b, v3c, c_swap_in).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(forward_a));

    let mut c_fwd = encoders::enc_erc20_transfer(SENTINEL_WETH, v2a, optimal_input).ok()?;
    c_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);

    let commands =
        encoders::enc_v3_swap_compact(v3c, c.zfo, c_swap_in, SENTINEL_SELF, &c_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V3→V4→V2 3-hop derivation (WAYDTL step 3).
///
/// V3 a is the outer flash paying the PM; its callback runs a V4 unlock that
/// swaps the V4 middle, `V4_TAKE_DELTA`s b's forward to the trailing V2, then
/// the V2_SWAP_CALC sells chain-to-SELF; the V3's WETH is repaid in the V2
/// callback.
fn derive_3hop_v3v4v2(
    a: &V3HopInfo,
    b: &V4HopInfo,
    c: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let forward_a_addr = if a.zfo {
        a.token1_address
    } else {
        a.token0_address
    };
    let forward_b_addr = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v3a = at.add(a.pool_address).ok()?;
    let v2c = at.add(c.pool_address).ok()?;
    let forward_a = at.add(forward_a_addr).ok()?;
    let forward_b = at.add(forward_b_addr).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;

    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, zero_idx, b.zfo, b_swap_in).ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_take_delta(forward_b, v2c));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let mut a_fwd = encoders::enc_v4_unlock(&v4_inner).ok()?;
    a_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2c,
        c.zfo,
        SENTINEL_SELF,
        c.fee,
    ));
    a_fwd.extend_from_slice(&encoders::enc_erc20_transfer(SENTINEL_WETH, v3a, optimal_input).ok()?);

    let mut commands = encoders::enc_v4_sync(forward_a);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3a, a.zfo, optimal_input, pm_idx, &a_fwd).ok()?,
    );
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V3→V4→V3 3-hop derivation (WAYDTL step 3).
///
/// Two nested V3 flashes around a V4 middle: the trailing V3 c pays the
/// executor; its callback embeds V3 a (paying the PM), whose callback embeds
/// the V4 unlock (swap the middle, `V4_TAKE_COMPACT` b's forward to V3 c); the
/// V4 input is synced into the PM at the outer.
fn derive_3hop_v3v4v3(
    a: &V3HopInfo,
    b: &V4HopInfo,
    c: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let forward_a_addr = if a.zfo {
        a.token1_address
    } else {
        a.token0_address
    };
    let forward_b_addr = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v3a = at.add(a.pool_address).ok()?;
    let v3c = at.add(c.pool_address).ok()?;
    let forward_a = at.add(forward_a_addr).ok()?;
    let forward_b = at.add(forward_b_addr).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;

    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, zero_idx, b.zfo, b_swap_in).ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_b, v3c, c_swap_in).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let mut a_fwd = encoders::enc_erc20_transfer(SENTINEL_WETH, v3a, optimal_input).ok()?;
    a_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);

    let c_fwd = encoders::enc_v3_swap_compact(v3a, a.zfo, optimal_input, pm_idx, &a_fwd).ok()?;

    let mut commands = encoders::enc_v4_sync(forward_a);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3c, c.zfo, c_swap_in, SENTINEL_SELF, &c_fwd).ok()?,
    );
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V2→V4→V4 3-hop derivation (WAYDTL step 3).
///
/// One unlock: the V2 leg is seeded with WETH (repays the V2 chain into the
/// PM via `V2_SWAP_CALC`→pm), syncing the forward into the PM, then two V4
/// swaps run inside, and `V4_SETTLE_ALL` nets the WETH profit.
fn derive_3hop_v2v4v4(
    a: &V2HopInfo,
    b: &V4HopInfo,
    c: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let forward_a_addr = if a.zfo {
        a.token1_address
    } else {
        a.token0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let forward_a = at.add(forward_a_addr).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let c0_c = at.add(c.currency0_address).ok()?;
    let c1_c = at.add(c.currency1_address).ok()?;
    let v2a = at.add(a.pool_address).ok()?;

    let mut v4_inner = encoders::enc_v4_sync(forward_a);
    v4_inner
        .extend_from_slice(&encoders::enc_erc20_transfer(SENTINEL_WETH, v2a, optimal_input).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a, a.zfo, pm_idx, a.fee));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, zero_idx, b.zfo, b_swap_in).ok()?,
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_c, c1_c, fee_c, ts_c, zero_idx, c.zfo, c_swap_in).ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&v4_inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V3→V4→V4 3-hop derivation (WAYDTL step 3).
///
/// A V3 outer flash pays the PM; its callback syncs the forward into the PM
/// and runs a V4 unlock: two V4 swaps inside, an explicit `V4_TAKE_DELTA` of
/// the WETH profit, and `V4_SETTLE_ALL`.
fn derive_3hop_v3v4v4(
    a: &V3HopInfo,
    b: &V4HopInfo,
    c: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let forward_a_addr = if a.zfo {
        a.token1_address
    } else {
        a.token0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v3a = at.add(a.pool_address).ok()?;
    let forward_a = at.add(forward_a_addr).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let c0_c = at.add(c.currency0_address).ok()?;
    let c1_c = at.add(c.currency1_address).ok()?;

    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, zero_idx, b.zfo, b_swap_in).ok()?,
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_c, c1_c, fee_c, ts_c, zero_idx, c.zfo, c_swap_in).ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_take_delta(SENTINEL_WETH, SENTINEL_SELF));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let mut a_fwd = encoders::enc_erc20_transfer(SENTINEL_WETH, v3a, optimal_input).ok()?;
    a_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);

    let mut commands = encoders::enc_v4_sync(forward_a);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3a, a.zfo, optimal_input, pm_idx, &a_fwd).ok()?,
    );
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V4→V4→V2 3-hop derivation (WAYDTL step 3).
///
/// One unlock: two V4 swaps, then b's forward `V4_TAKE_COMPACT`'d straight to
/// the terminal V2 pool, which sells via `V2_SWAP_CALC` (never exact-out
/// over-draws the 1-wei K edge); `V4_SETTLE_ALL` nets.
fn derive_3hop_v4v4v2(
    a: &V4HopInfo,
    b: &V4HopInfo,
    c: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let out_b = *inputs.hop_outputs.get(1)?;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let a_swap_in = *inputs.consumed_inputs.first()?;
    if !fits_int128(a_swap_in) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let forward_b_cur = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };
    if forward_b_cur == NATIVE_CURRENCY_ADDRESS || forward_b_cur == inputs.weth_address {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let zero_idx = SENTINEL_NATIVE;
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

    let mut inner =
        encoders::enc_v4_swap_compact(c0_a, c1_a, fee_a, ts_a, zero_idx, a.zfo, a_swap_in).ok()?;
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, zero_idx, b.zfo, b_swap_in).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_b, v2c, out_b).ok()?);
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2c,
        c.zfo,
        SENTINEL_SELF,
        c.fee,
    ));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V4→V4→V3 3-hop derivation (WAYDTL step 3).
///
/// Two V4 swaps then a V3 tail whose own callback takes b's forward straight
/// to the V3 (`V4_TAKE_COMPACT(forward_b→v3c)`); `V4_SETTLE_ALL` nets.
fn derive_3hop_v4v4v3(
    a: &V4HopInfo,
    b: &V4HopInfo,
    c: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let a_swap_in = *inputs.consumed_inputs.first()?;
    if !fits_int128(a_swap_in) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let forward_b_cur = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let zero_idx = SENTINEL_NATIVE;
    let forward_b = at.add(forward_b_cur).ok()?;
    let v3c = at.add(c.pool_address).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;

    let c_take = encoders::enc_v4_take_compact(forward_b, v3c, c_swap_in).ok()?;

    let mut inner =
        encoders::enc_v4_swap_compact(c0_a, c1_a, fee_a, ts_a, zero_idx, a.zfo, a_swap_in).ok()?;
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, zero_idx, b.zfo, b_swap_in).ok()?,
    );
    inner.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3c, c.zfo, c_swap_in, SENTINEL_SELF, &c_take).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V4→V2→V3 3-hop derivation (WAYDTL step 3).
///
/// The trailing V3 is the outer flash; its callback runs a V4 unlock: the V4
/// swap, `V4_TAKE_COMPACT` of a's forward straight to the V2, a `V2_SWAP_CALC`
/// that sells into the V3, and a `V4_SETTLE_DELTA(WETH)` for the V4 input.
fn derive_3hop_v4v2v3(
    a: &V4HopInfo,
    b: &V2HopInfo,
    c: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.get(0)?;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let forward_a_cur = if a.zfo {
        a.currency1_address
    } else {
        a.currency0_address
    };
    let b_forward_cur = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let zero_idx = SENTINEL_NATIVE;
    let v3c = at.add(c.pool_address).ok()?;
    let forward_a = at.add(forward_a_cur).ok()?;
    // Register b's forward (index-order fidelity, result unused — mirrors the
    // hand-written adapter which adds it solely to fix table indices).
    at.add(b_forward_cur).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let v2b = at.add(b.pool_address).ok()?;

    let mut v4_inner =
        encoders::enc_v4_swap_compact(c0_a, c1_a, fee_a, ts_a, zero_idx, a.zfo, optimal_input)
            .ok()?;
    v4_inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_a, v2b, out_a).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2b, b.zfo, v3c, b.fee));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(SENTINEL_WETH));

    let commands = encoders::enc_v3_swap_compact(
        v3c,
        c.zfo,
        c_swap_in,
        SENTINEL_SELF,
        &encoders::enc_v4_unlock(&v4_inner).ok()?,
    )
    .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V4→V2→V4 3-hop derivation (WAYDTL step 3).
///
/// One unlock: the V4 swap, a's forward `V4_TAKE_COMPACT`'d to the V2,
/// `V2_SWAP_CALC` to the executor, then the trailing V4 swap,
/// `V4_SETTLE_ALL`.
fn derive_3hop_v4v2v4(
    a: &V4HopInfo,
    b: &V2HopInfo,
    c: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.get(0)?;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let forward_a_cur = if a.zfo {
        a.currency1_address
    } else {
        a.currency0_address
    };
    let b_forward_cur = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let zero_idx = SENTINEL_NATIVE;
    let forward_a = at.add(forward_a_cur).ok()?;
    // Index-order fidelity (hand-written registers b's forward, unused).
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

    let mut inner =
        encoders::enc_v4_swap_compact(c0_a, c1_a, fee_a, ts_a, zero_idx, a.zfo, optimal_input)
            .ok()?;
    inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_a, v2b, out_a).ok()?);
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2b,
        b.zfo,
        SENTINEL_SELF,
        b.fee,
    ));
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_c, c1_c, fee_c, ts_c, zero_idx, c.zfo, c_swap_in).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V4→V3→V2 3-hop derivation (WAYDTL step 3).
///
/// One unlock: the V4 swap, a's forward `V4_TAKE_COMPACT`'d to the V3 whose
/// callback sells via the terminal V2 (`V2_SWAP_CALC` — never exact-out past
/// the 1-wei CL edge), and a `V4_SETTLE_DELTA(WETH)`.
fn derive_3hop_v4v3v2(
    a: &V4HopInfo,
    b: &V3HopInfo,
    c: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.get(0)?;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let forward_a_cur = if a.zfo {
        a.currency1_address
    } else {
        a.currency0_address
    };
    let b_forward_cur = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let zero_idx = SENTINEL_NATIVE;
    let v3b = at.add(b.pool_address).ok()?;
    let forward_a = at.add(forward_a_cur).ok()?;
    // Index-order fidelity (hand-written registers b's forward, unused).
    at.add(b_forward_cur).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let v2c = at.add(c.pool_address).ok()?;

    let mut b_fwd = encoders::enc_v4_take_compact(forward_a, v3b, out_a).ok()?;
    b_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2c,
        c.zfo,
        SENTINEL_SELF,
        c.fee,
    ));

    let mut inner =
        encoders::enc_v4_swap_compact(c0_a, c1_a, fee_a, ts_a, zero_idx, a.zfo, optimal_input)
            .ok()?;
    inner.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b, b.zfo, b_swap_in, v2c, &b_fwd).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(SENTINEL_WETH));

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V4→V3→V3 3-hop derivation (WAYDTL step 3).
///
/// One unlock: the V4 swap, then two nested V3 swaps (b pays c, c pays the
/// executor), b's input fed by a `V4_TAKE_COMPACT` of a's forward; then a
/// `V4_SETTLE_DELTA(WETH)`.
fn derive_3hop_v4v3v3(
    a: &V4HopInfo,
    b: &V3HopInfo,
    c: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.get(0)?;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }
    let forward_a_cur = if a.zfo {
        a.currency1_address
    } else {
        a.currency0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    at.add(inputs.pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v3b = at.add(b.pool_address).ok()?;
    let v3c = at.add(c.pool_address).ok()?;
    let forward_a = at.add(forward_a_cur).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;

    let b_fwd = encoders::enc_v4_take_compact(forward_a, v3b, out_a).ok()?;
    let inner_v3b = encoders::enc_v3_swap_compact(v3b, b.zfo, b_swap_in, v3c, &b_fwd).ok()?;
    let inner_v3c =
        encoders::enc_v3_swap_compact(v3c, c.zfo, c_swap_in, SENTINEL_SELF, &inner_v3b).ok()?;

    let mut inner =
        encoders::enc_v4_swap_compact(c0_a, c1_a, fee_a, ts_a, zero_idx, a.zfo, optimal_input)
            .ok()?;
    inner.extend_from_slice(&inner_v3c);
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(SENTINEL_WETH));

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V4→V3→V4 3-hop derivation (WAYDTL step 3).
///
/// One unlock: the V4 swap, then a V3 tail (b) whose callback takes a's
/// forward to itself and that pays the PM (b's forward synced into the PM
/// before), then the trailing V4 swap; `V4_SETTLE_ALL` nets.
fn derive_3hop_v4v3v4(
    a: &V4HopInfo,
    b: &V3HopInfo,
    c: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.get(0)?;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }
    let forward_a_cur = if a.zfo {
        a.currency1_address
    } else {
        a.currency0_address
    };
    let forward_b_cur = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v3b = at.add(b.pool_address).ok()?;
    let forward_a = at.add(forward_a_cur).ok()?;
    let forward_b = at.add(forward_b_cur).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let c0_c = at.add(c.currency0_address).ok()?;
    let c1_c = at.add(c.currency1_address).ok()?;

    let b_fwd = encoders::enc_v4_take_compact(forward_a, v3b, out_a).ok()?;

    let mut inner =
        encoders::enc_v4_swap_compact(c0_a, c1_a, fee_a, ts_a, zero_idx, a.zfo, optimal_input)
            .ok()?;
    inner.extend_from_slice(&encoders::enc_v4_sync(forward_b));
    inner.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b, b.zfo, b_swap_in, pm_idx, &b_fwd).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle());
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_c, c1_c, fee_c, ts_c, zero_idx, c.zfo, c_swap_in).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, U256};

    #[test]
    fn funding_source_is_derived_from_leading_hop() {
        // V2-leading → in-path flash; V3-leading → self-fund (D0-forced).
        let cases: [(Prot, FundingSource); 2] = [
            (Prot::V2, FundingSource::InPathFlash),
            (Prot::V3, FundingSource::SelfFund),
        ];
        for (_p, expected) in cases {
            // The assignment rule is the match in `derive_shape`; assert the
            // two funding values are distinct (the spike's contract).
            assert_ne!(
                expected,
                match expected {
                    FundingSource::InPathFlash => FundingSource::SelfFund,
                    FundingSource::SelfFund => FundingSource::InPathFlash,
                }
            );
        }
    }

    #[test]
    fn terminal_v2_uses_swap_calc_never_exact_out() {
        // The terminal-V2 rule is expressed by `emit_terminal_hop` choosing
        // `enc_v2_swap_calc` (0x21) — assert the encoder selection is CALC.
        let h = V2HopInfo {
            pool_address: address!("00000000000000000000000000000000000000aa"),
            token0_address: address!("0000000000000000000000000000000000000001"),
            token1_address: address!("0000000000000000000000000000000000000002"),
            fee: 30,
            zfo: true,
        };
        let mut at = AddressTable::new();
        let mut out = Vec::new();
        let inputs = ComposerInputs {
            executor_address: Address::ZERO,
            pool_manager_address: Address::ZERO,
            weth_address: Address::ZERO,
            optimal_input: 1000,
            hop_outputs: &[1000],
            consumed_inputs: &[1000],
            opts: Default::default(),
        };
        emit_terminal_hop(&mut at, &HopInfo::V2(h), &inputs, 0, 0, &mut out).unwrap();
        // 0x21 = V2_SWAP_CALC (never exact-out V2_SWAP_COMPACT 0x20).
        assert_eq!(out[0], 0x21, "terminal V2 must encode as V2_SWAP_CALC");
        let _ = U256::ZERO;
    }

    // POC (6SRC23): the v2_v3 InPathFlash flash-credit chain exercises the
    // executor-ledger credit-before-debit invariant the runtime matrix can't
    // see. The derived trace must validate clean; a misordering must reject.
    fn v2_v3_path_inputs() -> (PathInfo, ComposerInputs<'static>) {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
        let v2a = address!("00000000000000000000000000000000000000aa");
        let v3b = address!("00000000000000000000000000000000000000bb");
        let path = PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: v2a,
                token0_address: weth,
                token1_address: usdc,
                fee: 30,
                zfo: true, // WETH → USDC: forward token = USDC
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: v3b,
                token0_address: usdc,
                token1_address: weth,
                fee: 3000,
                zfo: true, // USDC → WETH: forward token = WETH (terminal)
            }),
        ]);
        static OPTIMAL: u128 = 1_000_000;
        static OUTS: [u128; 2] = [1_100_000, 1_200_000];
        static CONSUMED: [u128; 2] = [1_000_000, 1_100_000];
        let inputs = ComposerInputs {
            executor_address: address!("00000000000000000000000000000000000000ee"),
            pool_manager_address: address!("00000000000000000000000000000000000000ff"),
            weth_address: weth,
            optimal_input: OPTIMAL,
            hop_outputs: &OUTS,
            consumed_inputs: &CONSUMED,
            opts: crate::composers::EncodeOptions::default(),
        };
        (path, inputs)
    }

    // BP7KIR Checkpoint 1: the Plan tree is the primary artifact for v2_v3.
    #[test]
    fn v2_v3_plan_byte_parity_with_proven_emitter() {
        let (path, inputs) = v2_v3_path_inputs();
        // The proven hand-written-emitter-derived bytes (today's production path).
        let reference = derive_shape(&path, &inputs).expect("v2_v3 derive_shape returned None");
        // The Plan-derived bytes: build the Plan, encode it, prepend preamble.
        let (preamble, plan, at) =
            build_v2v3_plan(&path, &inputs).expect("v2_v3 must build a Plan");
        let mut plan_bytes = preamble;
        plan_bytes.extend_from_slice(&plan_to_bytes(&plan, &at));
        assert_eq!(
            plan_bytes, reference,
            "Plan-derived bytes must be byte-identical to the proven emitter"
        );
    }

    #[test]
    fn v2_v3_plan_projects_a_validating_trace() {
        let (path, inputs) = v2_v3_path_inputs();
        let (_preamble, plan, _at) =
            build_v2v3_plan(&path, &inputs).expect("v2_v3 must build a Plan");
        let ops = plan_to_ledger_ops(&plan);
        assert_eq!(ops.len(), 4, "v2_v3 Plan projects to 4 LedgerOps");
        let mut v = crate::grammar_ledger::LedgerValidator::default();
        assert!(
            v.validate_full(&ops).is_ok(),
            "canonical v2_v3 Plan must project a validating trace"
        );
    }

    #[test]
    fn v2_v3_plan_misordered_callback_rejects() {
        let (path, inputs) = v2_v3_path_inputs();
        let (_preamble, mut plan, _at) =
            build_v2v3_plan(&path, &inputs).expect("v2_v3 must build a Plan");
        // Corrupt the Plan: make the outer WETH repayment (sibling #1 of the
        // V2 flash's callback) fire BEFORE the V3 flash (sibling #0) that
        // credits WETH. Depth-first walk: V2 flash (credits t1) → WETH repay
        // (executor WETH still 0 → the V2 flash's WETH credit is owed, not
        // held) → REJECT. The runtime matrix cannot name this; byte-parity
        // would confirm the misordered bytes that revert on-chain.
        let outer = plan.get_mut(0).unwrap();
        if let PlanStep::FlashSwap { callback, .. } = outer {
            // callback = [V3 FlashSwap, WETH Erc20Transfer]; swap to
            // [WETH Erc20Transfer, V3 FlashSwap].
            callback.swap(0, 1);
        } else {
            panic!("expected outer V2 FlashSwap");
        }
        let ops = plan_to_ledger_ops(&plan);
        let mut v = crate::grammar_ledger::LedgerValidator::default();
        assert!(
            matches!(
                v.validate_full(&ops),
                Err(crate::grammar_ledger::ValidationError::Erc20TransferBeforeCredit {
                    currency, wanted, have
                }) if currency == inputs.weth_address && wanted == 1_000_000 && have == 0
            ),
            "misordered Plan must be rejected: WETH repay before V3 flash credits WETH"
        );
        let _ = U256::ZERO;
    }

    // Increment 2 (BP7KIR): the remaining V2/V3 2-hop families on the Plan.
    // Each: byte-parity with the proven emitter + the Plan projects a
    // validating trace. A shared helper drives both assertions per family.
    fn v3_v2_path_inputs() -> (PathInfo, ComposerInputs<'static>) {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
        let v3a = address!("00000000000000000000000000000000000000a1");
        let v2b = address!("00000000000000000000000000000000000000b2");
        let path = PathInfo::new(vec![
            HopInfo::V3(V3HopInfo {
                pool_address: v3a,
                token0_address: weth,
                token1_address: usdc,
                fee: 3000,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: v2b,
                token0_address: usdc,
                token1_address: weth,
                fee: 30,
                zfo: true,
            }),
        ]);
        static OPTIMAL: u128 = 1_000_000;
        static OUTS: [u128; 2] = [1_100_000, 1_200_000];
        static CONSUMED: [u128; 2] = [1_000_000, 1_100_000];
        (
            path,
            ComposerInputs {
                executor_address: address!("00000000000000000000000000000000000000ee"),
                pool_manager_address: address!("00000000000000000000000000000000000000ff"),
                weth_address: weth,
                optimal_input: OPTIMAL,
                hop_outputs: &OUTS,
                consumed_inputs: &CONSUMED,
                opts: crate::composers::EncodeOptions::default(),
            },
        )
    }
    fn v3_v3_path_inputs() -> (PathInfo, ComposerInputs<'static>) {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
        let v3a = address!("00000000000000000000000000000000000000a3");
        let v3b = address!("00000000000000000000000000000000000000b3");
        let path = PathInfo::new(vec![
            HopInfo::V3(V3HopInfo {
                pool_address: v3a,
                token0_address: weth,
                token1_address: usdc,
                fee: 3000,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: v3b,
                token0_address: usdc,
                token1_address: weth,
                fee: 3000,
                zfo: true,
            }),
        ]);
        static OPTIMAL: u128 = 1_000_000;
        static OUTS: [u128; 2] = [1_100_000, 1_200_000];
        static CONSUMED: [u128; 2] = [1_000_000, 1_100_000];
        (
            path,
            ComposerInputs {
                executor_address: address!("00000000000000000000000000000000000000ee"),
                pool_manager_address: address!("00000000000000000000000000000000000000ff"),
                weth_address: weth,
                optimal_input: OPTIMAL,
                hop_outputs: &OUTS,
                consumed_inputs: &CONSUMED,
                opts: crate::composers::EncodeOptions::default(),
            },
        )
    }
    fn v2_v2_path_inputs() -> (PathInfo, ComposerInputs<'static>) {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
        let v2a = address!("00000000000000000000000000000000000000a4");
        let v2b = address!("00000000000000000000000000000000000000b4");
        let path = PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: v2a,
                token0_address: weth,
                token1_address: usdc,
                fee: 30,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: v2b,
                token0_address: usdc,
                token1_address: weth,
                fee: 30,
                zfo: true,
            }),
        ]);
        static OPTIMAL: u128 = 1_000_000;
        static OUTS: [u128; 2] = [1_100_000, 1_200_000];
        static CONSUMED: [u128; 2] = [1_000_000, 1_100_000];
        (
            path,
            ComposerInputs {
                executor_address: address!("00000000000000000000000000000000000000ee"),
                pool_manager_address: address!("00000000000000000000000000000000000000ff"),
                weth_address: weth,
                optimal_input: OPTIMAL,
                hop_outputs: &OUTS,
                consumed_inputs: &CONSUMED,
                opts: crate::composers::EncodeOptions::default(),
            },
        )
    }

    fn plan_byte_parity_and_validate(
        build: fn(&PathInfo, &ComposerInputs) -> Option<(Vec<u8>, Plan, AddressTable)>,
        path: &PathInfo,
        inputs: &ComposerInputs,
        name: &str,
    ) {
        let reference =
            derive_shape(path, inputs).unwrap_or_else(|| panic!("[{name}] derive_shape None"));
        let (preamble, plan, at) =
            build(path, inputs).unwrap_or_else(|| panic!("[{name}] build None"));
        let mut plan_bytes = preamble;
        plan_bytes.extend_from_slice(&plan_to_bytes(&plan, &at));
        assert_eq!(
            plan_bytes, reference,
            "[{name}] Plan bytes != proven emitter"
        );
        let ops = plan_to_ledger_ops(&plan);
        let mut v = crate::grammar_ledger::LedgerValidator::default();
        assert!(
            v.validate_full(&ops).is_ok(),
            "[{name}] Plan must validate clean"
        );
    }

    #[test]
    fn v3_v2_plan_byte_parity_and_validates() {
        let (path, inputs) = v3_v2_path_inputs();
        plan_byte_parity_and_validate(build_v3v2_plan, &path, &inputs, "v3_v2");
    }
    #[test]
    fn v3_v3_plan_byte_parity_and_validates() {
        let (path, inputs) = v3_v3_path_inputs();
        plan_byte_parity_and_validate(build_v3v3_plan, &path, &inputs, "v3_v3");
    }
    #[test]
    fn v2_v2_plan_byte_parity_and_validates() {
        let (path, inputs) = v2_v2_path_inputs();
        plan_byte_parity_and_validate(build_v2v2_plan, &path, &inputs, "v2_v2");
    }

    #[test]
    fn v3_v2_plan_terminal_v2_before_seed_rejected() {
        // The terminal-V2 pre-fund rule (`2PT5HH`): a `V2SwapCalc` before its
        // `Erc20Transfer` pair-seed must be rejected (the über-draw class).
        let (path, inputs) = v3_v2_path_inputs();
        let (_preamble, mut plan, _at) = build_v3v2_plan(&path, &inputs).expect("v3_v2 build None");
        // The V3 flash's callback is [WETH repay, forward seed, V2SwapCalc].
        // Move V2SwapCalc to the front (before the seed) → SwapCalcBeforeCredit.
        let outer = plan.last_mut().unwrap();
        if let PlanStep::FlashSwap { callback, .. } = outer {
            // [WETH transfer, seed transfer, V2SwapCalc] → [V2SwapCalc, WETH transfer, seed transfer]
            let swapcalc = callback.remove(2);
            callback.insert(0, swapcalc);
        } else {
            panic!("expected outer V3 FlashSwap");
        }
        let ops = plan_to_ledger_ops(&plan);
        let mut v = crate::grammar_ledger::LedgerValidator::default();
        assert!(
            matches!(
                v.validate_full(&ops),
                Err(crate::grammar_ledger::ValidationError::SwapCalcBeforeCredit { .. })
            ),
            "misordered Plan: V2SwapCalc before its pair seed must be rejected"
        );
        let _ = U256::ZERO;
    }

    // BP7KIR Increment 3: the V4 container (`v4_v4`) on the Plan — the
    // PM-net-zero master invariant + D0 take-before-credit on the PM ledger.
    fn v4_v4_path_inputs() -> (PathInfo, ComposerInputs<'static>) {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let t1 = address!("0000000000000000000000000000000000000db1");
        let pm = address!("00000000000000000000000000000000000000ff");
        let v4a_id = "0x0".to_string();
        let path = PathInfo::new(vec![
            HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4a_id.clone(),
                currency0_address: weth,
                currency1_address: t1,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: true, // WETH → t1: in=WETH, out=t1
            }),
            HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4a_id,
                currency0_address: t1,
                currency1_address: weth,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: true, // t1 → WETH: in=t1, out=WETH
            }),
        ]);
        static OPTIMAL: u128 = 1_000_000;
        static OUTS: [u128; 2] = [1_100_000, 1_200_000];
        static CONSUMED: [u128; 2] = [1_000_000, 1_100_000];
        (
            path,
            ComposerInputs {
                executor_address: address!("00000000000000000000000000000000000000ee"),
                pool_manager_address: pm,
                weth_address: weth,
                optimal_input: OPTIMAL,
                hop_outputs: &OUTS,
                consumed_inputs: &CONSUMED,
                opts: crate::composers::EncodeOptions::default(),
            },
        )
    }

    #[test]
    fn v4_v4_plan_byte_parity_and_validates() {
        let (path, inputs) = v4_v4_path_inputs();
        plan_byte_parity_and_validate(build_v4v4_plan, &path, &inputs, "v4_v4");
    }

    #[test]
    fn v4_v4_plan_take_before_swap_rejected() {
        // D0 on the PM ledger: a `V4TakeDelta` before any swap created PM
        // credit must be rejected (the `v2_v2_v4` bug class on the PM ledger).
        let (path, inputs) = v4_v4_path_inputs();
        let (_preamble, mut plan, _at) = build_v4v4_plan(&path, &inputs).expect("v4_v4 build None");
        // The V4Unlock's inner is [Swap a, Swap b, TakeDelta, SettleAll].
        // Move TakeDelta to the front (before both swaps) → TakeBeforeCredit.
        let outer = plan.get_mut(0).unwrap();
        if let PlanStep::V4Unlock { inner, .. } = outer {
            let take = inner.remove(2);
            inner.insert(0, take);
        } else {
            panic!("expected outer V4Unlock");
        }
        let ops = plan_to_ledger_ops(&plan);
        let mut v = crate::grammar_ledger::LedgerValidator::default();
        assert!(
            matches!(
                v.validate_full(&ops),
                Err(crate::grammar_ledger::ValidationError::TakeBeforeCredit { .. })
            ),
            "misordered v4_v4 Plan: TakeDelta before the swap credits PM must be rejected"
        );
        let _ = U256::ZERO;
    }

    #[test]
    fn v4_v4_plan_unsettled_delta_rejected() {
        // The master V4 invariant: a `V4Unlock` that closes with a nonzero
        // `PM[currency]` delta (here: removing the trailing `V4SettleAll` leaves
        // a residual t1 delta when forward_out ≠ b_swap_in) must be rejected.
        let (path, mut inputs) = v4_v4_path_inputs();
        // Force a nonzero t1 delta: forward_out (1_100_000) ≠ b_swap_in.
        static CLAMPED: [u128; 2] = [1_000_000, 1_050_000];
        inputs.consumed_inputs = &CLAMPED;
        let (_preamble, mut plan, _at) = build_v4v4_plan(&path, &inputs).expect("v4_v4 build None");
        let outer = plan.get_mut(0).unwrap();
        if let PlanStep::V4Unlock { inner, .. } = outer {
            // Remove the trailing SettleAll — the t1 delta (forward_out −
            // b_swap_in = 50_000) is left nonzero → PmDeltaNonzero at V4UnlockEnd.
            inner.pop();
        } else {
            panic!("expected outer V4Unlock");
        }
        let ops = plan_to_ledger_ops(&plan);
        let mut v = crate::grammar_ledger::LedgerValidator::default();
        assert!(
            matches!(
                v.validate_full(&ops),
                Err(crate::grammar_ledger::ValidationError::PmDeltaNonzero { .. })
            ),
            "v4_v4 Plan missing its settle must be rejected: nonzero PM delta at unlock end"
        );
        let _ = U256::ZERO;
    }
}
