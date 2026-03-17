# Issue: REPAY with Interest Exceeding Repayment Uses Wrong Rounding

## Date
2026-03-16

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...). User AaveV3User(...) scaled balance (17721819044852) does not match contract balance (17722426994605) at block 23001115
```

## Root Cause

When a REPAY operation has accrued interest that exceeds the repayment amount, the VariableDebtToken emits a **Mint** event instead of a Burn event. The Mint event's `amount` field contains `balance_increase - repay_amount` (the net debt increase), not the actual repayment amount.

The enrichment layer (`src/degenbot/aave/enrichment.py`) has special handling for WITHDRAW operations with COLLATERAL_MINT when interest exceeds withdrawal amount (lines 195-206), which correctly switches the calculation from COLLATERAL_MINT to COLLATERAL_BURN to use the correct rounding.

**However, there is NO equivalent special handling for REPAY + DEBT_MINT when interest > repayment.**

The code calculates `scaled_amount` using DEBT_MINT calculation (which uses **ceil rounding** via `get_debt_mint_scaled_amount`), but it should use DEBT_BURN calculation (which uses **floor rounding** via `get_debt_burn_scaled_amount`).

Additionally, the V1 processors (used for token revisions 1-3) **ignore the `scaled_delta` parameter**, recalculating from event data instead. This causes double-scaling when synthetic events are created.

Furthermore, the REPAY handler was using `pool_revision` (8) to select TokenMath, but vToken revision 3 uses V1 (half-up rounding), not V5 (floor rounding).

For TokenMath versions:
- V1 (token revisions 1-3): Uses half-up rounding
- V4 (token revision 4): Uses explicit floor/ceil
- V5 (token revisions 5+): Uses explicit floor/ceil

These issues compound to cause balance mismatches.

## Verification

**Status**: ✅ **FIXED**

**Test Results**:
- Block 23001115: ✅ Passed (original failing block)
- Blocks 23001115-23001125: ✅ 10 blocks passed
- Blocks 23001115-23001225: ✅ 100 blocks passed

**Balance Verification**:
- Before: 17722454717512
- After: 17722426994605
- Expected burn: 27722907
- Actual burn: 27722907 ✅
- Difference: 0 wei ✅

## Transaction Details

| Field | Value |
|-------|-------|
| **Transaction Hash** | 0x5d5ef017c0e052a1188d342bb1a45166971941347e9704fe044ef5f9cff35692 |
| **Block** | 23001115 |
| **Type** | REPAY (Variable Debt) |
| **User** | 0x5130985cE6A0e54f369712Cd6f2fDEC084026E54 |
| **Asset** | USDC (0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48) |
| **vToken** | 0x72E95b8931767C79bA4EeE721354d6E99a61D004 |
| **Pool Revision** | 8 |
| **vToken Revision** | 3 |
| **Repayment Amount** | 32,797,700 |
| **Accrued Interest** | 724,312,271 |
| **Net Debt Change** | +691,514,571 (interest > repayment) |

### Event Sequence

1. **ERC20 Transfer** (logIndex 245): Mint of 691,514,571 debt tokens (informational)
2. **Mint Event** (logIndex 246): value=691,514,571, balanceIncrease=724,312,271, index=1,183,054,148,573,171,154,846,304,432
3. **ReserveDataUpdated** (logIndex 247): Updates borrow index
4. **ERC20 Transfer** (logIndex 248): Transfer 32,797,700 USDC from user to aToken
5. **Repay Event** (logIndex 249): Confirms 32,797,700 repayment

### The Math

**Contract Calculation (Correct):**
```
amountScaled = repay_amount * RAY / index
             = 32,797,700 * 10^27 / 1,183,054,148,573,171,154,846,304,432
             = 27,722,906 (floor rounding)

newScaledBalance = 17,722,454,717,512 - 27,722,906
                 = 17,722,426,994,606 ≈ 17,722,426,994,605
```

**Current Code Calculation (Incorrect):**
- Enrichment uses DEBT_MINT calculation (ceil rounding) on the net mint amount
- This results in an incorrect scaled amount
- The difference accumulates to 607,949,753 over the transaction

## Fix

Three changes were required to fix this issue:

### Fix 1: Enrichment Layer (`src/degenbot/aave/enrichment.py`)

**Location**: Lines 174-186 (raw_amount extraction) and lines 208-218 (calculation_event_type)

**Changes**:
1. Added special case extraction for REPAY + DEBT_MINT (lines 174-186):
```python
elif (
    # Special case: When interest exceeds repayment, the VariableDebtToken emits
    # a Mint event with amount = balance_increase - repay_amount (net debt increase).
    # But we need the actual repay amount to calculate the scaled burn.
    # Detection: In a REPAY operation, if DEBT_MINT is emitted, interest > repayment.
    operation.operation_type.name in {"REPAY", "GHO_REPAY"}
    and scaled_event.event_type == ScaledTokenEventType.DEBT_MINT
    and scaled_event.balance_increase is not None
):
    # Interest exceeds repayment - extract the actual repay amount
    extractor = RawAmountExtractor(
        pool_event=operation.pool_event,
        pool_revision=self.pool_revision,
    )
    raw_amount = extractor.extract()
```

2. Added calculation_event_type override (lines 208-218):
```python
elif (
    # Special case: When interest exceeds repayment amount, the VariableDebtToken
    # emits a Mint event instead of a Burn event (VariableDebtToken _burnScaled).
    # In this case, use DEBT_BURN calculation (floor rounding) instead of
    # DEBT_MINT (ceil rounding) to match contract behavior.
    operation.operation_type.name in {"REPAY", "GHO_REPAY"}
    and scaled_event.event_type == ScaledTokenEventType.DEBT_MINT
    and scaled_event.balance_increase is not None
):
    # Use DEBT_BURN for burn rounding (floor)
    calculation_event_type = ScaledTokenEventType.DEBT_BURN
```

### Fix 2: V1 Processor (`src/degenbot/aave/processors/debt/v1.py`)

**Location**: Lines 119-175 (`process_burn_event` method)

**Change**: Added support for `scaled_delta` parameter (similar to V4/V5):
```python
def process_burn_event(
    self,
    event_data: DebtBurnEvent,
    previous_balance: int,
    previous_index: int,
    scaled_delta: int | None = None,  # Now used instead of ignored
) -> ScaledTokenBurnResult:
    if scaled_delta is not None:
        # Use pre-calculated scaled amount from paybackAmount
        return ScaledTokenBurnResult(
            balance_delta=-scaled_delta,
            new_index=event_data.index,
        )
    # ... rest of method unchanged
```

### Fix 3: REPAY Handler (`src/degenbot/cli/aave.py`)

**Location**: Lines 3093-3104 (`_process_debt_mint_with_match`)

**Change**: Use token revision instead of pool revision for TokenMath:
```python
# OLD (incorrect):
token_math = TokenMathFactory.get_token_math(operation.pool_revision)

# NEW (correct):
token_math = TokenMathFactory.get_token_math_for_token_revision(
    debt_asset.v_token_revision
)
```

**Critical**: The vToken revision (3) uses half-up rounding (TokenMathV1), while the Pool revision (8) uses floor rounding (TokenMathV5). Using the wrong TokenMath caused a 1 wei rounding error.

### Fix 4: Collateral V1 Processor (`src/degenbot/aave/processors/collateral/v1.py`)

**Location**: Lines 89-124 (`process_burn_event` method)

**Change**: Added support for `scaled_delta` parameter (similar to debt processor fix):
```python
def process_burn_event(
    self,
    event_data: CollateralBurnEvent,
    previous_balance: int,
    previous_index: int,
    scaled_delta: int | None = None,  # Now used instead of ignored
) -> ScaledTokenBurnResult:
    if scaled_delta is not None:
        # Use pre-calculated scaled amount from withdraw amount
        # This is critical for WITHDRAW with Mint event when interest > withdrawal
        return ScaledTokenBurnResult(
            balance_delta=-scaled_delta,
            new_index=event_data.index,
        )
    # ... rest of method unchanged
```

### Fix 5: Event Handler (`src/degenbot/cli/aave.py`)

**Location**: Lines 2035-2051 (`_process_scaled_token_operation` for CollateralBurnEvent)

**Change**: Pass `scaled_delta` parameter to `process_burn_event`:
```python
case CollateralBurnEvent():
    # ...
    burn_result: ScaledTokenBurnResult = collateral_processor.process_burn_event(
        event_data=event,
        previous_balance=position.balance,
        previous_index=position.last_index or 0,
        scaled_delta=event.scaled_amount,  # Added this parameter
    )
```

### Fix 6: MINT_TO_TREASURY Validation (`src/degenbot/aave/models.py`)

**Location**: Lines 204-212 (`validate_scaled_amount` in `IndexScaledEvent`)

**Change**: Extended MINT_TO_TREASURY validation skip to all pool revisions:
```python
# OLD (only pool revision >= 9):
if event_type == ScaledTokenEventType.COLLATERAL_MINT and pool_rev >= 9:
    return self

# NEW (all revisions when scaled_amount is None):
if event_type == ScaledTokenEventType.COLLATERAL_MINT and scaled is None:
    return self
```

**Rationale**: The enrichment layer sets `scaled_amount = None` for MINT_TO_TREASURY operations because the calculation requires position data (user balance and last_index) that's only available during processing. The previous fix only skipped validation for pool revision >= 9, but later blocks with pool revision 8 failed validation.

## Key Insight

**Event names can be misleading.** A Mint event on a debt token during repayment is actually representing a **burn** of scaled tokens when the repayment amount is less than accrued interest. The Mint event's `amount` field shows the net position change (interest - repayment), but the actual scaled token operation is burning `repay_amount / index` tokens.

This is analogous to the existing fix for WITHDRAW operations (Issue 0012), where interest exceeding withdrawal causes a Mint event but requires Burn calculation.

## Refactoring

1. **Consolidate Special Cases**: Both WITHDRAW+COLLATERAL_MINT and REPAY+DEBT_MINT special cases should follow the same pattern - when interest exceeds the operation amount, treat the Mint event as a Burn event for calculation purposes.

2. **Early Amount Extraction**: Consider extracting the raw amount from pool events before the calculation type determination, so special cases can use the correct amount directly.

3. **Documentation**: Add clear comments explaining that Mint events during repayment/withdrawal (when interest > amount) are informational only and the actual operation is a burn of scaled tokens.

4. **Test Coverage**: Add test cases for REPAY operations where:
   - Interest > repayment amount (should use DEBT_BURN calculation)
   - Interest < repayment amount (should use normal DEBT_BURN from Burn event)

## References

- Contract: `contract_reference/aave/VariableDebtToken/rev_3.sol` - `_burnScaled()` function
- Related Issues: `0012 - V4 Withdraw Emits Mint When Interest Exceeds Withdrawal.md`
- Pool Flow Documentation: `docs/cli/aave_pool_flows.md` - Repay flow
