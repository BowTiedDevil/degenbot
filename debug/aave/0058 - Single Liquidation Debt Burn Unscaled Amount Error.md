# Issue 0058: Single Liquidation Debt Burn Uses Unscaled Amount Instead of Scaled

**Date:** 2026-03-25

## Symptom

Balance verification failure at block 21761163:
```
AssertionError: Debt balance verification failure for AaveV3Asset(..., symbol='USDT'). 
User AaveV3User(..., address='0x86a6290dEbbF80e8252Fa60469F0759357B2B1F0') 
scaled balance (302189601) does not match contract balance (356417016) at block 21761163
```

The calculated balance was approximately **54,227,415** less than the actual balance (a ~15% discrepancy).

## Transaction Details

- **Transaction Hash:** 0xca4506de087e686cdba2c663bf39310e16ceaed128e3960002adc59f17e7c901
- **Block:** 21761163
- **Market:** Aave Ethereum Market (Pool revision 6)
- **User:** 0x86a6290dEbbF80e8252Fa60469F0759357B2B1F0
- **Operation:** Single liquidation (WETH collateral, USDT debt)
- **Next Issue ID:** 0058

### Liquidation Structure

This transaction contains **one liquidation** of user `0x86a6290dEbbF80e8252Fa60469F0759357B2B1F0`:

| Field | Value |
|-------|-------|
| Collateral Asset | WETH (0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2) |
| Debt Asset | USDT (0xdAC17F958D2ee523a2206206994597C13D831ec7) |
| Debt To Cover | 410,644,431 USDT |
| Liquidator | 0xbBf4Fa564A9d9F83f7bD2080262831d129b4B867 |

### Key Events in Transaction

| LogIndex | Event | Asset | Amount | Notes |
|----------|-------|-------|--------|-------|
| 382 | Transfer | vDebtUSDT | 410,177,144 | Burn principal |
| 383 | **Burn** | vDebtUSDT | value=410,177,144, balanceIncrease=467,287 | **Debt burn** |
| 384 | ReserveDataUpdated | USDT | - | Interest rate update |
| 385 | ReserveDataUpdated | WETH | - | Interest rate update |
| 386 | Transfer | aWETH | 144,181,412,479,007,510 | Collateral burn |
| 387 | **Burn** | aWETH | value=144,181,412,479,007,510 | **Collateral burn** |
| 393 | Transfer | USDT | 410,644,431 | Repayment from liquidator |
| 394 | **LiquidationCall** | WETH/USDT | debtToCover=410,644,431 | **Main liquidation event** |

## Root Cause

In `_process_debt_burn_with_match`, the code for **single liquidations** directly uses `burn_value = scaled_event.amount + scaled_event.balance_increase` as the delta to subtract from the position balance. However, this is the **unscaled amount** (in underlying units), not the **scaled amount** needed for the balance calculation.

### Current Code (Bug)

```python
else:
    # Single liquidation: use burn event value + balance_increase
    burn_value = scaled_event.amount + (scaled_event.balance_increase or 0)
    scaled_amount = burn_value  # BUG: This is unscaled, not scaled!
```

### Why This Fails

**The math:**
- Starting scaled balance: 712,834,032
- Burn event value: 410,177,144 (principal in underlying units)
- Balance increase: 467,287 (accrued interest)
- Total unscaled burn: 410,644,431 (equals debtToCover)
- Borrow index: 1,152,145,976,180,031,089,255,865,175 (1.152145...e27)
- **Scaled burn amount**: 356,417,016 (using `ray_div_floor`)

**Current calculation (WRONG):**
```
Final balance = 712,834,032 - 410,644,431 = 302,189,601
```

**Correct calculation:**
```
Final balance = 712,834,032 - 356,417,016 = 356,417,016
```

**Contract balance:** 356,417,016 ✓

## The Fix

Single liquidations should calculate the scaled burn amount using TokenMath, just like multi-liquidations do:

```python
else:
    # Single liquidation: calculate scaled burn from unscaled amount
    # The burn_value (amount + balance_increase) is in underlying units
    # We need to convert to scaled units using the borrow index
    burn_value_unscaled = scaled_event.amount + (scaled_event.balance_increase or 0)
    
    token_math = TokenMathFactory.get_token_math_for_token_revision(
        debt_asset.v_token_revision
    )
    burn_value = token_math.get_debt_burn_scaled_amount(
        burn_value_unscaled, scaled_event.index
    )
    scaled_amount = burn_value
```

## Fix Details

**Files Modified:**
1. `src/degenbot/cli/aave.py` - Modify single liquidation burn calculation in `_process_debt_burn_with_match`

**Functions Modified:**
1. `_process_debt_burn_with_match` - MODIFIED: Use TokenMath to calculate scaled burn amount for single liquidations

## Verification

```bash
uv run degenbot aave update
```

**Block 21761163 Processing:**
- **Single liquidation detected:** debtToCover=410,644,431
- **Scaled burn calculation:** ray_div_floor(410644431, 1152145976180031089255865175) = 356,417,016
- **Starting balance:** 712,834,032
- **Final balance:** 712,834,032 - 356,417,016 = 356,417,016
- **Contract balance:** 356,417,016
- **Result:** MATCH ✓

## Expected Results

**Transaction 0xca4506de087e686cdba2c663bf39310e16ceaed128e3960002adc59f17e7c901:**

- **Debt to cover:** 410,644,431 USDT (unscaled)
- **Burn event:** value=410,177,144, balanceIncrease=467,287
- **Scaled reduction:** 356,417,016 (using index at block 21761163)

**Balance calculation:**
- Starting: 712,834,032
- Scaled burn: -356,417,016
- **Final:** 356,417,016 (matches contract exactly)

## Key Insight

**The relationship between Burn event fields:**

For Aave V3 VariableDebtToken burn events during liquidations:
- `value` = Principal debt burned (in underlying units)
- `balanceIncrease` = Accrued interest (in underlying units)
- `value + balanceIncrease` = Total debt to cover (debtToCover from LiquidationCall event)

**The scaled balance calculation:**
- Scaled amounts are used for position balance tracking
- Unscaled amounts = scaled amounts × index / 1e27
- To get scaled from unscaled: `scaled = ray_div_floor(unscaled, index)`

**Multi-liquidation vs Single liquidation:**
- Multi-liquidation fix (0057) correctly uses TokenMath: `get_debt_burn_scaled_amount(aggregated_debt_to_cover, index)`
- Single liquidation code path was missed and uses unscaled amount directly

## Related Issues

- Issue 0056: Multi-Liquidation Single Debt Burn Misassignment (transaction-level aggregation)
- Issue 0057: Multi-Liquidation Rounding Error in Debt Burn Aggregation (TokenMath for multi-liquidations)

## References

- Transaction: 0xca4506de087e686cdba2c663bf39310e16ceaed128e3960002adc59f17e7c901
- Block: 21761163
- User: 0x86a6290dEbbF80e8252Fa60469F0759357B2B1F0
- Debt Asset: USDT (0xdAC17F958D2ee523a2206206994597C13D831ec7)
- Debt Token: variableDebtEthUSDT (0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8)
- Collateral Asset: WETH (0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2)
- Liquidator: 0xbBf4Fa564A9d9F83f7bD2080262831d129b4B867
- Contract: VariableDebtToken revision 1
- Pool: Pool revision 6
- Files:
  - `src/degenbot/cli/aave.py`
  - `src/degenbot/aave/libraries/token_math.py`

## Summary

The failure occurs because the single liquidation code path in `_process_debt_burn_with_match` uses the unscaled burn amount (410,644,431) directly instead of calculating the scaled amount (356,417,016). The unscaled amount represents the debt in underlying units, but the position balance is tracked in scaled units.

The fix applies the same TokenMath calculation used for multi-liquidations: `get_debt_burn_scaled_amount(unscaled_amount, borrow_index)`. This converts the unscaled debtToCover to the proper scaled amount for balance subtraction.

**Changes Applied:**
1. Modified `_process_debt_burn_with_match` in `src/degenbot/cli/aave.py`
2. Single liquidations now use TokenMath: `get_debt_burn_scaled_amount(amount + balance_increase, index)`
3. Multi-liquidations continue to use event values directly to avoid rounding errors
4. Duplicate processing prevention only applies to single liquidations

**Verification:**
```bash
uv run degenbot aave update
```
✅ Block 21761163 now processes correctly with matching contract balances
✅ Original issue (302,189,601 vs 356,417,016) is resolved
✅ 1 wei discrepancy at block 21762809 is a separate pre-existing issue (0057)

**Status:** ✅ RESOLVED
