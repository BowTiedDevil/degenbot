//! Executor grammar — full axis model (GCC6I6) + a **ledger-validator** that
//! makes the two real bug classes unrepresentable (ADR-029 D1/D2, D5).
//!
//! Two halves, per ADR-029 D4 (hybrid):
//! * **axis types** — [`Prot`], [`FundingSource`], [`ProfitCapture`],
//!   [`Bribe`], [`ShapeClass`] — the user-visible + derived axes that key a
//!   family (D1). Funding source is a **runtime, per-path** choice
//!   (strategy/operator, economic knob); capture, bribe, ledger and hop
//!   coupling are the open sets the derivation reasons over.
//! * **ledger-validator** — a [`LedgerOp`] IR + a stateful walker
//!   ([`LedgerValidator`]) that simulates credit/debit per [`Ledger`] and
//!   rejects any stream that violates **credit-before-debit**. It encodes the
//!   two invariants from DS4OQD:
//!   - `D0` — a `V4_TAKE*`/`V4_MINT*` may not debit `PM[currency]` unless a
//!     prior swap left `PM[currency] ≥ amount` (the `v2_v2_v4`/`v2_v4_v4`
//!     bug);
//!   - terminal-V2 — a `V2_SWAP_CALC` may not consume `H[pool,input]` unless
//!     the pair was credited first (the `2PT5HH` / `path-182449` über-draw).

use std::collections::HashMap;

use alloy::primitives::Address;

// ═══════════════════════════════════════════════════════════════════════
// Axis types (ADR-029 D1)
// ═══════════════════════════════════════════════════════════════════════

/// A hop-protocol family member.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prot {
    V2,
    V3,
    V4,
}

/// The declared origin of a command stream's **entry (seed)** capital
/// (ADR-029 D1). **One per stream, chosen at runtime by the strategy/operator**
/// — an economic knob (self-fund = cheaper gas for small opportunities; flash
/// = access to outside capital for large ones).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FundingSource {
    /// Executor holds the entry WETH and pre-funds the leading hop.
    SelfFund,
    /// The outermost pool's own swap-callback extends the entry credit, repaid
    /// by the path itself (in-path flash source).
    InPathFlash,
    /// PoolManager delta accounting carries the entry credit (no-prefund V4).
    PmLedger,
    /// An external lender flash (Aave-shape; modeled, executable only after
    /// the external-ledger work — VIXQYH stubs it).
    ExternalLender,
    /// Burn a held ERC-6909 claim to fund settlement.
    Erc6909BurnToSettle,
}

/// The declared destination of the stream's **terminal profit** (the excess
/// over the entry capital). **One per stream.** Modeled values are declared
/// even where the current executor cannot yet express them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProfitCapture {
    /// Executor holds the terminal asset in its own balance.
    Custody,
    /// Sent to `OWNER_ADDR`.
    Owner,
    /// Held as native ETH.
    Native,
    /// Minted as an ERC-6909 claim (needs `check_mode=2`).
    Erc6909,
    /// (Balancer) captured into the external Vault ledger — modeled, not yet
    /// executable by the current executor.
    BalancerVault,
}

/// Whether and how the stream pays a **builder bribe** (ADR-029 Q3 — a **live**
/// axis distinct from profit capture). `None` = no bribe (the default today).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bribe {
    None,
    /// `bips` of profit (1–10000) to `recipient_idx` (0 = `block.coinbase`).
    Some {
        bips: u16,
        recipient_idx: u8,
    },
}

/// A family's shape: hop-protocol sequence + the three output-bearing axes.
#[derive(Clone, Debug)]
pub struct ShapeClass {
    /// Ordered hop protocols.
    pub protocols: Vec<Prot>,
    /// Which seed capital supplies the stream (runtime, per-path).
    pub funding: FundingSource,
    /// Where the terminal profit goes.
    pub capture: ProfitCapture,
    /// Whether/how a builder bribe is paid.
    pub bribe: Bribe,
}

// ═══════════════════════════════════════════════════════════════════════
// Ledgers (ADR-029 D2 — an open set, never a closed enum)
// ═══════════════════════════════════════════════════════════════════════

/// An accounting location a command reads/writes. The five current instances
/// plus the open extension point for external Vault/lender ledgers (a config
/// change, not a new grammar shape).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Ledger {
    /// Executor's ERC-20 balance of `token` (incl. WETH).
    Erc20(Address),
    /// Executor's native ETH balance.
    Native,
    /// PoolManager delta for `currency` (positive = PM owes executor).
    Pm(Address),
    /// Executor's ERC-6909 held claim for `currency`.
    Erc6909(Address),
    /// V2 pair-handoff: tokens deposited into `pool` but not yet in reserves.
    PairHandoff(Address),
    /// (Extension) an external Balancer-shaped Vault / Aave-shaped lender.
    External(&'static str),
}

// ═══════════════════════════════════════════════════════════════════════
// LedgerOp IR + validator
// ═══════════════════════════════════════════════════════════════════════

/// A single command expressed as a ledger operation — the IR the validator
/// reasons over, decoupled from byte layout (so it validates the *decisions*,
/// not a specific encoding; the wire form is an encoder concern, ADR-029 D5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LedgerOp {
    /// A `V4_SWAP_*`: creates `PM[out]` credit AND `PM[in]` debt (both legs
    /// modeled so the net-zero-at-unlock-close invariant is checkable). The
    /// concrete output currency is what the downstream take/mint consumes.
    V4Swap {
        in_currency: Address,
        in_amount: u128,
        out_currency: Address,
        out_amount: u128,
    },
    /// `V4_TAKE(cur→rcp, amount)` — debits `PM[cur]` credit (D0).
    Take { currency: Address, amount: u128 },
    /// `V4_MINT(cur, amount)` — converts `PM[cur]` credit to `F[cur]` (D0).
    Mint { currency: Address, amount: u128 },
    /// `V4_SETTLE(cur, amount)` — the executor pays `amount` of `cur` into the
    /// PM, cancelling debt: `PM[cur] += amount` (nets a negative delta toward
    /// zero). The net-zero-at-unlock-close invariant is the V4 master rule
    /// (`executor-v4-ledger-rules.md`).
    V4Settle { currency: Address, amount: u128 },
    /// `V4_SETTLE_DELTA(cur)` — auto-settle one currency: nets `PM[cur]` to 0
    /// (if negative, executor pays; if positive, take to executor).
    V4SettleDelta { currency: Address },
    /// `V4_SETTLE_ALL` — auto-settle every touched PM currency to 0.
    V4SettleAll,
    /// `V4_TAKE_DELTA(cur→rcp)` — take the ENTIRE positive `PM[cur]` delta to
    /// `rcp` (the profit capture). Debits whatever credit `PM[cur]` holds
    /// (amount is runtime state — the current balance). Requires `PM[cur] > 0`
    /// immediately before (the D0 credit-before-debit rule on the PM ledger).
    V4TakeDelta {
        currency: Address,
        recipient_idx: u8,
    },
    /// `V4_UNLOCK` callback end — the master V4 invariant: every touched
    /// `PM[currency]` must net to zero by callback end
    /// (`executor-v4-ledger-rules.md`). The validator rejects if any PM delta
    /// is nonzero. Emitted by the Plan's `V4Unlock` node after its inner Plan.
    V4UnlockEnd,
    /// Seed a V2 pair's excess (credit `H[pool]`) — a transfer/take *to the
    /// pair* that a later `SwapCalc` consumes.
    SeedPair { pool: Address, amount: u128 },
    /// `V2_SWAP_CALC(pool)` — consumes `H[pool]` credit (terminal-V2 rule) AND
    /// credits `out_currency` to the executor (the swap's computed output, the
    /// profit / downstream repayment source). Option (B): swaps credit their
    /// output so the executor ledger fully accounts (ADR-029 D5).
    SwapCalc {
        pool: Address,
        amount_in: u128,
        out_currency: Address,
        out_amount: u128,
    },
    // ── POC (6SRC23): V2/V3 flash-credit chain for `v2_v3` (ADR-029 D4/D5). ──
    /// A `V2_SWAP_COMPACT` flash: the pool extends `out_currency` credit to the
    /// executor (the swap output, before repayment), and incurs an `in_currency`
    /// flash debt repayable within the callback. Extends the executor `Erc20`
    /// ledger (the same credit-before-debit rule as `PM`, on a different ledger).
    V2Flash {
        out_currency: Address,
        out_amount: u128,
        in_currency: Address,
        in_amount: u128,
    },
    /// A `V3_SWAP_COMPACT` flash: same shape as [`V2Flash`] — the V3 pool credits
    /// `out_currency` and is owed `in_currency` within the callback.
    V3Flash {
        out_currency: Address,
        out_amount: u128,
        in_currency: Address,
        in_amount: u128,
    },
    /// An `ERC20_TRANSFER(cur→rcp, amount)` debiting the executor's `Erc20[cur]`
    /// balance. When `repays_flash` is `Some(pool)`, the transfer is a flash
    /// repayment: it debits `min(amount, flash_debt[cur])` (saturating against
    /// the owed debt) so the auto-pay-at-callback-end case (empty-callback V2/V3
    /// flash) zeroes the debt without over-debiting. Requires the executor held
    /// credit ≥ the debited amount immediately before (credit-before-debit).
    Erc20Transfer {
        currency: Address,
        amount: u128,
        repays_flash: Option<Address>,
    },
    /// A self-fund seed — the executor **holds** `amount` of `currency` as entry
    /// capital before the stream starts (ADR-029 FundingSource::SelfFund). Credits
    /// `Erc20[currency]` (the same ledger flashes extend, just sourced from the
    /// executor's own balance rather than a flash). Not a command; a stream
    /// precondition modeled so the SelfFund families' repayments validate.
    SelfFund { currency: Address, amount: u128 },
    /// The executor-side credit half of a **cross-ledger move** — a V4
    /// `V4_TAKE_COMPACT(cur→SELF)` physically transfers `cur` from the PM to the
    /// executor's balance, so alongside the PM debit (`Take`) the executor's
    /// `Erc20[cur]` is credited by `amount`. Without this, a downstream V2/V3
    /// flash that consumes `cur` (e.g. the V3 auto-repay in `v4_v3`) would see
    /// `Erc20[cur] == 0` and be rejected — the cross-ledger analogue of D0
    /// (the boundary take must precede the outside-ledger consume).
    Erc20Credit { currency: Address, amount: u128 },
    /// The executor→PM native pay-in leg of a **native settle** (BP7KIR 3c).
    /// On-chain, native flows to the PM as `msg.value` on the `V4_SETTLE`
    /// call (no separate transfer instruction); this op models the executor's
    /// native balance debit explicitly (the settle credits PM via
    /// `V4SettleDelta`/`V4Settle`). Keeping the debit separate from the settle
    /// credit means a missing settle half is caught by PM-net-zero rather than
    /// silently absorbed — the gate's core value. Requires `Native ≥ amount`
    /// immediately before (credit-before-debit; a `WethWithdraw` or native V4
    /// take must have produced the native first).
    NativeTransfer { amount: u128 },
    /// The native credit half of a `V4TakeCompact(native→SELF)` — the native
    /// output physically arrives at the executor's native balance (the mirror
    /// of `Erc20Credit` for the native ledger). Pure credit; a later
    /// `NativeTransfer` or `WethDeposit` consumes it.
    NativeCredit { amount: u128 },
    /// `WETH_WITHDRAW(amount)` — unwrap WETH to native: debits `Erc20[WETH]`
    /// and credits `Native` (the source of the native that a `NativeTransfer`
    /// then pays into the PM). Carries `weth` so the validator debits the right
    /// Erc20 entry.
    WethWithdraw { weth: Address, amount: u128 },
    /// `WETH_DEPOSIT(amount)` — wrap native to WETH: debits `Native` and
    /// credits `Erc20[WETH]` (the native came from a V4 `V4TakeCompact(native→
    /// SELF)`).
    WethDeposit { weth: Address, amount: u128 },
}

/// A `V4_SWAP` term op returned by the proto-trace builder — kept separate from
/// [`LedgerOp`] so the validator can synthesize the credit relations without
/// trusting a hand-written credit ledger.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LedgerEffect {
    /// Create `PM[cur]` credit of `amount`.
    PmCredit(Address, u128),
    /// Create `H[pool]` credit of `amount` (pair seeded).
    PairCredit(Address, u128),
}

impl LedgerOp {
    /// A single processable op. The validator special-cases term helpers
    /// ([`LedgerEffect`]) directly; this is the effect of non-term ops.
    ///
    /// `V2Flash`/`V3Flash`/`Erc20Transfer` are handled directly in [`LedgerValidator::push`]` ([`LedgerEffect`] covers only the PM/pair ledgers).
    fn effect(&self) -> Option<LedgerEffect> {
        match *self {
            LedgerOp::SeedPair { pool, amount } => Some(LedgerEffect::PairCredit(pool, amount)),
            LedgerOp::V4Swap { .. }
            | LedgerOp::Take { .. }
            | LedgerOp::Mint { .. }
            | LedgerOp::V4Settle { .. }
            | LedgerOp::V4SettleDelta { .. }
            | LedgerOp::V4SettleAll
            | LedgerOp::V4TakeDelta { .. }
            | LedgerOp::V4UnlockEnd
            | LedgerOp::SwapCalc { .. }
            | LedgerOp::V2Flash { .. }
            | LedgerOp::V3Flash { .. }
            | LedgerOp::Erc20Transfer { .. }
            | LedgerOp::SelfFund { .. }
            | LedgerOp::Erc20Credit { .. }
            | LedgerOp::NativeTransfer { .. }
            | LedgerOp::NativeCredit { .. }
            | LedgerOp::WethWithdraw { .. }
            | LedgerOp::WethDeposit { .. } => None,
        }
    }
}

/// Rejects a command stream if it violates **credit-before-debit** within any
/// ledger (the DS4OQD invariants: D0 take/mint-before-credit; terminal-V2
/// über-draw). Fail-fast on the first violation.
///
/// `take`/`mint` require `PM[currency] ≥ amount` **immediately before**; a
/// `SwapCalc` requires the pair to have been seeded (`H[pool] ≥ 0`) first.
///
/// POC (`6SRC23`): the executor's own `Erc20[currency]` balance is the same
/// credit-before-debit ledger for V2/V3 flash swaps — a flash repayment
/// (`Erc20Transfer`) is only legal after a prior flash extended the credit.
/// Flash debts must be fully repaid by `finish()` (the V2/V3 analogue of the
/// V4 "every delta nets to zero by callback end" invariant).
#[derive(Debug, Default)]
pub struct LedgerValidator {
    /// `PM[currency]` balance (positive = PM owes executor).
    pm: HashMap<Address, i128>,
    /// `H[pool]` — seeded-but-unswapped pair excess per pool.
    pair: HashMap<Address, u128>,
    /// `Erc20[currency]` — the executor's own balance per currency (extended
    /// by flash swaps, consumed by `Erc20Transfer`).
    erc20: HashMap<Address, i128>,
    /// `Native` — the executor's native ETH balance. Extended by V4 native
    /// takes (`V4TakeCompact(native→SELF)`) and `WethWithdraw`; consumed by
    /// `NativeTransfer` (the pay-into-PM leg of a native settle) and
    /// `WethDeposit` (wrap). Same credit-before-debit rule.
    native: i128,
    /// Outstanding flash debt per currency (owed by the executor, awaiting
    /// repayment within a callback). Checked zero at `finish()`.
    flash_debt: HashMap<Address, u128>,
}

/// Why a stream was rejected — the invariant that fired and the offending op.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValidationError {
    /// A `take`/`mint` fired before the PoolManager held credit (the `D0`
    /// bug class).
    TakeBeforeCredit {
        currency: Address,
        wanted: u128,
        have: i128,
    },
    /// A `V2_SWAP_CALC` fired before the pair was seeded (the terminal-V2 /
    /// `2PT5HH` über-draw class).
    SwapCalcBeforeCredit { pool: Address },
    /// An `ERC20_TRANSFER` debiting the executor fired before the executor held
    /// `currency` credit (the V2/V3 flash-repay-before-credit class; surfaced
    /// by the `6SRC23` POC — byte-parity cannot see this ordering defect).
    Erc20TransferBeforeCredit {
        currency: Address,
        wanted: u128,
        have: i128,
    },
    /// A flash debt was left unpaid at `finish()` — the V2/V3 analogue of the
    /// V4 "every delta nets to zero by callback end" invariant.
    FlashDebtUnpaid { currency: Address, amount: u128 },
    /// A `V4_UNLOCK` closed with a nonzero `PM[currency]` delta — the V4 master
    /// invariant violation (a touched currency was not settled to zero by
    /// callback end).
    PmDeltaNonzero { currency: Address, delta: i128 },
    /// A `NativeTransfer` (the executor→PM native pay-in leg of a native
    /// settle) debited the executor's native balance before it held credit (the
    /// native analogue of `Erc20TransferBeforeCredit`). Surfaced by the
    /// BP7KIR 3c native-gap work — a `WethWithdraw` (or native V4 take) must
    /// precede the native pay-in.
    NativeTransferBeforeCredit { wanted: u128, have: i128 },
}

impl LedgerValidator {
    /// The ledger balance effect of one op; non-term ops are no-ops here and
    /// are checked (enforced) in [`Self::push`] instead.
    fn apply(&mut self, e: LedgerEffect) {
        match e {
            LedgerEffect::PmCredit(cur, amt) => {
                *self.pm.entry(cur).or_default() += amt as i128;
            }
            LedgerEffect::PairCredit(pool, amt) => {
                *self.pair.entry(pool).or_default() += amt;
            }
        }
    }

    /// Push one ledger op, enforcing credit-before-debit. Returns `Err` on the
    /// first violation (the op is **not** applied to the state on error).
    pub fn push(&mut self, op: LedgerOp) -> Result<(), ValidationError> {
        // Term ops create credit; apply them first so later debits see it.
        if let Some(e) = op.effect() {
            self.apply(e);
            return Ok(());
        }
        match op {
            LedgerOp::SeedPair { .. } => Ok(()), // handled above
            // V4 swap: PM[in] −= in_amount (debt), PM[out] += out_amount
            // (credit). Both legs so the net-zero-at-unlock-close invariant is
            // checkable (option B full modeling on the PM ledger).
            LedgerOp::V4Swap {
                in_currency,
                in_amount,
                out_currency,
                out_amount,
            } => {
                *self.pm.entry(in_currency).or_default() -= in_amount as i128;
                *self.pm.entry(out_currency).or_default() += out_amount as i128;
                Ok(())
            }
            // V4 settle: executor pays `amount` of `currency` into the PM,
            // cancelling debt (PM[currency] += amount). Models the executor
            // holding the token; the net-zero check fires at `V4UnlockEnd`.
            LedgerOp::V4Settle { currency, amount } => {
                *self.pm.entry(currency).or_default() += amount as i128;
                Ok(())
            }
            // V4_SETTLE_DELTA: auto-net one currency's PM delta to 0.
            LedgerOp::V4SettleDelta { currency } => {
                self.pm.insert(currency, 0);
                Ok(())
            }
            // V4_SETTLE_ALL: auto-net every touched PM currency to 0.
            LedgerOp::V4SettleAll => {
                for v in self.pm.values_mut() {
                    *v = 0;
                }
                Ok(())
            }
            // V4_TAKE_DELTA: take the entire positive PM[currency] delta to rcp.
            // Requires PM[currency] > 0 immediately before (credit-before-debit).
            LedgerOp::V4TakeDelta { currency, .. } => {
                let have = *self.pm.get(&currency).unwrap_or(&0);
                if have <= 0 {
                    return Err(ValidationError::TakeBeforeCredit {
                        currency,
                        wanted: 1,
                        have,
                    });
                }
                self.pm.insert(currency, 0);
                Ok(())
            }
            // V4_UNLOCK callback end: the master invariant — every touched
            // PM currency must net to zero by callback end. (A prior
            // `V4SettleAll` would have zeroed them; this catches any stream
            // that forgot to settle.)
            LedgerOp::V4UnlockEnd => {
                for (currency, delta) in &self.pm {
                    if *delta != 0 {
                        return Err(ValidationError::PmDeltaNonzero {
                            currency: *currency,
                            delta: *delta,
                        });
                    }
                }
                Ok(())
            }
            LedgerOp::Take { currency, amount } => {
                let have = *self.pm.get(&currency).unwrap_or(&0);
                if have < amount as i128 {
                    return Err(ValidationError::TakeBeforeCredit {
                        currency,
                        wanted: amount,
                        have,
                    });
                }
                // Credit consumed by the take.
                self.pm.insert(currency, have - amount as i128);
                Ok(())
            }
            LedgerOp::Mint { currency, amount } => {
                let have = *self.pm.get(&currency).unwrap_or(&0);
                if have < amount as i128 {
                    return Err(ValidationError::TakeBeforeCredit {
                        currency,
                        wanted: amount,
                        have,
                    });
                }
                self.pm.insert(currency, have - amount as i128);
                Ok(())
            }
            LedgerOp::SwapCalc {
                pool,
                out_currency,
                out_amount,
                ..
            } => {
                let have = *self.pair.get(&pool).unwrap_or(&0);
                if have == 0 {
                    return Err(ValidationError::SwapCalcBeforeCredit { pool });
                }
                // The pool's seeded excess is consumed by the swap, and the
                // swap's computed output is credited to the executor (option B:
                // swaps credit their output so the executor ledger fully accounts).
                self.pair.insert(pool, have - 1);
                *self.erc20.entry(out_currency).or_default() += out_amount as i128;
                Ok(())
            }
            // POC (6SRC23): V2/V3 flash swaps — term ops extending executor
            // `Erc20` credit and incurring flash debt (repayable within the
            // callback). Same credit-before-debit rule as `V4Swap`→`PM`, on the
            // executor-ledger axis.
            LedgerOp::V2Flash {
                out_currency,
                out_amount,
                in_currency,
                in_amount,
            }
            | LedgerOp::V3Flash {
                out_currency,
                out_amount,
                in_currency,
                in_amount,
            } => {
                *self.erc20.entry(out_currency).or_default() += out_amount as i128;
                *self.flash_debt.entry(in_currency).or_default() += in_amount;
                Ok(())
            }
            // Self-fund seed OR cross-ledger credit (`V4_TAKE_COMPACT(cur→SELF)`):
            // both credit the executor's `Erc20` balance (SelfFund sources it
            // from the executor's own held entry capital; Erc20Credit from a V4
            // take that physically moved the token PM→executor). No debt, no
            // D0 check — both are pure credits a later debit consumes.
            LedgerOp::SelfFund { currency, amount }
            | LedgerOp::Erc20Credit { currency, amount } => {
                *self.erc20.entry(currency).or_default() += amount as i128;
                Ok(())
            }
            // Native pay-in (executor→PM, native settle leg): debit the
            // executor's native balance. Requires Native ≥ amount immediately
            // before — a `WethWithdraw` or native V4 take must have produced it.
            LedgerOp::NativeTransfer { amount } => {
                if self.native < amount as i128 {
                    return Err(ValidationError::NativeTransferBeforeCredit {
                        wanted: amount,
                        have: self.native,
                    });
                }
                self.native -= amount as i128;
                Ok(())
            }
            // Unwrap WETH → native: debit Erc20[WETH], credit Native.
            LedgerOp::WethWithdraw { weth, amount } => {
                let have = *self.erc20.get(&weth).unwrap_or(&0);
                if have < amount as i128 {
                    return Err(ValidationError::Erc20TransferBeforeCredit {
                        currency: weth,
                        wanted: amount,
                        have,
                    });
                }
                self.erc20.insert(weth, have - amount as i128);
                self.native += amount as i128;
                Ok(())
            }
            // Wrap native → WETH: debit Native, credit Erc20[WETH].
            LedgerOp::WethDeposit { weth, amount } => {
                if self.native < amount as i128 {
                    return Err(ValidationError::NativeTransferBeforeCredit {
                        wanted: amount,
                        have: self.native,
                    });
                }
                self.native -= amount as i128;
                *self.erc20.entry(weth).or_default() += amount as i128;
                Ok(())
            }
            // Native credit half of V4TakeCompact(native→SELF).
            LedgerOp::NativeCredit { amount } => {
                self.native += amount as i128;
                Ok(())
            }
            LedgerOp::Erc20Transfer {
                currency,
                amount,
                repays_flash,
            } => {
                // A flash repayment only debits what is actually still owed
                // (`min(amount, debt)`) so the auto-pay-at-callback-end case
                // (empty-callback V2/V3 flash, which fires a full `in_amount`
                // repayment) zeroes the debt without over-debiting when the
                // callback already repaid part. A plain transfer (seed/bridge)
                // debits the full amount.
                let debit = if repays_flash.is_some() {
                    let owed = *self.flash_debt.get(&currency).unwrap_or(&0);
                    amount.min(owed)
                } else {
                    amount
                };
                let have = *self.erc20.get(&currency).unwrap_or(&0);
                if have < debit as i128 {
                    return Err(ValidationError::Erc20TransferBeforeCredit {
                        currency,
                        wanted: debit,
                        have,
                    });
                }
                self.erc20.insert(currency, have - debit as i128);
                if repays_flash.is_some() {
                    let owed = self.flash_debt.entry(currency).or_default();
                    *owed = owed.saturating_sub(debit);
                }
                Ok(())
            }
        }
    }

    /// Finish the stream: every flash debt must have been repaid (the V2/V3
    /// analogue of the V4 "every delta nets to zero by callback end"
    /// invariant). Call after the last `push` (or after `validate`).
    pub fn finish(&mut self) -> Result<(), ValidationError> {
        for (currency, amount) in &self.flash_debt {
            if *amount > 0 {
                return Err(ValidationError::FlashDebtUnpaid {
                    currency: *currency,
                    amount: *amount,
                });
            }
        }
        Ok(())
    }

    /// Convenience: validate a whole stream. Stops at the first violation.
    /// Does **not** call [`Self::finish`] — call it separately to enforce the
    /// flash-debt-net-zero invariant, or use [`Self::validate_full`].
    pub fn validate(&mut self, ops: &[LedgerOp]) -> Result<(), ValidationError> {
        for op in ops {
            self.push(*op)?;
        }
        Ok(())
    }

    /// Validate a whole stream AND assert every flash debt was repaid at the
    /// end (the full D4/D5 gate for streams that include V2/V3 flashes).
    pub fn validate_full(&mut self, ops: &[LedgerOp]) -> Result<(), ValidationError> {
        self.validate(ops)?;
        self.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    fn weth() -> Address {
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
    }
    fn usdc() -> Address {
        address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48")
    }
    fn pool() -> Address {
        address!("00000000000000000000000000000000000000bb")
    }
    fn native() -> Address {
        Address::ZERO
    }

    /// D0 — take-before-credit is rejected (the pre-fix `v2_v2_v4` bug).
    #[test]
    fn take_before_credit_is_rejected() {
        let mut v = LedgerValidator::default();
        let err = v.push(LedgerOp::Take {
            currency: weth(),
            amount: 1_000_000,
        });
        assert!(matches!(err, Err(ValidationError::TakeBeforeCredit { .. })));
    }

    /// D0 — a take AFTER a swap that produced the credit is accepted.
    #[test]
    fn take_after_credit_is_accepted() {
        let mut v = LedgerValidator::default();
        v.push(LedgerOp::V4Swap {
            in_currency: usdc(),
            in_amount: 1_000_000,
            out_currency: weth(),
            out_amount: 2_000_000,
        })
        .unwrap();
        assert!(v
            .push(LedgerOp::Take {
                currency: weth(),
                amount: 1_000_000,
            })
            .is_ok());
    }

    /// D0 — the pre-fix `v2_v2_v4` stream (take-WETH before any V4 swap creates
    /// a positive WETH delta) is rejected end-to-end.
    #[test]
    fn prefixed_v2_v2_v4_take_before_swap_rejected() {
        let mut v = LedgerValidator::default();
        let stream = [
            // The bug: an early take of WETH with no prior PM[WETH] credit.
            LedgerOp::Take {
                currency: weth(),
                amount: 100_000,
            },
            LedgerOp::V4Swap {
                in_currency: usdc(),
                in_amount: 300_000,
                out_currency: weth(),
                out_amount: 300_000,
            },
        ];
        assert_eq!(
            v.validate(&stream),
            Err(ValidationError::TakeBeforeCredit {
                currency: weth(),
                wanted: 100_000,
                have: 0,
            })
        );
    }

    /// terminal-V2 — a `V2_SWAP_CALC` with an un-seeded pair is rejected
    /// (the `2PT5HH` / `path-182449` über-draw class).
    #[test]
    fn swap_calc_without_seeded_pair_rejected() {
        let mut v = LedgerValidator::default();
        assert_eq!(
            v.push(LedgerOp::SwapCalc {
                pool: pool(),
                amount_in: 10_000,
                out_currency: weth(),
                out_amount: 0,
            }),
            Err(ValidationError::SwapCalcBeforeCredit { pool: pool() })
        );
    }

    /// terminal-V2 — seed-then-`V2_SWAP_CALC` is the accepted ordering.
    #[test]
    fn seed_then_swap_calc_accepted() {
        let mut v = LedgerValidator::default();
        v.push(LedgerOp::SeedPair {
            pool: pool(),
            amount: 10_000,
        })
        .unwrap();
        assert!(v
            .push(LedgerOp::SwapCalc {
                pool: pool(),
                amount_in: 10_000,
                out_currency: weth(),
                out_amount: 0,
            })
            .is_ok());
    }

    /// A corrected `v2_v2_v4` ordering (V4 swap produces the WETH delta, THEN
    /// the take) validates clean.
    #[test]
    fn corrected_v4_swap_then_take_accepted() {
        let mut v = LedgerValidator::default();
        let stream = [
            LedgerOp::V4Swap {
                in_currency: weth(),
                in_amount: 200_000,
                out_currency: usdc(),
                out_amount: 200_000,
            },
            LedgerOp::V4Swap {
                in_currency: usdc(),
                in_amount: 300_000,
                out_currency: weth(),
                out_amount: 300_000,
            },
            LedgerOp::Take {
                currency: weth(),
                amount: 100_000,
            },
        ];
        assert!(v.validate(&stream).is_ok());
    }

    // ════════════════════════════════════════════════════════════════════
    // POC (6SRC23): the V2/V3 flash-credit chain for `v2_v3` (InPathFlash).
    // The executor starts at 0; a flash repayment (ERC20_TRANSFER from the
    // executor) is only legal AFTER the flash that extended that currency's
    // credit. Byte-parity cannot see this ordering defect; the gate can.
    // ════════════════════════════════════════════════════════════════════

    /// The canonical `v2_v3` (InPathFlash) trace in stream order — the same
    /// ordering `derive_2hop_v2v3_trace` (grammar_shape) will emit. The V2
    /// flash credits t1 (forward); the V3 flash credits WETH (terminal); each
    /// flash's repayment consumes the credit the OTHER flash extended, in
    /// order. Must validate clean.
    #[test]
    fn v2_v3_flash_chain_accepted() {
        let mut v = LedgerValidator::default();
        // 1. V2 flash: credit t1, owe WETH.
        v.push(LedgerOp::V2Flash {
            out_currency: usdc(),
            out_amount: 1_000_000,
            in_currency: weth(),
            in_amount: 900_000,
        })
        .unwrap();
        // 2. V3 flash: credit WETH, owe t1.
        v.push(LedgerOp::V3Flash {
            out_currency: weth(),
            out_amount: 1_200_000,
            in_currency: usdc(),
            in_amount: 1_000_000,
        })
        .unwrap();
        // 3. Repay the V3 flash (t1) — credit extended by op 1.
        v.push(LedgerOp::Erc20Transfer {
            currency: usdc(),
            amount: 1_000_000,
            repays_flash: Some(pool()),
        })
        .unwrap();
        // 4. Repay the V2 flash (WETH) — credit extended by op 2.
        v.push(LedgerOp::Erc20Transfer {
            currency: weth(),
            amount: 900_000,
            repays_flash: Some(pool()),
        })
        .unwrap();
        assert!(v.finish().is_ok(), "fully-repaid flash chain must validate");
    }

    /// Misordered `v2_v3`: the WETH flash repayment (op 4) is hoisted BEFORE
    /// the V3 flash (op 2) extends WETH credit. The executor's WETH balance is
    /// still 0 at that point → rejected. This is the structural defect the
    /// runtime matrix cannot see (the bytes would revert on-chain with an
    /// opaque transfer-revert; the gate names the invariant).
    #[test]
    fn v2_v3_flash_repay_before_credit_rejected() {
        let mut v = LedgerValidator::default();
        v.push(LedgerOp::V2Flash {
            out_currency: usdc(),
            out_amount: 1_000_000,
            in_currency: weth(),
            in_amount: 900_000,
        })
        .unwrap();
        // BUG: repay the V2 flash (WETH) BEFORE the V3 flash credits WETH.
        assert_eq!(
            v.push(LedgerOp::Erc20Transfer {
                currency: weth(),
                amount: 900_000,
                repays_flash: Some(pool()),
            }),
            Err(ValidationError::Erc20TransferBeforeCredit {
                currency: weth(),
                wanted: 900_000,
                have: 0,
            })
        );
    }

    /// An underpaid flash debt is rejected at `finish()` (the V2/V3 analogue of
    /// the V4 "every delta nets to zero by callback end" invariant).
    #[test]
    fn underpaid_flash_debt_rejected_at_finish() {
        let mut v = LedgerValidator::default();
        v.push(LedgerOp::V2Flash {
            out_currency: usdc(),
            out_amount: 1_000_000,
            in_currency: weth(),
            in_amount: 900_000,
        })
        .unwrap();
        v.push(LedgerOp::V3Flash {
            out_currency: weth(),
            out_amount: 1_200_000,
            in_currency: usdc(),
            in_amount: 1_000_000,
        })
        .unwrap();
        // Repay V3 fully, but "forget" to repay the V2 flash's WETH debt.
        v.push(LedgerOp::Erc20Transfer {
            currency: usdc(),
            amount: 1_000_000,
            repays_flash: Some(pool()),
        })
        .unwrap();
        assert!(matches!(
            v.finish(),
            Err(ValidationError::FlashDebtUnpaid {
                currency, amount
            }) if currency == weth() && amount == 900_000
        ));
    }

    // ═══════════════════════════════════════════════════════════════════
    // BP7KIR Increment 3b: the `v4_v3` cross-ledger boundary take. The V4
    // swap credits PM[t1]; `V4TakeCompact(t1→SELF)` debits PM[t1] AND credits
    // the executor `Erc20[t1]` (the token physically arrives); the V3 flash's
    // auto-repay then debits that `Erc20[t1]`. The gate enforces the boundary
    // ordering — the structural defect byte-parity cannot see.
    // ═══════════════════════════════════════════════════════════════════

    /// The canonical `v4_v3` ledger trace in stream order validates clean: the
    /// boundary take credits `Erc20[t1]` before the V3 auto-repay debits it, PM
    /// nets to zero (`V4UnlockEnd`), and the V3 flash debt is repaid.
    #[test]
    fn v4_v3_boundary_take_chain_accepted() {
        let mut v = LedgerValidator::default();
        // V4 swap a: PM[WETH] −= optimal_input, PM[t1] += forward_out.
        v.push(LedgerOp::V4Swap {
            in_currency: weth(),
            in_amount: 100_000,
            out_currency: usdc(),
            out_amount: 110_000,
        })
        .unwrap();
        // Boundary take (→SELF): PM[t1] −= forward_out; Erc20[t1] += forward_out.
        v.push(LedgerOp::Take {
            currency: usdc(),
            amount: 110_000,
        })
        .unwrap();
        v.push(LedgerOp::Erc20Credit {
            currency: usdc(),
            amount: 110_000,
        })
        .unwrap();
        // Terminal V3 flash: credits WETH (profit), owes t1 — auto-repaid from
        // the Erc20[t1] credit the boundary take created.
        v.push(LedgerOp::V3Flash {
            out_currency: weth(),
            out_amount: 120_000,
            in_currency: usdc(),
            in_amount: 110_000,
        })
        .unwrap();
        // Auto-repay (empty callback): debits min(110_000, 110_000) of t1.
        v.push(LedgerOp::Erc20Transfer {
            currency: usdc(),
            amount: 110_000,
            repays_flash: Some(pool()),
        })
        .unwrap();
        // Settle the V4 input debt + residual, then the net-zero assertion.
        v.push(LedgerOp::V4SettleDelta { currency: weth() })
            .unwrap();
        v.push(LedgerOp::V4SettleAll).unwrap();
        v.push(LedgerOp::V4UnlockEnd).unwrap();
        assert!(
            v.finish().is_ok(),
            "v4_v3 boundary-take chain must validate"
        );
    }

    /// The structural defect: the boundary `Erc20Credit` (the V4 take's
    /// recipient-side) is omitted, so the V3 auto-repay fires against
    /// `Erc20[t1] == 0` → rejected. This is the cross-ledger analogue of D0 —
    /// the outside-ledger consume must follow the V4 take that funds it. A
    /// byte-parity check cannot see this (the bytes would revert on-chain with
    /// an opaque transfer-revert); the gate names the invariant.
    #[test]
    fn v4_v3_boundary_take_omitted_rejected() {
        let mut v = LedgerValidator::default();
        v.push(LedgerOp::V4Swap {
            in_currency: weth(),
            in_amount: 100_000,
            out_currency: usdc(),
            out_amount: 110_000,
        })
        .unwrap();
        // BUG: the `V4TakeCompact`'s Erc20Credit half is missing — the take
        // debited PM[t1] but never credited the executor's Erc20[t1].
        v.push(LedgerOp::V3Flash {
            out_currency: weth(),
            out_amount: 120_000,
            in_currency: usdc(),
            in_amount: 110_000,
        })
        .unwrap();
        assert_eq!(
            v.push(LedgerOp::Erc20Transfer {
                currency: usdc(),
                amount: 110_000,
                repays_flash: Some(pool()),
            }),
            Err(ValidationError::Erc20TransferBeforeCredit {
                currency: usdc(),
                wanted: 110_000,
                have: 0,
            })
        );
    }

    // ══════════════════════════════════════════════════════════════════
    // BP7KIR Increment 3b: the `v4_v2` boundary-seed family. The V4 forward
    // output is taken DIRECTLY to the V2 pair (PM→pool via `SeedPair`),
    // consumed by a `V2SwapCalc` (2PT5HH across the PM boundary); the V4
    // WETH-input debt is settled by `Erc20Transfer(WETH→PM)` + `V4Settle`,
    // funded by the V2 swap's WETH output credit.
    // ══════════════════════════════════════════════════════════════════

    /// The canonical `v4_v2` ledger trace validates clean: the boundary take
    /// seeds the pair before the `V2SwapCalc` consumes it, and the V2 WETH
    /// output credit precedes the PM pay-in (`Erc20Transfer→PM`). PM nets to
    /// zero (`V4UnlockEnd`) and the profit remains in `Erc20[WETH]`.
    #[test]
    fn v4_v2_boundary_seed_chain_accepted() {
        let mut v = LedgerValidator::default();
        // V4 swap a: PM[WETH] −= optimal_input, PM[t1] += forward_out.
        v.push(LedgerOp::V4Swap {
            in_currency: weth(),
            in_amount: 100_000,
            out_currency: usdc(),
            out_amount: 110_000,
        })
        .unwrap();
        // Boundary take → V2 pair: PM[t1] −= forward_out; SeedPair(v2, forward_out).
        v.push(LedgerOp::Take {
            currency: usdc(),
            amount: 110_000,
        })
        .unwrap();
        v.push(LedgerOp::SeedPair {
            pool: pool(),
            amount: 110_000,
        })
        .unwrap();
        // Terminal V2 SwapCalc: consumes the seeded pair, credits Erc20[WETH].
        v.push(LedgerOp::SwapCalc {
            pool: pool(),
            amount_in: 0,
            out_currency: weth(),
            out_amount: 120_000,
        })
        .unwrap();
        // Boundary-seed: pay WETH into the PM from the V2 output, settle the
        // V4 input debt (V4Sync is delta-neutral — modeled here by the
        // Erc20Transfer debit + V4Settle credit pair).
        v.push(LedgerOp::Erc20Transfer {
            currency: weth(),
            amount: 100_000,
            repays_flash: None,
        })
        .unwrap();
        v.push(LedgerOp::V4Settle {
            currency: weth(),
            amount: 100_000,
        })
        .unwrap();
        v.push(LedgerOp::V4SettleAll).unwrap();
        v.push(LedgerOp::V4UnlockEnd).unwrap();
        assert!(
            v.finish().is_ok(),
            "v4_v2 boundary-seed chain must validate"
        );
    }

    /// The structural defect: the boundary take+seed is omitted, so the
    /// `V2SwapCalc` fires against `pair[v2] == 0` → rejected (the 2PT5HH
    // terminal-V2 rule across the PM boundary).
    #[test]
    fn v4_v2_pair_seed_omitted_rejected() {
        let mut v = LedgerValidator::default();
        v.push(LedgerOp::V4Swap {
            in_currency: weth(),
            in_amount: 100_000,
            out_currency: usdc(),
            out_amount: 110_000,
        })
        .unwrap();
        // BUG: the V4TakeCompact(→v2 pair, SeedPair) is missing — the pair is
        // never seeded.
        assert_eq!(
            v.push(LedgerOp::SwapCalc {
                pool: pool(),
                amount_in: 0,
                out_currency: weth(),
                out_amount: 120_000,
            }),
            Err(ValidationError::SwapCalcBeforeCredit { pool: pool() })
        );
    }

    /// The cross-ledger defect: the PM pay-in (`Erc20Transfer(WETH→PM)`) is
    /// hoisted before the `V2SwapCalc` that credits `Erc20[WETH]`. The
    /// executor's WETH balance is still 0 → rejected (the outside→PM settle
    /// must follow the V2 output that funds it).
    #[test]
    fn v4_v2_pm_payin_before_v2_output_rejected() {
        let mut v = LedgerValidator::default();
        v.push(LedgerOp::V4Swap {
            in_currency: weth(),
            in_amount: 100_000,
            out_currency: usdc(),
            out_amount: 110_000,
        })
        .unwrap();
        v.push(LedgerOp::Take {
            currency: usdc(),
            amount: 110_000,
        })
        .unwrap();
        v.push(LedgerOp::SeedPair {
            pool: pool(),
            amount: 110_000,
        })
        .unwrap();
        // BUG: pay WETH into the PM BEFORE the V2SwapCalc credits Erc20[WETH].
        assert_eq!(
            v.push(LedgerOp::Erc20Transfer {
                currency: weth(),
                amount: 100_000,
                repays_flash: None,
            }),
            Err(ValidationError::Erc20TransferBeforeCredit {
                currency: weth(),
                wanted: 100_000,
                have: 0,
            })
        );
    }

    // ══════════════════════════════════════════════════════════════════
    // BP7KIR Increment 3c: native settle. The V4 native-input debt (PM[native])
    // is settled by `WethWithdraw` (credit Native) + `NativeTransfer` (debit
    // Native → PM) + `V4SettleDelta(native)` (zero PM[native]). The
    // `NativeTransfer` is the executor-debit half, separate from the
    // `SettleDelta` PM-credit half — the gate's core value.
    // ══════════════════════════════════════════════════════════════════

    /// The native settle chain validates clean: the WethWithdraw credits
    /// Native before the NativeTransfer debits it, and PM[native] nets to zero.
    #[test]
    fn native_settle_chain_accepted() {
        let mut v = LedgerValidator::default();
        // V4 swap with native input: PM[native] −= debt, PM[t1] += forward_out.
        v.push(LedgerOp::V4Swap {
            in_currency: native(),
            in_amount: 100_000,
            out_currency: usdc(),
            out_amount: 110_000,
        })
        .unwrap();
        // (forward output handling elided — focus on the native settle.)
        // Seed the executor's WETH (the source of the unwrapped native).
        v.push(LedgerOp::Erc20Credit {
            currency: weth(),
            amount: 100_000,
        })
        .unwrap();
        // Unwrap WETH → native (credits Native).
        v.push(LedgerOp::WethWithdraw {
            weth: weth(),
            amount: 100_000,
        })
        .unwrap();
        // Native pay-in (debit Native) + settle (zero PM[native]).
        v.push(LedgerOp::NativeTransfer { amount: 100_000 })
            .unwrap();
        v.push(LedgerOp::V4SettleDelta { currency: native() })
            .unwrap();
        v.push(LedgerOp::V4SettleAll).unwrap();
        v.push(LedgerOp::V4UnlockEnd).unwrap();
        assert!(v.finish().is_ok(), "native settle chain must validate");
    }

    /// The structural defect: the `NativeTransfer` (native pay-in) fires BEFORE
    /// the `WethWithdraw` that produces the native — the executor's Native
    /// balance is 0 → rejected (the native analogue of D0). Byte-parity cannot
    /// see this ordering (the bytes would revert on-chain with an opaque
    /// native-settle revert); the gate names the invariant.
    #[test]
    fn native_transfer_before_credit_rejected() {
        let mut v = LedgerValidator::default();
        v.push(LedgerOp::V4Swap {
            in_currency: native(),
            in_amount: 100_000,
            out_currency: usdc(),
            out_amount: 110_000,
        })
        .unwrap();
        v.push(LedgerOp::Erc20Credit {
            currency: weth(),
            amount: 100_000,
        })
        .unwrap();
        // BUG: native pay-in BEFORE the WethWithdraw credits Native.
        assert_eq!(
            v.push(LedgerOp::NativeTransfer { amount: 100_000 }),
            Err(ValidationError::NativeTransferBeforeCredit {
                wanted: 100_000,
                have: 0,
            })
        );
    }
}
