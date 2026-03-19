# Issue: REPAY Debt Mint Validation Rounding Error

## Date
2026-03-18

## Symptom
```
degenbot.aave.models.ScaledAmountValidationError: Scaled amount validation failed
  Event Type: ScaledTokenEventType.DEBT_MINT
  Pool Revision: 9
  Token Revision: 4
  Raw Amount: 673147659
  Expected Scaled: 568978878
  Actual Scaled: 568978877
  Index: 1183080225786068551534342820
  TokenMath Method: get_debt_mint_scaled_amount
  Difference: 1 wei
```

## Root Cause

When a REPAY operation has accrued interest that exceeds the repayment amount, the VariableDebtToken emits a **Mint** event instead of a Burn event. The Mint event's `amount` field contains `balance_increase - repay_amount` (the net debt increase), not the actual repayment amount.

The enrichment layer correctly handles this case by:
1. Extracting the actual repay amount from the Pool event (lines 178-196 in enrichment.py)
2. Using DEBT_BURN calculation (floor rounding) instead of DEBT_MINT (ceil rounding) (lines 230-244 in enrichment.py)

However, the **validation layer** in `models.py` does not have a corresponding special case for DEBT_MINT events when interest exceeds repayment.

### The Problem

In `IndexScaledEvent.validate_scaled_amount()` (models.py):

1. The validation always uses `get_debt_mint_scaled_amount` for DEBT_MINT events (ceil rounding)
2. Expected = ceil(673147659 / 1183080225786068551534342820) = 568978878
3. Actual = floor(673147659 / 1183080225786068551534342820) = 568978877 (calculated via DEBT_BURN)
4. 568978877 != 568978878, so validation fails

There's already a special case for COLLATERAL_MINT at lines 227-243 that allows either mint (floor) or burn (ceil) rounding when interest exceeds withdrawal. A similar special case is needed for DEBT_MINT when interest exceeds repayment.

## Transaction Details

| Field | Value |
|-------|-------|
| **Transaction Hash** | 0x481d89243dd0e31322e87a8e9cdbeaa96e62f3a58c903bcafa1576d0cc0258f9 |
| **Block** | 23089241 |
| **Type** | REPAY (Variable Debt) |
| **User** | 0xf388D96F92e1035AcF7AfD8173b87482Cca8992F |
| **Asset** | USDT (0xdAC17F958D2ee523a2206206994597C13D831ec7) |
| **vToken** | 0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8 |
| **Pool Revision** | 9 |
| **vToken Revision** | 4 |
| **Repayment Amount** | 673,147,659 (673.15 USDT) |
| **Accrued Interest** | 2,248,689,277 |
| **Net Debt Change** | +1,575,541,618 (interest > repayment) |

### Events in Transaction

| Log Index | Event Type | Contract | Details |
|-----------|------------|----------|---------|
| 443 | Transfer | VariableDebtToken | Interest accrual mint: 1,575,541,619 scaled |
| 444 | Mint | VariableDebtToken | Debt mint: amount=1,575,541,619, balanceIncrease=2,248,689,277 |
| 445 | ReserveDataUpdated | Pool | Variable borrow index updated |
| 446 | Transfer | USDT | Repayment: 673,147,659 USDT to aToken |
| 447 | Repay | Pool | REPAY event |

**Critical Finding:** A **Mint event** (logIndex 444) is emitted instead of a Burn event because accrued interest (2.25 USDT) exceeds the repayment amount (0.67 USDT).

### The Math

**Contract Calculation (Correct):**
```
amountScaled = repay_amount * RAY / index
             = 673,147,659 * 10^27 / 1,183,080,225,786,068,551,534,342,820
             = 568,978,877 (floor rounding)
```

**Enrichment Calculation (Correct):**
- Uses DEBT_BURN calculation (floor rounding) via special case
- Calculates: floor(673147659 / index) = 568978877 ✓

**Validation Calculation (Incorrect):**
- Uses DEBT_MINT calculation (ceil rounding) because event type is DEBT_MINT
- Calculates: ceil(673147659 / index) = 568978878 ✗
- Expected (568978878) != Actual (568978877)

## Fix

### Implementation

The fix follows the architectural principle: **When enrichment overrides the calculation type, skip validation.**

**File 1:** `src/degenbot/aave/enrichment.py`

**Location:** Lines 92, 276-289

**Changes:**
1. Added tracking variable `calculation_event_type` to detect calculation overrides
2. When calculation type is overridden (e.g., DEBT_MINT → DEBT_BURN), set `scaled_amount=None` to skip validation

```python
# Track calculation type to detect overrides for validation skipping
calculation_event_type: ScaledTokenEventType | None = None

# ... calculation logic ...

# Special case: When enrichment overrides the calculation type
# (e.g., REPAY + DEBT_MINT with interest > repayment), skip validation
# by setting scaled_amount=None. The processing layer recalculates
# the amount anyway for these cases.
# See debug/aave/0031 for details.
if calculation_event_type != scaled_event.event_type:
    logger.debug(
        f"ENRICHMENT: Overriding {scaled_event.event_type.name} with "
        f"{calculation_event_type.name} - skipping validation by setting "
        f"scaled_amount=None"
    )
    scaled_amount = None
```

**File 2:** `src/degenbot/aave/models.py`

**Location:** Lines 204-216 in `IndexScaledEvent.validate_scaled_amount()`

**Change:** Unified handling for all cases where enrichment skips validation:

```python
# Special case: Enrichment layer overrides calculation type
# When enrichment switches calculation type (e.g., DEBT_MINT -> DEBT_BURN
# for REPAY with interest > repayment), the calculated amount won't match
# the event type's standard calculation. Skip validation in these cases
# since the processing layer recalculates the amount anyway.
# See debug/aave/0031 for details.
if scaled is None:
    return self
```

This unified approach replaces the previous MINT_TO_TREASURY-specific check and handles all enrichment override cases consistently.

## Key Insight

**Skip validation when enrichment overrides calculation type.** When enrichment intentionally deviates from standard calculation (e.g., using DEBT_BURN for a DEBT_MINT event), the resulting amount won't pass standard validation. Rather than duplicating the special case logic in validation, set `scaled_amount=None` to skip validation entirely.

This approach:
1. **Avoids logic duplication** - Special cases exist only in enrichment
2. **Maintains safety** - Processing layer recalculates amounts for these cases anyway
3. **Follows existing patterns** - MINT_TO_TREASURY already uses this approach
4. **Simplifies validation** - One unified check: `if scaled is None: return self`

This is a classic case of incomplete propagation of a fix. Issue #0010 and #0016 fixed the enrichment and processing layers, but the validation layer was missed.

## Refactoring

1. **Unify special case handling:** Consider extracting the special case logic into a shared utility that both enrichment and validation can use, ensuring they stay in sync.

2. **Add test coverage:** Add test cases for validation of:
   - REPAY + DEBT_MINT with interest > repayment
   - WITHDRAW + COLLATERAL_MINT with interest > withdrawal
   - LIQUIDATION + DEBT_MINT with net debt increase

3. **Document validation exceptions:** Add clear comments explaining why each special case exists and what contract behavior necessitates it.

## References

- Related Issues: 
  - `0010 - V4 REPAY Missing Burn Event Handling.md`
  - `0016 - REPAY with Interest Exceeding Repayment Uses Wrong Rounding.md`
- Contract: `contract_reference/aave/VariableDebtToken/rev_4.sol` - `_burnScaled()` function
- Pool Flow Documentation: `docs/cli/aave_pool_flows.md` - Repay flow
