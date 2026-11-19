//! Aave V3 scaled-token processors — the signed `balance_delta` for an aToken
//! or vToken `Mint`/`Burn` event.
//!
//! # One rounding precedence
//!
//! The scaled amount is computed once, outside this module: the call site takes
//! the Pool event's raw amount and divides it through the token revision's
//! on-chain `TokenMath` row ([`enricher_collateral_mint`],
//! [`enricher_collateral_burn`], [`enricher_debt_mint`],
//! [`enricher_debt_burn`]), then hands the result in as
//! [`ScaledTokenEventData::scaled_amount`]. A processor stamps the delta's sign
//! from the event's `value` against its `balance_increase`, and passes the
//! event's index through as the new index. Nothing here divides.
//!
//! The split is what keeps the port exact. A processor that re-derives the
//! scaled amount from `value ± balance_increase` is a *second* rounding source:
//! it disagrees with the Pool's own word by ±1 wei whenever the two round
//! differently — the emitted `value` is not `balance_increase ± raw_amount` to
//! the wei — and one wei per event accumulates until a final full repay
//! underflows the position's scaled balance. An event that reaches a processor
//! with no enriched amount is a caller contract violation and is refused
//! ([`ProcessorError::MissingScaledAmount`]), never re-derived.
//!
//! # The pieces
//!
//! [`ScaledTokenProcessor`] carries the four entry points (collateral/debt ×
//! mint/burn); [`RayDivMode`] is the `TokenMath` vocabulary the enricher
//! functions answer in. The results ([`ScaledTokenMintResult`] /
//! [`ScaledTokenBurnResult`]) carry the delta plus the event's index, which the
//! apply fn reconciles against the position's `last_index` by max-with-prev
//! ([`crate::run::apply_aave_chunk_writes_on_conn`]).
//!
//! The GHO vToken is not here: [`crate::gho_processor::UnifiedGhoProcessor`]
//! owns the discount surface plus the per-revision rounding pair it shares with
//! this module as [`RoundingStrategy`].

#![expect(clippy::doc_lazy_continuation)] // multi-line rustdoc phrasing

use crate::RayRounding;
use alloy::primitives::{I256, U256};

// ── the TokenMath rounding vocabulary ─────────────────────────────────────

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

/// A per-revision `(mint, burn)` `ray_div` rounding pair.
///
/// The shape a token's rounding is named in: the vToken `TokenMath` row
/// ([`debt_strategy`]) and the GHO vToken's own table
/// ([`crate::gho_processor::gho_strategy`], deliberately distinct at V4). The
/// processors never consult a pair — their rounding arrives inside the
/// enriched [`ScaledTokenEventData::scaled_amount`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundingStrategy {
    /// The `ray_div` rounding for the mint leg.
    pub mint: RayDivMode,
    /// The `ray_div` rounding for the burn leg.
    pub burn: RayDivMode,
}

// ── the enricher math (ports `libraries/token_math.py`) ────────────────────
//
// These four functions are the on-chain rounding the Pool applies to compute a
// scaled amount — `TokenMathFactory.get_token_math_for_token_revision`
// (`libraries/token_math.py:267-285`), `ExplicitRoundingMath` for rev ≥ 4 and
// `HalfUpRoundingMath` for rev ≤ 3. They are the only rounding source the
// scaled-token paths consult, and the rev-4 collateral-mint row (FLOOR) is the
// one that a naive "same rounding for both legs" table gets wrong: the Pool's
// `AToken` rev 9 uses `getATokenMintScaledAmount = rayDivFloor`
// (`contract_reference/aave/Pool/rev_9.sol:executeSupply:9312`), so a HALF_UP
// division there lands +1 wei above chain whenever
// `(raw_amount * RAY) % index > index // 2`.

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

/// The vToken's `TokenMath` row as a `(mint, burn)` pair — the same ceiling the
/// per-leg enrichers answer, named once so the GHO vToken's distinct V4 row
/// ([`crate::gho_processor::gho_strategy`]) has something to be compared
/// against.
#[must_use]
pub const fn debt_strategy(revision: u32) -> RoundingStrategy {
    RoundingStrategy {
        mint: enricher_debt_mint(revision),
        burn: enricher_debt_burn(revision),
    }
}

// ── the event data (ports `processors/base.py` event dataclasses) ──────────

/// The decoded fields of a collateral (aToken) or debt (vToken) `Mint`/`Burn`
/// event. The Pool contract emits these with `value` (the user-facing amount)
/// + `balance_increase` (the interest accrued since the last event) + `index`
/// (the asset's current `liquidity_index` for collateral / `borrow_index` for
/// debt) + an optional `scaled_amount` (the pre-calculated delta the Pool
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
    /// The enriched scaled amount — `raw_amount` divided by `index` at this
    /// token revision's `TokenMath` rounding ([`enricher_collateral_mint`] and
    /// its siblings). It is the delta's only rounding source, so the
    /// processors refuse an event that leaves it `None`
    /// ([`ProcessorError::MissingScaledAmount`]). `Option` because the type is
    /// shared with the GHO processor's own enrichment
    /// ([`crate::gho_processor::UnifiedGhoProcessor`]).
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
    /// The event carries no enriched scaled amount. The processors never
    /// re-derive one: a `value ± balance_increase` division is a second
    /// rounding source that diverges from the Pool's own word by ±1 wei.
    #[error(
        "event carries no enriched scaled amount — the TokenMath value is the only rounding source"
    )]
    MissingScaledAmount,
    /// The computed `balance_delta` doesn't fit in `I256` (overflow). This
    /// surfaces only on pathological inputs; the apply layer clips to
    /// `I256::MAX`/`MIN` instead (the position-balance never legitimately
    /// exceeds `I256`).
    #[error("balance_delta overflow: {0}")]
    DeltaOverflow(String),
}

/// The aToken/vToken processor pair — the four `Mint`/`Burn` delta entry
/// points. Stateless: the enriched scaled amount carries the rounding, so no
/// token revision or rounding table survives in the type. Mirrors the Python
/// `UnifiedCollateralProcessor` / `UnifiedDebtProcessor`, whose only remaining
/// difference is which leg a call site names.
#[derive(Debug, Clone, Copy)]
pub struct ScaledTokenProcessor;

impl ScaledTokenProcessor {
    /// The aToken (collateral) processor.
    #[must_use]
    pub const fn collateral() -> Self {
        Self
    }

    /// The vToken (debt) processor.
    #[must_use]
    pub const fn debt() -> Self {
        Self
    }

    /// Process a collateral (aToken) `Mint` event. Port of
    /// `UnifiedCollateralProcessor::process_mint_event`.
    ///
    /// `value` against `balance_increase` is the entire classification:
    ///
    /// - `value < balance_increase` — the interest accrued since the last
    ///   event exceeds the event's amount: the aToken contract mints the
    ///   interest first and the transaction burns the rest, so the net delta is
    ///   a burn of the enriched amount, flagged `is_repay`.
    /// - `value >= balance_increase` — the deposit side (`_mintScaled`), or a
    ///   matched deposit whose `value` equals its own interest: a mint of the
    ///   enriched amount.
    ///
    /// The magnitude is the enriched amount in both arms; every arm reports the
    /// event's index as `new_index`.
    ///
    /// # Errors
    ///
    /// [`ProcessorError::MissingScaledAmount`] if the event carries no enriched
    /// amount, or [`ProcessorError::DeltaOverflow`] if it exceeds `I256::MAX`.
    pub fn process_collateral_mint(
        &self,
        event: &ScaledTokenEventData,
    ) -> Result<ScaledTokenMintResult, ProcessorError> {
        mint_delta(event)
    }

    /// Process a collateral (aToken) `Burn` event. Port of
    /// `UnifiedCollateralProcessor::process_burn_event`.
    ///
    /// The delta is the negated enriched amount: `scaled_delta` when the caller
    /// already holds it (the withdraw-with-Mint-event amount), else the event's
    /// `scaled_amount`. Both routes are the same precedence, so which one a
    /// dispatch path uses cannot change the delta.
    ///
    /// # Errors
    ///
    /// Same as [`Self::process_collateral_mint`].
    pub fn process_collateral_burn(
        &self,
        event: &ScaledTokenEventData,
        scaled_delta: Option<U256>,
    ) -> Result<ScaledTokenBurnResult, ProcessorError> {
        burn_delta(event, scaled_delta)
    }

    /// Process a debt (vToken) `Mint` event. Port of
    /// `UnifiedDebtProcessor::process_mint_event`.
    ///
    /// - `value >= balance_increase` — the BORROW path (`_mintScaled`): a mint
    ///   of the enriched amount.
    /// - `value < balance_increase` — the REPAY path: the Mint fires when
    ///   interest exceeds the repayment, and its net effect is burning the
    ///   scaled repayment amount, flagged `is_repay`.
    ///
    /// # Errors
    ///
    /// Same as [`Self::process_collateral_mint`].
    pub fn process_debt_mint(
        &self,
        event: &ScaledTokenEventData,
    ) -> Result<ScaledTokenMintResult, ProcessorError> {
        mint_delta(event)
    }

    /// Process a debt (vToken) `Burn` event. Port of
    /// `UnifiedDebtProcessor::process_burn_event`.
    ///
    /// Same contract as [`Self::process_collateral_burn`]: the negated enriched
    /// amount, `scaled_delta` first, the event's `scaled_amount` otherwise.
    ///
    /// # Errors
    ///
    /// Same as [`Self::process_collateral_mint`].
    pub fn process_debt_burn(
        &self,
        event: &ScaledTokenEventData,
        scaled_delta: Option<U256>,
    ) -> Result<ScaledTokenBurnResult, ProcessorError> {
        burn_delta(event, scaled_delta)
    }
}

/// The one rounding precedence: an explicit `scaled_delta` override, else the
/// event's enriched `scaled_amount`. A caller that supplies neither gets an
/// error — the processor has no rounding table to fall back to.
fn enriched_scaled_amount(
    event: &ScaledTokenEventData,
    scaled_delta: Option<U256>,
) -> Result<U256, ProcessorError> {
    scaled_delta
        .or(event.scaled_amount)
        .ok_or(ProcessorError::MissingScaledAmount)
}

/// The shared `Mint` delta: the sign follows `value` against
/// `balance_increase`, the magnitude is the enriched amount.
fn mint_delta(event: &ScaledTokenEventData) -> Result<ScaledTokenMintResult, ProcessorError> {
    let scaled = enriched_scaled_amount(event, None)?;
    let is_repay = event.value < event.balance_increase;
    Ok(ScaledTokenMintResult {
        balance_delta: if is_repay {
            negate_to_i256(scaled)?
        } else {
            to_i256(scaled)?
        },
        new_index: event.index,
        is_repay,
    })
}

/// The shared `Burn` delta: always negative, the magnitude is the enriched
/// amount.
fn burn_delta(
    event: &ScaledTokenEventData,
    scaled_delta: Option<U256>,
) -> Result<ScaledTokenBurnResult, ProcessorError> {
    let scaled = enriched_scaled_amount(event, scaled_delta)?;
    Ok(ScaledTokenBurnResult {
        balance_delta: negate_to_i256(scaled)?,
        new_index: event.index,
    })
}

// ── U256 → I256 conversion helpers (the signed-delta bridge) ──────────────

/// Convert a `U256` to a non-negative `I256`. Every value fits `I256`, so the
/// only refusal is a magnitude past `I256::MAX`. Returns
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
    use crate::RAY;

    /// Build a scaled-token event with every field set independently.
    fn scaled_event(
        value: U256,
        balance_increase: U256,
        index: U256,
        scaled_amount: Option<U256>,
    ) -> ScaledTokenEventData {
        ScaledTokenEventData {
            value,
            balance_increase,
            index,
            scaled_amount,
        }
    }

    /// The `TokenMath` table the enricher consults — the only rounding source
    /// left standing: rev ≤ 3 divides `HALF_UP` on both legs, rev ≥ 4 is the
    /// `ExplicitRoundingMath` row (collateral mint FLOOR, collateral burn CEIL,
    /// debt mint CEIL, debt burn FLOOR).
    #[test]
    fn enricher_table_rounds_by_token_revision() {
        for revision in 0..=3u32 {
            assert_eq!(
                enricher_collateral_mint(revision),
                RayDivMode::HalfUp,
                "rev {revision} collateral mint"
            );
            assert_eq!(
                enricher_collateral_burn(revision),
                RayDivMode::HalfUp,
                "rev {revision} collateral burn"
            );
            assert_eq!(
                enricher_debt_mint(revision),
                RayDivMode::HalfUp,
                "rev {revision} debt mint"
            );
            assert_eq!(
                enricher_debt_burn(revision),
                RayDivMode::HalfUp,
                "rev {revision} debt burn"
            );
        }
        for revision in [4u32, 5, 9, 99] {
            assert_eq!(
                enricher_collateral_mint(revision),
                RayDivMode::Floor,
                "rev {revision} collateral mint divides FLOOR"
            );
            assert_eq!(
                enricher_collateral_burn(revision),
                RayDivMode::Ceil,
                "rev {revision} collateral burn divides CEIL"
            );
            assert_eq!(
                enricher_debt_mint(revision),
                RayDivMode::Ceil,
                "rev {revision} debt mint divides CEIL"
            );
            assert_eq!(
                enricher_debt_burn(revision),
                RayDivMode::Floor,
                "rev {revision} debt burn divides FLOOR"
            );
        }
    }

    /// All four entry points take the same precedence: the enriched amount is
    /// the delta's magnitude, whichever leg and whatever `value`,
    /// `balance_increase`, or `index` the event carries. The magnitude (999) is
    /// deliberately unreachable by a division of the event's own fields
    /// (`ray_div(10 RAY, 3 RAY) = 3.33 RAY` on the mint side,
    /// `ray_div(9 RAY, 3 RAY) = 3 RAY` on the burn side).
    #[test]
    fn every_processor_method_takes_the_enriched_scaled_amount() {
        let index = U256::from(3u8) * RAY;
        let enriched = U256::from(999u64);
        let enriched_i = I256::try_from(enriched).unwrap();
        let mint_side = scaled_event(U256::from(10u8) * RAY, U256::ZERO, index, Some(enriched));
        let burn_side = scaled_event(
            U256::from(7u8) * RAY,
            U256::from(2u8) * RAY,
            index,
            Some(enriched),
        );
        let cases = [
            (
                "collateral mint",
                ScaledTokenProcessor::collateral()
                    .process_collateral_mint(&mint_side)
                    .unwrap()
                    .balance_delta,
                enriched_i,
            ),
            (
                "debt mint",
                ScaledTokenProcessor::debt()
                    .process_debt_mint(&mint_side)
                    .unwrap()
                    .balance_delta,
                enriched_i,
            ),
            (
                "collateral burn",
                ScaledTokenProcessor::collateral()
                    .process_collateral_burn(&burn_side, None)
                    .unwrap()
                    .balance_delta,
                -enriched_i,
            ),
            (
                "debt burn",
                ScaledTokenProcessor::debt()
                    .process_debt_burn(&burn_side, None)
                    .unwrap()
                    .balance_delta,
                -enriched_i,
            ),
        ];
        for (leg, actual, expected) in cases {
            assert_eq!(actual, expected, "{leg}");
        }
    }

    /// The debt burn's delta is the enriched Pool word, never
    /// `value + balance_increase`: at the repay-with-Mint shape of block
    /// `23_093_579` (`repayAmount = 10_000_000`, `balance_increase =
    /// 461_000_000`, Mint `value` one wei above `balance_increase -
    /// repayAmount`) the enriched word floor-divides to `8_437_166` while the
    /// recompute (`912_000_001`) floor-divides to `769_469_578`.
    #[test]
    fn debt_burn_takes_the_enriched_scaled_amount() {
        let index = U256::from(1_185_232_043_555_857_500_018_371_098u128);
        let enriched = U256::from(8_437_166u64);
        let event = scaled_event(
            U256::from(451_000_001u64),
            U256::from(461_000_000u64),
            index,
            Some(enriched),
        );
        let recompute = crate::ray_div(
            event.value + event.balance_increase,
            index,
            Some(RayRounding::Floor),
        )
        .unwrap();
        assert_eq!(
            recompute,
            U256::from(769_469_578u64),
            "fixture: the value + balance_increase recompute"
        );
        let result = ScaledTokenProcessor::debt()
            .process_debt_burn(&event, None)
            .unwrap();
        assert_eq!(
            result.balance_delta,
            -I256::try_from(enriched).unwrap(),
            "the enriched scaled amount wins over the recompute"
        );
        assert_eq!(result.new_index, index);
    }

    /// The Mint side of the same repay: `balance_increase - value` is
    /// `9_999_999` (one wei short of the Pool word), and floor-dividing it lands
    /// one scaled wei below the Pool word's `8_437_166` — the divergence that
    /// accumulates until the final full repay underflows the scaled balance.
    #[test]
    fn debt_mint_repay_takes_the_enriched_scaled_amount() {
        let index = U256::from(1_185_232_043_555_857_500_018_371_098u128);
        let enriched = U256::from(8_437_166u64);
        let recompute = crate::ray_div(
            U256::from(10_000_000u64 - 1),
            index,
            Some(RayRounding::Floor),
        )
        .unwrap();
        assert_eq!(
            recompute,
            enriched - U256::ONE,
            "fixture: the balance_increase - value recompute lands one wei short"
        );
        let event = scaled_event(
            U256::from(451_000_001u64),
            U256::from(461_000_000u64),
            index,
            Some(enriched),
        );
        let result = ScaledTokenProcessor::debt()
            .process_debt_mint(&event)
            .unwrap();
        assert_eq!(
            result.balance_delta,
            -I256::try_from(enriched).unwrap(),
            "the enriched scaled amount wins over the recompute"
        );
        assert!(result.is_repay, "interest exceeds the repayment");
    }

    /// The collateral mint at position 173339 (block `23_089_231`): one wei of
    /// underlying at a 1.0483-ray liquidity index. The rev-4 `TokenMath` row
    /// floors it to 0 scaled wei; a `HALF_UP` division carries it to 1, which
    /// is the +1 wei of the documented divergence.
    #[test]
    fn collateral_mint_takes_the_floored_enriched_scaled_amount() {
        let index = U256::from(1_048_300_000_000_000_000_000_000_000u128);
        let raw = U256::ONE;
        let enriched = crate::ray_div(raw, index, Some(RayRounding::Floor)).unwrap();
        assert_eq!(enriched, U256::ZERO, "fixture: the FLOOR value is 0");
        assert_eq!(
            crate::ray_div(raw, index, Some(RayRounding::HalfUp)).unwrap(),
            U256::ONE,
            "fixture: a HALF_UP division carries the wei"
        );
        let event = scaled_event(raw, U256::ZERO, index, Some(enriched));
        let result = ScaledTokenProcessor::collateral()
            .process_collateral_mint(&event)
            .unwrap();
        assert_eq!(result.balance_delta, I256::ZERO);
        assert!(!result.is_repay);
    }

    /// The two ways to hand a burn its enriched amount — the `scaled_delta`
    /// override and the event's `scaled_amount` — are one precedence, so a
    /// dispatch path that passes `None` and one that passes the same value
    /// compute the identical delta. This is what makes either route exact.
    #[test]
    fn burn_delta_is_identical_via_the_override_and_the_event() {
        let index = U256::from(1_059_681_714_000_000_000_000_000_000u128);
        let enriched = U256::from(1_234_567u64);
        let event = scaled_event(
            U256::from(9u8) * RAY,
            U256::from(4u8) * RAY,
            index,
            Some(enriched),
        );
        let expected = -I256::try_from(enriched).unwrap();
        for (route, override_arg) in [("override", Some(enriched)), ("event", None)] {
            assert_eq!(
                ScaledTokenProcessor::collateral()
                    .process_collateral_burn(&event, override_arg)
                    .unwrap()
                    .balance_delta,
                expected,
                "collateral burn via {route}"
            );
            assert_eq!(
                ScaledTokenProcessor::debt()
                    .process_debt_burn(&event, override_arg)
                    .unwrap()
                    .balance_delta,
                expected,
                "debt burn via {route}"
            );
        }
    }

    /// An event with no enriched amount is refused by every entry point — the
    /// processors have no rounding table to fall back to, and a fallback is
    /// exactly the ±1 wei divergence. An explicit `scaled_delta` is enough on
    /// its own.
    #[test]
    fn a_missing_enriched_scaled_amount_is_refused() {
        let event = scaled_event(U256::from(5u8) * RAY, U256::ZERO, RAY, None);
        assert!(matches!(
            ScaledTokenProcessor::collateral().process_collateral_mint(&event),
            Err(ProcessorError::MissingScaledAmount)
        ));
        assert!(matches!(
            ScaledTokenProcessor::debt().process_debt_mint(&event),
            Err(ProcessorError::MissingScaledAmount)
        ));
        assert!(matches!(
            ScaledTokenProcessor::collateral().process_collateral_burn(&event, None),
            Err(ProcessorError::MissingScaledAmount)
        ));
        assert!(matches!(
            ScaledTokenProcessor::debt().process_debt_burn(&event, None),
            Err(ProcessorError::MissingScaledAmount)
        ));
        let expected = -I256::try_from(U256::from(77u64)).unwrap();
        assert_eq!(
            ScaledTokenProcessor::debt()
                .process_debt_burn(&event, Some(U256::from(77u64)))
                .unwrap()
                .balance_delta,
            expected,
            "the override alone supplies the enriched amount"
        );
    }

    /// `value` against `balance_increase` decides the sign and `is_repay` and
    /// nothing else: one wei either side of equality brackets the flip, and the
    /// magnitude stays the enriched amount on both sides.
    #[test]
    fn mint_sign_follows_the_interest_comparison() {
        let index = U256::from(3u8) * RAY;
        let interest = U256::from(5u8) * RAY;
        let enriched = U256::from(42u64);
        let magnitude = I256::try_from(enriched).unwrap();
        let cases = [
            (interest - U256::ONE, -magnitude, true),
            (interest, magnitude, false),
            (interest + U256::ONE, magnitude, false),
        ];
        for (value, expected_delta, expected_repay) in cases {
            let event = scaled_event(value, interest, index, Some(enriched));
            for (leg, result) in [
                (
                    "collateral",
                    ScaledTokenProcessor::collateral()
                        .process_collateral_mint(&event)
                        .unwrap(),
                ),
                (
                    "debt",
                    ScaledTokenProcessor::debt()
                        .process_debt_mint(&event)
                        .unwrap(),
                ),
            ] {
                assert_eq!(
                    result.balance_delta, expected_delta,
                    "{leg} value = {value}"
                );
                assert_eq!(result.is_repay, expected_repay, "{leg} value = {value}");
                assert_eq!(result.new_index, index, "{leg} value = {value}");
            }
        }
    }

    /// A pure accrual (`value == balance_increase`, enriched `0`) needs no
    /// usable index: nothing divides, so a zero index still passes through.
    #[test]
    fn accrual_needs_no_usable_index() {
        let interest = U256::from(7u8) * RAY;
        let event = scaled_event(interest, interest, U256::ZERO, Some(U256::ZERO));
        let collateral = ScaledTokenProcessor::collateral()
            .process_collateral_mint(&event)
            .unwrap();
        assert_eq!(collateral.balance_delta, I256::ZERO);
        assert!(!collateral.is_repay);
        assert_eq!(collateral.new_index, U256::ZERO);
        let debt = ScaledTokenProcessor::debt()
            .process_debt_mint(&event)
            .unwrap();
        assert_eq!(debt.balance_delta, I256::ZERO);
        assert!(!debt.is_repay);
    }

    /// An enriched amount past `I256::MAX` is a delta overflow, not a wrapped
    /// negative balance.
    #[test]
    fn oversized_enriched_amount_overflows() {
        let event = scaled_event(U256::ZERO, U256::ZERO, RAY, Some(U256::MAX));
        assert!(matches!(
            ScaledTokenProcessor::debt().process_debt_burn(&event, None),
            Err(ProcessorError::DeltaOverflow(_))
        ));
    }
}
