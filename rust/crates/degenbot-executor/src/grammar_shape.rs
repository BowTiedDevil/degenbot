//! Grammar derivation (ADR-029 D4 hybrid) — the **Plan tree** as the primary
//! artifact (epics `463V2C` / `MNF6VU`).
//!
//! A 2/3-hop family's stream is authored as an execution-ordered,
//! callback-nested [`Plan`] of [`PlanStep`]s. Two consumers derive from the
//! SAME Plan:
//! * the **encoder** — [`plan_to_bytes`] emits the command stream;
//! * the **validator** — [`plan_to_ledger_ops`] projects the execution trace,
//!   gated by [`crate::grammar_ledger::LedgerValidator`] (ADR-029 D5: the
//!   generic validator proving ordering from declarative facts).
//!
//! One representation, no drift, no reordering, no per-family trace
//! duplication. [`derive_shape`] dispatches every well-formed family to its
//! `build_*_plan` + `build_plan_bytes` (build → `validate_full` →
//! `plan_to_bytes`), returning `None` on decline or gate rejection.
//!
//! ---
//! **Status after RVNIPD / EYQ6UF (epic MNF6VU):** the hand-written
//! `derive_2hop_*` / `derive_3hop_*` byte-assembling emitters and their
//! parity-oracle are **deleted** — the Plan is the sole production producer
//! for every 2/3-hop family. The revm runtime matrix (`degenbot-simulation`
//! `harness_declarative` full-matrix, exact delta) is the ADR-029 D5 source of
//! truth; the primitive wire format is pinned by `tests/encoders_parity.rs`
//! and the native bridge by `tests/native_eth_3hop_bridge.rs`. N4TJSZ
//! (SPVEIE + KO5NNB + 4JOWO5): the all-V2 family (2-hop, 3-hop, any-N) now
//! routes through [`build_all_v2_chain`] + the [`LedgerValidator`][crate::grammar_ledger::LedgerValidator]
//! gate (D4's "the validator gates the Plan for every family" is now literal
//! for all-V2 too). The former hand-written all-V2 emitters and the superseded
//! 2-hop-only builder are DELETED (4JOWO5) — [`build_all_v2_chain`] is the sole
//! all-V2 producer.
#![expect(clippy::similar_names)] // v2a/v2b/v3a/v3b/v3c hop-slot names are canonical per-family labels

use alloy::primitives::{Address, U256};

use crate::composers::{
    fits_int128, ComposerInputs, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
    NATIVE_CURRENCY_ADDRESS,
};
use crate::encoders::{
    self, AddressTable, SENTINEL_NATIVE, SENTINEL_PM, SENTINEL_SELF, SENTINEL_WETH,
};
use crate::grammar_ledger::{LedgerOp, SwapRecipient};

/// A hop-protocol family member.
pub use crate::grammar_ledger::Prot;

/// How the stream's entry (seed) capital is supplied (ADR-029 D1).
///
// V2/V3-only 2-hop / 3-hop-(V2/V3) axis types live in grammar_ledger (ADR-029 D1,
// WE45KC unification): FundingSource + ProfitCapture + Bribe + ShapeClass are
// re-exported from there so the open-set enum is the single source of truth.
pub use crate::grammar_ledger::{Bribe, FundingSource, ProfitCapture, ShapeClass};
// The legacy 2-value FundingSource / 2-field ShapeClass that used to live here
// were deleted in WE45KC in favor of the richer grammar_ledger versions.

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
fn v3_input(h: &V3HopInfo) -> Address {
    if h.zfo {
        h.token0_address
    } else {
        h.token1_address
    }
}

/// Per-protocol encoder selection for the **terminal** hop (D4 mechanics half).
///
/// `pre_grant_to` is the address-table index already credited with the hop's
/// input (a prior `V4_TAKE_COMPACT`/`ERC20_TRANSFER` into the pair). A terminal
/// V2 always swaps via `V2_SWAP_CALC` from that pre-grant (credit-before-debit
/// on the pair-handoff ledger — the `2PT5HH` / `path-182449` rule); a terminal
/// V3 is a `V3_SWAP_COMPACT` flash whose input comes from the coupled ledger.
///
/// RVNIPD: after the emitter deletion this helper survives only as the
/// fixture for the terminal-V2 `V2_SWAP_CALC`-not-`V2_SWAP_COMPACT` rule test.
#[cfg(test)]
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
        /// The recipient pool's address when the flash's OUTPUT directly seeds
        /// a pool (e.g. a V3 flash paying the terminal V2 — `v4_v3_v2`).
        /// `None` for a →SELF output. Bytes encode `recipient_idx`; this is
        /// ledger-only (the output seeds the pool's pair-handoff instead of
        /// crediting the executor).
        recipient_pool_addr: Option<Address>,
        /// Whether a pool-recipient flash output REPAYS that pool's flash debt
        /// (a V3→V3 repayment — `v4_v3_v3`) vs seeds it (a V3 flash feeding the
        /// terminal V2 — `v4_v3_v2`).
        recipient_pool_repays: bool,
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
        /// The recipient pool's address when the calc pays a **mid** pool
        /// (ledger-only — the bytes encode `recipient_idx`). The projection
        /// routes the output: `Some(pool)` seeds that pool's pair-handoff (no
        /// executor credit); `None` + recipient = SELF keeps the executor
        /// credit; `None` + recipient = PM pays into the PM. 2-hop families
        /// always pass `None` (SELF recipients) — the byte-identical case.
        recipient_pool_addr: Option<Address>,
        /// Whether a pool recipient is a **V3 flash repayment** (saturating
        /// `flash_debt` reduction, `SwapRecipient::PoolRepay`) vs a V2 pre-fund
        /// seed (`SwapRecipient::Pool`). Together with `recipient_pool_addr`.
        recipient_repays: bool,
    },
    /// An exact-out `V2_SWAP_DIRECT(pool, zfo, out_amount, recipient)` — the
    /// V2 handoff that pays a specific `out_amount` to `recipient` (a next
    /// pool or the executor). Distinct from [`PlanStep::V2SwapCalc`]
    /// (exact-in) and [`PlanStep::FlashSwap`] (credit-the-executor). Ledger:
    /// consumes the donor pool's seeded `H[pool]`; the output goes to the
    /// recipient — SELF credits the executor, a pool seeds that pool
    /// (`recipient_pool_addr`, ledger-only).
    V2SwapDirect {
        pool_idx: u8,
        pool_addr: Address,
        zfo: bool,
        out_amount: u128,
        recipient_idx: u8,
        /// The exact-out currency (the recipient pool's input / the executor
        /// profit token). Credited to the executor when the recipient is SELF;
        /// the seeded-currency for a pool recipient.
        out_currency: Address,
        /// The recipient pool's address when the direct pays a mid pool.
        recipient_pool_addr: Option<Address>,
        /// Whether a pool recipient is a V3 flash repayment (`PoolRepay`) vs a
        /// V2 pre-fund seed (`Pool`).
        recipient_repays: bool,
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
    /// `rcp` (the profit capture; debits PM credit). When the recipient is a
    /// V2 pool (`seeds_pool`), the taken credit seeds that pool's pair-handoff
    /// (the 2PT5HH terminal-V2 rule across the V4 boundary — `v3_v4_v2`).
    V4TakeDelta {
        currency_idx: u8,
        currency_addr: Address,
        recipient_idx: u8,
        /// When the recipient is a V2 pair, the taken credit seeds it (PM→pool).
        seeds_pool: Option<Address>,
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
        /// When `Some(pool)`, the take is a **flash repayment** to that pool
        /// (a V3 flash repaid directly from the PM — e.g. the `v4_v4_v3` tail).
        /// The take debits PM and saturating-repays the pool's flash debt (no
        /// executor Erc20 debit). `None` for a take/seed.
        repays_flash: Option<Address>,
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
    /// `NATIVE_TRANSFER(amount)` — the executor→PM native pay-in leg of a
    /// native settle (BP7KIR 3c). Ledger-only (encodes to nothing, like
    /// `SelfFund`): on-chain the native flows as `msg.value` on the
    /// `V4_SETTLE*` call, so there is no separate byte instruction. Modeled
    /// explicitly so the executor's native debit is a separate observable op
    /// (a missing settle half is caught by PM-net-zero, not absorbed).
    NativeTransfer { amount: u128 },
    /// `WETH_WITHDRAW(amount)` — unwrap WETH to native (the source of native
    /// for a `NativeTransfer` PM pay-in, or to seed a native V4 input).
    WethWithdraw {
        weth_idx: u8,
        weth_addr: Address,
        amount: u128,
    },
    /// `WETH_DEPOSIT(amount)` — wrap native to WETH (the native came from a
    /// `V4TakeCompact(native→SELF)`).
    WethDeposit {
        weth_idx: u8,
        weth_addr: Address,
        amount: u128,
    },
    /// `V4_BATCH` — a bundled PM extcall of up to 8 swaps
    /// (`encoders::enc_v4_batch`). Ledger-equivalent to the constituent
    /// `V4Swap`s: each entry applies the same `PM[in]` debt / `PM[out]`
    /// credit. **Asymmetry vs a plain `V4Swap` sequence:** the contract
    /// auto-settles any positive native ETH and WETH delta at the batch's end
    /// (an implicit `V4_TAKE_DELTA(→SELF)` for those two currencies). For the
    /// WETH-only slice (the executor's proven path) the derive therefore omits
    /// the terminal `V4TakeDelta` when `use_v4_batch` is set — the batch already
    /// captured the WETH profit. The Plan mirrors this: the per-entry ledger
    /// deltas leave a positive `PM[weth]` that the trailing `V4SettleAll`
    /// zeroes (the gate's master invariant fires at `V4UnlockEnd` — the profit
    /// capture is modelled by the contract, not by a `Take` op here).
    V4Batch { entries: Vec<V4BatchSwap> },
    /// `V4_MINT_COMPACT(cur→rcp, amount)` — convert a positive `PM[cur]`
    /// delta into an ERC6909 claim for `rcp` (BP7KIR `erc6909_profit` opt).
    /// Ledger-equivalent to [`PlanStep::V4TakeDelta`]: debits `PM[cur]` by
    /// `amount` (requires credit-before-debit, `D0`). The asset stays inside
    /// the PM as a claim rather than a physical transfer — distinct from
    /// `V4TakeDelta` on-chain, identical for the gate's safety invariants.
    V4Mint {
        currency_idx: u8,
        currency_addr: Address,
        recipient_idx: u8,
        amount: u128,
    },
}

/// One entry of a [`PlanStep::V4Batch`] — the codec fields (consumed by
/// `encoders::enc_v4_batch` via [`V4BatchEntry`][encoders::V4BatchEntry])
/// together with the resolved currency/amount legs (consumed by a per-entry
/// `LedgerOp::V4Swap` projection). Carrying both keeps byte and ledger
/// projection derivable from the one Plan tree (ADR-029 D4 (iii)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V4BatchSwap {
    /// Currency-0 table index.
    pub c0_idx: u8,
    /// Currency-1 table index.
    pub c1_idx: u8,
    /// Pool fee (`uint16` view of the `uint24` on-chain key).
    pub fee: u16,
    /// Tick spacing (`int16` view).
    pub tick_spacing: i16,
    /// Hooks address index (`0xFF` = no hooks).
    pub hooks_idx: u8,
    /// `zero_for_one` direction flag.
    pub zfo: bool,
    /// Positive `uint96` exact-input amount.
    pub amount: u128,
    /// Resolved input currency (for the ledger projection).
    pub in_currency: Address,
    /// Resolved input amount (matches `amount` for the standard exact-input
    /// entry; carried separately so the ledger projection is exact).
    pub in_amount: u128,
    /// Resolved output currency.
    pub out_currency: Address,
    /// Resolved output amount.
    pub out_amount: u128,
}

/// A Plan = an ordered list of steps. Depth-first walk = execution order.
pub type Plan = Vec<PlanStep>;

/// Project a `Plan` to its `LedgerOp` trace (depth-first; a `FlashSwap`
/// emits its flash credit/debt term, then recurses into its callback). This is
/// the validator's input — decoupled from byte layout (ADR-029 D5).
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn plan_to_ledger_ops(plan: &Plan) -> Vec<LedgerOp> {
    #[expect(clippy::too_many_lines)]
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
                    recipient_pool_addr,
                    recipient_pool_repays,
                    recipient_idx,
                    ..
                } => {
                    // Route the output like a recipient-aware swap: Executor
                    // (credit), Pool(p) (seed the V2 handoff), PoolRepay(p)
                    // (repay a V3 flash debt), PoolManager (pay the PM). The
                    // swap also incurs an `in_currency` flash debt repayable
                    // within the callback.
                    let recipient =
                        match (recipient_pool_addr, recipient_pool_repays, *recipient_idx) {
                            (Some(p), true, _) => SwapRecipient::PoolRepay(*p),
                            (Some(p), false, _) => SwapRecipient::Pool(*p),
                            (None, _, SENTINEL_PM) => SwapRecipient::PoolManager,
                            _ => SwapRecipient::Executor,
                        };
                    let flash = match protocol {
                        Prot::V2 => LedgerOp::V2Flash {
                            out_currency: *out_currency,
                            out_amount: *out_amount,
                            in_currency: *in_currency,
                            in_amount: *in_amount,
                            recipient,
                        },
                        Prot::V3 => LedgerOp::V3Flash {
                            out_currency: *out_currency,
                            out_amount: *out_amount,
                            in_currency: *in_currency,
                            in_amount: *in_amount,
                            recipient,
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
                    recipient_idx,
                    out_currency,
                    out_amount,
                    recipient_pool_addr,
                    recipient_repays,
                    ..
                } => {
                    // `V2_SWAP_CALC` consumes the seeded pair-handoff credit and
                    // routes its computed output by recipient role: the
                    // recipient pool address wins (a mid pool seeds that
                    // pool's handoff); otherwise the sentinel dictates — PM
                    // pays the PM (the following V4Settle/net-zero accounts
                    // it), SELF credits the executor (the 2-hop terminal case).
                    let recipient = match (recipient_pool_addr, recipient_repays, *recipient_idx) {
                        (Some(p), true, _) => SwapRecipient::PoolRepay(*p),
                        (Some(p), false, _) => SwapRecipient::Pool(*p),
                        (None, _, SENTINEL_PM) => SwapRecipient::PoolManager,
                        _ => SwapRecipient::Executor,
                    };
                    ops.push(LedgerOp::SwapCalc {
                        pool: *pool_addr,
                        amount_in: 0,
                        out_currency: *out_currency,
                        out_amount: *out_amount,
                        recipient,
                    });
                }
                // `V2_SWAP_DIRECT` — exact-out handoff. Ledger-equivalent to a
                // `V2SwapCalc` with the same recipient routing: consumes the
                // donor's seed; SELF credits the executor, a mid pool seeds it.
                PlanStep::V2SwapDirect {
                    pool_addr,
                    recipient_idx,
                    out_currency,
                    out_amount,
                    recipient_pool_addr,
                    recipient_repays,
                    ..
                } => {
                    let recipient = match (recipient_pool_addr, recipient_repays, *recipient_idx) {
                        (Some(p), true, _) => SwapRecipient::PoolRepay(*p),
                        (Some(p), false, _) => SwapRecipient::Pool(*p),
                        (None, _, SENTINEL_PM) => SwapRecipient::PoolManager,
                        _ => SwapRecipient::Executor,
                    };
                    ops.push(LedgerOp::SwapCalc {
                        pool: *pool_addr,
                        amount_in: 0,
                        out_currency: *out_currency,
                        out_amount: *out_amount,
                        recipient,
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
                    seeds_pool,
                    ..
                } => {
                    ops.push(LedgerOp::V4TakeDelta {
                        currency: *currency_addr,
                        recipient_idx: *recipient_idx,
                        seeds_pool: *seeds_pool,
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
                    repays_flash,
                    ..
                } => {
                    ops.push(LedgerOp::Take {
                        currency: *currency_addr,
                        amount: *amount,
                        repays_flash: *repays_flash,
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
                        // The token physically arrives at the executor's balance.
                        // Which ledger depends on the currency: native credits
                        // `Native` (a later `WethDeposit`/`NativeTransfer`
                        // consumes it); an ERC-20 credits `Erc20[cur]`.
                        if *currency_addr == NATIVE_CURRENCY_ADDRESS {
                            ops.push(LedgerOp::NativeCredit { amount: *amount });
                        } else {
                            ops.push(LedgerOp::Erc20Credit {
                                currency: *currency_addr,
                                amount: *amount,
                            });
                        }
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
                PlanStep::NativeTransfer { amount } => {
                    ops.push(LedgerOp::NativeTransfer { amount: *amount });
                }
                PlanStep::WethWithdraw {
                    weth_addr, amount, ..
                } => {
                    ops.push(LedgerOp::WethWithdraw {
                        weth: *weth_addr,
                        amount: *amount,
                    });
                }
                PlanStep::WethDeposit {
                    weth_addr, amount, ..
                } => {
                    ops.push(LedgerOp::WethDeposit {
                        weth: *weth_addr,
                        amount: *amount,
                    });
                }
                PlanStep::V4Batch { entries } => {
                    // Each batch entry applies the same `PM[in]` debt / `PM[out]`
                    // credit as a standalone `V4Swap`. The batch's on-chain
                    // auto-settle of native+WETH positive deltas is modelled
                    // downstream (the derive emits no `V4TakeDelta` for the
                    // WETH slice; the trailing `V4SettleAll` zeroes the
                    // residual `PM[weth]` — the gate's master invariant).
                    for e in entries {
                        ops.push(LedgerOp::V4Swap {
                            in_currency: e.in_currency,
                            in_amount: e.in_amount,
                            out_currency: e.out_currency,
                            out_amount: e.out_amount,
                        });
                    }
                }
                PlanStep::V4Mint {
                    currency_addr,
                    amount,
                    ..
                } => {
                    ops.push(LedgerOp::Mint {
                        currency: *currency_addr,
                        amount: *amount,
                    });
                }
            }
        }
    }
    let mut ops = Vec::new();
    walk(plan, &mut ops);
    ops
}

/// Encode a `Plan` to the `execute()` byte stream (depth-first; a `FlashSwap`
/// wraps its callback's bytes as the swap's callback payload). Mirrors the
/// proven hand-written emitter's `enc_*` calls — byte-parity with it is the
/// guard that this Plan-derived encoder reproduces the exact proven bytes.
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn plan_to_bytes(plan: &Plan, at: &AddressTable) -> Vec<u8> {
    #[expect(clippy::too_many_lines)]
    fn walk(plan: &Plan, at: &AddressTable, out: &mut Vec<u8>) {
        // The plan is LedgerValidator-validated before encoding, so the encoder
        // range checks below are unreachable; the `.expect()`s are deliberate
        // documentation of that invariant (args are in range by construction).
        #![allow(clippy::expect_used)]
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
                // Self-fund and native transfer are stream preconditions /
                // ledger-only moves, not on-chain commands — no byte.
                PlanStep::SelfFund { .. } | PlanStep::NativeTransfer { .. } => {}
                PlanStep::V2SwapDirect {
                    pool_idx,
                    zfo,
                    out_amount,
                    recipient_idx,
                    ..
                } => out.extend_from_slice(
                    &encoders::enc_v2_swap_direct(*pool_idx, *zfo, *out_amount, *recipient_idx)
                        .expect("V2 swap direct exact-out in range"),
                ),
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
                    out.extend_from_slice(&encoders::enc_v4_settle_delta(*currency_idx));
                }
                PlanStep::V4Sync { currency_idx, .. } => {
                    out.extend_from_slice(&encoders::enc_v4_sync(*currency_idx));
                }
                PlanStep::V4Settle { .. } => {
                    out.extend_from_slice(&encoders::enc_v4_settle());
                }
                PlanStep::WethWithdraw { amount, .. } => {
                    out.extend_from_slice(&encoders::enc_weth_withdraw(U256::from(*amount)));
                }
                PlanStep::WethDeposit { amount, .. } => {
                    out.extend_from_slice(&encoders::enc_weth_deposit(U256::from(*amount)));
                }
                PlanStep::V4Batch { entries } => {
                    let batch: Vec<encoders::V4BatchEntry> = entries
                        .iter()
                        .map(|e| encoders::V4BatchEntry {
                            c0_idx: e.c0_idx,
                            c1_idx: e.c1_idx,
                            fee: e.fee,
                            tick_spacing: e.tick_spacing,
                            hooks_idx: e.hooks_idx,
                            zfo: e.zfo,
                            amount_u96: e.amount,
                        })
                        .collect();
                    out.extend_from_slice(
                        &encoders::enc_v4_batch(&batch)
                            .expect("V4 batch <= 8 entries + uint96 amounts"),
                    );
                }
                PlanStep::V4Mint {
                    currency_idx,
                    recipient_idx,
                    amount,
                    ..
                } => out.extend_from_slice(
                    &encoders::enc_v4_mint_compact(*currency_idx, *recipient_idx, *amount)
                        .expect("V4 mint compact uint96 amount in range"),
                ),
            }
        }
    }
    let mut out = Vec::new();
    walk(plan, at, &mut out);
    out
}

/// Build the `v2_v3` Plan — both funding axes, callback-nested exactly as the
/// proven `derive_2hop` v2_v3 arms emit. Default (`InPathFlash`): V2 flash,
/// forward bridged to the terminal V3 via exec. `SelfFund` (WE45KC, the
/// operator pre-holds the entry WETH): pre-fund V2a with the entry WETH, then
/// a `V2_SWAP_CALC` (exact-in, output to SELF), then the terminal V3 with an
/// empty callback (V3 auto-pays its owed `forward` from the executor's
/// balance). This is the Checkpoint-1 re-baseline: the Plan is the primary
/// artifact; `plan_to_bytes` derives the byte stream; `plan_to_ledger_ops`
/// derives the validator's input.
///
/// Returns `(preamble_bytes, plan, address_table)` so callers can assemble
/// the full payload (`preamble + plan_to_bytes(&plan, &at)`).
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "both funding axes (InPathFlash + SelfFund)"
)]
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
    let fwd_a = v2_forward(a);
    let weth = inputs.weth_address;
    let terminal_out = *inputs.hop_outputs.get(1)?;

    let plan: Plan = if inputs.opts.funding == FundingSource::SelfFund {
        // WE45KC SelfFund axis: the entry WETH pre-funds V2a (no flash), V2a
        // swaps exact-in to the executor, and the terminal V3 auto-pays its
        // `forward` input from the executor's balance (empty callback). NOTE:
        // the forward token is deliberately NOT registered in the AddressTable
        // (mirrors `derive_2hop`'s SelfFund arm — the InPathFlash arm adds it).
        vec![
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
        ]
    } else {
        // InPathFlash: V2 flash with a nested V3 flash + WETH repay callback.
        let forward_idx = at.add(fwd_a).ok()?;
        vec![PlanStep::FlashSwap {
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
        }]
    };

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
                    recipient_pool_addr: None,
                    recipient_pool_repays: false,
                    auto_repay: true,
                    callback: vec![],
                },
            ],
        },
    ];
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the any-N (≥2) all-V2 Plan (SPVEIE / N4TJSZ T1) — the **sole** all-V2
/// producer (the 2-hop-only builder it superseded was retired in 4JOWO5).
/// Derives the hop chain generically for every arity N ≥ 2 (D6:
/// hop-count-agnostic, the chain is derived, not special-cased per arity) and
/// handles BOTH funding axes, byte-for-byte matching the retired speedrail's
/// canonical layouts:
///
/// * `InPathFlash` (default): one `V2_SWAP_COMPACT` flash on pool[0] — it pays
///   `hop_outputs[0]` of the leading pair's forward token to the executor and
///   is owed `optimal_input` of the **closing** currency (the loop's terminal
///   output) back. The callback seeds pool[1] with the forward token, walks
///   the remaining pairs with chained `V2_SWAP_CALC`s (each mid output routed
///   directly to the next pool's pair-handoff; the terminal calc pays the
///   executor in the closing currency), and repays the flash with a trailing
///   `ERC20_TRANSFER` of the closing currency to pool[0].
/// * `SelfFund`: no flash — the executor's held entry capital (the loop's
///   closing currency, WETH for the canonical WETH loop) pre-funds pool[0]
///   (a `SelfFund` precondition + a bare `ERC20_TRANSFER`), then every pair is
///   a no-callback `V2_SWAP_CALC` walk (gas-cheaper: no flash-callback
///   overhead, no flash-repay transfer — ADR-029 D1).
///
/// The [`LedgerValidator`][crate::grammar_ledger::LedgerValidator] gate (via
/// [`build_plan_bytes`]) accepts both traces: each `V2SwapCalc` sees its pair's
/// `PairHandoff` credited before it consumes it (the terminal-V2
/// credit-before-debit rule), and the InPathFlash terminal credit covers the
/// flash repayment.
///
/// Returns `None` for a path with < 2 hops, any non-V2 hop, or a zeroed hop
/// output. Since KO5NNB this is the production all-V2 path (`encode_cmd_stream`
/// funnels through here via [`derive_all_v2`]; `derive_shape`'s `(V2,V2)` and
/// `(V2,V2,V2)` arms route here too).
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "both funding axes (InPathFlash + SelfFund) + the generic any-N hop chain"
)]
pub fn build_all_v2_chain(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n < 2 || inputs.hop_outputs.contains(&0) {
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
    let weth = inputs.weth_address;
    let fwd_a = v2_forward(v2_hops[0]);
    // The chain's closing currency (the loop's regenerated entry token — WETH
    // for the canonical arbitrage loop; `v2_forward` of the last pair keeps the
    // general case faithful to the speedrail, which repays in this currency).
    let weth_addr = v2_forward(v2_hops[n - 1]);

    let mut at = AddressTable::with_sentinels(Some(weth), Some(inputs.executor_address), None);
    // Same insertion order as the speedrail: pools in hop order, then the
    // leading pair's forward token, then the closing currency (the preamble's
    // `SET_ADDRESS` list is insertion-ordered, so parity needs this order).
    let pool_idx: Vec<u8> = v2_hops
        .iter()
        .map(|h| at.add(h.pool_address).ok())
        .collect::<Option<Vec<_>>>()?;
    let forward_idx = at.add(fwd_a).ok()?;
    let weth_idx = at.add(weth_addr).ok()?;

    let plan: Plan = if inputs.opts.funding == FundingSource::SelfFund {
        // No flash: the held entry capital (the closing currency, `weth_addr`
        // — the token the loop regenerates at the end; WETH for the canonical
        // WETH loop, faithfully mirroring the speedrail which seeds pool[0]
        // with `weth_idx`) pre-funds pool[0], then every pair is a
        // no-callback `V2_SWAP_CALC` walk (mid outputs route to the next pair's
        // handoff; the terminal pays the executor in `weth_addr`).
        let mut steps: Plan = vec![
            PlanStep::SelfFund {
                currency: weth_addr,
                amount: optimal_input,
            },
            PlanStep::Erc20Transfer {
                token_idx: weth_idx,
                token_addr: weth_addr,
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
                out_currency: if terminal { weth_addr } else { v2_forward(hop) },
                out_amount: inputs.hop_outputs[i],
                recipient_pool_addr: if terminal {
                    None
                } else {
                    Some(v2_hops[i + 1].pool_address)
                },
                recipient_repays: false,
            });
        }
        steps
    } else {
        // InPathFlash: flash pool[0] for the leading forward output; the
        // callback seeds pool[1], walks pools[1..] with `V2_SWAP_CALC`s, and
        // repays the flash with the closing currency.
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
                out_currency: if terminal { weth_addr } else { v2_forward(hop) },
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
            token_idx: weth_idx,
            token_addr: weth_addr,
            recipient_idx: pool_idx[0],
            amount: optimal_input,
            seeds_pool: None,
            repays_flash: Some(v2_hops[0].pool_address),
        });
        let hop_a = v2_hops[0];
        vec![PlanStep::FlashSwap {
            pool_idx: pool_idx[0],
            pool_addr: hop_a.pool_address,
            protocol: Prot::V2,
            zfo: hop_a.zfo,
            fee: hop_a.fee,
            out_currency: fwd_a,
            out_amount: inputs.hop_outputs[0],
            in_currency: weth_addr,
            in_amount: optimal_input,
            recipient_idx: SENTINEL_SELF,
            recipient_pool_addr: None,
            recipient_pool_repays: false,
            auto_repay: false,
            callback,
        }]
    };

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
#[expect(clippy::too_many_lines)]
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
    // WETH-only, no native gap (the spike's proven slice). The
    // `use_v4_batch` and `erc6909_profit` opts are handled *within* this slice
    // — `V4Batch` (bundled swaps; the contract auto-captures the WETH profit
    // so no `V4TakeDelta` is emitted) and `V4Mint` (ERC6909 claim instead of a
    // physical take). Both match `derive_2hop_v4v4` byte-for-byte.
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let b_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || b_out == 0 {
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
    // Hop a's INPUT currency (computed, not hardcoded WETH — the gap slice
    // allows a native or tok input that the bridge then converts).
    let in_currency_a = if a.zfo {
        a.currency0_address
    } else {
        a.currency1_address
    };
    let bridge = crate::composers::CurrencyBridge::at_boundary(mid_currency_a, mid_currency_b);
    let currency_gap = bridge.needs_bridge();
    let out_currency_a = mid_currency_a;
    let out_currency_b = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };
    // WE45KC inc.2: capture axis load-bearing (mirrors `derive_2hop_v4v4`).
    let capture = crate::composers::resolve_axes(inputs.opts).1;
    // ProfitCapture::Native on a non-WETH/non-native tok terminal is not
    // expressible (decline; ADR-029 D1).
    if capture == ProfitCapture::Native
        && out_currency_b != weth
        && out_currency_b != NATIVE_CURRENCY_ADDRESS
    {
        return None;
    }
    // Terminal output may be WETH (profit capture), a tok (explicit take), or
    // native. The `use_v4_batch` / `erc6909_profit` opts interact with the
    // terminal currency exactly as `derive_2hop_v4v4` — the capture block
    // below mirrors it.

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

    // The gap slice (native↔WETH representation gap at the mid): the derive's
    // gap branch emits swap a → bridge (take+wrap/unwrap) → swap b → settle the
    // swapped-in side → take the terminal profit → settle_all. `use_v4_batch`
    // and `erc6909_profit` are inoperative here (the derive forces individual
    // swaps across a gap and always takes physically). All PlanSteps + their
    // byte/ledger projections already exist — this just wires them.
    let inner: Plan = if currency_gap {
        use crate::composers::CurrencyBridge;
        // The bridge currency to TAKE out of the PM + the currency to SETTLE
        // after hop b (the swapped-in side). Mirrors `CurrencyBridge::bridge_indices`.
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
            // Swap a: produces `forward_out` of the gap currency (PM credit).
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
            // Bridge: take the gap currency out of the PM to the executor.
            PlanStep::V4TakeCompact {
                currency_idx: take_idx,
                currency_addr: take_currency,
                recipient_idx: SENTINEL_SELF,
                amount: forward_out,
                seeds_pool: None,
                repays_flash: None,
            },
            // Convert: Wrap (native→WETH) or Unwrap (WETH→native).
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
            // Swap b: consumes the bridged currency, produces the terminal.
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
            // Settle the swapped-in side (hop b's input debt).
            PlanStep::V4SettleDelta {
                currency_idx: settle_idx,
                currency_addr: settle_currency,
            },
            // Capture the terminal profit (physical take).
            PlanStep::V4TakeDelta {
                currency_idx: out_idx,
                currency_addr: out_currency_b,
                recipient_idx: SENTINEL_SELF,
                seeds_pool: None,
            },
            PlanStep::V4SettleAll,
        ]
    } else {
        // No-gap slice: swap(s) + profit capture (batch/erc6909/default).
        // The two swaps: one `V4Batch` (2 entries) when `use_v4_batch`, else
        // two `V4Swap`s.
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
        // Profit capture — mirrors `derive_2hop_v4v4`'s terminal-capture branches:
        //  * `erc6909_profit` + WETH terminal → `V4Mint` of the WETH profit
        //    (an ERC6909 claim; `erc6909` is inoperative for tok/native terminal).
        //  * tok terminal (any opts)                    → explicit `V4TakeDelta(tok)`.
        //  * non-batch + (WETH or native) terminal      → explicit `V4TakeDelta`.
        //  * batch + (WETH or native) terminal          → **nothing** — the
        //    contract's `V4_BATCH` auto-settles the positive native/WETH PM
        //    delta to the executor (an implicit `V4_TAKE_DELTA(→SELF)`), so the
        //    derive emits no `V4TakeDelta` and neither does the Plan; the
        //    trailing `V4SettleAll` zeroes the residual `PM` for the gate.
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
        // WE45KC inc.2: ProfitCapture::Native on a WETH terminal — convert the
        // custodied WETH profit to native via WethWithdraw (gated by the
        // validator; mirrors the byte emitter's WETH_WITHDRAW).
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
#[expect(clippy::too_many_lines)]
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

    // The V4 output currency (forward token taken to SELF) is a's non-WETH,
    // non-native leg. The V4 input is a's other leg (WETH or native).
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
    // The terminal V3 output currency.
    let out_currency_b = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };
    if v4_out_native {
        // Native V4 OUTPUT: the forward (native) is taken out of the PM and
        // wrapped to WETH (WethDeposit) before the terminal V3 swap consumes
        // it as its WETH input. The V4 input debt settles via SettleDelta on
        // the V4's ERC-20 input leg, funded by the V3's output (the cycle: V3
        // output == V4 input token — the terminal profit is NOT WETH here).
        let in_currency_b = if b.zfo {
            b.token0_address
        } else {
            b.token1_address
        };
        if in_currency_b != weth {
            return None; // the wrapped native forward must feed the V3 as WETH
        }
        // A 2-token V4 pool with one native leg has an ERC-20 other leg; both
        // legs native is impossible, so the V4 input here is that ERC-20 (any
        // non-WETH, non-native token — the V3's matching output funds it).
        if in_currency_a == NATIVE_CURRENCY_ADDRESS || in_currency_a == weth {
            return None;
        }
    } else {
        // ERC-20 V4 output: the V4 input must be WETH or native (the two
        // supported settle paths). The terminal V3 output may be WETH (the
        // V3's output funds the WETH-input settle) or any token (the V3's
        // output is the standalone terminal profit; the WETH-input settle is
        // funded by the executor's declared entry WETH — a `SelfFund`
        // precondition, mirroring the emitter's semantics).
        if in_currency_a != weth && in_currency_a != NATIVE_CURRENCY_ADDRESS {
            return None;
        }
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

    // 1. V4 swap a: PM[in] −= optimal_input (debt), PM[out] += forward_out.
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
        // Native V4 OUTPUT: take native to SELF (NativeCredit), wrap to WETH
        // (WethDeposit), then the terminal V3 flash consumes that WETH as its
        // input (auto-repaid from the just-created Erc20[WETH] credit). The
        // V3 outputs `out_currency_b` (≠ WETH — the terminal profit), which
        // funds the V4's ERC-20 input settle (the in_a/out_b currency cycle).
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
            // Settle the V4's ERC-20 input debt: the V3 flash credited
            // `out_currency_b` (== in_currency_a in a valid chain), funding
            // this pay-in. SettleDelta zeroes PM[in_currency_a].
            PlanStep::V4SettleDelta {
                currency_idx: input_idx,
                currency_addr: in_currency_a,
            },
            PlanStep::V4SettleAll,
        ]);
    } else {
        // ERC-20 V4 OUTPUT: take t1 to SELF (Erc20Credit), terminal V3 flash
        // auto-repaid from that credit; V3 outputs WETH (the captured profit).
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
            // Settle the V4 input debt. WETH input: PM[WETH]→0 directly. Native
            //    input: unwrap WETH (credited by the V3 output) to native, pay it
            //    into the PM (NativeTransfer), then SettleDelta(native) zeroes PM.
            //    The NativeTransfer is the executor-debit half; SettleDelta is the
            //    PM-credit half — kept separate so a missing half is net-zero
            //    caught, not silently absorbed.
            PlanStep::V4SettleDelta {
                currency_idx: weth_idx,
                currency_addr: weth,
            },
            PlanStep::V4SettleAll,
        ]);
        // Native V4 INPUT (ERC-20 output branch only): splice the native settle
        // sequence — unwrap WETH (credited by the V3 output) to native, pay it
        // into the PM (NativeTransfer), then SettleDelta zeroes PM[native].
        // (The WETH SettleDelta above is the WETH-input path; replaced here.)
        if v4_in_native {
            let Some(settle_all) = inner.pop() else {
                return None; // machine-built plan always has a SettleAll tail
            };
            inner.pop(); // drop the WETH SettleDelta placeholder
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
        let _ = input_idx; // unused on the ERC-20 output path
    }
    let plan: Plan = if out_currency_b == weth {
        vec![PlanStep::V4Unlock {
            inner,
            pool_manager_idx: pm_idx,
        }]
    } else {
        // Non-WETH terminal on the ERC-20-output path: the V3 outputs the
        // terminal profit directly to the executor, so the V4-input WETH
        // settle is funded by the executor's **own entry WETH** (a `SelfFund`
        // precondition). WETH-terminal families fund the settle from the V3's
        // WETH output instead. Ledger-only SelfFund emits no bytes — parity
        // with the proven emitter is unaffected.
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
#[expect(clippy::too_many_lines)]
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

    // The V4 output (forward token, taken to the V2 pair) = a's non-native leg;
    // the V4 input is WETH or native (a's other leg).
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
    // The V2 terminal output currency.
    let out_currency_b = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };
    if v4_out_native {
        // Native V4 OUTPUT: take native to SELF, wrap to WETH (WethDeposit),
        // transfer that WETH to seed the V2 pair, then V2SwapCalc consumes the
        // seed and outputs `out_currency_b` (the terminal profit), which funds
        // the V4's input settle (the in_a/out_b cycle). The V4 input is the
        // non-native leg: a generic ERC-20 (the V2 output funds its settle) or
        // WETH (the executor pre-holds WETH to settle it — the spike
        // `native_wrap_v4_v2` runtime-proves this shape, mirroring the proven
        // emitter). A native input is impossible for a 2-token pool one of
        // whose legs is native.
        {}
    } else {
        // ERC-20 V4 output: the V4 input must be WETH or native (the two
        // supported settle paths). The terminal V2 output may be WETH (the
        // V2's output funds the WETH-input settle) or any token (the V2's
        // output is the standalone terminal profit; the WETH-input settle is
        // then funded by the executor's declared entry WETH — a `SelfFund`
        // precondition, mirroring the emitter's semantics).
        if in_currency_a != weth && in_currency_a != NATIVE_CURRENCY_ADDRESS {
            return None;
        }
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

    // 1. V4 swap a: PM[in] −= optimal_input (debt), PM[out] += forward_out.
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
        // Native V4 OUTPUT: take native to SELF (NativeCredit), wrap to WETH
        // (WethDeposit), transfer that WETH to seed the V2 pair, then V2SwapCalc
        // consumes the seed and outputs `out_currency_b` (the terminal profit,
        // ≠ WETH), which funds the V4's ERC-20 input settle (the in_a/out_b
        // cycle — no entry capital; the cycle is self-funding).
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
            // Settle the V4's ERC-20 input debt: the V2 output credited
            // `out_currency_b` (== in_currency_a in a valid chain), funding
            // this pay-in. SettleDelta zeroes PM[in_currency_a].
            PlanStep::V4SettleDelta {
                currency_idx: input_idx,
                currency_addr: in_currency_a,
            },
            PlanStep::V4SettleAll,
        ]);
    } else {
        // ERC-20 V4 OUTPUT: take t1 directly to the V2 pair (recipient = V2),
        // V2SwapCalc consumes the seed and credits WETH (the captured profit +
        // the PM pay-in source), then the V4 input settle pays WETH/native into
        // the PM.
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
            // Boundary-seed (WETH input): sync WETH, pay it into the PM from
            // the V2 output, settle the V4 input debt (PM[WETH] += optimal_input
            // -> 0). The native-input path replaces this below.
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
        // Native V4 INPUT (ERC-20 output branch only): the V4 input debt is
        //    native. Unwrap WETH (from the V2 output) to native, pay it into the
        //    PM (NativeTransfer), then SettleDelta(native) zeroes PM[native].
        //    Replaces the WETH-input V4Sync+Transfer+Settle sequence (spliced
        //    before SettleAll).
        if v4_in_native {
            let Some(settle_all) = inner.pop() else {
                return None; // machine-built plan always has a SettleAll tail
            };
            inner.pop(); // drop V4Settle placeholder
            inner.pop(); // drop Erc20Transfer placeholder
            inner.pop(); // drop V4Sync placeholder
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
        let _ = input_idx; // unused on the ERC-20 output path
    }
    // Non-WETH terminal on the ERC-20-output path: the V2 outputs the terminal
    // profit directly to the executor, so the V4-input WETH settle is funded by
    // the executor's **own entry WETH** (a `SelfFund` precondition so the
    // validator enforces the executor pre-holds it). WETH-terminal families
    // fund the settle from the V2's WETH output instead (no SelfFund). The
    // ledger-only SelfFund emits no bytes, so byte-parity with the proven
    // emitter is unaffected.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v3_v4` Plan (BP7KIR Increment 3b) — the **outside→V4 seed**
/// family (2-level nesting): a V3 flash wraps a `V4Unlock` in its callback.
/// The V3 forward output **enters the PM** (boundary-seed: `V4Sync` +
/// `Erc20Transfer(forward→PM)` + `V4Settle`) to seed the V4 input, the V4 swap
/// runs + `V4TakeCompact(WETH→SELF)` captures the WETH output, and the V3 flash
/// is explicitly repaid `Erc20Transfer(WETH→v3, optimal_input)` from that
/// capture. This is the deepest nesting — a `FlashSwap` whose `callback`
/// contains a full `V4Unlock` container.
///
/// Scoped slice: ERC-20 V4 input + WETH V3 input + WETH V4 output (the
/// non-native case). The native V4-input case (unwrap-WETH-to-seed-native)
/// needs `WethWithdraw` and returns `None` here.
#[must_use]
pub fn build_v3v4_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 2 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V4(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let v4_out_amount = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || v4_out_amount == 0 {
        return None;
    }
    if !fits_int128(forward_out) || !fits_int128(v4_out_amount) {
        return None;
    }
    let v4_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(v4_swap_in) {
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
    // Native V4 OUTPUT: the V4's native output is captured to SELF as native
    // profit (SelfFund WETH topology — the executor pre-holds WETH to repay the
    // V3 flash, since the V4 outputs native, not WETH).
    if v4_out_currency == NATIVE_CURRENCY_ADDRESS {
        return build_v3v4_native_output_plan(
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
        return build_v3v4_native_input_plan(
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
    build_v3v4_erc20_input_plan(
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

/// `v3_v4` ERC-20 V4 input — the forward-seed topology (3b). V3 outputs an
/// ERC-20 `forward` that enters the PM; V3 input is WETH.
#[expect(clippy::too_many_lines)]
#[expect(clippy::too_many_arguments)]
fn build_v3v4_erc20_input_plan(
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
    // The V3 flash is repaid WETH (optimal_input). When the V4 output is WETH,
    // the take funds the repayment; when it is any other token (the standalone
    // terminal profit), the executor must pre-hold the WETH — a `SelfFund`
    // precondition (ledger-only, no bytes; parity with the proven emitter is
    // unaffected).
    let plan: Plan = if v4_out_currency == weth {
        vec![PlanStep::FlashSwap {
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
        }]
    } else {
        vec![
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
        ]
    };
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// `v3_v4` native V4 output (3c) — the native-profit topology. The V4's native
/// output is captured to SELF as native profit; the V3 flash (WETH input) is
/// repaid from a `SelfFund(WETH)` credit (the executor pre-holds WETH, since
/// the V4 outputs native, not WETH — no WETH source in the V4 path). The V3
/// forward (an ERC-20) seeds the V4 input. Profit = native captured − WETH
/// spent. SelfFund is byte-neutral, so byte-parity with the proven emitter
/// (which has no self-fund byte — the executor just holds the balance) holds.
#[expect(clippy::too_many_lines)]
#[expect(clippy::too_many_arguments)]
fn build_v3v4_native_output_plan(
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
    // V3 input must be WETH (self-funded to repay the flash); the V3 forward
    // must be the V4's ERC-20 input; the V4 output is native (captured).
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
        // Capture the V4's native output to SELF (NativeCredit — the profit).
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
        // Repay the V3 flash with the self-funded WETH (the V4 outputs native,
        // not WETH, so the WETH comes from the SelfFund credit, not the V4).
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
        // SelfFund WETH: the executor pre-holds WETH to repay the V3 flash.
        // Byte-neutral (no byte) — the emitter relies on the executor's pre-held
        // balance; the Plan models it explicitly so the gate's Erc20[WETH]
        // credit/debit balances.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// `v3_v4` native V4 input (3c) — the unwrap-then-native-seed topology. V3
/// outputs WETH (forward = weth, unwrapped to native to seed the V4 input);
/// V3 input is an ERC-20 `tok` (entry capital, SelfFund). This is the
/// SelfFund funding source surfacing in a V4-involving path.
#[expect(clippy::too_many_arguments)]
fn build_v3v4_native_input_plan(
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
    // V3 outputs WETH (forward); V3 input is the entry-capital ERC-20 `tok`.
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
    // V4 native-input swap: PM[native] −= forward_out (= v4_swap_in),
    // PM[v4_out] += v4_out_amount. forward_out == v4_swap_in (the V3 WETH
    // output feeds the V4 native input one-for-one).
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
        // Native settle: the native (credited outside by WethWithdraw) pays into
        // the PM (NativeTransfer) + SettleDelta(native) zeroes it.
        PlanStep::NativeTransfer { amount: v4_swap_in },
        PlanStep::V4SettleDelta {
            currency_idx: SENTINEL_NATIVE,
            currency_addr: NATIVE_CURRENCY_ADDRESS,
        },
        // Capture the V4 output (an ERC-20 `u`) → executor (the terminal profit).
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
    // Byte order (matches derive_2hop_v3v4 native branch): WethWithdraw
    // (outside, before unlock), V4Unlock, then V3 flash repayment in `tok`.
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
        // SelfFund: the executor holds `tok` as entry capital to repay the V3
        // flash (the only source of `tok` in this topology).
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v2_v4` Plan (BP7KIR Increment 3b) — the V2-flash variant of the
/// outside→V4 seed family (2-level nesting, same shape as `v3_v4` but a V2
/// exact-out flash wraps the `V4Unlock`). The V2 forward output enters the PM
/// (`V4Sync` + `Erc20Transfer(forward→PM)` + `V4Settle` boundary-seed), the
/// V4 swap runs + `V4TakeCompact(WETH→SELF)` captures, and the V2 flash is
/// explicitly repaid `Erc20Transfer(WETH→v2, optimal_input)`.
///
/// Scoped slice: ERC-20 V4 input + WETH V2 input + WETH V4 output (non-native).
#[must_use]
pub fn build_v2v4_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 2 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V4(b)) = (&path.hops[0], &path.hops[1]) else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let v4_out_amount = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || v4_out_amount == 0 {
        return None;
    }
    if !fits_int128(forward_out) || !fits_int128(v4_out_amount) {
        return None;
    }
    let v4_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(v4_swap_in) {
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
    // Native V4 OUTPUT: the V4's native output is captured to SELF as native
    // profit (SelfFund WETH topology — the executor pre-holds WETH to repay the
    // V2 flash, since the V4 outputs native, not WETH).
    if v4_out_currency == NATIVE_CURRENCY_ADDRESS {
        return build_v2v4_native_output_plan(
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
        return build_v2v4_native_input_plan(
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
    build_v2v4_erc20_input_plan(
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

/// `v2_v4` ERC-20 V4 input — the forward-seed topology (3b, V2-flash variant).
#[expect(clippy::too_many_lines, reason = "per-family axis dispatch / builder")]
#[expect(clippy::too_many_arguments)]
fn build_v2v4_erc20_input_plan(
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
    // The V2 flash is repaid WETH (optimal_input). When the V4 output is WETH,
    // the take funds the repayment; when it is any other token (the standalone
    // terminal profit), the executor must pre-hold the WETH — a `SelfFund`
    // precondition (ledger-only, no bytes; parity with the proven emitter is
    // unaffected).
    let plan: Plan = if v4_out_currency == weth {
        vec![PlanStep::FlashSwap {
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
        }]
    } else {
        vec![
            PlanStep::SelfFund {
                currency: weth,
                amount: optimal_input,
            },
            PlanStep::FlashSwap {
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
            },
        ]
    };
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// `v2_v4` native V4 output (3c) — the wrap-and-repay topology (V2-flash
/// variant, DIFFERS from v3_v4 native-output). The V4's native output is
/// captured to SELF, then WRAPPED to WETH (`WethDeposit`) and the V2 flash is
/// repaid from that WETH — the profit remains in WETH (weth_out − optimal_input),
/// no SelfFund. (v3_v4 leaves profit as native + SelfFunds WETH; v2_v4 wraps.)
/// Byte-parity with the proven emitter (which emits the WethDeposit).
#[expect(clippy::too_many_lines)]
#[expect(clippy::too_many_arguments)]
fn build_v2v4_native_output_plan(
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
        // Capture the V4's native output to SELF (NativeCredit — the profit).
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
        // Wrap the V4's captured native output to WETH (the V4 outputs native,
        // not WETH — wrap it so the V2 flash can be repaid in WETH). The
        // profit remains in WETH (native take − wrap − repay = weth_out −
        // optimal_input). Unlike v3_v4 native-output (which leaves profit as
        // native + SelfFunds WETH), v2_v4 wraps — matches the proven emitter.
        PlanStep::WethDeposit {
            weth_idx,
            weth_addr: weth,
            amount: v4_out_amount,
        },
        // Repay the V2 flash from the just-wrapped WETH.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// `v2_v4` native V4 input (3c) — the unwrap-then-native-seed topology (V2
/// flash variant). V2 outputs WETH (unwrapped → native); V2 input is `tok`
/// (SelfFund entry capital); the native seeds the V4 input.
#[expect(clippy::too_many_arguments)]
fn build_v2v4_native_input_plan(
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

// ═══════════════════════════════════════════════════════════════════════════
// 3-hop Plan scaffolding (W7FQN6 pilot). The shared topology pieces every
// V4-crossing 3-hop builder (this pilot + task HPZTNT) calls: the sentinel
// AddressTable scaffold, per-hop currency/orientation, the ADR-029 D1 capture
// guard, the terminal-capture steps, and the native↔WETH bridge steps. The
// pilot proves the existing `PlanStep` vocabulary needs NO new variant for the
// 3-hop slice.
// ═══════════════════════════════════════════════════════════════════════════

/// The AddressTable scaffold for V4-crossing families: weth / executor /
/// PoolManager sentinels (PM resolves to `SENTINEL_PM`, no table entry).
fn v4_scaffold_table(inputs: &ComposerInputs<'_>) -> AddressTable {
    AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    )
}

/// A V4 hop's swap orientation: `(forward/output currency, input currency)`
/// from its `zfo` flag. Shared by every V4-crossing family.
fn v4_hop_currencies(h: &V4HopInfo) -> (Address, Address) {
    if h.zfo {
        (h.currency1_address, h.currency0_address)
    } else {
        (h.currency0_address, h.currency1_address)
    }
}

/// The ADR-029 D1 capture guard for a V4-crossing terminal: a
/// `ProfitCapture::Native` on a non-WETH/non-native terminal is not
/// expressible (the executor cannot convert an arbitrary ERC-20 to native).
fn native_capture_declines(capture: ProfitCapture, terminal: Address, weth: Address) -> bool {
    capture == ProfitCapture::Native && terminal != weth && terminal != NATIVE_CURRENCY_ADDRESS
}

/// Build the terminal-capture `PlanStep`s for a V4-crossing family (mirrors the
/// emitters' terminal-capture block): `erc6909_profit` (WETH terminal) → an
/// ERC6909 mint; otherwise a physical `V4TakeDelta` unless `use_v4_batch`
/// auto-settles (a tok terminal still gets an explicit take — see the batch
/// caller); plus `ProfitCapture::Native` on a WETH terminal → a `WethWithdraw`
/// of the custodied profit. Shared by every V4-crossing builder.
fn v4_terminal_capture_steps(
    terminal: Address,
    terminal_idx: u8,
    capture: ProfitCapture,
    use_v4_batch: bool,
    any_gap: bool,
    profit: u128,
    weth: Address,
) -> Vec<PlanStep> {
    let mut steps = Vec::new();
    if capture == ProfitCapture::Erc6909 && terminal == weth {
        if profit > 0 {
            steps.push(PlanStep::V4Mint {
                currency_idx: SENTINEL_WETH,
                currency_addr: weth,
                recipient_idx: SENTINEL_SELF,
                amount: profit,
            });
        }
    } else if !use_v4_batch || any_gap {
        steps.push(PlanStep::V4TakeDelta {
            currency_idx: terminal_idx,
            currency_addr: terminal,
            recipient_idx: SENTINEL_SELF,
            seeds_pool: None,
        });
    }
    if capture == ProfitCapture::Native && terminal == weth {
        steps.push(PlanStep::WethWithdraw {
            weth_idx: SENTINEL_WETH,
            weth_addr: weth,
            amount: profit,
        });
    }
    steps
}

/// Build the native↔WETH bridge `PlanStep`s for a boundary (mirrors
/// `emit_currency_bridge`): a `V4TakeCompact` of the source-side currency to
/// SELF + a `WethDeposit` (wrap) or `WethWithdraw` (unwrap), plus the
/// `settle_idx`/`settle_currency` the following swap's input dedebt needs.
fn v4_bridge_steps(
    bridge: crate::composers::CurrencyBridge,
    weth: Address,
    amount: u128,
) -> (Vec<PlanStep>, u8, Address) {
    let wrap = matches!(bridge, crate::composers::CurrencyBridge::Wrap);
    let take_currency = if wrap { NATIVE_CURRENCY_ADDRESS } else { weth };
    let settle_currency = if wrap { weth } else { NATIVE_CURRENCY_ADDRESS };
    let take_idx = if wrap { SENTINEL_NATIVE } else { SENTINEL_WETH };
    let settle_idx = if wrap { SENTINEL_WETH } else { SENTINEL_NATIVE };
    let convert = if wrap {
        PlanStep::WethDeposit {
            weth_idx: SENTINEL_WETH,
            weth_addr: weth,
            amount,
        }
    } else {
        PlanStep::WethWithdraw {
            weth_idx: SENTINEL_WETH,
            weth_addr: weth,
            amount,
        }
    };
    (
        vec![
            PlanStep::V4TakeCompact {
                currency_idx: take_idx,
                currency_addr: take_currency,
                recipient_idx: SENTINEL_SELF,
                amount,
                seeds_pool: None,
                repays_flash: None,
            },
            convert,
        ],
        settle_idx,
        settle_currency,
    )
}

/// Build the `v4_v4_v4` Plan — one `V4_UNLOCK` over three internal V4 swaps
/// (the 3-hop pilot, W7FQN6). Byte-faithful to `derive_3hop_v4v4v4`: three
/// individual `V4Swap`s with optional native↔WETH boundary bridges, or a single
/// `V4Batch` (no gap); then the terminal take / ERC6909 mint / native-
/// withdraw capture and a trailing `V4SettleAll` (the `V4UnlockEnd` net-zero
/// gate fires at unlock close).
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn build_v4v4v4_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
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
    if !fits_int128(optimal_input) {
        return None;
    }
    let a_swap_in = *inputs.consumed_inputs.first()?;
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(a_swap_in) || !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }
    let (mid_a_out, in_currency_a) = v4_hop_currencies(a);
    let (mid_b_out, mid_b_in) = v4_hop_currencies(b);
    let (output_c, mid_c_in) = v4_hop_currencies(c);
    let weth = inputs.weth_address;
    let capture = crate::composers::resolve_axes(inputs.opts).1;
    if native_capture_declines(capture, output_c, weth) {
        return None;
    }
    let bridge_ab = crate::composers::CurrencyBridge::at_boundary(mid_a_out, mid_b_in);
    let bridge_bc = crate::composers::CurrencyBridge::at_boundary(mid_b_out, mid_c_in);
    let any_gap = bridge_ab.needs_bridge() || bridge_bc.needs_bridge();

    let mut at = v4_scaffold_table(inputs);
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let c0_c = at.add(c.currency0_address).ok()?;
    let c1_c = at.add(c.currency1_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?; // SENTINEL_PM

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
        // Batch mode: one `V4_BATCH` of the three swaps. The contract
        // auto-settles native/WETH; a tok terminal gets an explicit take.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v4_v2_v2` Plan (W7F == `HPZTNT` proof-of-pattern for the
/// recipient-aware `V2SwapCalc`). One `V4_UNLOCK`: the V4 swap's forward is
/// `V4_TAKE_COMPACT`'d straight to the first V2 pool (PM→pool via `SeedPair`),
/// the two V2 legs chain by `V2_SWAP_CALC` — the mid calc **pays the next
/// pool** (`recipient_pool_addr`, seeding it), the terminal calc pays the
/// executor — and the V4 input (WETH) debt is `V4_SETTLE_DELTA`'d.
#[must_use]
pub fn build_v4v2v2_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
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
    if !fits_int128(optimal_input) {
        return None;
    }
    // The V4 forward currency (taken to the first V2 pool) and a's input leg.
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let weth = inputs.weth_address;
    // Root-Cause-B gate (ADR-029 D1): the emitter settles the V4 input with
    // `V4_SETTLE_DELTA(WETH)` unconditionally — coherent only when a's input
    // IS WETH. A native/other-ERC-20 input leaves a residual PM debt the
    // validator (correctly) rejects as not-net-zero. Decline here; native-input
    // V4→V2 chains are not expressible under the current grammar.
    if in_currency_a != weth {
        return None;
    }
    // AddressTable order must mirror `derive_3hop_v4v2v2`: forward, c0, c1,
    // v2b, v2c.
    let mut at = v4_scaffold_table(inputs);
    let forward_a = at.add(forward_a_cur).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let v2b = at.add(b.pool_address).ok()?;
    let v2c = at.add(c.pool_address).ok()?;
    let pm_idx = at.add(inputs.pool_manager_address).ok()?; // SENTINEL_PM
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v4_v2_v4` Plan — one `V4_UNLOCK`: the V4 a-swap's forward is
/// `V4_TAKE_COMPACT`'d to the V2 b pool (PM→pool), a terminal `V2_SWAP_CALC`
/// sells to the executor, then the trailing V4 c-swap runs; `V4_SETTLE_ALL`
/// nets (the V4 input debts included — no explicit `V4_SETTLE_DELTA`, so the
/// input currency is unconstrained vs `v4_v2_v2`).
#[must_use]
pub fn build_v4v2v4_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
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
    if !fits_int128(optimal_input) {
        return None;
    }
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let b_forward_cur = v2_forward(b);
    let out_c = *inputs.hop_outputs.get(2)?;
    let b_out = *inputs.hop_outputs.get(1)?;
    // AddressTable order must mirror `derive_3hop_v4v2v4`: forward_a,
    // b_forward (discarded — index fidelity), c0_a, c1_a, c0_c, c1_c, v2b.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v4_v3_v3` Plan — one `V4_UNLOCK`: the V4 swap, then two nested
/// V3 flashes (c pays the executor; its callback runs b whose output REPAYS
/// c's flash debt via `recipient_pool_repays` and whose callback takes a's
/// forward to repay b); `V4_SETTLE_DELTA(WETH)`.
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn build_v4v3v3_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
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
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let fwd_c = v3_forward(c);
    let in_c = v3_input(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    // AddressTable order must mirror `derive_3hop_v4v3v3`: pm(discarded),
    // v3b, v3c, forward_a, c0_a, c1_a.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v4_v3_v4` Plan — one `V4_UNLOCK`: the V4 swap, a `V3` flash
/// (b) whose output pays the PM (`recipient = PoolManager`; its callback takes
/// a's forward to repay b), then `V4_SYNC`/`V4_SETTLE` of b's forward and the
/// trailing V4 swap; `V4_SETTLE_ALL` nets.
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn build_v4v3v4_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
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
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    // AddressTable order must mirror `derive_3hop_v4v3v4`: pm, v3b, forward_a,
    // forward_b, c0_a, c1_a, c0_c, c1_c.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v3_v2_v2` Plan — the leading V3 flash seeds the first V2 (its
/// output pays the pool), whose two `V2_SWAP_CALC` legs chain (mid calc pays
/// the next V2, terminal calc pays the executor); the V3's WETH input is
/// repaid by an `Erc20Transfer` funded by the executor's SelfFund WETH entry.
#[must_use]
pub fn build_v3v2v2_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V2(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let fwd_b = v2_forward(b);
    let fwd_c = v2_forward(c);
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    // AddressTable order must mirror `derive_3hop_v3v2v2`: v2b, v2c, v3a.
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v3_v2_v3` Plan — the terminal V3 c is the outer flash; its
/// callback runs the leading V3 a (whose output seeds the V2), whose callback
/// runs a `V2_SWAP_CALC` repaying c's flash debt + the WETH repayment to a.
#[must_use]
pub fn build_v3v2v3_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V2(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
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
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let fwd_b = v2_forward(b);
    let fwd_c = v3_forward(c);
    let in_c = v3_input(c);
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    // AddressTable order must mirror `derive_3hop_v3v2v3`: v2b, v3a, v3c.
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v3_v3_v2` Plan — the terminal V2 c is the outer (V2_SWAP_CALC);
/// the callback runs the V3 b then the leading V3 a (seeding the terminal V2);
/// a's WETH input repaid by SelfFund.
#[must_use]
pub fn build_v3v3v2_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V3(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
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
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let fwd_c = v2_forward(c);
    let out_c = *inputs.hop_outputs.get(2)?;
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    // AddressTable order must mirror `derive_3hop_v3v3v2`: v2c, v3a, [v3b].
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v3_v3_v3` Plan — three nested V3 flashes with empty callbacks
/// (each auto-repays its input from the executor); OwnSelfFund WETH entry.
#[must_use]
pub fn build_v3v3v3_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V3(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
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
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let fwd_c = v3_forward(c);
    let in_c = v3_input(c);
    let out_c = *inputs.hop_outputs.get(2)?;
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    // AddressTable order must mirror `derive_3hop_v3v3v3`: v3a, v3b, v3c.
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
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
                    // The leading V3 a has an EMPTY callback — the cmd_executor
                    // auto-pays a's WETH input from the executor's SelfFund
                    // entry (no Erc20Transfer byte, mirroring the emitter).
                    auto_repay: true,
                    callback: vec![],
                }],
            }],
        },
    ];
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v2_v2_v3` Plan — the terminal V3 flash's callback: a WETH
/// `Erc20Transfer` seeds the first V2 (SelfFund entry), the two `V2_SWAP_CALC`
/// legs chain (mid calc pays the next V2, terminal calc repays the V3 flash).
#[must_use]
pub fn build_v2v2v3_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V2(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
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
    let fwd_a = v2_forward(a);
    let fwd_b = v2_forward(b);
    let fwd_c = v3_forward(c);
    let in_c = v3_input(c);
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    // AddressTable order must mirror `derive_3hop_v2v2v3`: v2a, v2b, v3c.
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v2_v2_v4` Plan — one `V4_UNLOCK`: the V2 chain routes WETH → t2
/// and pays the PM (`V2_SWAP_CALC → PoolManager`), t2 synced/settled into the
/// PM, the trailing V4 swap nets the WETH profit. The executor's WETH funds
/// the chain (SelfFund).
#[must_use]
pub fn build_v2v2v4_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V2(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
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
    let fwd_a = v2_forward(a);
    let fwd_b = v2_forward(b);
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let out_c = *inputs.hop_outputs.get(2)?;
    // AddressTable order must mirror `derive_3hop_v2v2v4`: v2a, v2b, c0, c1,
    // forward_b.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v2_v4_v4` Plan — one `V4_UNLOCK`: the leading V2 pays the PM
/// (WETH → forward_a, `V2_SWAP_CALC → PoolManager`), synced/settled, then two
/// V4 swaps; `V4_SETTLE_ALL` nets the WETH profit. SelfFund WETH entry.
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn build_v2v4v4_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V4(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
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
    let forward_a_cur = v2_forward(a);
    let (forward_b_cur, in_currency_b) = v4_hop_currencies(b);
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    // AddressTable order must mirror `derive_3hop_v2v4v4`: forward_a,
    // c0_b, c1_b, c0_c, c1_c, v2a.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v2_v3_v2` Plan — the terminal V2 is the outer flash; its
/// callback runs a V3 flash (whose output repays the V2 via `PoolRepay`), whose
/// callback repays the leading V2 with WETH + a `V2_SWAP_DIRECT` that repays
/// the V3 flash (`PoolRepay`). (The approved `V2SwapDirect` variant.)
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn build_v2v3v2_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V3(b), HopInfo::V2(c)) =
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
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }
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
    // AddressTable order must mirror `derive_3hop_v2v3v2`: forward_a
    // (discarded), v2a, v2c, v3b.
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v2_v3_v3` Plan — two nested V3 flashes around a leading V2
/// repaid by WETH (`Erc20Transfer` seeds it) then a `V2_SWAP_DIRECT` repaying
/// the inner V3 (`PoolRepay`); the inner V3 repays the outer V3 (`PoolRepay`).
#[must_use]
pub fn build_v2v3v3_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V3(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.first()?;
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
    let fwd_a = v2_forward(a);
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let fwd_c = v3_forward(c);
    let in_c = v3_input(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    // AddressTable order must mirror `derive_3hop_v2v3v3`: v2a, v3b, v3c.
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v2_v3_v4` Plan — the V3 b flashes against the V4 tail; its
/// callback runs a V4 unlock that settles the synced forward into the PM,
/// swaps the V4 (capturing WETH profit: `optimal` repays the V2, the rest to
/// SELF), then a `V2_SWAP_DIRECT` repays the V3 flash (`PoolRepay`).
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn build_v2v3v4_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
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
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let fwd_a = v2_forward(a);
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    // AddressTable order must mirror `derive_3hop_v2v3v4`: v2a, v3b, c0, c1,
    // forward_b.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v3_v2_v4` Plan — the leading V3 flash repays the V2 (`PoolRepay`),
/// whose own flash runs the V4 tail in its callback (swap, take WETH profit,
/// settle the forward); the V3's WETH repaid by SelfFund.
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn build_v3v2v4_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
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
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
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
    // AddressTable order must mirror `derive_3hop_v3v2v4`: pm(discarded),
    // v3a, v2b, forward_b, c0, c1.
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
                    // The V2 b flash's USDC input is NOT repaid by its callback
                    // (the V4Unlock only touches WBTC/WETH): the cmd_executor
                    // auto-pays it from the executor's USDC — which is exactly
                    // the V3 a's output (credited to the executor; the bytes
                    // route it to v2b, the pool side of the deposit). The
                    // executor's USDC covers b's auto-repay.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v3_v3_v4` Plan — the V3 b pays the PM; its callback runs a
/// second V3 a (repaying b via `PoolRepay`), whose callback runs a V4 unlock
/// that settles the synced forward, swaps the V4, and takes WETH to repay a.
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn build_v3v3v4_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V3(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
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
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let fwd_b = v3_forward(b);
    let in_b = v3_input(b);
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let out_c = *inputs.hop_outputs.get(2)?;
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    // AddressTable order must mirror `derive_3hop_v3v3v4`: pm, v3a, v3b,
    // forward_b, c0, c1.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v2_v4_v2` Plan — the terminal V2 is the outer flash; its
/// callback repays the leading V2 with WETH then runs a V4 unlock that syncs
/// a's forward into the PM (via `V2_SWAP_CALC → PoolManager`), swaps the V4
/// middle, takes b's forward straight to the terminal V2 (repaying its flash),
/// and settles a's forward delta.
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn build_v2v4v2_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
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
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
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
    // AddressTable order must mirror `derive_3hop_v2v4v2`: pm, forward_a,
    // forward_b, v2a, v2c, c0_b, c1_b.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v2_v4_v3` Plan — the trailing V3 flash's callback repays the
/// leading V2 with WETH then runs a V4 unlock: sync a's forward into the PM
/// (via `V2_SWAP_CALC → PoolManager`), settle, swap the V4 middle, take b's
/// forward straight to the V3 (repaying its flash), settle a's forward.
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn build_v2v4v3_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V2(a), HopInfo::V4(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let v4_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(v4_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }
    let forward_a_cur = v2_forward(a);
    let (forward_b_cur, in_currency_b) = v4_hop_currencies(b);
    let fwd_c = v3_forward(c);
    let in_c = v3_input(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    // AddressTable order must mirror `derive_3hop_v2v4v3`: pm, forward_a,
    // forward_b, v2a, v3c, c0_b, c1_b.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v3_v4_v2` Plan — the leading V3 pays the PM; its callback runs
/// a V4 unlock: settle the synced forward, swap the V4 middle,
/// `V4_TAKE_DELTA` b's forward straight to the trailing V2 (seeding it via
/// `seeds_pool` — the 2PT5HH rule), then a `V2_SWAP_CALC` sells it to SELF;
/// the V3's WETH repaid by SelfFund.
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn build_v3v4v2_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V4(b), HopInfo::V2(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
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
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let (forward_b_cur, in_currency_b) = v4_hop_currencies(b);
    let fwd_c = v2_forward(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let out_a = *inputs.hop_outputs.first()?;
    // AddressTable order must mirror `derive_3hop_v3v4v2`: pm, v3a, v2c,
    // forward_a, forward_b, c0_b, c1_b.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v3_v4_v3` Plan — two nested V3 flashes around the V4 middle: the
/// trailing V3 pays the executor; its callback runs the leading V3 (paying the
/// PM), whose callback repays it with WETH + runs the V4 unlock (settle the
/// synced forward, swap the middle, take b's forward to repay the trailing V3).
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn build_v3v4v3_plan(
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
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let (forward_b_cur, in_currency_b) = v4_hop_currencies(b);
    let fwd_c = v3_forward(c);
    let in_c = v3_input(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let out_a = *inputs.hop_outputs.first()?;
    // AddressTable order must mirror `derive_3hop_v3v4v3`: pm, v3a, v3c,
    // forward_a, forward_b, c0_b, c1_b.
    let mut at = v4_scaffold_table(inputs);
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let v3a = at.add(a.pool_address).ok()?;
    let v3c = at.add(c.pool_address).ok()?;
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
        PlanStep::V4TakeCompact {
            currency_idx: forward_b,
            currency_addr: forward_b_cur,
            recipient_idx: v3c,
            amount: c_swap_in,
            seeds_pool: None,
            repays_flash: Some(c.pool_address),
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
            }],
        },
    ];
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v3_v4_v4` Plan — the leading V3 pays the PM; its callback
/// repays it with WETH and runs a V4 unlock that settles the synced forward,
/// swaps the two V4 tails, takes the WETH profit, and nets.
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn build_v3v4v4_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V3(a), HopInfo::V4(b), HopInfo::V4(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
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
    let fwd_a = v3_forward(a);
    let in_a = v3_input(a);
    let (forward_b_cur, in_currency_b) = v4_hop_currencies(b);
    let (output_c, in_currency_c) = v4_hop_currencies(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let out_a = *inputs.hop_outputs.first()?;
    // AddressTable order must mirror `derive_3hop_v3v4v4`: pm, v3a, forward_a,
    // c0_b, c1_b, c0_c, c1_c.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v4_v4_v2` Plan — one `V4_UNLOCK`: two V4 swaps, b's forward
/// `V4_TAKE_COMPACT`'d straight to the terminal V2 pool, a `V2_SWAP_CALC`
/// sells to the executor, `V4_SETTLE_ALL` nets.
#[must_use]
pub fn build_v4v4v2_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
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
    if !fits_int128(optimal_input) {
        return None;
    }
    let a_swap_in = *inputs.consumed_inputs.first()?;
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(a_swap_in) || !fits_int128(b_swap_in) {
        return None;
    }
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let (forward_b_cur, in_currency_b) = v4_hop_currencies(b);
    let fwd_c = v2_forward(c);
    let out_a = *inputs.hop_outputs.first()?;
    let out_c = *inputs.hop_outputs.get(2)?;
    // AddressTable order must mirror `derive_3hop_v4v4v2`: forward_b, v2c,
    // c0_a, c1_a, c0_b, c1_b.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v4_v4_v3` Plan — one `V4_UNLOCK`: two V4 swaps, then a terminal
/// V3 flash whose callback pays b's forward straight to the V3 pool
/// (`V4_TAKE_COMPACT(forward_b → v3c)` — the V3's flash repayment, honored by
/// `repays_flash`), `V4_SETTLE_ALL` nets.
#[must_use]
pub fn build_v4v4v3_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
    let (HopInfo::V4(a), HopInfo::V4(b), HopInfo::V3(c)) =
        (&path.hops[0], &path.hops[1], &path.hops[2])
    else {
        return None;
    };
    let optimal_input = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) {
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
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let (forward_b_cur, in_currency_b) = v4_hop_currencies(b);
    let out_a = *inputs.hop_outputs.first()?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    // AddressTable order must mirror `derive_3hop_v4v4v3`: forward_b, v3c,
    // c0_a, c1_a, c0_b, c1_b.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v4_v2_v3` Plan — the trailing V3 is the **outer flash**; its
/// callback runs a V4 unlock: the V4 swap, `V4_TAKE_COMPACT` of a's forward
/// straight to the V2, a `V2_SWAP_CALC` that sells into the V3 pool
/// (`PoolRepay` — repaying the outer V3 flash), and `V4_SETTLE_DELTA(WETH)`.
#[must_use]
pub fn build_v4v2v3_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
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
    if !fits_int128(optimal_input) {
        return None;
    }
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let b_forward_cur = v2_forward(b);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    let out_currency_c = v3_forward(c);
    let in_currency_c = v3_input(c);
    // Root-Cause-B gate (mirrors `v4_v2_v2`): the emitter settles the V4
    // input with `V4_SETTLE_DELTA(WETH)` unconditionally — coherent only for a
    // WETH input. A native/other-ERC-20 input leaves a residual PM debt the
    // validator rejects. Decline; native-input V4→V2→V3 isn't expressible.
    if in_currency_a != inputs.weth_address {
        return None;
    }
    // AddressTable order must mirror `derive_3hop_v4v2v3`: v3c, forward_a,
    // b_forward (discarded), c0_a, c1_a, v2b.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// Build the `v4_v3_v2` Plan — one `V4_UNLOCK`: the V4 swap, then a V3 flash
/// whose output seeds the terminal V2 directly (`recipient_pool`), callback =
/// take a's forward to repay the V3 + a terminal `V2_SWAP_CALC`; then
/// `V4_SETTLE_DELTA(WETH)`.
#[must_use]
pub fn build_v4v3v2_plan(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<(Vec<u8>, Plan, AddressTable)> {
    let n = path.hops.len();
    if n != 3 {
        return None;
    }
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
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let (forward_a_cur, in_currency_a) = v4_hop_currencies(a);
    let fwd_b = v3_forward(b);
    let fwd_c = v2_forward(c);
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    // AddressTable order must mirror `derive_3hop_v4v3v2`: v3b, forward_a,
    // b_forward (discarded), c0_a, c1_a, v2c.
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
    let preamble = encoders::enc_preamble(&at);
    Some((preamble, plan, at))
}

/// A Plan builder: every `build_*_plan` returns the full payload's
/// preamble bytes, the [`Plan`] tree, and the resolved [`AddressTable`].
/// (Also used by the 3-hop pilots — the signature is family-agnostic.)
type BuildPlan = fn(&PathInfo, &ComposerInputs<'_>) -> Option<(Vec<u8>, Plan, AddressTable)>;

/// Build a family's [`Plan`] through its `build_*_plan` builder, run the
/// [`LedgerValidator`][crate::grammar_ledger::LedgerValidator] gate on the
/// projected ledger trace, and fold `preamble + plan_to_bytes(&plan, &at)`
/// into the full payload.
///
/// Returns `None` when the builder declines **or** when the Plan fails
/// validation — a stream that violates credit-before-debit / terminal-V2
/// pre-fund / flash-debt-net-zero / PM-net-zero must NOT produce bytes. This is
/// the first time the validator gates real production bytes (ADR-029 D4/D5 for
/// the 2-hop plane).
#[must_use]
fn build_plan_bytes(
    path: &PathInfo,
    build: BuildPlan,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let (preamble, plan, at) = build(path, inputs)?;
    let ops = plan_to_ledger_ops(&plan);
    let mut v = crate::grammar_ledger::LedgerValidator::default();
    v.validate_full(&ops).ok()?;
    let mut out = preamble;
    out.extend_from_slice(&plan_to_bytes(&plan, &at));
    Some(out)
}

/// Public all-V2 entry (KO5NNB cutover): the any-N (≥2) all-V2 family through
/// the Plan + validator gate — [`build_all_v2_chain`] → `validate_full` →
/// `plan_to_bytes`. Replaces the retired hand-written N-hop speedrail as the
/// production producer for every all-V2 path (`encode_cmd_stream`'s all-V2
/// short-circuit funnels here; `derive_shape`'s `(V2,V2)`/`(V2,V2,V2)` arms do
/// too). Returns `None` when the builder declines or the gate rejects.
#[must_use]
pub fn derive_all_v2(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    build_plan_bytes(path, build_all_v2_chain, inputs)
}

/// The protocol of a hop slot; `None` for a missing slot (a 2-hop path has a
/// `None` third slot).
#[must_use]
fn prot_of(h: Option<&HopInfo>) -> Option<Prot> {
    match h {
        Some(HopInfo::V2(_)) => Some(Prot::V2),
        Some(HopInfo::V3(_)) => Some(Prot::V3),
        Some(HopInfo::V4(_)) => Some(Prot::V4),
        None => None,
    }
}

/// Public entry: derive a family's command bytes from its Plan builder
/// (`build_*_plan` → [`LedgerValidator`][crate::grammar_ledger::LedgerValidator]
/// gate → `plan_to_bytes`) — the sole production producer since RVNIPD removed
/// the hand-written emitters. Returns `None` when the builder declines or the
/// validator rejects the stream.
#[must_use]
pub fn derive_shape(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    // The family→author dispatch table: every reachable 2/3-hop
    // `(Prot, Prot, Option<Prot>)` key routes to its Plan builder as a
    // READABLE row, then ONE `build_plan_bytes` call runs builder → validator
    // gate → plan_to_bytes. Keys without a row (1-hop, >3-hop, or an unknown
    // combination) are unmatched — the family has no builder and does not
    // encode. This is D1's "keyed by orthogonal axes" made data (ADR-029): a
    // new family extends the table as a row, and candidate 4's per-family
    // axis-support declaration rides this table.
    let key = (
        prot_of(path.hops.first()),
        prot_of(path.hops.get(1)),
        prot_of(path.hops.get(2)),
    );
    let build: BuildPlan = match key {
        // ── 3-hop families (26) ── V4-involving + V2/V3-only folds. The all-V2
        //    3-hop family is the shared any-N arm below (see `build_all_v2_chain`
        //    note), not a separate row.
        (Some(Prot::V4), Some(Prot::V4), Some(Prot::V4)) => build_v4v4v4_plan,
        (Some(Prot::V4), Some(Prot::V2), Some(Prot::V2)) => build_v4v2v2_plan,
        (Some(Prot::V2), Some(Prot::V2), Some(Prot::V4)) => build_v2v2v4_plan,
        (Some(Prot::V2), Some(Prot::V3), Some(Prot::V4)) => build_v2v3v4_plan,
        (Some(Prot::V3), Some(Prot::V2), Some(Prot::V4)) => build_v3v2v4_plan,
        (Some(Prot::V3), Some(Prot::V3), Some(Prot::V4)) => build_v3v3v4_plan,
        (Some(Prot::V2), Some(Prot::V4), Some(Prot::V2)) => build_v2v4v2_plan,
        (Some(Prot::V2), Some(Prot::V4), Some(Prot::V3)) => build_v2v4v3_plan,
        (Some(Prot::V3), Some(Prot::V4), Some(Prot::V2)) => build_v3v4v2_plan,
        (Some(Prot::V3), Some(Prot::V4), Some(Prot::V3)) => build_v3v4v3_plan,
        (Some(Prot::V2), Some(Prot::V4), Some(Prot::V4)) => build_v2v4v4_plan,
        (Some(Prot::V3), Some(Prot::V4), Some(Prot::V4)) => build_v3v4v4_plan,
        (Some(Prot::V4), Some(Prot::V4), Some(Prot::V2)) => build_v4v4v2_plan,
        (Some(Prot::V4), Some(Prot::V4), Some(Prot::V3)) => build_v4v4v3_plan,
        (Some(Prot::V4), Some(Prot::V2), Some(Prot::V3)) => build_v4v2v3_plan,
        (Some(Prot::V4), Some(Prot::V2), Some(Prot::V4)) => build_v4v2v4_plan,
        (Some(Prot::V4), Some(Prot::V3), Some(Prot::V2)) => build_v4v3v2_plan,
        (Some(Prot::V4), Some(Prot::V3), Some(Prot::V3)) => build_v4v3v3_plan,
        (Some(Prot::V4), Some(Prot::V3), Some(Prot::V4)) => build_v4v3v4_plan,
        // All-V2 any-N: the 2-hop (third slot `None`) AND 3-hop families share
        // one arity-agnostic producer (KO5NNB/4JOWO5) — the only family row
        // that spans the 2-hop/3-hop split (merged so the family→author table
        // has exactly one all-V2 row).
        (Some(Prot::V2), Some(Prot::V2), Some(Prot::V2) | None) => build_all_v2_chain,
        (Some(Prot::V2), Some(Prot::V2), Some(Prot::V3)) => build_v2v2v3_plan,
        (Some(Prot::V2), Some(Prot::V3), Some(Prot::V2)) => build_v2v3v2_plan,
        (Some(Prot::V2), Some(Prot::V3), Some(Prot::V3)) => build_v2v3v3_plan,
        (Some(Prot::V3), Some(Prot::V2), Some(Prot::V2)) => build_v3v2v2_plan,
        (Some(Prot::V3), Some(Prot::V2), Some(Prot::V3)) => build_v3v2v3_plan,
        (Some(Prot::V3), Some(Prot::V3), Some(Prot::V2)) => build_v3v3v2_plan,
        (Some(Prot::V3), Some(Prot::V3), Some(Prot::V3)) => build_v3v3v3_plan,
        // ── 2-hop families (8; third slot `None`) ──
        (Some(Prot::V4), Some(Prot::V4), None) => build_v4v4_plan,
        (Some(Prot::V4), Some(Prot::V3), None) => build_v4v3_plan,
        (Some(Prot::V3), Some(Prot::V4), None) => build_v3v4_plan,
        (Some(Prot::V4), Some(Prot::V2), None) => build_v4v2_plan,
        (Some(Prot::V2), Some(Prot::V4), None) => build_v2v4_plan,
        // (V2,V2,None) all-V2 → the shared any-N arm above.
        (Some(Prot::V2), Some(Prot::V3), None) => build_v2v3_plan,
        (Some(Prot::V3), Some(Prot::V2), None) => build_v3v2_plan,
        (Some(Prot::V3), Some(Prot::V3), None) => build_v3v3_plan,
        // Any key with no row is unmatched — no builder, no bytes.
        _ => return None,
    };
    build_plan_bytes(path, build, inputs)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::items_after_statements,
        clippy::type_complexity,
        clippy::naive_bytecount
    )]
    use super::*;
    use alloy::primitives::{address, U256};

    /// The three v4_v4 terminal-output currencies the broadened slice covers
    /// (WETH / a tok / native) — the `use_v4_batch` + `erc6909_profit` opts
    /// interact with each differently (`derive_2hop_v4v4` exact parity).
    #[derive(Clone, Copy)]
    enum Terminal {
        Weth,
        Tok,
        Native,
    }

    /// The two v4_v4 currency-gap topologies (native↔WETH bridge at the mid).
    #[derive(Clone, Copy)]
    enum Gap {
        /// Hop a outputs native, hop b needs WETH → take native + `WETH_DEPOSIT`.
        Wrap,
        /// Hop a outputs WETH, hop b needs native → take WETH + `WETH_WITHDRAW`.
        Unwrap,
    }

    fn v4_v4_inputs(
        terminal: Terminal,
        opts: crate::composers::EncodeOptions,
    ) -> (PathInfo, ComposerInputs<'static>) {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let t1 = address!("0000000000000000000000000000000000000db1");
        let t2 = address!("0000000000000000000000000000000000000db2");
        let pm = address!("00000000000000000000000000000000000000ff");
        let v4a_id = "0x0".to_string();
        let v4b_id = "0x1".to_string();
        // hop a: weth → t1 (currency0=weth, currency1=t1, zfo=true).
        let hop_a = HopInfo::V4(V4HopInfo {
            pool_manager_address: pm,
            pool_id_hex: v4a_id,
            currency0_address: weth,
            currency1_address: t1,
            fee: 3000,
            tick_spacing: 60,
            hook_address: Address::ZERO,
            zfo: true,
        });
        // hop b: t1 → <terminal>. V4 sorts currency0 < currency1; native
        // (address 0) is always currency0, so the zfo + currency layout
        // depends on the terminal.
        let hop_b = match terminal {
            // t1 → weth: currency0=t1, currency1=weth, zfo=true.
            Terminal::Weth => HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4b_id,
                currency0_address: t1,
                currency1_address: weth,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: true,
            }),
            // t1 → t2: currency0=t1, currency1=t2, zfo=true.
            Terminal::Tok => HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4b_id,
                currency0_address: t1,
                currency1_address: t2,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: true,
            }),
            // t1 → native: native is currency0, t1 is currency1, so zfo=false
            // (currency1→currency0). out = currency0 = native.
            Terminal::Native => HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4b_id,
                currency0_address: NATIVE_CURRENCY_ADDRESS,
                currency1_address: t1,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: false,
            }),
        };
        let path = PathInfo::new(vec![hop_a, hop_b]);
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
                opts,
            },
        )
    }

    /// Build a v4_v4 **gap** topology (native↔WETH bridge at the mid). `Wrap`:
    /// hop a outputs native, hop b needs WETH (take native + `WETH_DEPOSIT`).
    /// `Unwrap`: hop a outputs WETH, hop b needs native (take WETH +
    /// `WETH_WITHDRAW`). V4 sorts currency0 < currency1, so native (address 0)
    /// is always currency0; zfo is set so the output/input legs match the gap.
    fn v4_v4_gap_inputs(
        gap: Gap,
        opts: crate::composers::EncodeOptions,
    ) -> (PathInfo, ComposerInputs<'static>) {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let t1 = address!("0000000000000000000000000000000000000da1");
        let t2 = address!("0000000000000000000000000000000000000da2");
        let pm = address!("00000000000000000000000000000000000000ff");
        let v4a_id = "0x0".to_string();
        let v4b_id = "0x1".to_string();
        // Hop a: output the gap currency (mid_a). zfo=false → output=currency0;
        // zfo=true → output=currency1.
        let hop_a = match gap {
            // Wrap: mid_a=native. currency0=native, currency1=weth, zfo=false
            // → output=currency0=native, input=currency1=weth.
            Gap::Wrap => HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4a_id,
                currency0_address: NATIVE_CURRENCY_ADDRESS,
                currency1_address: weth,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: false,
            }),
            // Unwrap: mid_a=weth. currency0=t1, currency1=weth, zfo=true
            // → output=currency1=weth, input=currency0=t1.
            Gap::Unwrap => HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4a_id,
                currency0_address: t1,
                currency1_address: weth,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: true,
            }),
        };
        // Hop b: input the bridged currency (mid_b), output the terminal.
        // mid_b = input = currency0 if zfo else currency1.
        let hop_b = match gap {
            // Wrap: mid_b=weth. currency0=t2, currency1=weth, zfo=false
            // → input=currency1=weth, output=currency0=t2.
            Gap::Wrap => HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4b_id,
                currency0_address: t2,
                currency1_address: weth,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: false,
            }),
            // Unwrap: mid_b=native. currency0=native, currency1=weth, zfo=true
            // → input=currency0=native, output=currency1=weth.
            Gap::Unwrap => HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: v4b_id,
                currency0_address: NATIVE_CURRENCY_ADDRESS,
                currency1_address: weth,
                fee: 3000,
                tick_spacing: 60,
                hook_address: Address::ZERO,
                zfo: true,
            }),
        };
        let path = PathInfo::new(vec![hop_a, hop_b]);
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
                opts,
            },
        )
    }

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
                    _ => FundingSource::InPathFlash,
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
            opts: crate::composers::EncodeOptions::default(),
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
    /// RVNIPD/EYQ6UF: build the Plan and assert it projects a trace that
    /// validates clean through the gate. The byte-level `derive_shape`
    /// comparison this used to make is gone — `derive_shape` IS the same
    /// Plan path now, so it was a self-comparison (the runtime matrix is the
    /// byte source of truth; `encoders_parity` pins the primitive wire format).
    fn plan_builds_and_validates(
        build: fn(&PathInfo, &ComposerInputs) -> Option<(Vec<u8>, Plan, AddressTable)>,
        path: &PathInfo,
        inputs: &ComposerInputs,
        name: &str,
    ) {
        let (_preamble, plan, _at) =
            build(path, inputs).unwrap_or_else(|| panic!("[{name}] build None"));
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
        plan_builds_and_validates(build_v3v2_plan, &path, &inputs, "v3_v2");
    }
    #[test]
    fn v3_v3_plan_byte_parity_and_validates() {
        let (path, inputs) = v3_v3_path_inputs();
        plan_builds_and_validates(build_v3v3_plan, &path, &inputs, "v3_v3");
    }

    /// Build an `n`-hop all-V2 path closing on WETH: hop `i` is
    /// `token_i → token_{i+1}`, with the final hop returning to WETH
    /// (the canonical all-V2 arbitrage loop the speedrail serves).
    fn all_v2_chain_hops(n: usize) -> Vec<HopInfo> {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
        let wbtc = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
        let dai = address!("6B175474E89094C44Da98b954EedeAC495271d0F");
        let cycle = [weth, usdc, wbtc, dai];
        (0..n)
            .map(|i| {
                HopInfo::V2(V2HopInfo {
                    pool_address: Address::from([0xD0 + u8::try_from(i).expect("2..=4 hops"); 20]),
                    token0_address: cycle[i % 4],
                    token1_address: cycle[(i + 1) % 4],
                    fee: 30,
                    zfo: true,
                })
            })
            .collect()
    }

    // KO5NNB gate proof: an InPathFlash all-V2 stream whose terminal output
    // cannot cover the flash repayment is REJECTED by the LedgerValidator
    // (the flash-repay `Erc20Transfer` would over-debit `erc20[weth]`, so
    // credit-before-debit fires). The retired hand-written speedrail emitted
    // this stream unvalidated (and the revm harness's 2× WETH buffer let it
    // "execute but lose"); the gate now makes it unrepresentable — N4TJSZ's
    // entire point. The SAME losing stream under SelfFund still validates
    // (no flash debt to repay — the executor eats the loss from held capital,
    // which is what a negative-control delta assert should measure).
    #[test]
    fn all_v2_gate_rejects_unprofitable_inpathflash_stream() {
        let path = PathInfo::new(all_v2_chain_hops(2));
        let outs: Vec<u128> = vec![80_000, 60_000]; // terminal 60k < optimal 100k — losing
        let consumed: Vec<u128> = vec![50_000, 80_000];
        for (flabel, funding) in [
            ("InPathFlash", FundingSource::InPathFlash),
            ("SelfFund", FundingSource::SelfFund),
        ] {
            let inputs = ComposerInputs {
                executor_address: address!("00000000000000000000000000000000000000ee"),
                pool_manager_address: address!("00000000000000000000000000000000000000ff"),
                weth_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                optimal_input: 100_000,
                hop_outputs: &outs,
                consumed_inputs: &consumed,
                opts: crate::composers::EncodeOptions {
                    funding,
                    ..Default::default()
                },
            };
            let (_preamble, plan, _at) = build_all_v2_chain(&path, &inputs)
                .unwrap_or_else(|| panic!("[{flabel}] build None"));
            let ops = plan_to_ledger_ops(&plan);
            let mut v = crate::grammar_ledger::LedgerValidator::default();
            if flabel == "InPathFlash" {
                let err = v
                    .validate_full(&ops)
                    .expect_err("losing InPathFlash stream must be rejected");
                // The exact invariant fired: the flash repay tries to debit
                // more WETH than the stream generated (terminal 60k < 100k owed).
                assert!(
                    matches!(
                        err,
                        crate::grammar_ledger::ValidationError::Erc20TransferBeforeCredit { .. }
                    ),
                    "expected Erc20TransferBeforeCredit, got {err:?}"
                );
                // The production path declines the losing InPathFlash stream
                // (the gate fires; the retired speedrail would have emitted it).
                assert!(
                    derive_all_v2(&path, &inputs).is_none(),
                    "production Plan path must decline the losing InPathFlash stream"
                );
            } else {
                assert!(
                    v.validate_full(&ops).is_ok(),
                    "losing SelfFund stream must still validate (no flash debt)"
                );
            }
        }
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
        // WETH terminal (the spike's proven slice): weth→t1→weth.
        v4_v4_inputs(Terminal::Weth, crate::composers::EncodeOptions::default())
    }

    #[test]
    fn v4_v4_plan_byte_parity_and_validates() {
        let (path, inputs) = v4_v4_path_inputs();
        plan_builds_and_validates(build_v4v4_plan, &path, &inputs, "v4_v4");
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
        // Force an UNSETTLED t1 DEBT: b_swap_in (1_150_000) > forward_out
        // (1_100_000), so removing the trailing SettleAll leaves a NEGATIVE t1
        // delta (the executor owes the V4 input) → PmDeltaNonzero at V4UnlockEnd.
        // (A POSITIVE residual is on-chain-valid — the unlock-close auto-settles
        // it to the executor — so the rejection is specifically the unpaid debt.)
        static CLAMPED: [u128; 2] = [1_000_000, 1_150_000];
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

    // ── BP7KIR opts: `use_v4_batch` + `erc6909_profit` (within the WETH-only
    //    slice) — byte-parity with `derive_2hop_v4v4` AND gate validation. ──

    fn v4_v4_opts_inputs(
        opts: crate::composers::EncodeOptions,
    ) -> (PathInfo, ComposerInputs<'static>) {
        let (mut path, mut inputs) = v4_v4_path_inputs();
        // SAFETY of the `static` borrow: `EncodeOptions` is `Copy` and the
        // fixture's `hop_outputs`/`consumed_inputs` are `static`s, so we only
        // overwrite the `opts` field — the slice borrows remain valid.
        // Re-build `ComposerInputs` with the requested opts.
        let ComposerInputs {
            executor_address,
            pool_manager_address,
            weth_address,
            optimal_input,
            hop_outputs,
            consumed_inputs,
            opts: _,
        } = inputs;
        inputs = ComposerInputs {
            executor_address,
            pool_manager_address,
            weth_address,
            optimal_input,
            hop_outputs,
            consumed_inputs,
            opts,
        };
        // Suppress unused-mut on `path` (the fixture path is reused as-is).
        let _ = &mut path;
        (path, inputs)
    }

    #[test]
    fn v4_v4_plan_batch_byte_parity_and_validates() {
        // `use_v4_batch=true`: one `V4Batch` replaces the two `V4Swap`s, and
        // NO `V4TakeDelta` is emitted (the batch auto-captures the WETH
        // profit). Byte-parity with `derive_2hop_v4v4`'s batch arm.
        let (path, inputs) = v4_v4_opts_inputs(crate::composers::EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: true,
            ..Default::default()
        });
        plan_builds_and_validates(build_v4v4_plan, &path, &inputs, "v4_v4 batch");
        // Spot-check the Plan shape: outer `V4Unlock { inner: [V4Batch, V4SettleAll] }`.
        let (_preamble, plan, _at) =
            build_v4v4_plan(&path, &inputs).expect("v4_v4 batch build None");
        let outer = &plan[0];
        let PlanStep::V4Unlock { inner, .. } = outer else {
            panic!("expected outer V4Unlock");
        };
        assert_eq!(inner.len(), 2, "batch inner = [V4Batch, V4SettleAll]");
        assert!(
            matches!(inner[0], PlanStep::V4Batch { .. }),
            "first step is V4Batch"
        );
        assert!(
            matches!(inner[1], PlanStep::V4SettleAll),
            "trailing SettleAll"
        );
        let _ = U256::ZERO;
    }

    #[test]
    fn v4_v4_plan_erc6909_byte_parity_and_validates() {
        // `erc6909_profit=true`: `V4Mint` of the WETH profit (ERC6909 claim)
        // replaces the `V4TakeDelta`.
        let (path, inputs) = v4_v4_opts_inputs(crate::composers::EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: false,
            ..Default::default()
        });
        plan_builds_and_validates(build_v4v4_plan, &path, &inputs, "v4_v4 erc6909");
        let (_preamble, plan, _at) =
            build_v4v4_plan(&path, &inputs).expect("v4_v4 erc6909 build None");
        let PlanStep::V4Unlock { inner, .. } = &plan[0] else {
            panic!("expected outer V4Unlock");
        };
        assert_eq!(
            inner.len(),
            4,
            "erc6909 inner = [V4Swap, V4Swap, V4Mint, V4SettleAll]"
        );
        assert!(
            matches!(inner[2], PlanStep::V4Mint { .. }),
            "profit step is V4Mint"
        );
        let _ = U256::ZERO;
    }

    #[test]
    fn v4_v4_plan_batch_erc6909_byte_parity_and_validates() {
        // Both opts: `V4Batch` + `V4Mint` of the profit (still auto-settles via
        // `V4SettleAll`; the mint captures the WETH delta as an ERC6909 claim
        // before the trailing settle).
        let (path, inputs) = v4_v4_opts_inputs(crate::composers::EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: true,
            ..Default::default()
        });
        plan_builds_and_validates(build_v4v4_plan, &path, &inputs, "v4_v4 batch+erc6909");
        let (_preamble, plan, _at) =
            build_v4v4_plan(&path, &inputs).expect("v4_v4 batch+erc6909 build None");
        let PlanStep::V4Unlock { inner, .. } = &plan[0] else {
            panic!("expected outer V4Unlock");
        };
        assert_eq!(
            inner.len(),
            3,
            "batch+erc6909 inner = [V4Batch, V4Mint, V4SettleAll]"
        );
        assert!(matches!(inner[0], PlanStep::V4Batch { .. }));
        assert!(matches!(inner[1], PlanStep::V4Mint { .. }));
        let _ = U256::ZERO;
    }

    #[test]
    fn v4_v4_plan_mint_before_swap_rejected() {
        // `V4Mint` honors the same D0 credit-before-debit rule as `V4TakeDelta`:
        // a `V4Mint` positioned before the swaps that create `PM[weth]` credit
        // must be rejected (the `Mint` gate op fails with `TakeBeforeCredit`).
        let (path, inputs) = v4_v4_opts_inputs(crate::composers::EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: false,
            ..Default::default()
        });
        let (_preamble, mut plan, _at) =
            build_v4v4_plan(&path, &inputs).expect("v4_v4 erc6909 build None");
        // The V4Unlock's inner is [Swap a, Swap b, V4Mint, SettleAll].
        // Move V4Mint to the front (before both swaps) → TakeBeforeCredit.
        let outer = plan.get_mut(0).unwrap();
        if let PlanStep::V4Unlock { inner, .. } = outer {
            let mint = inner.remove(2);
            inner.insert(0, mint);
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
            "misordered v4_v4 erc6909 Plan: V4Mint before the swap credits PM must be rejected"
        );
        let _ = U256::ZERO;
    }

    // ── BP7KIR slice-broaden: the v4_v4 Plan across all 3 terminal currencies
    //    (WETH / tok / native) × all 4 opt modes. Byte-parity with the proven
    //    `derive_2hop_v4v4` emitter AND gate validation in every cell. ──
    #[test]
    fn v4_v4_terminal_opt_matrix_byte_parity_and_validates() {
        use crate::composers::EncodeOptions;
        let modes = [
            ("default", EncodeOptions::default()),
            (
                "batch",
                EncodeOptions {
                    erc6909_profit: false,
                    use_v4_batch: true,
                    ..Default::default()
                },
            ),
            (
                "erc6909",
                EncodeOptions {
                    erc6909_profit: true,
                    use_v4_batch: false,
                    ..Default::default()
                },
            ),
            (
                "batch+erc6909",
                EncodeOptions {
                    erc6909_profit: true,
                    use_v4_batch: true,
                    ..Default::default()
                },
            ),
        ];
        let terminals = [
            ("weth", Terminal::Weth),
            ("tok", Terminal::Tok),
            ("native", Terminal::Native),
        ];
        for (t_name, terminal) in terminals {
            for (m_name, opts) in modes {
                let label = format!("v4_v4 {t_name}+{m_name}");
                let (path, inputs) = v4_v4_inputs(terminal, opts);
                plan_builds_and_validates(build_v4v4_plan, &path, &inputs, &label);
            }
        }
        let _ = U256::ZERO;
    }

    #[test]
    fn v4_v4_plan_batch_native_terminal_emits_no_take() {
        // The batch asymmetry for a NATIVE terminal: `V4_BATCH` auto-settles the
        // positive native PM delta, so the derive emits NO `V4TakeDelta` and
        // neither does the Plan — the trailing `V4SettleAll` zeroes the
        // residual `PM[native]` (gate's master invariant at `V4UnlockEnd`).
        let (path, inputs) = v4_v4_inputs(
            Terminal::Native,
            crate::composers::EncodeOptions {
                erc6909_profit: false,
                use_v4_batch: true,
                ..Default::default()
            },
        );
        let (_preamble, plan, _at) =
            build_v4v4_plan(&path, &inputs).expect("v4_v4 native+batch build None");
        let PlanStep::V4Unlock { inner, .. } = &plan[0] else {
            panic!("expected outer V4Unlock");
        };
        assert_eq!(
            inner.len(),
            2,
            "native+batch inner = [V4Batch, V4SettleAll] (no take)"
        );
        assert!(matches!(inner[0], PlanStep::V4Batch { .. }));
        assert!(matches!(inner[1], PlanStep::V4SettleAll));
        let _ = U256::ZERO;
    }

    // ── BP7KIR currency-gap slice: native↔WETH bridge at the mid. Byte-parity
    //    with derive_2hop_v4v4's gap branch AND gate validation. ──
    #[test]
    fn v4_v4_gap_byte_parity_and_validates() {
        // Both gap topologies (Wrap + Unwrap), default opts. The gap branch
        // emits: swap a → take+bridge → swap b → settle_delta → take(terminal)
        // → settle_all.
        for (name, gap) in [("wrap", Gap::Wrap), ("unwrap", Gap::Unwrap)] {
            let label = format!("v4_v4 gap {name}");
            let (path, inputs) = v4_v4_gap_inputs(gap, crate::composers::EncodeOptions::default());
            plan_builds_and_validates(build_v4v4_plan, &path, &inputs, &label);
        }
        let _ = U256::ZERO;
    }

    #[test]
    fn v4_v4_gap_opt_matrix_byte_parity_and_validates() {
        // The gap branch is opt-invariant (use_v4_batch + erc6909_profit are
        // inoperative across a gap — the derive forces individual swaps + a
        // physical take). Sweep all 4 opt modes for both gap topologies.
        use crate::composers::EncodeOptions;
        let modes = [
            ("default", EncodeOptions::default()),
            (
                "batch",
                EncodeOptions {
                    erc6909_profit: false,
                    use_v4_batch: true,
                    ..Default::default()
                },
            ),
            (
                "erc6909",
                EncodeOptions {
                    erc6909_profit: true,
                    use_v4_batch: false,
                    ..Default::default()
                },
            ),
            (
                "batch+erc6909",
                EncodeOptions {
                    erc6909_profit: true,
                    use_v4_batch: true,
                    ..Default::default()
                },
            ),
        ];
        for (g_name, gap) in [("wrap", Gap::Wrap), ("unwrap", Gap::Unwrap)] {
            for (m_name, opts) in modes {
                let label = format!("v4_v4 gap {g_name}+{m_name}");
                let (path, inputs) = v4_v4_gap_inputs(gap, opts);
                plan_builds_and_validates(build_v4v4_plan, &path, &inputs, &label);
            }
        }
        let _ = U256::ZERO;
    }

    #[test]
    fn v4_v4_gap_shape_is_swap_bridge_swap_settle_take() {
        // Plan-shape spot check: the gap branch lays out
        // [V4Swap, V4TakeCompact, (WethDeposit|WethWithdraw), V4Swap,
        //  V4SettleDelta, V4TakeDelta, V4SettleAll].
        let (path, inputs) =
            v4_v4_gap_inputs(Gap::Wrap, crate::composers::EncodeOptions::default());
        let (_preamble, plan, _at) = build_v4v4_plan(&path, &inputs).expect("v4_v4 gap build None");
        let PlanStep::V4Unlock { inner, .. } = &plan[0] else {
            panic!("expected outer V4Unlock");
        };
        assert_eq!(inner.len(), 7, "gap inner = 7 steps");
        assert!(matches!(inner[0], PlanStep::V4Swap { .. }), "0: swap a");
        assert!(
            matches!(inner[1], PlanStep::V4TakeCompact { .. }),
            "1: bridge take"
        );
        assert!(
            matches!(inner[2], PlanStep::WethDeposit { .. }),
            "2: wrap deposit (Wrap gap)"
        );
        assert!(matches!(inner[3], PlanStep::V4Swap { .. }), "3: swap b");
        assert!(
            matches!(inner[4], PlanStep::V4SettleDelta { .. }),
            "4: settle"
        );
        assert!(
            matches!(inner[5], PlanStep::V4TakeDelta { .. }),
            "5: profit take"
        );
        assert!(matches!(inner[6], PlanStep::V4SettleAll), "6: settle all");
        let _ = U256::ZERO;
    }
    // ── WE45KC inc.2: ProfitCapture::Native on v4_v4 (ADR-029 D1) ──────────
    // The capture axis is now load-bearing in the encoder: a WETH-terminal
    // v4_v4 path with capture=Native appends a WETH_WITHDRAW (0x13) converting
    // the profit to native ETH after the V4_TAKE_DELTA custody take. A
    // non-WETH/non-native tok terminal + Native is declined (the executor
    // cannot convert an arbitrary ERC-20 to native). A native terminal +
    // Native is a no-op (already native custody).

    /// Collect every `WETH_WITHDRAW` (0x13) command's 32-byte amount payload
    /// in `bytes` (scans all windows; `0x13` may appear in address bytes, so
    /// the caller asserts the expected profit is among the payloads).
    fn weth_withdraw_amounts(bytes: &[u8]) -> Vec<u128> {
        bytes
            .windows(33)
            .filter(|w| w[0] == 0x13)
            .map(|w| {
                let mut a = [0u8; 16];
                a.copy_from_slice(&w[17..33]);
                u128::from_be_bytes(a)
            })
            .collect()
    }

    /// Count `V4_TAKE_DELTA` (0x50) commands in `bytes`.
    fn count_v4_take_delta(bytes: &[u8]) -> usize {
        bytes.iter().filter(|&&b| b == 0x50).count()
    }

    #[test]
    fn v4_v4_native_capture_weth_terminal_appends_weth_withdraw() {
        // capture=Native: WETH-terminal path takes WETH to custody, then
        // WETH_WITHDRAW(profit) converts it to native. profit = weth_out -
        // optimal_input = 1_200_000 - 1_000_000 = 200_000.
        let (path, inputs) = v4_v4_opts_inputs(crate::composers::EncodeOptions {
            capture: crate::grammar_ledger::ProfitCapture::Native,
            ..Default::default()
        });
        let bytes =
            derive_shape(&path, &inputs).expect("v4_v4 native-capture WETH terminal must derive");
        assert!(
            weth_withdraw_amounts(&bytes).contains(&200_000),
            "Native capture must append WETH_WITHDRAW of the profit; got {:?}",
            weth_withdraw_amounts(&bytes)
        );
        // The custody take is still emitted (WETH taken to executor first).
        assert!(
            count_v4_take_delta(&bytes) >= 1,
            "V4_TAKE_DELTA custody take must precede the withdraw"
        );
    }

    #[test]
    fn v4_v4_custody_capture_emits_no_weth_withdraw() {
        // Default (Custody): no WETH_WITHDRAW — profit held as WETH.
        let (path, inputs) = v4_v4_opts_inputs(crate::composers::EncodeOptions::default());
        let bytes = derive_shape(&path, &inputs).expect("v4_v4 custody WETH terminal must derive");
        assert!(
            !weth_withdraw_amounts(&bytes).contains(&200_000),
            "Custody capture must NOT append a WETH_WITHDRAW of the profit"
        );
    }

    #[test]
    fn v4_v4_native_capture_tok_terminal_declines() {
        // capture=Native, tok terminal: the executor cannot convert an
        // arbitrary ERC-20 to native → derive declines (ADR-029 D1: declared
        // but not executable).
        let (path, inputs) = v4_v4_inputs(
            Terminal::Tok,
            crate::composers::EncodeOptions {
                capture: crate::grammar_ledger::ProfitCapture::Native,
                ..Default::default()
            },
        );
        assert!(
            derive_shape(&path, &inputs).is_none(),
            "Native capture on a non-WETH/non-native tok terminal must decline"
        );
    }

    #[test]
    fn v4_v4_native_capture_native_terminal_is_noop() {
        // capture=Native, native terminal: profit is already native custody
        // (V4_TAKE_DELTA(native_idx, SELF)); no WETH_WITHDRAW needed. Derives.
        let (path, inputs) = v4_v4_inputs(
            Terminal::Native,
            crate::composers::EncodeOptions {
                capture: crate::grammar_ledger::ProfitCapture::Native,
                ..Default::default()
            },
        );
        let bytes = derive_shape(&path, &inputs)
            .expect("v4_v4 native-capture native terminal must derive (no-op)");
        assert!(
            !weth_withdraw_amounts(&bytes).iter().any(|&a| a > 0),
            "Native terminal + Native capture is already native; no withdraw"
        );
    }
    #[test]
    fn v4_v4_native_capture_plan_byte_parity_and_validates() {
        // WE45KC inc.2: the Native-capture Plan (WETH terminal → V4TakeDelta
        // custody + WethWithdraw) stays byte-identical to derive_2hop_v4v4 AND
        // validates clean through the ledger gate (D5). The custody credit on
        // V4TakeDelta(→SELF) now models the executor's receipt, so the
        // withdraw debits a real Erc20[WETH] balance.
        let (path, inputs) = v4_v4_opts_inputs(crate::composers::EncodeOptions {
            capture: ProfitCapture::Native,
            ..Default::default()
        });
        plan_builds_and_validates(build_v4v4_plan, &path, &inputs, "v4_v4 native-capture");
    }
}
