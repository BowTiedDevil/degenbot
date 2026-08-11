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
    /// A `V4_SWAP_*`: creates `PM[out]` credit and `PM[in]` debt. The concrete
    /// output currency is what the downstream take/mint consumes.
    V4Swap { output: Address, amount_out: u128 },
    /// `V4_TAKE(cur→rcp, amount)` — debits `PM[cur]` credit (D0).
    Take { currency: Address, amount: u128 },
    /// `V4_MINT(cur, amount)` — converts `PM[cur]` credit to `F[cur]` (D0).
    Mint { currency: Address, amount: u128 },
    /// Seed a V2 pair's excess (credit `H[pool]`) — a transfer/take *to the
    /// pair* that a later `SwapCalc` consumes.
    SeedPair { pool: Address, amount: u128 },
    /// `V2_SWAP_CALC(pool)` — consumes `H[pool]` credit (terminal-V2 rule).
    SwapCalc { pool: Address, amount_in: u128 },
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
    fn effect(&self) -> Option<LedgerEffect> {
        match *self {
            LedgerOp::V4Swap { output, amount_out } => {
                Some(LedgerEffect::PmCredit(output, amount_out))
            }
            LedgerOp::SeedPair { pool, amount } => Some(LedgerEffect::PairCredit(pool, amount)),
            LedgerOp::Take { .. } | LedgerOp::Mint { .. } | LedgerOp::SwapCalc { .. } => None,
        }
    }
}

/// Rejects a command stream if it violates **credit-before-debit** within any
/// ledger (the DS4OQD invariants: D0 take/mint-before-credit; terminal-V2
/// über-draw). Fail-fast on the first violation.
///
/// `take`/`mint` require `PM[currency] ≥ amount` **immediately before**; a
/// `SwapCalc` requires the pair to have been seeded (`H[pool] ≥ 0`) first.
#[derive(Debug, Default)]
pub struct LedgerValidator {
    /// `PM[currency]` balance (positive = PM owes executor).
    pm: HashMap<Address, i128>,
    /// `H[pool]` — seeded-but-unswapped pair excess per pool.
    pair: HashMap<Address, u128>,
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
            LedgerOp::V4Swap { .. } | LedgerOp::SeedPair { .. } => Ok(()), // handled above
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
            LedgerOp::SwapCalc { pool, .. } => {
                let have = *self.pair.get(&pool).unwrap_or(&0);
                if have == 0 {
                    return Err(ValidationError::SwapCalcBeforeCredit { pool });
                }
                // The pool's seeded excess is consumed by the swap.
                self.pair.insert(pool, have - 1);
                Ok(())
            }
        }
    }

    /// Convenience: validate a whole stream. Stops at the first violation.
    pub fn validate(&mut self, ops: &[LedgerOp]) -> Result<(), ValidationError> {
        for op in ops {
            self.push(*op)?;
        }
        Ok(())
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
            output: weth(),
            amount_out: 2_000_000,
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
                output: weth(),
                amount_out: 300_000,
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
                output: usdc(),
                amount_out: 200_000,
            },
            LedgerOp::V4Swap {
                output: weth(),
                amount_out: 300_000,
            },
            LedgerOp::Take {
                currency: weth(),
                amount: 100_000,
            },
        ];
        assert!(v.validate(&stream).is_ok());
    }
}
