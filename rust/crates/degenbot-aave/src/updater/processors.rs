//! Aave V3 scaled-token processors — the revision-aware `balance_delta`
//! computation for aToken/vToken Mint/Burn events (5Z3QQ2 — SCALEAPPLY).
//!
//! Port of the Python `src/degenbot/aave/processors/processor.py` (the
//! `UnifiedCollateralProcessor` + `UnifiedDebtProcessor`) and the
//! `src/degenbot/aave/processors/strategies.py` module (the `RoundingStrategy`
//! struct + the per-revision `COLLATERAL_STRATEGIES` / `DEBT_STRATEGIES`
//! dicts). The GHO processor (`UnifiedGhoProcessor`) is sibling `RYKCC4`
//! (SPECIALAPPLY)'s scope — NOT ported here.
//!
//! # The two-layer split (mirrors the Python architecture)
//!
//! This module is the processor: pure CPU, no I/O. It takes the event's
//! decoded fields (`value`, `balance_increase`, `index`, `scaled_amount`)
//! and the position's `previous_balance`/`previous_index` (the Python passes
//! these but the current processors don't read them — kept for protocol
//! parity) plus the token's revision (selects the rounding strategy) and
//! returns [`ScaledTokenMintResult`] / [`ScaledTokenBurnResult`] carrying the
//! signed `balance_delta` and the `new_index` (the event's index, applied to
//! the position's `last_index` via the max-with-prev reconciliation in
//! [`crate::run::apply_aave_chunk_writes_on_conn`]'s apply fn dispatch).
//!
//! The apply fn (`DegenbotDb::apply_scaled_token_mint_on_conn` in
//! `degenbot-db/src/write.rs`) is the pure substrate that mutates the position
//! row's `balance` and `last_index` columns, taking the pre-computed
//! `balance_delta` from this processor.
//!
//! # §4.2 parity (U5YIBG)
//!
//! The revision-strategy dicts and the `process_mint_event` /
//! `process_burn_event` branch logic are byte-for-byte ports of the Python
//! (the §4.2 cross-check compares Rust-written rows vs the Python oracle, so
//! any deviation bites there). The ray-math primitives (`ray_div` with
//! floor/ceil/half-up) come from `degenbot-evm-math::wad_ray_math`.

#![expect(clippy::doc_lazy_continuation)] // multi-line rustdoc phrasing

use alloy::primitives::{I256, U256};
use degenbot_evm_math::{RayRounding, WadRayError};

// ── the revision strategies (ports `processors/strategies.py`) ────────────

/// The rounding mode for a `ray_div` op. Mirrors `RoundingMode` (`HALF_UP` /
/// FLOOR / CEIL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RayDivMode {
    /// `(a * RAY + b // 2) // b` — round half up.
    HalfUp,
    /// `(a * RAY) // b` — truncate toward zero.
    Floor,
    /// `(a * RAY) // b + ((a * RAY) % b != 0)` — round toward +∞.
    Ceil,
}

impl From<RayDivMode> for Option<RayRounding> {
    fn from(mode: RayDivMode) -> Self {
        match mode {
            RayDivMode::HalfUp => Some(RayRounding::HalfUp),
            RayDivMode::Floor => Some(RayRounding::Floor),
            RayDivMode::Ceil => Some(RayRounding::Ceil),
        }
    }
}

/// The per-revision rounding strategy for a token (collateral or debt).
/// Mirrors `RoundingStrategy` (`mint_rounding` + `burn_rounding`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundingStrategy {
    /// The `ray_div` rounding for `process_mint_event`'s fallback path.
    pub mint: RayDivMode,
    /// The `ray_div` rounding for `process_burn_event`'s fallback path.
    pub burn: RayDivMode,
}

/// The collateral (aToken) rounding strategy per revision. Port of
/// `COLLATERAL_STRATEGIES` — V1/V3: `HALF_UP` for all ops; V4/V5: `HALF_UP` mint,
/// CEIL burn (the V4 burn-rounding-up change is the bit that's bitten ports
/// before; the §4.2 cross-check will catch a missed branch).
#[must_use]
pub const fn collateral_strategy(revision: u32) -> RoundingStrategy {
    match revision {
        // V4/V5: HALF_UP mint, CEIL burn.
        4 | 5 => RoundingStrategy {
            mint: RayDivMode::HalfUp,
            burn: RayDivMode::Ceil,
        },
        // V1/V2/V3 and unknown: HALF_UP for all ops.
        _ => RoundingStrategy {
            mint: RayDivMode::HalfUp,
            burn: RayDivMode::HalfUp,
        },
    }
}

/// The debt (vToken) rounding strategy per revision. Port of
/// `DEBT_STRATEGIES` — V1/V3: `HALF_UP` for all ops; V4/V5: CEIL mint, FLOOR
/// burn.
#[must_use]
pub const fn debt_strategy(revision: u32) -> RoundingStrategy {
    match revision {
        // V4/V5: CEIL mint, FLOOR burn.
        4 | 5 => RoundingStrategy {
            mint: RayDivMode::Ceil,
            burn: RayDivMode::Floor,
        },
        // V1/V2/V3 and unknown: HALF_UP for all ops.
        _ => RoundingStrategy {
            mint: RayDivMode::HalfUp,
            burn: RayDivMode::HalfUp,
        },
    }
}

// ── the enricher math (ports `libraries/token_math.py`) ────────────────────
//
// These four functions mirror `TokenMathFactory.get_token_math_for_token_revision`
// (`libraries/token_math.py:267-285`) — NOT the `RoundingStrategy` above. The
// Python codebase keeps two separate sources for, in principle, the same
// decision, and they **disagree on collateral mint for rev ≥4**:
//
//   - `strategies.py:COLLATERAL_STRATEGIES[4].mint_rounding = HALF_UP`
//   - `token_math.py:ExplicitRoundingMath.get_collateral_mint_scaled_amount`
//     returns `ray_div_floor` (FLOOR)
//
// The disagreement is invisible in Python because the processor's `None`
// fallback (which uses `RoundingStrategy`) is dead code in practice: the
// enricher always supplies `ScaledTokenEventData.scaled_amount`, routing
// through `ScaledAmountCalculator` → `TokenMathFactory.get_token_math_for_token_revision`
// (`enrichment/context.py:225-241` → `calculator.py:21,58`). Python's enricher
// therefore computes the chain-side `rayDivFloor` for rev ≥4 collateral
// mints, matching `Pool/rev_9.sol:executeSupply:9312`:
//
//   `scaledAmount = params.amount.getATokenMintScaledAmount(index) = rayDivFloor`
//
// The Rust port originally collapsed both sources into `collateral_strategy`/
// `debt_strategy`, and the `build_scaled_event_chunk_event` enricher hooked
// `strategy_mode.{mint,burn}` — i.e. the **processor-fallback** table. That yields
// `ray_div_half_up(raw_amount, index)` for rev_4 collateral mints, which is
// +1 wei above chain whenever `(raw_amount * RAY) % index > index // 2`
// (root cause of the pos-173339 divergence @ block 23_089_231).
//
// The fix surfaces the `token_math.py` table as first-class in Rust so the
// enricher can no longer accidentally reuse the processor fallback.

/// Enricher rounding for a collateral `mint` (= the **supplied** side), by
/// **`token_revision`** of the aToken. Port of
/// `TokenMathFactory.get_token_math_for_token_revision` → `get_collateral_mint_scaled_amount`.
/// rev ≥4 ⇒ FLOOR (`ExplicitRoundingMath`); rev ≤3 ⇒ `HALF_UP` (`HalfUpRoundingMath`).
#[must_use]
pub const fn enricher_collateral_mint(token_revision: u32) -> RayDivMode {
    match token_revision {
        0..=3 => RayDivMode::HalfUp,
        _ => RayDivMode::Floor,
    }
}

/// Enricher rounding for a collateral `burn`. Port of
/// `ExplicitRoundingMath::get_collateral_burn_scaled_amount = ray_div_ceil`.
#[must_use]
pub const fn enricher_collateral_burn(token_revision: u32) -> RayDivMode {
    match token_revision {
        0..=3 => RayDivMode::HalfUp,
        _ => RayDivMode::Ceil,
    }
}

/// Enricher rounding for a debt `mint`. Port of
/// `ExplicitRoundingMath::get_debt_mint_scaled_amount = ray_div_ceil`.
#[must_use]
pub const fn enricher_debt_mint(token_revision: u32) -> RayDivMode {
    match token_revision {
        0..=3 => RayDivMode::HalfUp,
        _ => RayDivMode::Ceil,
    }
}

/// Enricher rounding for a debt `burn`. Port of
/// `ExplicitRoundingMath::get_debt_burn_scaled_amount = ray_div_floor`.
#[must_use]
pub const fn enricher_debt_burn(token_revision: u32) -> RayDivMode {
    match token_revision {
        0..=3 => RayDivMode::HalfUp,
        _ => RayDivMode::Floor,
    }
}

// ── the event data (ports `processors/base.py` event dataclasses) ──────────

/// The decoded fields of a collateral (aToken) or debt (vToken) `Mint`/`Burn`
/// event. The Pool contract emits these with `value` (the user-facing amount)
/// + `balance_increase` (the interest accrued since the last event) + `index`
/// (the asset's current `liquidity_index` for collateral / `borrow_index` for
/// debt) + an optional `scaled_amount` (a pre-calculated delta the Pool
/// contract handed the token contract for V4+ revisions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScaledTokenEventData {
    /// The event's `value` field (the user-facing amount).
    pub value: U256,
    /// The accrued interest since the last event (the `balanceIncrease`
    /// field).
    pub balance_increase: U256,
    /// The asset's current index (collateral: `liquidity_index`; debt:
    /// `borrow_index`).
    pub index: U256,
    /// Optional pre-calculated scaled amount from the Pool contract (V4+).
    /// When `Some`, the processor uses it directly (no `ray_div`).
    pub scaled_amount: Option<U256>,
}

// ── the result types (ports `ScaledTokenMintResult` / `ScaledTokenBurnResult`) ──

/// The result of processing a scaled-token `Mint` event. Carries the signed
/// `balance_delta` to apply to the position + the `new_index` (the event's
/// index — the position's `last_index` is reconciled to it via max-with-prev
/// in the apply fn) + `is_repay` (a flow-classification hint the parser uses
/// to label the operation; the apply fn ignores it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScaledTokenMintResult {
    /// The signed delta to add to the position's `balance`. Positive for a
    /// true mint (deposit/borrow), negative for the interest-exceeds-value
    /// edge case (the Mint event is treated as an effective burn).
    pub balance_delta: I256,
    /// The event's `index` (the apply fn reconciles the position's
    /// `last_index` to this via max-with-prev).
    pub new_index: U256,
    /// `true` when the Mint is on the repay/withdrawal path (interest exceeds
    /// the deposit). Used by the parser's operation-classification; the apply
    /// fn ignores it. Mirrors `ScaledTokenMintResult.is_repay`.
    pub is_repay: bool,
}

/// The result of processing a scaled-token `Burn` event. Carries the signed
/// `balance_delta` (always negative — a burn decrements) + the `new_index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScaledTokenBurnResult {
    /// The signed delta (negative — a burn decrements the scaled balance).
    pub balance_delta: I256,
    /// The event's `index`.
    pub new_index: U256,
}

// ── the processors (ports `UnifiedCollateralProcessor` / `UnifiedDebtProcessor`) ──

/// Errors from the scaled-token processors.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProcessorError {
    /// A ray-math overflow or division by zero (from `degenbot-evm-math`).
    #[error("ray-math error: {0}")]
    RayMath(#[from] WadRayError),
    /// The computed `balance_delta` doesn't fit in `I256` (overflow). This
    /// surfaces only on pathological inputs; the apply layer clips to
    /// `I256::MAX`/`MIN` instead (the position-balance never legitimately
    /// exceeds `I256`).
    #[error("balance_delta overflow: {0}")]
    DeltaOverflow(String),
}

/// A collateral (aToken) or debt (vToken) processor — the strategy-bound
/// `process_mint_event` / `process_burn_event` pair. Stateless (mirrors the
/// Python `UnifiedCollateralProcessor` / `UnifiedDebtProcessor` which are
/// constructed per-revision but hold no mutable state).
#[derive(Debug, Clone, Copy)]
pub struct ScaledTokenProcessor {
    strategy: RoundingStrategy,
}

impl ScaledTokenProcessor {
    /// Construct a processor for an aToken (collateral) at `revision`.
    /// Mirrors `TokenProcessorFactory::get_collateral_processor`.
    #[must_use]
    pub fn collateral(revision: u32) -> Self {
        Self {
            strategy: collateral_strategy(revision),
        }
    }

    /// Construct a processor for a vToken (debt) at `revision`.
    /// Mirrors `TokenProcessorFactory::get_debt_processor`.
    #[must_use]
    pub fn debt(revision: u32) -> Self {
        Self {
            strategy: debt_strategy(revision),
        }
    }

    /// Process a collateral (aToken) `Mint` event. Port of
    /// `UnifiedCollateralProcessor::process_mint_event`. See the Python doc
    /// (`processor.py:75-130`) for the branch logic — the three arms:
    /// `balance_increase > value` (interest exceeds deposit → effective burn),
    /// `value >= balance_increase` (standard deposit), `value ==
    /// balance_increase` (pure interest accrual or matched deposit).
    ///
    /// # Errors
    ///
    /// Returns [`ProcessorError::RayMath`] on a `ray_div` overflow/div-zero,
    /// or [`ProcessorError::DeltaOverflow`] if the computed delta doesn't
    /// fit in `I256`.
    pub fn process_collateral_mint(
        &self,
        event: &ScaledTokenEventData,
    ) -> Result<ScaledTokenMintResult, ProcessorError> {
        if event.balance_increase > event.value {
            // Interest accrual exceeds deposit amount — emitted during withdraw
            // (the aToken contract mints the interest first, then burns the
            // withdrawal). Net effect: a burn of (balance_increase - value).
            let requested_amount = event.balance_increase - event.value;
            // Withdrawal-interest path uses HALF_UP (same for all revisions).
            let raw = degenbot_evm_math::ray_div(
                requested_amount,
                event.index,
                Some(RayRounding::HalfUp),
            )?;
            let balance_delta = negate_to_i256(raw)?;
            Ok(ScaledTokenMintResult {
                balance_delta,
                new_index: event.index,
                is_repay: true,
            })
        } else if event.value >= event.balance_increase {
            // Standard deposit — emitted in `_mintScaled` during supply.
            let balance_delta = if let Some(scaled) = event.scaled_amount {
                // Use pre-calculated scaled amount from Pool contract (V4+).
                to_i256(scaled)?
            } else {
                let requested_amount = event.value - event.balance_increase;
                let raw = degenbot_evm_math::ray_div(
                    requested_amount,
                    event.index,
                    self.strategy.mint.into(),
                )?;
                to_i256(raw)?
            };
            Ok(ScaledTokenMintResult {
                balance_delta,
                new_index: event.index,
                is_repay: false,
            })
        } else {
            // value == balance_increase: deposit equals accrued interest, OR
            // pure interest accrual (no deposit). If a scaled_amount is
            // provided from a matched SUPPLY event, it's a deposit; else pure
            // interest accrual (delta = 0, only the index updates).
            let balance_delta = match event.scaled_amount {
                Some(scaled) => to_i256(scaled)?,
                None => I256::ZERO,
            };
            Ok(ScaledTokenMintResult {
                balance_delta,
                new_index: event.index,
                is_repay: false,
            })
        }
    }

    /// Process a collateral (aToken) `Burn` event. Port of
    /// `UnifiedCollateralProcessor::process_burn_event`. Three arms: a
    /// pre-calculated `scaled_delta` (use directly), a pre-calculated
    /// `scaled_amount` (use directly), or the fallback `ray_div` path.
    ///
    /// # Errors
    ///
    /// Same as [`Self::process_collateral_mint`].
    pub fn process_collateral_burn(
        &self,
        event: &ScaledTokenEventData,
        scaled_delta: Option<U256>,
    ) -> Result<ScaledTokenBurnResult, ProcessorError> {
        if let Some(scaled) = scaled_delta {
            // Pre-calculated scaled amount from the withdraw amount (critical
            // for WITHDRAW with Mint event when interest > withdrawal).
            return Ok(ScaledTokenBurnResult {
                balance_delta: negate_to_i256(scaled)?,
                new_index: event.index,
            });
        }
        if let Some(scaled) = event.scaled_amount {
            // Pre-calculated scaled amount from Pool contract.
            return Ok(ScaledTokenBurnResult {
                balance_delta: negate_to_i256(scaled)?,
                new_index: event.index,
            });
        }
        // Fallback: calculate from event data. `amountToBurn = amount +
        // balanceIncrease`; `amountScaled = amountToBurn.rayDiv(index)`.
        let requested_amount = event.value + event.balance_increase;
        let raw =
            degenbot_evm_math::ray_div(requested_amount, event.index, self.strategy.burn.into())?;
        Ok(ScaledTokenBurnResult {
            balance_delta: negate_to_i256(raw)?,
            new_index: event.index,
        })
    }

    /// Process a debt (vToken) `Mint` event. Port of
    /// `UnifiedDebtProcessor::process_mint_event`. Two arms: `value >=
    /// balance_increase` (BORROW path) or `value < balance_increase` (REPAY
    /// path — the Mint is emitted when interest > repayment).
    ///
    /// # Errors
    ///
    /// Same as [`Self::process_collateral_mint`].
    pub fn process_debt_mint(
        &self,
        event: &ScaledTokenEventData,
    ) -> Result<ScaledTokenMintResult, ProcessorError> {
        if event.value >= event.balance_increase {
            // BORROW path: emitted in `_mintScaled`.
            let balance_delta = if let Some(scaled) = event.scaled_amount {
                to_i256(scaled)?
            } else {
                // Solidity: amountToMint = amount - balanceIncrease.
                let requested_amount = event.value - event.balance_increase;
                let raw = degenbot_evm_math::ray_div(
                    requested_amount,
                    event.index,
                    self.strategy.mint.into(),
                )?;
                to_i256(raw)?
            };
            Ok(ScaledTokenMintResult {
                balance_delta,
                new_index: event.index,
                is_repay: false,
            })
        } else {
            // REPAY path: emitted in `_burnScaled`. The Mint event fires when
            // interest > repayment amount; the net balance change is burning
            // the scaled repayment amount. `amount = balanceIncrease - value`.
            let amount_repaid = event.balance_increase - event.value;
            let raw =
                degenbot_evm_math::ray_div(amount_repaid, event.index, self.strategy.burn.into())?;
            Ok(ScaledTokenMintResult {
                balance_delta: negate_to_i256(raw)?,
                new_index: event.index,
                is_repay: true,
            })
        }
    }

    /// Process a debt (vToken) `Burn` event. Port of
    /// `UnifiedDebtProcessor::process_burn_event`. Two arms: a pre-calculated
    /// `scaled_delta` (use directly) or the fallback `ray_div` path.
    ///
    /// # Errors
    ///
    /// Same as [`Self::process_collateral_mint`].
    pub fn process_debt_burn(
        &self,
        event: &ScaledTokenEventData,
        scaled_delta: Option<U256>,
    ) -> Result<ScaledTokenBurnResult, ProcessorError> {
        if let Some(scaled) = scaled_delta {
            return Ok(ScaledTokenBurnResult {
                balance_delta: negate_to_i256(scaled)?,
                new_index: event.index,
            });
        }
        // Fallback: `amountToBurn = amount + balanceIncrease`.
        let requested_amount = event.value + event.balance_increase;
        let raw =
            degenbot_evm_math::ray_div(requested_amount, event.index, self.strategy.burn.into())?;
        Ok(ScaledTokenBurnResult {
            balance_delta: negate_to_i256(raw)?,
            new_index: event.index,
        })
    }
}

// ── U256 → I256 conversion helpers (thesigned-delta bridge) ──────────────

/// Convert a `U256` to a non-negative `I256`. Mirrors the Python's implicit
/// `int` (arbitrary precision, no overflow). Returns
/// [`ProcessorError::DeltaOverflow`] if the value exceeds `I256::MAX`.
fn to_i256(value: U256) -> Result<I256, ProcessorError> {
    I256::try_from(value)
        .map_err(|_| ProcessorError::DeltaOverflow(format!("U256 {value} exceeds I256::MAX")))
}

/// Negate a `U256` into a negative `I256` (a burn's delta). Mirrors the
/// Python's `-ray_div(...)` unary negate. Returns
/// [`ProcessorError::DeltaOverflow`] if the value exceeds `I256::MAX` (so
/// `-value` would underflow).
fn negate_to_i256(value: U256) -> Result<I256, ProcessorError> {
    let positive = to_i256(value)?;
    Ok(-positive)
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use degenbot_evm_math::RAY;

    /// V1 collateral mint, standard deposit: value=2 RAY, `balance_increase=0`,
    /// index=1 RAY → `balance_delta` = ray_div(2-0, 1, `HalfUp`) = 2 RAY.
    #[test]
    fn collateral_mint_standard_deposit() {
        let proc = ScaledTokenProcessor::collateral(1);
        let event = ScaledTokenEventData {
            value: U256::from(2u8) * RAY,
            balance_increase: U256::ZERO,
            index: RAY,
            scaled_amount: None,
        };
        let result = proc.process_collateral_mint(&event).unwrap();
        assert_eq!(
            result.balance_delta,
            I256::try_from(U256::from(2u8) * RAY).unwrap()
        );
        assert!(!result.is_repay);
        assert_eq!(result.new_index, RAY);
    }

    /// V4 collateral burn (CEIL rounding): value=10 RAY, `balance_increase=0`,
    /// index=3 RAY. Fallback path: amountToBurn = 10; `ray_div(10`, 3, Ceil) =
    /// 4 RAY (because 10*RAY*RAY % 3 != 0 → bump); negative delta.
    #[test]
    fn collateral_burn_v4_ceil_rounding() {
        let proc = ScaledTokenProcessor::collateral(4);
        let event = ScaledTokenEventData {
            value: U256::from(10u8) * RAY,
            balance_increase: U256::ZERO,
            index: U256::from(3u8) * RAY,
            scaled_amount: None,
        };
        let result = proc.process_collateral_burn(&event, None).unwrap();
        // ray_div_ceil(10*RAY, 3*RAY) — let me compute via the primitives.
        let expected_raw =
            degenbot_evm_math::ray_div_ceil(U256::from(10u8) * RAY, U256::from(3u8) * RAY).unwrap();
        assert_eq!(
            result.balance_delta,
            -I256::try_from(expected_raw).unwrap(),
            "V4 collateral burn uses CEIL rounding"
        );
    }

    /// V1 vs V4 collateral burn differ on rounding (`HALF_UP` vs CEIL).
    #[test]
    fn collateral_burn_v1_halfup_vs_v4_ceil() {
        let event = ScaledTokenEventData {
            value: U256::from(10u8) * RAY,
            balance_increase: U256::ZERO,
            index: U256::from(3u8) * RAY,
            scaled_amount: None,
        };
        let v1 = ScaledTokenProcessor::collateral(1)
            .process_collateral_burn(&event, None)
            .unwrap();
        let v4 = ScaledTokenProcessor::collateral(4)
            .process_collateral_burn(&event, None)
            .unwrap();
        assert_ne!(
            v1.balance_delta, v4.balance_delta,
            "V1 (HALF_UP) ≠ V4 (CEIL)"
        );
    }

    /// Collateral mint with interest exceeding deposit (effective burn):
    /// value=1 RAY, `balance_increase=3` RAY, index=1 RAY → delta =
    /// -ray_div(3-1, 1, `HalfUp`) = -2 RAY.
    #[test]
    fn collateral_mint_interest_exceeds_deposit() {
        let proc = ScaledTokenProcessor::collateral(1);
        let event = ScaledTokenEventData {
            value: RAY,
            balance_increase: U256::from(3u8) * RAY,
            index: RAY,
            scaled_amount: None,
        };
        let result = proc.process_collateral_mint(&event).unwrap();
        assert_eq!(
            result.balance_delta,
            -I256::try_from(U256::from(2u8) * RAY).unwrap()
        );
        assert!(
            result.is_repay,
            "interest-exceeds-deposit is the repay path"
        );
    }

    /// Debt mint (BORROW path) V4 uses CEIL rounding.
    #[test]
    fn debt_mint_v4_ceil() {
        let proc = ScaledTokenProcessor::debt(4);
        let event = ScaledTokenEventData {
            value: U256::from(10u8) * RAY,
            balance_increase: U256::ZERO,
            index: U256::from(3u8) * RAY,
            scaled_amount: None,
        };
        let result = proc.process_debt_mint(&event).unwrap();
        let expected_raw =
            degenbot_evm_math::ray_div_ceil(U256::from(10u8) * RAY, U256::from(3u8) * RAY).unwrap();
        assert_eq!(result.balance_delta, I256::try_from(expected_raw).unwrap());
        assert!(!result.is_repay);
    }

    /// Debt mint (REPAY path): value < `balance_increase` → negative delta.
    #[test]
    fn debt_mint_repay_path() {
        let proc = ScaledTokenProcessor::debt(1);
        let event = ScaledTokenEventData {
            value: RAY,
            balance_increase: U256::from(3u8) * RAY,
            index: RAY,
            scaled_amount: None,
        };
        let result = proc.process_debt_mint(&event).unwrap();
        // amount_repaid = 3-1 = 2 RAY; ray_div(2, 1, HalfUp) = 2 RAY; negative.
        assert_eq!(
            result.balance_delta,
            -I256::try_from(U256::from(2u8) * RAY).unwrap()
        );
        assert!(result.is_repay);
    }

    /// Pre-calculated `scaled_amount` short-circuits the `ray_div`.
    #[test]
    fn scaled_amount_short_circuits() {
        let proc = ScaledTokenProcessor::collateral(1);
        let event = ScaledTokenEventData {
            value: U256::from(10u8) * RAY,
            balance_increase: U256::ZERO,
            index: U256::from(3u8) * RAY,
            scaled_amount: Some(U256::from(999u16)), // arbitrary pre-computed value
        };
        let result = proc.process_collateral_mint(&event).unwrap();
        assert_eq!(
            result.balance_delta,
            I256::try_from(U256::from(999u16)).unwrap()
        );
    }

    /// Unknown revision defaults to V1/V3 (`HALF_UP` for all).
    #[test]
    fn unknown_revision_defaults_to_halfup() {
        let s = collateral_strategy(99);
        assert_eq!(s.mint, RayDivMode::HalfUp);
        assert_eq!(s.burn, RayDivMode::HalfUp);
    }
}
