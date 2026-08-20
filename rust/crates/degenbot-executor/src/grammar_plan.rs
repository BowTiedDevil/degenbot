//! The Plan walker — the **deep, stable** half of the grammar (`grammar_shape.rs`
//! split, ERP6ES / candidate 2 of `architecture-review-1786663110.html`).
//!
//! A 2/3-hop family's stream is authored as an execution-ordered,
//! callback-nested [`Plan`] of [`PlanStep`]s by the builders in
//! [`crate::grammar_shape`]. This module is the **single representation** both
//! consumers derive from:
//! * the **encoder** — [`plan_to_bytes`] emits the command stream;
//! * the **validator** — [`plan_to_ledger_ops`] projects the execution trace,
//!   gated by [`crate::grammar_ledger::LedgerValidator`] (ADR-029 D5: the
//!   generic validator proving ordering from declarative facts).
//!
//! The `PlanStep` vocabulary + the two walkers are the **deep, stable**
//! interface (`Plan → LedgerOp`, `Plan → bytes`) the grammar exists to serve —
//! ~770 lines, churned rarely. The wide, churning surface — the `build_walk`
//! pipeline + the `derive_shape` dispatch — lives in
//! [`crate::grammar_shape`], so a family addition stops churning this file
//! and its tests.
//!
//! One representation, no drift, no reordering, no per-family trace
//! duplication. [`crate::grammar_shape::derive_shape`] dispatches every
//! well-formed family through `build_walk` + the `LedgerValidator` gate,
//! returning `None` on decline or gate rejection.
//!
//! ---
//! **Parity sources of truth:** the revm runtime matrix (`degenbot-simulation`
//! full_matrix, exact delta); the primitive wire-format layer
//! (`tests/encoders_parity.rs`); the native bridge byte-golden
//! (`tests/native_eth_3hop_bridge.rs`).
use alloy::primitives::{Address, U256};

use crate::composers::{V2HopInfo, V3HopInfo, NATIVE_CURRENCY_ADDRESS};
use crate::encoders::{self, AddressTable, SENTINEL_PM, SENTINEL_SELF};
use crate::grammar_ledger::{LedgerOp, SwapRecipient};

/// A hop-protocol family member.
pub use crate::grammar_ledger::Prot;

// The 2/3-hop axis types (FundingSource + ProfitCapture + Bribe + ShapeClass)
// live in grammar_ledger (ADR-029 D1, WE45KC unification): the open-set enum is
// the single source of truth, re-exported here for the builders + consumers.
pub use crate::grammar_ledger::{
    Axis, AxisSupport, Bribe, FundingSource, ProfitCapture, ShapeClass,
};

// The retained terminal-hop fixture (`emit_terminal_hop`) takes `ComposerInputs`
// and matches on `HopInfo`; scoped to test builds so the lib build has no
// unused-import warning.
#[cfg(test)]
use crate::composers::{ComposerInputs, HopInfo};

pub(crate) fn v2_forward(h: &V2HopInfo) -> Address {
    if h.zfo {
        h.token1_address
    } else {
        h.token0_address
    }
}
pub(crate) fn v3_forward(h: &V3HopInfo) -> Address {
    if h.zfo {
        h.token1_address
    } else {
        h.token0_address
    }
}
pub(crate) fn v3_input(h: &V3HopInfo) -> Address {
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
#[derive(Clone, Debug, PartialEq)]
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
    /// `V4_BATCH` (0x42) / `V4_BATCH_OPEN_WETH` (0x43) — a bundled PM extcall
    /// of up to 8 swaps (`encoders::enc_v4_batch` /
    /// `encoders::enc_v4_batch_open_weth`). Ledger-equivalent to the constituent
    /// `V4Swap`s: each entry applies the same `PM[in]` debt / `PM[out]`
    /// credit. **Asymmetry vs a plain `V4Swap` sequence:** the 0x42 contract
    /// auto-settles any positive native ETH and WETH delta at the batch's end
    /// (an implicit `V4_TAKE_DELTA(→SELF)` for those two currencies). For the
    /// WETH-only slice (the executor's proven path) the derive therefore omits
    /// the terminal `V4TakeDelta` when `use_v4_batch` is set — the batch already
    /// captured the WETH profit. The Plan mirrors this: the per-entry ledger
    /// deltas leave a positive `PM[weth]` that the trailing `V4SettleAll`
    /// zeroes (the gate's master invariant fires at `V4UnlockEnd` — the profit
    /// capture is modelled by the contract, not by a `Take` op here).
    ///
    /// `open_weth`: `false` = 0x42 (full tail-settle); `true` = 0x43 — the
    /// WETH tail-settle is SKIPPED, so the positive `PM[weth]` delta is left
    /// OPEN for a trailing `V4Mint` (ERC6909 capture, TGUZCT/SW42JA). The 0x43
    /// projection additionally emits the `LedgerOp::OpenWethPairing` gate op,
    /// which requires a WETH `Mint` before `V4UnlockEnd`.
    V4Batch {
        entries: Vec<V4BatchSwap>,
        open_weth: bool,
    },
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
                PlanStep::V4Batch { entries, open_weth } => {
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
                    if *open_weth {
                        // TGUZCT/SW42JA: the 0x43 variant leaves the terminal
                        // (WETH) delta open — arm the pairing gate for the
                        // trailing `V4_MINT_COMPACT`.
                        let weth = entries.last().map(|e| e.out_currency).unwrap_or_default();
                        ops.push(LedgerOp::OpenWethPairing { weth });
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
    #[expect(clippy::too_many_lines, clippy::expect_used)]
    fn walk(plan: &Plan, at: &AddressTable, out: &mut Vec<u8>) {
        // The plan is LedgerValidator-validated before encoding, so the encoder
        // range checks below are unreachable; the `.expect()`s are deliberate
        // documentation of that invariant (args are in range by construction).
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
                PlanStep::V4Batch { entries, open_weth } => {
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
                    let encoded = if *open_weth {
                        encoders::enc_v4_batch_open_weth(&batch)
                    } else {
                        encoders::enc_v4_batch(&batch)
                    }
                    .expect("V4 batch <= 8 entries + uint96 amounts");
                    out.extend_from_slice(&encoded);
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
#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used)]

    use super::*;
    use alloy::primitives::{address, Address};

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
}
