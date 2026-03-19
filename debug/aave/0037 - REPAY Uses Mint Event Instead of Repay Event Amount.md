# Issue: REPAY Uses Mint Event Instead of Repay Event Amount

## Date
2026-03-19

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...). User ... scaled balance 
does not match contract balance at block 23125581
```

**Difference: 1 wei** (Python is 1 higher than contract)

**Affected Tokens**: Both GHO and non-GHO debt tokens during REPAY operations where interest exceeds repayment amount.

## Root Cause

When processing REPAY operations where interest exceeds repayment, the VariableDebtToken emits a `Mint` event instead of a `Burn` event. The enrichment layer calculates `scaled_amount` using mint rounding (ceil), but the contract actually uses burn rounding (floor) for the debt reduction.

**For GHO tokens specifically:** The `GhoV5Processor.process_mint_event()` method derives the repayment amount from the Mint event's `value` and `balance_increase` fields:

```python
# In GhoV5Processor.process_mint_event (lines 136-151)
amount_repaid = event_data.balance_increase - event_data.value  # 163379921396609913374
balance_delta = -wad_ray_math.ray_div_floor(
    a=amount_repaid,
    b=event_data.index,
)
```

However, the contract calculates the scaled burn amount using the **actual repay amount from the Repay event**, which is 1 wei different:

```solidity
// In BorrowLogic.executeRepay (Pool rev 9)
IVariableDebtToken(reserveCache.variableDebtTokenAddress).burn(
    params.onBehalfOf,
    amountToRepay,  // 163379921396609913375 (from Repay event)
    reserveCache.nextVariableBorrowIndex
);
```

The **1 wei discrepancy** between the Mint event derived amount and the Repay event amount causes the rounding error in the scaled balance calculation.

### Why the 1 Wei Difference Exists

The Mint event is emitted in `_burnScaled` when interest exceeds repayment:

```solidity
// In GHO VariableDebtToken._burnScaled
if (balanceIncrease > amount) {
    uint256 amountToMint = balanceIncrease - amount;
    emit Transfer(address(0), user, amountToMint);
    emit Mint(user, user, amountToMint, balanceIncrease, index);
}
```

The Mint event `value` field is `amountToMint = balanceIncrease - amount`, where `amount` is the actual repayment. However, due to **integer truncation during interest accrual**, the relationship is:

```
Repay event amount = Mint.balanceIncrease - Mint.value + 1 (sometimes)
```

This happens because:
1. Interest is calculated as: `scaledBalance * (newIndex - oldIndex) / RAY`
2. This is integer division (truncation), which can lose 1 wei
3. When the Mint event calculates `balanceIncrease - value`, it reverses this truncated calculation
4. The original `amount` (from Repay event) is the "true" value before truncation

## Transaction Details

| Field | Value |
|-------|-------|
| **Hash** | `0xf5267829bac23e68ab3223e5dea29d98ac736e08455e7e23c195d17143cb0fd2` |
| **Block** | 23125581 |
| **Type** | GHO_REPAY |
| **User** | `0x4E8ffddB1403CF5306C6c7B31dC72EF5f44BC4F5` |
| **Asset** | GHO (`0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f`) |
| **Pool** | `0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2` (rev 9) |
| **vGHO Token** | `0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B` (rev 5) |

### Event Analysis

| Event | Field | Value |
|-------|-------|-------|
| **Repay** | amount | 163379921396609913375 |
| **Mint** | value | 12097795423754548853 |
| **Mint** | balanceIncrease | 175477716820364462227 |
| **Mint** | index | 1144359184762906710085913096 |

**Derived from Mint**: `175477716820364462227 - 12097795423754548853 = 163379921396609913374`  
**Actual from Repay**: `163379921396609913375`  
**Difference**: `1 wei`

### Balance Calculation

| Metric | Value Before Fix | Value After Fix | Contract Value |
|--------|------------------|-----------------|----------------|
| Scaled Balance Before | 87,538,492,328,853,159,510,923 | 87,538,492,328,853,159,510,923 | 87,538,492,328,853,159,510,923 |
| Scaled Balance After | 87,395,722,538,063,688,395,023 | 87,395,722,538,063,688,395,022 | 87,395,722,538,063,688,395,022 |
| Actual Burn | 142,769,790,789,471,115,900 | 142,769,790,789,471,115,901 | 142,769,790,789,471,115,901 |

**Status**: ✅ FIXED - After the fix, the Python calculated balance matches the contract balance exactly.

**Verification**: 
```
AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True) 
successfully updated to block 23,125,581
```

## Fix

The fix involves three coordinated changes to ensure the Repay event amount is used correctly:

### 1. Enrichment Layer (`src/degenbot/aave/enrichment.py`)

**Change**: Add `GHO_DEBT_MINT` to the REPAY special case handling

**Lines 233-247**: Check for both `DEBT_MINT` and `GHO_DEBT_MINT` event types
```python
elif (
    operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY}
    and scaled_event.event_type in {
        ScaledTokenEventType.DEBT_MINT,
        ScaledTokenEventType.GHO_DEBT_MINT,
    }
    and scaled_event.balance_increase is not None
):
```

**Lines 288-294**: Preserve `scaled_amount` for REPAY operations
```python
# Do NOT set scaled_amount=None for REPAY/GHO_REPAY with DEBT_MINT/GHO_DEBT_MINT.
# The processing layer now uses the enriched scaled_amount directly.
if (
    calculation_event_type != scaled_event.event_type
    and not (
        operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY}
        and scaled_event.event_type
        in {ScaledTokenEventType.DEBT_MINT, ScaledTokenEventType.GHO_DEBT_MINT}
    )
):
```

### 2. Processing Layer (`src/degenbot/cli/aave.py`)

**Change**: Use enriched `scaled_amount` directly for GHO_REPAY operations

**Lines 3116-3129**: For GHO_REPAY, bypass processor and use enriched value
```python
if operation.operation_type == OperationType.GHO_REPAY:
    # Use enriched scaled_amount (calculated from Repay event in enrichment layer)
    assert enriched_event.scaled_amount is not None
    debt_position.balance -= enriched_event.scaled_amount
```

### 3. Validation Layer (`src/degenbot/aave/models.py`)

**Change**: Add special case validation for DEBT_MINT and GHO_DEBT_MINT with burn rounding

**Lines 239-254**: Accept burn rounding for REPAY operations with interest accrual
```python
if (
    event_type in {
        ScaledTokenEventType.DEBT_MINT,
        ScaledTokenEventType.GHO_DEBT_MINT,
    }
    and self.balance_increase is not None
    and self.balance_increase > 0
):
    expected_burn = token_math.get_debt_burn_scaled_amount(
        amount=raw, borrow_index=idx
    )
    if scaled == expected_burn:
        return self  # Validation passes with burn rounding
```

**Note**: This handles both GHO and non-GHO tokens during REPAY operations where interest exceeds repayment.

## Key Insight

**The Repay event amount is the "source of truth" for repayment operations.** The Mint event's fields are computed from internal contract state and may have 1 wei differences due to integer truncation during interest calculations. Always use the Pool event amount for consistency with on-chain calculations.

This affects both GHO and non-GHO tokens. The fix ensures all REPAY operations with interest exceeding repayment use the correct burn rounding.

## Alternative Solutions

### Option 1: Fix in Debt Processors (Not Recommended)
Modify `process_mint_event` in both GHO and standard debt processors to accept an optional `actual_repay_amount` parameter:

```python
def process_mint_event(..., actual_repay_amount: int | None = None):
    if actual_repay_amount is not None:
        amount_repaid = actual_repay_amount
    else:
        amount_repaid = event_data.balance_increase - event_data.value
```

**Cons**: Adds complexity to processor interfaces, creates inconsistency with how other event types are handled.

### Option 2: Use Tolerance in Verification (Already Implemented in #0035)
Add 1 wei tolerance to balance verification:

```python
assert abs(actual_scaled_balance - position.balance) <= 1, (...)
```

**Cons**: Masks the underlying issue, doesn't fix the calculation inconsistency.

### Option 3: Unified Processing (Recommended Architecture Change)
Refactor the processing logic to handle all debt mint events uniformly:

```python
# Extract repay amount from pool event once for all cases
if operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY}:
    repay_amount = extract_repay_amount(operation.pool_event)
    
# Then process based on token type
if is_gho:
    process_gho_repay_with_amount(repay_amount, ...)
else:
    process_standard_repay_with_amount(repay_amount, ...)
```

**Pros**: Consistent handling, easier to maintain, fixes root cause  
**Cons**: Requires refactoring existing code paths

## Refactoring

1. **Unify REPAY Processing**: Both GHO and non-GHO REPAY operations with Mint events now use the same logic. The processing layer bypasses processors and uses enriched scaled_amount directly. This should be documented as the standard pattern.

2. **Clarify Event Semantics**: Document that Mint event fields may differ from Repay event amounts by 1 wei due to integer truncation, and that Repay event amounts should be preferred for calculations.

3. **Add Regression Test**: Create test cases for both GHO and non-GHO tokens during REPAY operations where interest exceeds repayment to prevent future regressions.

## Related Issues

- Issue #0035: 1 Wei Rounding Error in vGHO Debt Position Verification (similar symptom, different root cause)
- Issue #0016: REPAY with Interest Exceeding Repayment Uses Wrong Rounding
- Issue #0031: REPAY Debt Mint Validation Rounding Error

## Files Referenced

- `src/degenbot/cli/aave.py` - Main processing logic (`_process_debt_mint_with_match`)
- `src/degenbot/aave/enrichment.py` - Event enrichment with operation-aware calculation
- `src/degenbot/aave/models.py` - Event validation with burn rounding special case
- `src/degenbot/aave/processors/debt/gho/v5.py` - GHO V5 processor (`process_mint_event`)
- `src/degenbot/aave/libraries/token_math.py` - TokenMath rounding implementations

---

*Report Generated: March 19, 2026*  
*Issue ID: 0037*  
*Updated: March 19, 2026 - Extended fix to handle both GHO and non-GHO tokens*
