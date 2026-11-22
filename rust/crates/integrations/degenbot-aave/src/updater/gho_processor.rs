//! Aave V3 GHO debt processor — the GHO scaled-balance math with discount
//! handling.
//!
//! The GHO scaled-balance model of the deployed Aave V3 GHO token: the
//! per-event math (`process_mint_event` / `process_burn_event`), the discount
//! accrual (`accrue_debt_on_action` / `get_discounted_balance`), and the
//! per-revision strategy tables (`GHO_STRATEGIES` + `GHO_DISCOUNT_STRATEGIES`).
//! The parity-gate cross-check (`docs/architecture/parity-gate.md`) compares
//! Rust-written rows against the recorded fixture rows byte-for-byte, so the
//! branch logic + the rounding selection MUST match exactly.
//!
//! # The GHO-vs-standard-debt split
//!
//! [`crate::processors::ScaledTokenProcessor`] is the standard
//! aToken/vToken processor — it has NO discount surface. GHO debt (the Aave
//! native stablecoin) is a SEPARATE concern: borrowers with staked AAVE receive
//! a discount on their interest, modeled by the `previous_discount` /
//! `should_refresh_discount` / `accrue_debt_on_action` /
//! `get_discounted_balance` machinery. The GHO processor is the standard
//! processor's sibling that owns this — the GHO apply dispatch consumes it
//! via the `process_gho_debt_mint` / `process_gho_debt_burn` fns.
//!
//! # π (zero DB, zero RPC)
//!
//! This module is pure CPU math: the processor is stateless — constructed per
//! revision, holding no mutable state. The RPC `raw_call` for
//! users-not-in-DB discounts is the driver shell's concern — the caller passes
//! `previous_discount` in.
//! The `should_refresh_discount` flag is what the GHO-discount machinery
//! consumes (the flag the processor sets when the discount needs refreshing
//! after a balance-changing operation).
//!
//! # The revision-strategy dispatch
//!
//! GHO debt has DISTINCT strategies from [`crate::processors::debt_strategy`]
//! — V1/V2/V3: `HALF_UP` mint + `HALF_UP` burn (discount takes precedence);
//! V4: `HALF_UP` mint + `HALF_UP` burn (no discount); V5/V6: `CEIL` mint +
//! `FLOOR` burn (no discount). The discount strategy is also revision-bound:
//! V1: `supports_discount=true, has_discounted_balance_method=false`;
//! V2/V3: both `true`; V4+: both `false`.

use std::cmp::Ordering;

use crate::{percent_mul, ray_div, ray_mul, wad_mul, WadRayError};
use alloy::primitives::{I256, U256};

use crate::processors::{RayDivMode, RoundingStrategy, ScaledTokenEventData};

// ── the GHO strategies ───────────────────────────────────────────────────

/// The GHO discount strategy per revision: whether the revision supports a
/// discount, and whether it carries the discounted-balance method.
///
/// - V1: discount supported, no `getDiscountedBalance` method (the V1 contract
///   use the `_burn(user, (amountScaled + discountScaled).toUint128())` path).
/// - V2/V3: discount supported with `getDiscountedBalance`.
/// - V4+: discount deprecated (the V4 contract removed the discount mechanism).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscountStrategy {
    /// `true` for V1/V2/V3 (the discount mechanism is active).
    pub supports_discount: bool,
    /// `true` for V2/V3 (the `getDiscountedBalance` method exists). `false`
    /// for V1 (no method — the V1 path uses `amountScaled + discountScaled`).
    pub has_discounted_balance_method: bool,
}

/// The GHO rounding + discount strategy per revision. V1/V2/V3: `HALF_UP` for all
/// ops (discount takes precedence); V4: `HALF_UP` for all ops (no discount);
/// V5/V6: `CEIL` mint, `FLOOR` burn (no discount).
///
/// **Distinct from [`crate::processors::debt_strategy`]** — the V4 GHO uses
/// `HALF_UP` (not `CEIL`) for mint + `HALF_UP` (not `FLOOR`) for burn, because
/// the V4 GHO contract predates the V4 vToken rounding change.
#[must_use]
pub const fn gho_strategy(revision: u32) -> RoundingStrategy {
    match revision {
        // V5/V6: CEIL mint, FLOOR burn (no discount).
        5 | 6 => RoundingStrategy {
            mint: RayDivMode::Ceil,
            burn: RayDivMode::Floor,
        },
        // V1/V2/V3/V4 and unknown (default): HALF_UP for all ops.
        _ => RoundingStrategy {
            mint: RayDivMode::HalfUp,
            burn: RayDivMode::HalfUp,
        },
    }
}

/// The GHO discount strategy per revision. V1: discount + no method;
/// V2/V3: discount + method; V4+: no discount.
#[must_use]
pub const fn gho_discount_strategy(revision: u32) -> DiscountStrategy {
    match revision {
        // V1: discount supported, no getDiscountedBalance method.
        1 => DiscountStrategy {
            supports_discount: true,
            has_discounted_balance_method: false,
        },
        // V2/V3: discount supported with the method.
        2 | 3 => DiscountStrategy {
            supports_discount: true,
            has_discounted_balance_method: true,
        },
        // V4+: discount deprecated (V4/V5/V6 + unknown default to false).
        _ => DiscountStrategy {
            supports_discount: false,
            has_discounted_balance_method: false,
        },
    }
}

// ── the result types ─────────────────────────────────────────────────────

/// The GHO operation classification for a Mint event, keyed on the event's
/// `value` against its `balance_increase` — the fields the GHO vToken's `Mint`
/// emission carries (`value = amount + balanceIncrease` in `_mintScaled`,
/// `value = balanceIncrease - amount` in `_burnScaled`, and
/// `value = balanceIncrease` on the discount accrual edition, which burns the
/// discount's scaled share before emitting). Used by the parser's
/// flow-classification; the apply fn ignores it (consumes `balance_delta` +
/// `new_index`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhoUserOperation {
    /// GHO BORROW — `value > balance_increase`: the Mint carried a requested
    /// amount.
    Borrow,
    /// GHO REPAY — `value < balance_increase`: `_burnScaled`'s Mint emission
    /// when the accrued interest exceeds the repayment.
    Repay,
    /// GHO INTEREST ACCRUAL — `value == balance_increase`: no requested
    /// amount, only the index-driven accrual.
    InterestAccrual,
}

impl GhoUserOperation {
    /// `true` when the Mint is on the repay path; the result types carry an
    /// `is_repay` derived from this.
    #[must_use]
    pub const fn is_repay(self) -> bool {
        matches!(self, Self::Repay)
    }
}

/// The result of processing a GHO debt `Mint` event: `balance_delta`,
/// `new_index`, `user_operation`, `discount_scaled`,
/// `should_refresh_discount`.
///
/// `discount_scaled` is the discount's own scaled contribution to the delta —
/// named for what it is, because the parity-gate fixture pins it as its own
/// column separate from the net `balance_delta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GhoScaledTokenMintResult {
    /// The signed delta to add to the position's scaled balance. A borrow is
    /// positive unless the discount's scaled share exceeds the requested
    /// amount; a repay is negative; an interest accrual is the negated
    /// discount share, or zero on a revision without a discount.
    pub balance_delta: I256,
    /// The event's `index` (the apply fn reconciles the position's
    /// `last_index` to this via max-with-prev).
    pub new_index: U256,
    /// The flow classification (borrow / repay / interest accrual).
    pub user_operation: GhoUserOperation,
    /// The discount-scaled amount (the `accrue_debt_on_action` result — 0 for
    /// revisions without discount support).
    pub discount_scaled: U256,
    /// `true` when the discount needs refreshing after a balance-changing
    /// operation (always `supports_discount`).
    pub should_refresh_discount: bool,
}

impl GhoScaledTokenMintResult {
    /// `is_repay` — derived from `user_operation == GhoUserOperation::Repay`.
    #[must_use]
    pub const fn is_repay(&self) -> bool {
        self.user_operation.is_repay()
    }
}

/// The result of processing a GHO debt `Burn` event: `balance_delta`,
/// `new_index`, `discount_scaled`, `should_refresh_discount`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GhoScaledTokenBurnResult {
    /// The signed delta (always negative — a burn decrements the scaled
    /// balance).
    pub balance_delta: I256,
    /// The event's `index`.
    pub new_index: U256,
    /// The discount-scaled amount (0 for revisions without discount support).
    pub discount_scaled: U256,
    /// `true` when the discount needs refreshing (always `supports_discount`).
    pub should_refresh_discount: bool,
}

// ── the processor ─────────────────────────────────────────────────────────

/// Errors from the GHO processor.
#[derive(Debug, Clone, thiserror::Error)]
pub enum GhoProcessorError {
    /// A `WadRayMath` or `PercentageMath` overflow / division by zero.
    #[error("math error: {0}")]
    RayMath(#[from] WadRayError),
    /// A `PercentageMath` overflow / division by zero.
    #[error("percentage math error: {0}")]
    PercentMath(#[from] crate::PercentError),
    /// The computed `balance_delta` doesn't fit in `I256` (overflow).
    #[error("balance_delta overflow: {0}")]
    DeltaOverflow(String),
}

/// The GHO debt processor — the strategy-bound `process_gho_debt_mint` /
/// `process_gho_debt_burn` pair with the discount machinery. Stateless
/// (constructed per revision, holding no mutable state).
#[derive(Debug, Clone, Copy)]
pub struct UnifiedGhoProcessor {
    rounding: RoundingStrategy,
    discount: DiscountStrategy,
    revision: u32,
}

impl UnifiedGhoProcessor {
    /// Construct a processor for a GHO vToken at `revision` — the
    /// `gho_strategy` + `gho_discount_strategy` lookup.
    #[must_use]
    pub fn new(revision: u32) -> Self {
        Self {
            rounding: gho_strategy(revision),
            discount: gho_discount_strategy(revision),
            revision,
        }
    }

    /// The revision this processor was constructed for.
    #[must_use]
    pub const fn revision(&self) -> u32 {
        self.revision
    }

    /// `true` if this revision's discount mechanism is active (V1/V2/V3).
    #[must_use]
    pub const fn supports_discount(&self) -> bool {
        self.discount.supports_discount
    }

    /// Perform a `ray_div` with the strategy's rounding mode.
    #[expect(clippy::unused_self, clippy::trivially_copy_pass_by_ref)]
    fn ray_div(&self, a: U256, b: U256, mode: RayDivMode) -> Result<U256, GhoProcessorError> {
        Ok(ray_div(a, b, mode.into())?)
    }

    /// Simulate the `_accrueDebtOnAction` contract fn (stateless). Computes the
    /// `discount_scaled` amount (the discount portion burned by the contract
    /// on a balance-changing action). Returns `0` for revisions without
    /// discount support.
    ///
    /// # Arguments
    ///
    /// - `prev_scaled_balance` — the position's scaled balance before the
    ///   action.
    /// - `prev_index` — the index at which `prev_scaled_balance` was
    ///   calculated.
    /// - `discount_percent` — the discount percent in effect (basis points,
    ///   `10_000 == 100.00%`).
    /// - `current_index` — the event's index (the current debt index).
    #[expect(clippy::missing_errors_doc)]
    pub fn accrue_debt_on_action(
        &self,
        prev_scaled_balance: U256,
        prev_index: U256,
        discount_percent: U256,
        current_index: U256,
    ) -> Result<U256, GhoProcessorError> {
        if !self.discount.supports_discount {
            return Ok(U256::ZERO);
        }
        // balance_increase = ray_mul(prev, current) - ray_mul(prev, prev_index)
        // (half-up `ray_mul`).
        let curr = ray_mul(prev_scaled_balance, current_index, None)?;
        let prev = ray_mul(prev_scaled_balance, prev_index, None)?;
        // No underflow: `current_index >= prev_index` (monotonic), so `curr >= prev`.
        let balance_increase = curr.saturating_sub(prev);
        if balance_increase.is_zero() || discount_percent.is_zero() {
            return Ok(U256::ZERO);
        }
        let discount = percent_mul(balance_increase, discount_percent)?;
        // `ray_div(discount, current_index)` — half-up.
        let discount_scaled = ray_div(discount, current_index, None)?;
        Ok(discount_scaled)
    }

    /// Calculate the discounted balance for a burn operation. Returns the balance
    /// with the discount applied (the amount the contract considers "owed"
    /// before a burn). Returns `0` for a zero scaled balance; returns the
    /// raw `ray_mul(scaled_balance, current_index)` for revisions without
    /// discount support.
    #[expect(clippy::missing_errors_doc)]
    pub fn get_discounted_balance(
        &self,
        scaled_balance: U256,
        prev_index: U256,
        current_index: U256,
        discount_percent: U256,
    ) -> Result<U256, GhoProcessorError> {
        if scaled_balance.is_zero() {
            return Ok(U256::ZERO);
        }
        let mut balance = ray_mul(scaled_balance, current_index, None)?;
        if !self.discount.supports_discount {
            return Ok(balance);
        }
        if current_index == prev_index {
            return Ok(balance);
        }
        if !discount_percent.is_zero() {
            let prev = ray_mul(scaled_balance, prev_index, None)?;
            let balance_increase = balance.saturating_sub(prev);
            balance = balance.saturating_sub(percent_mul(balance_increase, discount_percent)?);
        }
        Ok(balance)
    }

    /// Process a GHO debt `Mint` event (the `Mint` emission the GHO vToken's
    /// `_mintScaled`/`_burnScaled` paths carry). The classification is the
    /// exhaustive `value.cmp(balance_increase)`: greater is a borrow, less a
    /// repay, equal a pure interest accrual. Each arm carries its own signed
    /// delta, so the sign falls out of the classification.
    ///
    /// # Arguments
    ///
    /// - `event_data` — the Mint event's decoded fields (`value`,
    ///   `balance_increase`, `index`, `scaled_amount`).
    /// - `prev_balance` — the position's scaled balance before this event.
    /// - `prev_index` — the index at which `prev_balance` was calculated.
    /// - `prev_discount` — the discount percent in effect at the event's log
    ///   index (the GHO-discount-lookup machinery resolves it; the
    ///   orchestrator pre-fetches + passes it in).
    /// - `actual_repay_amount` — optional actual repay amount from a paired
    ///   `Repay` event (avoids the 1-wei rounding error from deriving it
    ///   from Mint event fields). The orchestrator threads this through when
    ///   available.
    ///
    #[expect(clippy::missing_errors_doc)]
    pub fn process_gho_debt_mint(
        &self,
        event_data: &ScaledTokenEventData,
        prev_balance: U256,
        prev_index: U256,
        prev_discount: U256,
        actual_repay_amount: Option<U256>,
    ) -> Result<GhoScaledTokenMintResult, GhoProcessorError> {
        // Accrue debt with discount (stateless — doesn't mutate the position).
        let discount_scaled =
            self.accrue_debt_on_action(prev_balance, prev_index, prev_discount, event_data.index)?;

        let (balance_delta, user_operation) =
            match event_data.value.cmp(&event_data.balance_increase) {
                Ordering::Greater => {
                    // GHO BORROW: emitted in `_mintScaled`.
                    //
                    // For V5+ (no discount, CEIL mint rounding): prefer the
                    // pre-calculated `scaled_amount` from the enrichment layer
                    // (it's computed via TokenMath which matches the on-chain
                    // rayDiv exactly for V5+). Deriving from Mint event fields
                    // introduces 1-wei rounding errors because `balanceIncrease` is
                    // itself rounded from rayMul. For V4 / V1-V3: derive from event
                    // data using the processor's own rounding mode (the enrichment
                    // uses pool-level TokenMath which differs from V4 GHO's
                    // HALF_UP).
                    let raw_delta = if !self.discount.supports_discount
                        && self.rounding.mint == RayDivMode::Ceil
                        && event_data.scaled_amount.is_some()
                    {
                        // SAFETY: checked `is_some()` above.
                        event_data.scaled_amount.unwrap_or_default()
                    } else {
                        let requested =
                            event_data.value.saturating_sub(event_data.balance_increase);
                        self.ray_div(requested, event_data.index, self.rounding.mint)?
                    };
                    let delta = if self.discount.supports_discount {
                        // `_mintScaled` splits on the same comparison one level down:
                        // `if (amountScaled > discountScaled) { _mint(amountScaled -
                        // discountScaled) } else { _burn(discountScaled - amountScaled) }`.
                        // One signed subtraction covers both sides.
                        to_signed(raw_delta)? - to_signed(discount_scaled)?
                    } else {
                        to_signed(raw_delta)?
                    };
                    (delta, GhoUserOperation::Borrow)
                }
                Ordering::Less => {
                    // GHO REPAY: emitted in `_burnScaled` (the Mint event fires when
                    // interest > repayment amount). Use `actual_repay_amount` from a
                    // paired Repay event if provided to avoid the 1-wei rounding error.
                    let amount_repaid = actual_repay_amount.unwrap_or_else(|| {
                        event_data.balance_increase.saturating_sub(event_data.value)
                    });
                    let repayment_scaled =
                        self.ray_div(amount_repaid, event_data.index, self.rounding.burn)?;
                    let delta_abs = if self.discount.supports_discount
                        && self.discount.has_discounted_balance_method
                    {
                        // V2/V3: getDiscountedBalance. If `amount_repaid >=
                        // balance_before_burn` → full repayment → burn all scaled
                        // balance. Else partial → burn (repayment_scaled +
                        // discount_scaled). (See `process_gho_debt_burn`'s V2/V3
                        // branch for the `>=` rationale.)
                        let balance_before = self.get_discounted_balance(
                            prev_balance,
                            prev_index,
                            event_data.index,
                            prev_discount,
                        )?;
                        if amount_repaid >= balance_before {
                            // Full repayment: burn all scaled balance.
                            prev_balance
                        } else {
                            // Partial: burn (repayment_scaled + discount_scaled).
                            repayment_scaled.saturating_add(discount_scaled)
                        }
                    } else if self.discount.supports_discount {
                        // V1 discount: no getDiscountedBalance method →
                        // `_burn(user, (amountScaled + discountScaled).toUint128())`.
                        repayment_scaled.saturating_add(discount_scaled)
                    } else {
                        // No discount (V4+): just the repayment.
                        repayment_scaled
                    };
                    (to_signed_neg(delta_abs)?, GhoUserOperation::Repay)
                }
                Ordering::Equal => {
                    // Pure interest accrual: the Mint carried no requested amount.
                    // Debt interest lives in the index, so a revision without a
                    // discount moves no scaled balance at all. The discount
                    // revisions settle the accrual through `_accrueDebtOnAction`,
                    // burn the discount's scaled share and then emit
                    // `Mint(0x0, user, balanceIncrease, balanceIncrease, index)` —
                    // the only scaled movement is that burn.
                    let delta = if self.discount.supports_discount {
                        to_signed_neg(discount_scaled)?
                    } else {
                        I256::ZERO
                    };
                    (delta, GhoUserOperation::InterestAccrual)
                }
            };

        Ok(GhoScaledTokenMintResult {
            balance_delta,
            new_index: event_data.index,
            user_operation,
            discount_scaled,
            should_refresh_discount: self.discount.supports_discount,
        })
    }

    /// Process a GHO debt `Burn` event (the `_burnScaled` burn path). For
    /// all GHO revisions,
    /// always calculate from event data (the `scaled_amount` field is NOT
    /// used for GHO except for V5+ no-discount FLOOR burns, where the
    /// enrichment's pre-calculated value avoids 1-wei rounding errors).
    ///
    /// # Arguments
    ///
    /// - `event_data` — the Burn event's decoded fields.
    /// - `prev_balance` — the position's scaled balance before this event.
    /// - `prev_index` — the index at which `prev_balance` was calculated.
    /// - `prev_discount` — the discount percent in effect at the event's log
    ///   index.
    ///
    #[expect(clippy::missing_errors_doc)]
    pub fn process_gho_debt_burn(
        &self,
        event_data: &ScaledTokenEventData,
        prev_balance: U256,
        prev_index: U256,
        prev_discount: U256,
    ) -> Result<GhoScaledTokenBurnResult, GhoProcessorError> {
        // `request_amount = amount + balance_increase` (the contract burns the
        // replenishment amount + the accrued interest).
        let requested_amount = event_data.value.saturating_add(event_data.balance_increase);

        // Calculate `amount_scaled` — for V5+ (no discount, FLOOR burn
        // rounding), prefer the pre-calculated `scaled_amount` from the
        // enrichment layer (avoids the 1-wei rounding error). For V4 / V1-V3,
        // derive from event fields (the enrichment uses pool-level TokenMath
        // which differs from V4 GHO's HALF_UP).
        let amount_scaled = if !self.discount.supports_discount
            && self.rounding.burn == RayDivMode::Floor
            && event_data.scaled_amount.is_some()
        {
            // SAFETY: checked `is_some()` above.
            event_data.scaled_amount.unwrap_or_default()
        } else {
            // `amount_scaled = requested_amount.rayDiv(index)` with the burn strategy.
            self.ray_div(requested_amount, event_data.index, self.rounding.burn)?
        };

        // Accrue debt with discount (stateless).
        let discount_scaled =
            self.accrue_debt_on_action(prev_balance, prev_index, prev_discount, event_data.index)?;

        let balance_delta_abs =
            if self.discount.supports_discount && self.discount.has_discounted_balance_method {
                // V2/V3: getDiscountedBalance. If `requested_amount >=
                // balance_before_burn` → full repayment → burn all scaled balance.
                //
                // The full-repay test is `>=`, never strict `==`: the
                // contract's full-repay Burn carries `requested_amount` (= Pool
                // Repay event amount, Aave's capped `balanceBefore` on-chain),
                // which can diverge by a few wei from this HALF_UP
                // `ray_mul(prev_balance, idx)` estimate — the stored
                // `prev_balance` drifts by wei across earlier GHO burns that
                // took the partial arm. Strict `==` misses that case and
                // applies `-amount_scaled` (= `ray_div_floor(requested, idx)`
                // > `prev_balance`), driving the balance negative. Aave-V3
                // `_burnScaled` semantics: when the contract caps the
                // repay amount to the actual outstanding debt, the emitted
                // Repay.amount (= requested_amount) is ALWAYS >= Rust's
                // approximation of `balanceBefore`; using `>=` mirrors that.
                let balance_before = self.get_discounted_balance(
                    prev_balance,
                    prev_index,
                    event_data.index,
                    prev_discount,
                )?;
                if requested_amount >= balance_before {
                    // Full repayment: burn all scaled balance.
                    prev_balance
                } else {
                    // Partial: burn (amount_scaled + discount_scaled).
                    amount_scaled.saturating_add(discount_scaled)
                }
            } else if self.discount.supports_discount {
                // V1: no getDiscountedBalance method →
                // `_burn(user, (amountScaled + discountScaled).toUint128())`.
                amount_scaled.saturating_add(discount_scaled)
            } else {
                // No discount (V4+): check for full repayment.
                let balance_before = ray_mul(prev_balance, event_data.index, None)?;
                if requested_amount >= balance_before {
                    // Full repayment: burn all scaled balance.
                    // (See the V2/V3 branch for the `>=` rationale.)
                    prev_balance
                } else {
                    amount_scaled
                }
            };

        let balance_delta = to_signed_neg(balance_delta_abs)?;

        Ok(GhoScaledTokenBurnResult {
            balance_delta,
            new_index: event_data.index,
            discount_scaled,
            should_refresh_discount: self.discount.supports_discount,
        })
    }
}

/// Wrap a `U256` into an `I256` (positive). Returns
/// [`GhoProcessorError::DeltaOverflow`] if the value exceeds `I256::MAX`.
fn to_signed(v: U256) -> Result<I256, GhoProcessorError> {
    I256::try_from(v)
        .map_err(|_| GhoProcessorError::DeltaOverflow(format!("value {v} > I256::MAX")))
}

/// Wrap a `U256` into a NEGATIVE `I256` (`-v`). Returns
/// [`GhoProcessorError::DeltaOverflow`] if the value exceeds `I256::MAX + 1`
/// (the absolute value of `I256::MIN`).
fn to_signed_neg(v: U256) -> Result<I256, GhoProcessorError> {
    // `I256::MIN = -(2^255)`; the max absolute value `I256` can hold is `2^255`.
    // `U256` values up to `2^255` fit; values above `2^255` + 1 don't.
    // `I256::try_from(v)` succeeds for `v <= I256::MAX` (= `2^255 - 1`); the
    // boundary `v == 2^255` (= `I256::MAX + 1` = `-(I256::MIN)` in absolute)
    // must map to `I256::MIN`.
    // `MAX_ABS` = `I256::MAX` = `2^255 - 1` (`0x7fff` is the HIGH
    // limb; little-endian `from_limbs`).
    const MAX_ABS: U256 = U256::from_limbs([
        0xffff_ffff_ffff_ffff,
        0xffff_ffff_ffff_ffff,
        0xffff_ffff_ffff_ffff,
        0x7fff_ffff_ffff_ffff,
    ]);
    if v > MAX_ABS + U256::from(1u8) {
        return Err(GhoProcessorError::DeltaOverflow(format!(
            "value {v} > I256::MIN absolute"
        )));
    }
    if v == MAX_ABS + U256::from(1u8) {
        // `2^255` → `I256::MIN` (= `-(2^255)`).
        Ok(I256::MIN)
    } else {
        Ok(-I256::try_from(v)
            .map_err(|_| GhoProcessorError::DeltaOverflow(format!("negate {v} overflow")))?)
    }
}

// ── the GHO discount-rate formula ─────────────────────────────────────────

/// The GHO discount rate (basis points, 0-3000). Mirrors the
/// `GhoDiscountRateStrategy` contract at mainnet
/// `0x4C38Ec4D1D2068540DfC11DFa4de41F733DDF812`. The rate
/// is the user's discount PERCENT, derived from the stkAAVE balance + the GHO
/// debt: 0 below the minimums, capped at `DISCOUNT_RATE_BPS` (30%) when the
/// discounted balance covers the debt, else proportional.
///
/// Called by the discount-refresh pass after a GHO borrow/accrual that signals
/// `should_refresh_discount` — recomputes `aave_v3_users.gho_discount` from the
/// post-operation debt balance + the user's stkAAVE balance; the caller
/// passes `debt_token_balance = ray_mul(scaled_balance, index)`.
///
/// # Errors
///
/// Returns [`WadRayError`] only if the internal `wad_mul` overflows (cannot
/// happen for realistic balances — stkAAVE + GHO debt are ≪ 2^244).
pub fn calculate_gho_discount_rate(
    debt_balance: U256,
    discount_token_balance: U256,
) -> Result<U256, WadRayError> {
    // 100 GHO discounted per 1 stkAAVE (100e18).
    let gho_discounted_per_discount_token = U256::from(100_000_000_000_000_000_000u128);
    // Max discount rate in basis points (3000 = 30.00%).
    let discount_rate_bps = U256::from(3_000u64);
    // Below this stkAAVE balance, no discount (1e15 = 0.001 stkAAVE).
    let min_discount_token_balance = U256::from(1_000_000_000_000_000u64);
    // Below this GHO debt, no discount (1e18 = 1 GHO).
    let min_debt_token_balance = U256::from(1_000_000_000_000_000_000u64);

    if discount_token_balance < min_discount_token_balance || debt_balance < min_debt_token_balance
    {
        return Ok(U256::ZERO);
    }
    let discounted_balance = wad_mul(discount_token_balance, gho_discounted_per_discount_token)?;
    if discounted_balance >= debt_balance {
        Ok(discount_rate_bps)
    } else {
        // Proportional: (discounted * max_rate) / debt — floor division. `discounted * 3000` cannot overflow for realistic
        // balances (discounted < debt < 2^244); saturating_mul is exact when
        // no overflow + safe if it ever did.
        Ok(discounted_balance.saturating_mul(discount_rate_bps) / debt_balance)
    }
}

#[expect(clippy::panic)]
#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::RAY;

    // ── to_signed_neg boundary (the 2^255 → I256::MIN mapping) ───────────────

    /// `to_signed_neg` must wrap `2^255` (the absolute value of `I256::MIN`)
    /// into `I256::MIN` and error only above it. Regression for the prior
    /// constant `from_limbs([0x7fff_ffff_ffff_ffff, 0xffff_ffff_ffff_ffff,
    /// 0xffff_ffff_ffff_ffff, 0xffff_ffff_ffff_ffff])`, which decodes to
    /// `2^256 - 2^63 - 1` instead of `2^255 - 1` — so `v == 2^255` never hit
    /// the `I256::MIN` arm (it overflow-errored) while a huge out-of-range
    /// `v == 2^256 - 2^63` was silently coerced to `I256::MIN`.
    #[test]
    fn to_signed_neg_maps_two_255_to_i256_min() {
        let two_255 = U256::from(1u8) << 255;
        // v == I256::MAX (2^255 - 1) → -I256::MAX (representable).
        assert_eq!(to_signed_neg(I256::MAX.into_raw()).unwrap(), -I256::MAX);
        // v == 2^255 (|I256::MIN|) → I256::MIN, NOT an overflow error.
        assert_eq!(to_signed_neg(two_255).unwrap(), I256::MIN);
        // v == 2^255 + 1 → DeltaOverflow.
        assert!(matches!(
            to_signed_neg(two_255 + U256::from(1u8)),
            Err(GhoProcessorError::DeltaOverflow(_))
        ));
        // Sanity: 0 → Ok(0).
        assert_eq!(to_signed_neg(U256::ZERO).unwrap(), I256::ZERO);
    }

    // ── strategy fns ──────────────────────────────────────────────────────────

    #[test]
    fn gho_strategy_revision_split() {
        // V1/V2/V3/V4 + unknown → HALF_UP for all ops.
        for rev in [1u32, 2, 3, 4, 7, 0, 99] {
            let s = gho_strategy(rev);
            assert_eq!(s.mint, RayDivMode::HalfUp, "rev {rev} mint");
            assert_eq!(s.burn, RayDivMode::HalfUp, "rev {rev} burn");
        }
        // V5/V6 → CEIL mint, FLOOR burn.
        for rev in [5u32, 6] {
            let s = gho_strategy(rev);
            assert_eq!(s.mint, RayDivMode::Ceil, "rev {rev} mint");
            assert_eq!(s.burn, RayDivMode::Floor, "rev {rev} burn");
        }
    }

    #[test]
    fn gho_discount_strategy_revision_split() {
        // V1: discount + no method.
        let v1 = gho_discount_strategy(1);
        assert!(v1.supports_discount);
        assert!(!v1.has_discounted_balance_method);
        // V2/V3: discount + method.
        for rev in [2u32, 3] {
            let s = gho_discount_strategy(rev);
            assert!(s.supports_discount, "rev {rev} supports_discount");
            assert!(
                s.has_discounted_balance_method,
                "rev {rev} has_discounted_balance_method"
            );
        }
        // V4+: no discount.
        for rev in [4u32, 5, 6, 0, 99] {
            let s = gho_discount_strategy(rev);
            assert!(!s.supports_discount, "rev {rev} no discount");
        }
    }

    #[test]
    fn gho_strategy_distinct_from_debt_strategy() {
        // GHO debt has DISTINCT strategies from debt_strategy.
        // V4: GHO = HALF_UP/HALF_UP; debt = CEIL/FLOOR. Must differ.
        let gho_v4 = gho_strategy(4);
        let debt_v4 = crate::processors::debt_strategy(4);
        assert_ne!(
            (gho_v4.mint, gho_v4.burn),
            (debt_v4.mint, debt_v4.burn),
            "V4 GHO ≠ debt strategies (the DP3 split)"
        );
        // V5: GHO = CEIL/FLOOR; debt = CEIL/FLOOR. These coincide (both V5).
        let gho_v5 = gho_strategy(5);
        let debt_v5 = crate::processors::debt_strategy(5);
        assert_eq!(
            (gho_v5.mint, gho_v5.burn),
            (debt_v5.mint, debt_v5.burn),
            "V5 GHO == debt strategies (coincide)"
        );
    }

    // ── accrue_debt_on_action ────────────────────────────────────────────────

    #[test]
    fn accrue_debt_no_discount_returns_zero() {
        // V4+ → no discount → always 0.
        let p = UnifiedGhoProcessor::new(4);
        assert_eq!(
            p.accrue_debt_on_action(
                U256::from(1_000u64),
                RAY,
                U256::from(5_000u64),
                RAY * U256::from(2u8)
            )
            .unwrap(),
            U256::ZERO
        );
    }

    #[test]
    fn accrue_debt_zero_balance_returns_zero() {
        let p = UnifiedGhoProcessor::new(2);
        assert_eq!(
            p.accrue_debt_on_action(U256::ZERO, RAY, U256::from(5_000u64), RAY * U256::from(2u8))
                .unwrap(),
            U256::ZERO
        );
    }

    #[test]
    fn accrue_debt_zero_percent_returns_zero() {
        let p = UnifiedGhoProcessor::new(2);
        // Non-zero balance + non-zero index delta + zero percent → 0.
        assert_eq!(
            p.accrue_debt_on_action(U256::from(1_000u64), RAY, U256::ZERO, RAY * U256::from(2u8))
                .unwrap(),
            U256::ZERO
        );
    }

    #[test]
    fn accrue_debt_no_index_change_returns_zero() {
        let p = UnifiedGhoProcessor::new(2);
        // With current_index == prev_index, balance_increase = 0 → 0.
        assert_eq!(
            p.accrue_debt_on_action(U256::from(1_000u64), RAY, U256::from(5_000u64), RAY)
                .unwrap(),
            U256::ZERO
        );
    }

    #[test]
    fn accrue_debt_v2_discounted_value() {
        // prev_balance=1000, prev_index=1 RAY, current_index=2 RAY (100% increase).
        // balance_increase = ray_mul(1000, 2 RAY) - ray_mul(1000, 1 RAY) = 2000 - 1000 = 1000.
        // discount = percent_mul(1000, 5000) = (1000*5000 + 5000) // 10000 = 500.
        // discount_scaled = ray_div(500, 2 RAY) = 250 (half-up: (500*RAY + RAY)//(2*RAY) = 250).
        let p = UnifiedGhoProcessor::new(2);
        assert_eq!(
            p.accrue_debt_on_action(
                U256::from(1_000u64),
                RAY,
                U256::from(5_000u64),
                RAY * U256::from(2u8)
            )
            .unwrap(),
            U256::from(250u64)
        );
    }

    // ── get_discounted_balance ───────────────────────────────────────────────

    #[test]
    fn discounted_balance_zero_returns_zero() {
        let p = UnifiedGhoProcessor::new(2);
        assert_eq!(
            p.get_discounted_balance(U256::ZERO, RAY, RAY * U256::from(2u8), U256::from(5_000u64))
                .unwrap(),
            U256::ZERO
        );
    }

    #[test]
    fn discounted_balance_no_discount_returns_ray_mul() {
        // V4+ → no discount → just ray_mul(scaled, current).
        let p = UnifiedGhoProcessor::new(4);
        // ray_mul(1000, 2 RAY) = 2000.
        assert_eq!(
            p.get_discounted_balance(
                U256::from(1_000u64),
                RAY,
                RAY * U256::from(2u8),
                U256::from(5_000u64)
            )
            .unwrap(),
            U256::from(2_000u64)
        );
    }

    #[test]
    fn discounted_balance_equal_indices_returns_ray_mul() {
        // current_index == prev_index → no discount applied (no accrual).
        let p = UnifiedGhoProcessor::new(2);
        assert_eq!(
            p.get_discounted_balance(U256::from(1_000u64), RAY, RAY, U256::from(5_000u64))
                .unwrap(),
            U256::from(1_000u64)
        );
    }

    // ── process_gho_debt_mint ────────────────────────────────────────────────

    #[test]
    fn mint_v4_borrow_no_discount() {
        // V4: no discount, HALF_UP mint. value=2000 >= balance_increase=0.
        // requested = 2000; balance_delta = ray_div(2000, 1 RAY, HALF_UP) = 2000.
        let p = UnifiedGhoProcessor::new(4);
        let ev = ScaledTokenEventData {
            value: U256::from(2_000u64),
            balance_increase: U256::ZERO,
            index: RAY,
            scaled_amount: None,
        };
        let r = p
            .process_gho_debt_mint(&ev, U256::ZERO, RAY, U256::ZERO, None)
            .unwrap();
        assert_eq!(r.balance_delta, I256::try_from(2_000u64).unwrap());
        assert_eq!(r.user_operation, GhoUserOperation::Borrow);
        assert!(!r.should_refresh_discount);
        assert!(!r.is_repay());
    }

    #[test]
    fn mint_equality_undiscounted_is_pure_interest_accrual() {
        // `value == balance_increase` means the Mint carried NO requested
        // amount: the event reports only the index-driven accrual. Debt
        // interest accrues through the index advance, not through a scaled
        // mint, so an undiscounted revision (V4/V5/V6) must leave the scaled
        // balance untouched.
        //
        // Balance of the shape: `_mintScaled` / `_burnScaled` both require a
        // non-zero amount (INVALID_MINT_AMOUNT / INVALID_BURN_AMOUNT) and
        // emit `value = amount + balanceIncrease` / `value = balanceIncrease
        // - amount`, so neither can produce equality; the arm exists to make
        // the classification total, never to mint.
        for rev in [4u32, 5, 6] {
            let p = UnifiedGhoProcessor::new(rev);
            let ev = ScaledTokenEventData {
                value: U256::from(1_000u64),
                balance_increase: U256::from(1_000u64),
                index: RAY * U256::from(2u8),
                scaled_amount: Some(U256::ZERO),
            };
            let r = p
                .process_gho_debt_mint(&ev, U256::from(1_000u64), RAY, U256::ZERO, None)
                .unwrap_or_else(|e| panic!("rev {rev}: {e:?}"));
            assert_eq!(
                r.user_operation,
                GhoUserOperation::InterestAccrual,
                "rev {rev}: value == balance_increase is interest accrual"
            );
            assert_eq!(
                r.balance_delta,
                I256::ZERO,
                "rev {rev}: no requested amount, no discount → no scaled delta"
            );
            assert!(!r.is_repay(), "rev {rev}: accrual is not a repay");
            assert!(
                !r.should_refresh_discount,
                "rev {rev}: no discount mechanism → no refresh"
            );
        }
    }

    #[test]
    fn mint_equality_discounted_burns_only_the_discount_scaled_share() {
        // Discounted equality is the contract's accrual edition:
        // `_accrueDebtOnAction` settles the accrued interest, the token burns
        // the discounted share (`_burn(user, discountScaled)`) and then emits
        // `Mint(address(0), user, balanceIncrease, balanceIncrease, index)` —
        // value == balance_increase by construction. The only scaled-balance
        // movement is the discount burn, so delta = -discount_scaled and the
        // requested amount never appears.
        //
        // prev=1000, prev_index=RAY, index=2 RAY, pct=5000 bps:
        // balance_increase = 2000 - 1000 = 1000, discount = 500,
        // discount_scaled = ray_div(500, 2 RAY) = 250.
        for rev in [1u32, 2, 3] {
            let p = UnifiedGhoProcessor::new(rev);
            let ev = ScaledTokenEventData {
                value: U256::from(1_000u64),
                balance_increase: U256::from(1_000u64),
                index: RAY * U256::from(2u8),
                scaled_amount: None,
            };
            let r = p
                .process_gho_debt_mint(&ev, U256::from(1_000u64), RAY, U256::from(5_000u64), None)
                .unwrap_or_else(|e| panic!("rev {rev}: {e:?}"));
            assert_eq!(
                r.user_operation,
                GhoUserOperation::InterestAccrual,
                "rev {rev}: value == balance_increase is interest accrual"
            );
            assert_eq!(r.discount_scaled, U256::from(250u64), "rev {rev}");
            assert_eq!(
                r.balance_delta,
                -I256::try_from(250u64).unwrap(),
                "rev {rev}: the discount burn is the whole delta"
            );
            assert!(!r.is_repay(), "rev {rev}: accrual is not a repay");
            assert!(r.should_refresh_discount, "rev {rev}: discount refreshes");
        }
    }

    #[test]
    fn mint_equality_neighbour_boundaries_classify_borrow_and_repay() {
        // The exhaustive `cmp` partition: one wei above equality is a borrow
        // of the requested amount, one wei below is a repay of the interest
        // shortfall. Both discount modes.
        let ev = |value: u64, balance_increase: u64| ScaledTokenEventData {
            value: U256::from(value),
            balance_increase: U256::from(balance_increase),
            index: RAY,
            scaled_amount: None,
        };

        for rev in [1u32, 4] {
            let p = UnifiedGhoProcessor::new(rev);
            let discount = if rev <= 3 {
                U256::from(5_000u64)
            } else {
                U256::ZERO
            };
            // Zero prev balance + equal indices → discount_scaled = 0, so the
            // neighbouring deltas are the requested/repay amounts verbatim.
            let borrow = p
                .process_gho_debt_mint(&ev(1_001, 1_000), U256::ZERO, RAY, discount, None)
                .unwrap_or_else(|e| panic!("rev {rev}: {e:?}"));
            assert_eq!(borrow.user_operation, GhoUserOperation::Borrow, "rev {rev}");
            assert_eq!(
                borrow.balance_delta,
                I256::try_from(1u64).unwrap(),
                "rev {rev}: value = balance_increase + 1 borrows 1 wei"
            );

            let repay = p
                .process_gho_debt_mint(&ev(999, 1_000), U256::from(1_000u64), RAY, discount, None)
                .unwrap_or_else(|e| panic!("rev {rev}: {e:?}"));
            assert_eq!(repay.user_operation, GhoUserOperation::Repay, "rev {rev}");
            assert_eq!(
                repay.balance_delta,
                -I256::try_from(1u64).unwrap(),
                "rev {rev}: value = balance_increase - 1 repays 1 wei"
            );
        }
    }

    #[test]
    fn mint_v5_borrow_uses_precomputed_scaled_amount() {
        // V5: no discount, CEIL mint → if scaled_amount is Some, use it directly.
        let p = UnifiedGhoProcessor::new(5);
        let ev = ScaledTokenEventData {
            value: U256::from(2_000u64),
            balance_increase: U256::ZERO,
            index: RAY,
            scaled_amount: Some(U256::from(1_999u64)), // the enrichment's pre-calculated value
        };
        let r = p
            .process_gho_debt_mint(&ev, U256::ZERO, RAY, U256::ZERO, None)
            .unwrap();
        assert_eq!(
            r.balance_delta,
            I256::try_from(1_999u64).unwrap(),
            "V5 CEIL mint with scaled_amount → use it directly"
        );
    }

    #[test]
    fn mint_v4_repay_uses_actual_repay_amount() {
        // V4: no discount, value < balance_increase → REPAY.
        // amount_repaid = actual_repay_amount if provided.
        // repayment_scaled = ray_div(amount_repaid, index, HALF_UP burn).
        let p = UnifiedGhoProcessor::new(4);
        let ev = ScaledTokenEventData {
            value: U256::from(100u64),
            balance_increase: U256::from(500u64), // balance_increase > value → REPAY
            index: RAY,
            scaled_amount: None,
        };
        let r = p
            .process_gho_debt_mint(
                &ev,
                U256::from(1_000u64),
                RAY,
                U256::ZERO,
                Some(U256::from(400u64)),
            )
            .unwrap();
        // repayment_scaled = ray_div(400, RAY, HALF_UP) = 400.
        // balance_delta = -400.
        assert_eq!(r.balance_delta, -I256::try_from(400u64).unwrap());
        assert_eq!(r.user_operation, GhoUserOperation::Repay);
        assert!(r.is_repay());
    }

    // ── process_gho_debt_burn ────────────────────────────────────────────────

    #[test]
    fn burn_v4_no_discount_partial_repay() {
        // V4: no discount, HALF_UP burn.
        // prev_balance=1000, prev_index=RAY. current_index=RAY (no change).
        // requested = value + balance_increase = 300 + 0 = 300.
        // amount_scaled = ray_div(300, RAY, HALF_UP) = 300.
        // balance_before_burn = ray_mul(1000, RAY) = 1000. requested(300) < 1000 → partial.
        // balance_delta = -300.
        let p = UnifiedGhoProcessor::new(4);
        let ev = ScaledTokenEventData {
            value: U256::from(300u64),
            balance_increase: U256::ZERO,
            index: RAY,
            scaled_amount: None,
        };
        let r = p
            .process_gho_debt_burn(&ev, U256::from(1_000u64), RAY, U256::ZERO)
            .unwrap();
        assert_eq!(r.balance_delta, -I256::try_from(300u64).unwrap());
        assert!(!r.should_refresh_discount);
    }

    #[test]
    fn burn_v4_full_repayment_burns_all() {
        // V4: no discount, full repayment → burn all scaled balance.
        // prev_balance=1000, prev_index=RAY. current_index=RAY (no change).
        // requested = value + balance_increase = 1000 + 0 = 1000.
        // balance_before_burn = ray_mul(1000, RAY) = 1000.
        // requested(1000) == balance_before_burn(1000) → full → burn all (1000).
        let p = UnifiedGhoProcessor::new(4);
        let ev = ScaledTokenEventData {
            value: U256::from(1_000u64),
            balance_increase: U256::ZERO,
            index: RAY,
            scaled_amount: None,
        };
        let r = p
            .process_gho_debt_burn(&ev, U256::from(1_000u64), RAY, U256::ZERO)
            .unwrap();
        assert_eq!(r.balance_delta, -I256::try_from(1_000u64).unwrap());
    }

    #[test]
    fn burn_v5_uses_precomputed_scaled_amount() {
        // V5: no discount, FLOOR burn → use scaled_amount if provided.
        // prev_balance=1000 so requested(300) < balance_before(1000) → partial.
        let p = UnifiedGhoProcessor::new(5);
        let ev = ScaledTokenEventData {
            value: U256::from(300u64),
            balance_increase: U256::ZERO,
            index: RAY,
            scaled_amount: Some(U256::from(301u64)), // the enrichment's pre-calculated value
        };
        let r = p
            .process_gho_debt_burn(&ev, U256::from(1_000u64), RAY, U256::ZERO)
            .unwrap();
        // amount_scaled = 301 (the pre-computed value).
        // balance_before = ray_mul(1000, RAY) = 1000. requested = 300. 300 < 1000 → partial.
        // balance_delta = -301.
        assert_eq!(r.balance_delta, -I256::try_from(301u64).unwrap());
    }

    // ── the parity-drift canary: V1 vs V4 vs V5 ────────────────────────────────

    #[test]
    fn gho_processor_revision_parity_smoke() {
        // The parity-gate zero-drift surface: a BORROW Mint with value=1000,
        // balance_increase=0, index=1 RAY, scaled_amount=None, prev=0, prev_index=RAY.
        // - V4 (no discount, HALF_UP): balance_delta = 1000.
        // - V5 (no discount, CEIL): balance_delta = ray_div(1000, 1 RAY, CEIL) = 1000
        //   (exact, no remainder → no ceil bump).
        // - V1 (discount, HALF_UP): balance_delta = 1000 (discount_scaled = 0
        //   since prev_balance=0).
        let ev = ScaledTokenEventData {
            value: U256::from(1_000u64),
            balance_increase: U256::ZERO,
            index: RAY,
            scaled_amount: None,
        };
        for rev in [1u32, 2, 3, 4, 5, 6] {
            let p = UnifiedGhoProcessor::new(rev);
            let r = p
                .process_gho_debt_mint(&ev, U256::ZERO, RAY, U256::ZERO, None)
                .unwrap_or_else(|e| panic!("rev {rev}: {e:?}"));
            assert_eq!(
                r.balance_delta,
                I256::try_from(1_000u64).unwrap(),
                "rev {rev} BORROW with zero prev_balance → 1000"
            );
            assert_eq!(r.user_operation, GhoUserOperation::Borrow);
            // should_refresh_discount = supports_discount (V1/V2/V3 → true, V4+ → false).
            assert_eq!(
                r.should_refresh_discount,
                rev <= 3,
                "rev {rev} should_refresh_discount"
            );
        }
    }

    #[test]
    fn v1_and_v4_strategies_differ_on_discount_flag() {
        // The V1-vs-V4 split: V1 supports_discount, V4 does not.
        let v1 = UnifiedGhoProcessor::new(1);
        let v4 = UnifiedGhoProcessor::new(4);
        assert!(v1.supports_discount(), "V1 supports discount");
        assert!(!v4.supports_discount(), "V4 no discount");
        // Both use HALF_UP rounding (the V4 GHO predates the V5 CEIL/FLOOR change).
        assert_eq!(v1.rounding, v4.rounding, "V1 == V4 rounding");
    }

    #[test]
    fn gho_user_operation_is_repay_flag() {
        assert!(!GhoUserOperation::Borrow.is_repay());
        assert!(GhoUserOperation::Repay.is_repay());
        assert!(!GhoUserOperation::InterestAccrual.is_repay());
    }

    // ── parity gate (`docs/architecture/parity-gate.md`) cross-check ────────
    //
    // Each fixture is `(rev, value, balance_increase, index, scaled_amount,
    // prev_balance, prev_index, prev_discount, actual_repay)` → the EXACT
    // `(user_operation, balance_delta, discount_scaled, should_refresh)` the
    // processor must produce. The rows are the recorded fixture: the surviving
    // record of the retired processor, and the final arbiter of this leaf's
    // zero-drift surface.

    #[test]
    fn parity_mint_borrow_v1_v2_v4_v5() {
        // BORROW path: rev=1 (V1 discount, prev=0→discount=0), rev=2 (discount
        // active but prev=0→discount=0), rev=4 (no discount HALF_UP), rev=5
        // (no discount CEIL → uses precomputed scaled_amount).
        let ev = |val, bi, sa: Option<u64>| ScaledTokenEventData {
            value: U256::from(val),
            balance_increase: U256::from(bi),
            index: RAY,
            scaled_amount: sa.map(U256::from),
        };
        // rev=1 BORROW: delta=1000, refresh=True (discount active).
        let p1 = UnifiedGhoProcessor::new(1);
        let r = p1
            .process_gho_debt_mint(&ev(1000, 0, None), U256::ZERO, RAY, U256::ZERO, None)
            .unwrap();
        assert_eq!(r.balance_delta, I256::try_from(1000u64).unwrap());
        assert_eq!(r.user_operation, GhoUserOperation::Borrow);
        assert!(r.should_refresh_discount);
        // rev=2 BORROW (discount=5000 but prev=0→discount_scaled=0): delta=1000.
        let p2 = UnifiedGhoProcessor::new(2);
        let r = p2
            .process_gho_debt_mint(
                &ev(1000, 0, None),
                U256::ZERO,
                RAY,
                U256::from(5_000u64),
                None,
            )
            .unwrap();
        assert_eq!(r.balance_delta, I256::try_from(1000u64).unwrap());
        // rev=4 BORROW (no discount): delta=1000, refresh=False.
        let p4 = UnifiedGhoProcessor::new(4);
        let r = p4
            .process_gho_debt_mint(&ev(1000, 0, None), U256::ZERO, RAY, U256::ZERO, None)
            .unwrap();
        assert_eq!(r.balance_delta, I256::try_from(1000u64).unwrap());
        assert!(!r.should_refresh_discount);
        // rev=5 BORROW (CEIL → uses precomputed scaled_amount=1500).
        let p5 = UnifiedGhoProcessor::new(5);
        let r = p5
            .process_gho_debt_mint(&ev(1000, 0, Some(1500)), U256::ZERO, RAY, U256::ZERO, None)
            .unwrap();
        assert_eq!(r.balance_delta, I256::try_from(1500u64).unwrap());
    }

    #[test]
    fn parity_mint_repay_v4_v2() {
        // REPAY path (balance_increase > value):
        // rev=4 (no discount): amount_repaid=actual_repay=400; delta=-400.
        // rev=4 fallback (no actual_repay): derive 500-100=400; delta=-400.
        // rev=2 (discount, has_method): balance_before = ray_mul(1000, RAY)=1000
        //   (current==prev); amount_repaid(400) != 1000 → partial → delta=-(400+0)=-400.
        let ev = |val, bi| ScaledTokenEventData {
            value: U256::from(val),
            balance_increase: U256::from(bi),
            index: RAY,
            scaled_amount: None,
        };
        let p4 = UnifiedGhoProcessor::new(4);
        let r = p4
            .process_gho_debt_mint(
                &ev(100, 500),
                U256::from(1000u64),
                RAY,
                U256::ZERO,
                Some(U256::from(400u64)),
            )
            .unwrap();
        assert_eq!(r.balance_delta, -I256::try_from(400u64).unwrap());
        assert_eq!(r.user_operation, GhoUserOperation::Repay);
        // fallback (no actual_repay → derive 500-100=400).
        let r = p4
            .process_gho_debt_mint(&ev(100, 500), U256::from(1000u64), RAY, U256::ZERO, None)
            .unwrap();
        assert_eq!(r.balance_delta, -I256::try_from(400u64).unwrap());
        // rev=2 with discount (current==prev → discount_scaled=0).
        let p2 = UnifiedGhoProcessor::new(2);
        let r = p2
            .process_gho_debt_mint(
                &ev(100, 500),
                U256::from(1000u64),
                RAY,
                U256::from(5_000u64),
                Some(U256::from(400u64)),
            )
            .unwrap();
        assert_eq!(r.balance_delta, -I256::try_from(400u64).unwrap());
        assert!(
            r.should_refresh_discount,
            "V2 REPAY always refreshes discount"
        );
    }

    #[test]
    fn parity_mint_v1_discounted_equality_burns_only_the_discount_scaled_share() {
        // Recorded row: GHO vToken 0x786dbff3 (rev=1), block 17893923 logIdx
        // 361 of tx 0xdaf7a4e911c3f5b1492fbc25b95137ca2db6766c56cbf2d8c855b5444fbeab27
        // — the contract's accrual edition. `_accrueDebtOnAction` settles the
        // accrued interest, `_burn(user, discountScaled)` moves the scaled
        // balance, and the token emits `Mint(address(0), user, value,
        // balanceIncrease, index)` carrying the net-of-discount increase:
        // value == balance_increase by construction, so the whole delta is the
        // discount burn and no requested amount is implied.
        //
        // Inputs, all read off chain:
        //   prev_balance   = 299812144457223276709515  scaledBalanceOf(0x417afF82)
        //                    at block 17893922 — also ray_div(300_000e18, prev_index)
        //   prev_index     = 1000626577496107821018090417  (1.000626577)
        //   prev_discount  = 1649 bps — DiscountPercentUpdated at logIdx 359 sets
        //                    1725 → 1649 before this log
        //   value          = 123683949235110136664 (123.683949 GHO)
        //   balance_increase = 123683949235110136664 (123.683949 GHO)
        //   index          = 1001120576006523063100148152 (1.001120576)
        //   interest = 148106752766267676522, discount = 24422803531157539858,
        //   net = 123683949235110136664 — the event's value, exactly.
        // The recorded result:
        //   balance_delta   = -24395466556666203902 (-24.395467 GHO)
        //   discount_scaled = 24395466556666203902 (24.395467 GHO)
        //   user_operation  = GhoUserOperation::InterestAccrual
        let p1 = UnifiedGhoProcessor::new(1);
        let ev = ScaledTokenEventData {
            value: U256::from(123_683_949_235_110_136_664u128),
            balance_increase: U256::from(123_683_949_235_110_136_664u128),
            index: U256::from(1_001_120_576_006_523_063_100_148_152u128),
            scaled_amount: None,
        };
        let prev_balance = U256::from(299_812_144_457_223_276_709_515u128);
        let prev_index = U256::from(1_000_626_577_496_107_821_018_090_417u128);
        let r = p1
            .process_gho_debt_mint(&ev, prev_balance, prev_index, U256::from(1_649u64), None)
            .unwrap();
        assert_eq!(
            r.user_operation,
            GhoUserOperation::InterestAccrual,
            "the accrual edition classifies as interest accrual"
        );
        assert_eq!(
            r.discount_scaled,
            U256::from(24_395_466_556_666_203_902u128),
            "the discount's scaled share, exactly as the contract burned it"
        );
        assert_eq!(
            r.balance_delta,
            -I256::try_from(r.discount_scaled).unwrap(),
            "discounted equality → the NEGATIVE discount_scaled burn, and nothing else (recorded fixture)"
        );
    }

    #[test]
    fn parity_burn_v1_v4_v5_v2() {
        // BURN path: rev=1 (V1 discount, prev=0→discount=0): delta=-300.
        //   rev=4 (no discount, partial): delta=-300.
        //   rev=4 full repayment: delta=-1000.
        //   rev=5 (FLOOR → uses precomputed scaled_amount=400): delta=-400.
        //   rev=2 (discount, current==prev→discount=0): delta=-300.
        let ev = |val, bi, sa: Option<u64>| ScaledTokenEventData {
            value: U256::from(val),
            balance_increase: U256::from(bi),
            index: RAY,
            scaled_amount: sa.map(U256::from),
        };
        // rev=1: delta=-300, refresh=True.
        let p1 = UnifiedGhoProcessor::new(1);
        let r = p1
            .process_gho_debt_burn(&ev(300, 0, None), U256::ZERO, RAY, U256::from(5_000u64))
            .unwrap();
        assert_eq!(r.balance_delta, -I256::try_from(300u64).unwrap());
        assert!(r.should_refresh_discount);
        // rev=4 partial (prev=1000): requested=300; balance_before=ray_mul(1000,RAY)=1000;
        //   300 < 1000 → partial → delta=-300; refresh=False.
        let p4 = UnifiedGhoProcessor::new(4);
        let r = p4
            .process_gho_debt_burn(&ev(300, 0, None), U256::from(1_000u64), RAY, U256::ZERO)
            .unwrap();
        assert_eq!(r.balance_delta, -I256::try_from(300u64).unwrap());
        assert!(!r.should_refresh_discount);
        // rev=4 full repayment: requested=1000; balance_before=ray_mul(1000,RAY)=1000;
        //   1000==1000 → full → delta=-1000.
        let r = p4
            .process_gho_debt_burn(&ev(1000, 0, None), U256::from(1000u64), RAY, U256::ZERO)
            .unwrap();
        assert_eq!(r.balance_delta, -I256::try_from(1000u64).unwrap());
        // rev=5 (FLOOR → precomputed scaled_amount=400): prev=1000 so partial. delta=-400.
        let p5 = UnifiedGhoProcessor::new(5);
        let r = p5
            .process_gho_debt_burn(
                &ev(300, 0, Some(400)),
                U256::from(1_000u64),
                RAY,
                U256::ZERO,
            )
            .unwrap();
        assert_eq!(r.balance_delta, -I256::try_from(400u64).unwrap());
        // rev=2 (discount, current==prev→discount_scaled=0): delta=-300.
        let p2 = UnifiedGhoProcessor::new(2);
        let r = p2
            .process_gho_debt_burn(
                &ev(300, 0, None),
                U256::from(1000u64),
                RAY,
                U256::from(5_000u64),
            )
            .unwrap();
        assert_eq!(r.balance_delta, -I256::try_from(300u64).unwrap());
        assert!(r.should_refresh_discount);
    }

    #[test]
    fn parity_accrue_v2_with_index_change() {
        // The discount-accrual path with a real index change — exercises the
        // `percent_mul(balance_increase, discount_percent)` + `ray_div` path.
        // prev=1000, prev_index=RAY, current=2*RAY (100% increase), pct=5000 (50%).
        // balance_increase = ray_mul(1000,2RAY)-ray_mul(1000,RAY) = 2000-1000 = 1000.
        // discount = percent_mul(1000,5000) = 500.
        // discount_scaled = ray_div(500, 2RAY) = 250.
        let p2 = UnifiedGhoProcessor::new(2);
        assert_eq!(
            p2.accrue_debt_on_action(
                U256::from(1_000u64),
                RAY,
                U256::from(5_000u64),
                RAY * U256::from(2u8),
            )
            .unwrap(),
            U256::from(250u64)
        );
    }

    // ── calculate_gho_discount_rate (port of GhoMath.calculate_discount_rate) ─

    fn e(n: u128) -> U256 {
        U256::from(n)
    }
    fn e18(n: u64) -> U256 {
        U256::from(n) * U256::from(10u64).pow(U256::from(18u64))
    }

    #[test]
    fn calculate_gho_discount_rate_zero_below_minimums() {
        // stkAAVE below 1e15 (0.001) → 0.
        assert_eq!(
            calculate_gho_discount_rate(e18(5000), e(500)).unwrap(),
            U256::ZERO
        );
        // debt below 1e18 (1 GHO) → 0.
        assert_eq!(
            calculate_gho_discount_rate(e(500), e18(100)).unwrap(),
            U256::ZERO
        );
    }

    #[test]
    fn calculate_gho_discount_rate_capped_at_max() {
        // 5000 GHO debt, 100 stkAAVE → 100*100e18 = 10000e18 discounted >=
        // 5000e18 debt → capped at 3000 bps.
        assert_eq!(
            calculate_gho_discount_rate(e18(5000), e18(100)).unwrap(),
            U256::from(3000u64)
        );
    }

    #[test]
    fn calculate_gho_discount_rate_proportional() {
        // discounted = wad_mul(10stk, 100e18) = 1000 GHO; debt = 2000 GHO →
        // (1000e18 * 3000) // 2000e18 = 1500.
        assert_eq!(
            calculate_gho_discount_rate(e18(2000), e18(10)).unwrap(),
            U256::from(1500u64)
        );
    }
}
