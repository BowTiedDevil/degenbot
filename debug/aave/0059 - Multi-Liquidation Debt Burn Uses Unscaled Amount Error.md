# Issue 0059: Multi-Liquidation Debt Burn Uses Unscaled Amount Instead of Scaled

**Date:** 2026-03-25

## Symptom

Balance verification failure at block 21762809:
```
AssertionError: Debt balance verification failure for AaveV3Asset(..., symbol='USDT').
User AaveV3User(..., address='0xC5BD7138680fAA7bF3E6415944EecC074CeD419f')
scaled balance (367261936148) does not match contract balance (433198067802) at block 21762809
```

The calculated balance was approximately **65,936,131,654** less than the actual balance (a ~15% discrepancy).

## Transaction Details

- **Transaction Hash:** 0x1c1984a4ae9f9e056dfc2751e68dcb7d5a02dbd15450624d07eaca768fb08026
- **Block:** 21762809
- **Market:** Aave Ethereum Market (Pool revision 6)
- **User:** 0xC5BD7138680fAA7bF3E6415944EecC074CeD419f
- **Operation:** Multi-liquidation (LINK and AAVE collateral, USDT debt)
- **Next Issue ID:** 0059

### Liquidation Structure

This transaction contains **TWO liquidations** of user `0xC5BD7138680fAA7bF3E6415944EecC074CeD419f`:

| Operation | Collateral | Debt Asset | debtToCover | Burn Event LogIndex |
|-----------|------------|------------|-------------|---------------------|
| 0 | LINK | USDT | 136,867,301 | 66 |
| 1 | AAVE | USDT | 498,998,889,124 | 79 |
| **Total** | - | USDT | **499,135,756,425** | - |

### Key Events in Transaction

| LogIndex | Event | Contract | Value | Notes |
|----------|-------|----------|-------|-------|
| 66 | **Burn** | variableDebtEthUSDT | value=135,280,454, balanceIncrease=1,586,847 | **Debt burn 1** |
| 78 | LiquidationCall | Aave Pool | debtToCover=136,867,301 | LINK liquidation |
| 79 | **Burn** | variableDebtEthUSDT | value=498,998,889,124, balanceIncrease=0 | **Debt burn 2** |
| 89 | LiquidationCall | Aave Pool | debtToCover=498,998,889,124 | AAVE liquidation |

### On-Chain Verification

```bash
# Pre-transaction vToken scaled balance (block 21762808)
cast call 0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8 "scaledBalanceOf(address)" \
  0xC5BD7138680fAA7bF3E6415944EecC074CeD419f --block 21762808
# Result: 866396105726

# Post-transaction vToken scaled balance (block 21762809)
cast call 0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8 "scaledBalanceOf(address)" \
  0xC5BD7138680fAA7bF3E6415944EecC074CeD419f --block 21762809
# Result: 433198067802
```

**Actual contract behavior:**
- Initial balance: 866,396,105,726 (scaled)
- Final balance: 433,198,067,802 (scaled)
- Actual burn: 433,198,037,924 (scaled)

## Root Cause

In `_process_debt_burn_with_match`, the code for **multi-liquidations** directly uses `burn_value = scaled_event.amount` as the delta to subtract from the position balance. However, this is the **unscaled amount** (in underlying units), not the **scaled amount** needed for the balance calculation.

### Current Code (Bug)

```python
if liquidation_count > 1:
    # Multi-liquidation: process each burn event individually
    # Don't skip - each liquidation has its own burn event
    # Multi-liquidation scenario: use burn event value directly
    # The contract emits individual burn events for each liquidation
    # with SCALED amounts already calculated.  # <-- THIS COMMENT IS WRONG!
    burn_value = scaled_event.amount  # BUG: This is unscaled, not scaled!
    scaled_amount = burn_value
```

### Why This Fails

**The contract behavior (VariableDebtToken rev 1):**

From `contract_reference/aave/VariableDebtToken/rev_1.sol` lines 2662-2686:
```solidity
function _burnScaled(address user, address target, uint256 amount, uint256 index) internal {
    uint256 amountScaled = amount.rayDiv(index);  // Convert underlying to scaled
    // ...
    _burn(user, amountScaled.toUint128());  // Burn scaled amount
    
    uint256 amountToBurn = amount - balanceIncrease;  // Principal in underlying
    emit Burn(user, target, amountToBurn, balanceIncrease, index);  // Event has underlying amount!
}
```

The Burn event emits `amountToBurn` which is in **underlying units**, not scaled units.

**The math:**

For multi-liquidations with USDT (vToken revision 1, using HalfUpRoundingMath):
- Burn 1: value=135,280,454, balance_increase=1,586,847
- Burn 2: value=498,998,889,124, balance_increase=0
- Index: 1,152,211,489,270,939,624,346,028,314 (1.152e27)

**Current calculation (WRONG):**
```
Final balance = 866,396,105,726 - 135,280,454 - 498,998,889,124 = 367,261,936,148
```

**Correct calculation:**
```python
# Convert underlying to scaled using ray_div (half-up for rev 1)
debt_to_cover_1 = 135,280,454 + 1,586,847 = 136,867,301
scaled_burn_1 = ray_div(136,867,301, index) = 118,788,760

debt_to_cover_2 = 498,998,889,124 + 0 = 498,998,889,124
scaled_burn_2 = ray_div(498,998,889,124, index) = 433,079,249,164

Final balance = 866,396,105,726 - 118,788,760 - 433,079,249,164 = 433,198,067,802
```

**Contract balance:** 433,198,067,802 ✓

## The Fix

Multi-liquidations should calculate the scaled burn amount using TokenMath, just like single liquidations do:

```python
if liquidation_count > 1:
    # Multi-liquidation: calculate scaled burn from unscaled amount
    # The Burn event's value field is the principal burned, but the actual
    # balance reduction is value + balance_increase (which equals debtToCover).
    # We must convert the unscaled debtToCover to scaled amount using TokenMath.
    debt_to_cover = scaled_event.amount + (scaled_event.balance_increase or 0)
    
    token_math = TokenMathFactory.get_token_math_for_token_revision(
        debt_asset.v_token_revision
    )
    burn_value = token_math.get_debt_burn_scaled_amount(
        debt_to_cover, scaled_event.index
    )
    scaled_amount = burn_value
```

## Fix Details

**Files Modified:**
1. `src/degenbot/cli/aave.py` - Modify multi-liquidation burn calculation in `_process_debt_burn_with_match`

**Functions Modified:**
1. `_process_debt_burn_with_match` - MODIFIED: Use TokenMath to calculate scaled burn amount for multi-liquidations

## Verification

```bash
uv run degenbot aave update
```

**Block 21762809 Processing:**
- **Multi-liquidation detected:** 2 operations with same debt asset
- **Burn 1 scaled:** ray_div(136,867,301, index) = 118,788,760
- **Burn 2 scaled:** ray_div(498,998,889,124, index) = 433,079,249,164
- **Starting balance:** 866,396,105,726
- **Final balance:** 866,396,105,726 - 118,788,760 - 433,079,249,164 = 433,198,067,802
- **Contract balance:** 433,198,067,802
- **Result:** MATCH ✓

## Expected Results

**Transaction 0x1c1984a4ae9f9e056dfc2751e68dcb7d5a02dbd15450624d07eaca768fb08026:**

- **Liquidation 1:** debtToCover=136,867,301 USDT (unscaled) → scaled burn=118,788,760
- **Liquidation 2:** debtToCover=498,998,889,124 USDT (unscaled) → scaled burn=433,079,249,164
- **Total scaled reduction:** 433,198,037,924

**Balance calculation:**
- Starting: 866,396,105,726
- Scaled burn 1: -118,788,760
- Scaled burn 2: -433,079,249,164
- **Final:** 433,198,067,802 (matches contract exactly)

## Key Insight

**The Burn event structure:**

For Aave V3 VariableDebtToken burn events:
- `value` = Principal debt burned (in **underlying units**)
- `balanceIncrease` = Accrued interest (in **underlying units**)
- `value + balanceIncrease` = Total debt reduction (equals debtToCover from LiquidationCall)

**The scaled balance calculation:**
- Scaled amounts are used for position balance tracking
- Unscaled amounts = scaled amounts × index / 1e27
- To get scaled from unscaled: `scaled = ray_div(unscaled, index)`

**Multi-liquidation vs Single liquidation:**
- Single liquidation fix (0058) correctly uses TokenMath for conversion
- Multi-liquidation code path was using unscaled amount directly
- Both paths should use TokenMath to convert underlying to scaled

## Related Issues

- Issue 0057: Multi-Liquidation Rounding Error in Debt Burn Aggregation (aggregation logic)
- Issue 0058: Single Liquidation Debt Burn Uses Unscaled Amount Error (similar fix for single liquidations)
- Issue 0056: Multi-Liquidation Single Debt Burn Misassignment (architectural foundation)

## References

- Transaction: 0x1c1984a4ae9f9e056dfc2751e68dcb7d5a02dbd15450624d07eaca768fb08026
- Block: 21762809
- User: 0xC5BD7138680fAA7bF3E6415944EecC074CeD419f
- Debt Asset: USDT (0xdAC17F958D2ee523a2206206994597C13D831ec7)
- Debt Token: variableDebtEthUSDT (0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8)
- Collateral Assets: LINK, AAVE
- Liquidator: 0x03BD055aaa45286465e668Ad22Adc0320Ca00003
- Contract: VariableDebtToken revision 1
- Pool: Pool revision 6
- Files:
  - `src/degenbot/cli/aave.py` (lines 3627-3642)
  - `src/degenbot/aave/libraries/token_math.py`
  - `contract_reference/aave/VariableDebtToken/rev_1.sol` (lines 2662-2686)

## Summary

The failure occurs because the multi-liquidation code path in `_process_debt_burn_with_match` uses the unscaled burn amount (135,280,454 and 498,998,889,124) directly instead of calculating the scaled amounts (118,788,760 and 433,079,249,164). The unscaled amount represents the debt in underlying units, but the position balance is tracked in scaled units.

The fix applies the same TokenMath calculation used for single liquidations: `get_debt_burn_scaled_amount(unscaled_amount, borrow_index)`. This converts the unscaled debtToCover to the proper scaled amount for balance subtraction.

**Changes Applied:**
1. Modified `_process_debt_burn_with_match` in `src/degenbot/cli/aave.py` (lines 3627-3642)
2. Multi-liquidations now use TokenMath: `get_debt_burn_scaled_amount(amount + balance_increase, index)`
3. Single liquidations continue to use TokenMath (already fixed in 0058)
4. Updated the misleading comment that claimed "already scaled"

**Verification:**
```bash
uv run degenbot aave update
```
✅ Block 21762809 now processes correctly with matching contract balances
✅ Original issue (367,261,936,148 vs 433,198,067,802) is resolved

**Status:** ✅ RESOLVED

**Note:** A subsequent failure was discovered at block 21762841, which is a separate issue that will be investigated in the next session.
